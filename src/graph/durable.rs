use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::compiler::CompiledGraph;
use super::schema::{ExecutorSpec, NodeKind, NodeSpec, Outcome, WorkspaceMode};
use crate::coord::store;
use crate::coord::tasks::{self, NewTask};
use crate::coord::verify::Verifier;

pub const AGENT_EXECUTE_CAPABILITY: &str = "agent.execute";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentTaskRequest {
    cwd: String,
    prompt: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    budget_usd: Option<f64>,
    #[serde(default)]
    timeout_min: Option<u32>,
    #[serde(default)]
    verify: Vec<Verifier>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurableSubmission {
    pub graph_run_id: String,
    pub graph_id: String,
    pub fingerprint: String,
    pub approval_digest: String,
    pub task_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurableNodeStatus {
    pub node_id: String,
    pub task_id: String,
    pub state: String,
    pub attempts: u32,
    pub outcome: Outcome,
    pub latest_verification: Option<(String, String)>,
    pub evidence: Vec<super::runner::EvidenceRecord>,
    pub approval: Option<super::mixed::ApprovalDecision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurableRunStatus {
    pub graph_run_id: String,
    pub graph_id: String,
    pub fingerprint: String,
    pub approval_digest: Option<String>,
    pub stored_state: String,
    pub effective_state: String,
    pub outcome: Outcome,
    pub nodes: Vec<DurableNodeStatus>,
}

pub fn submit(
    compiled: &CompiledGraph,
    allow_unsandboxed_shell: bool,
) -> Result<DurableSubmission, String> {
    let mut conn = store::open()?;
    submit_with_options(&mut conn, compiled, allow_unsandboxed_shell)
}

pub fn status(graph_run_id: &str) -> Result<DurableRunStatus, String> {
    let conn = store::open()?;
    status_with_connection(&conn, graph_run_id)
}

pub(crate) fn status_with_connection(
    conn: &Connection,
    graph_run_id: &str,
) -> Result<DurableRunStatus, String> {
    let run = conn
        .query_row(
            "SELECT graph_id, fingerprint, state FROM graph_runs WHERE id = ?1",
            params![graph_run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("query graph run: {e}"))?
        .ok_or_else(|| format!("graph run '{graph_run_id}' not found"))?;

    let tasks = tasks::list_tasks_for_graph_run(conn, graph_run_id)?;
    let mut nodes = Vec::with_capacity(tasks.len());
    for task in &tasks {
        let latest_verification = tasks::latest_verification(conn, &task.id)?;
        let evidence = super::mixed::evidence(conn, &task.id)?;
        let approval = super::mixed::approval(conn, &task.id)?;
        let outcome = if task.state == tasks::TaskState::NeedsHuman {
            if approval.as_ref().is_some_and(|decision| !decision.approved) {
                Outcome::Fail
            } else {
                evidence
                    .last()
                    .map(|e| e.outcome)
                    .unwrap_or_else(|| task_outcome(task.state, latest_verification.as_ref()))
            }
        } else {
            task_outcome(task.state, latest_verification.as_ref())
        };
        nodes.push(DurableNodeStatus {
            node_id: task
                .graph_node_id
                .clone()
                .unwrap_or_else(|| task.name.clone()),
            task_id: task.id.clone(),
            state: task.state.as_str().to_string(),
            attempts: tasks::attempt_count(conn, &task.id)?,
            outcome,
            latest_verification,
            evidence,
            approval,
        });
    }

    let effective_state = if tasks.is_empty() {
        "EMPTY"
    } else if tasks
        .iter()
        .all(|task| task.state == tasks::TaskState::Done)
    {
        "DONE"
    } else if tasks
        .iter()
        .any(|task| task.state == tasks::TaskState::NeedsHuman)
    {
        "NEEDS_HUMAN"
    } else if tasks
        .iter()
        .any(|task| task.state == tasks::TaskState::Cancelled)
    {
        "CANCELLED"
    } else if tasks.iter().any(|task| {
        matches!(
            task.state,
            tasks::TaskState::Assigned
                | tasks::TaskState::Running
                | tasks::TaskState::Verifying
                | tasks::TaskState::Retrying
                | tasks::TaskState::Resuming
        )
    }) {
        "RUNNING"
    } else if tasks
        .iter()
        .any(|task| task.state == tasks::TaskState::AwaitingApproval)
    {
        "AWAITING_APPROVAL"
    } else {
        "PENDING"
    }
    .to_string();

    let outcome = if nodes.iter().any(|node| node.outcome == Outcome::Error) {
        Outcome::Error
    } else if nodes.iter().any(|node| node.outcome == Outcome::Fail) {
        Outcome::Fail
    } else if !nodes.is_empty() && nodes.iter().all(|node| node.outcome == Outcome::Pass) {
        Outcome::Pass
    } else {
        Outcome::Unknown
    };

    Ok(DurableRunStatus {
        graph_run_id: graph_run_id.to_string(),
        graph_id: run.0,
        fingerprint: run.1,
        approval_digest: super::mixed::snapshot(conn, graph_run_id)?.map(|(digest, _)| digest),
        stored_state: run.2,
        effective_state,
        outcome,
        nodes,
    })
}

fn task_outcome(
    state: tasks::TaskState,
    latest_verification: Option<&(String, String)>,
) -> Outcome {
    match state {
        tasks::TaskState::Done => Outcome::Pass,
        tasks::TaskState::Cancelled => Outcome::Error,
        tasks::TaskState::NeedsHuman => {
            if latest_verification.is_some_and(|(_, verdict)| verdict.eq_ignore_ascii_case("FAIL"))
            {
                Outcome::Fail
            } else {
                Outcome::Unknown
            }
        }
        _ => Outcome::Unknown,
    }
}

#[cfg(test)]
fn submit_with_connection(
    conn: &mut Connection,
    compiled: &CompiledGraph,
) -> Result<DurableSubmission, String> {
    submit_with_options(conn, compiled, false)
}

pub(crate) fn submit_with_options(
    conn: &mut Connection,
    compiled: &CompiledGraph,
    allow_unsandboxed_shell: bool,
) -> Result<DurableSubmission, String> {
    let rebuilt = super::compiler::compile(compiled.spec.clone()).map_err(|e| e.to_string())?;
    if serde_json::to_value(&rebuilt).map_err(|e| e.to_string())?
        != serde_json::to_value(compiled).map_err(|e| e.to_string())?
    {
        return Err("compiled graph does not match canonical compilation".into());
    }
    validate_durable_graph(compiled)?;
    let prepared = super::mixed::prepare(compiled, allow_unsandboxed_shell)?;

    let graph_run_id = store::gen_id("graph");
    let task_ids: BTreeMap<String, String> = compiled
        .spec
        .nodes
        .iter()
        .map(|node| (node.id.clone(), store::gen_id("task")))
        .collect();

    let tx = conn
        .transaction()
        .map_err(|e| format!("begin graph submission transaction: {e}"))?;
    let now = crate::logger::timestamp_now();
    tx.execute(
        "INSERT INTO graph_runs (id, graph_id, fingerprint, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'PENDING', ?4, ?4)",
        params![graph_run_id, compiled.spec.id, compiled.fingerprint, now,],
    )
    .map_err(|e| format!("insert graph run: {e}"))?;

    let approval_digest = super::mixed::save_snapshot(
        &tx,
        &graph_run_id,
        compiled,
        &prepared,
        &task_ids,
        allow_unsandboxed_shell,
    )?;

    for node in &compiled.spec.nodes {
        let request = if node.kind == NodeKind::Agent {
            agent_request(node)?
        } else {
            AgentTaskRequest {
                cwd: prepared[&node.id].cwd.clone(),
                prompt: node.description.clone().unwrap_or_else(|| node.id.clone()),
                role: None,
                model: None,
                budget_usd: None,
                timeout_min: None,
                verify: vec![],
            }
        };
        let source_cwd = std::fs::canonicalize(&request.cwd).map_err(|e| {
            format!(
                "agent node '{}' cannot resolve request.cwd '{}': {e}",
                node.id, request.cwd
            )
        })?;
        if !source_cwd.is_dir() {
            return Err(format!(
                "agent node '{}' request.cwd is not a directory: {}",
                node.id,
                source_cwd.display()
            ));
        }

        let dependency_ids = compiled
            .dependencies
            .get(&node.id)
            .into_iter()
            .flatten()
            .map(|dependency_node| {
                task_ids
                    .get(dependency_node)
                    .cloned()
                    .ok_or_else(|| format!("missing task id for dependency '{dependency_node}'"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let task = NewTask {
            name: format!("{}:{}", compiled.spec.id, node.id),
            role: request.role,
            cwd: source_cwd.to_string_lossy().to_string(),
            prompt: request.prompt,
            model: request.model,
            budget_usd: request.budget_usd,
            max_retries: Some(node.retry.max_attempts.saturating_sub(1)),
            timeout_min: request.timeout_min,
            depends_on: dependency_ids,
            policy: Some(serde_json::json!({
                "graph": {
                    "snapshot_version": 1,
                    "run_id": graph_run_id.as_str(),
                    "graph_id": compiled.spec.id.as_str(),
                    "node_id": node.id.as_str(),
                    "fingerprint": compiled.fingerprint.as_str(),
                },
                "authority": &node.authority,
                "workspace": &node.workspace,
                "risk": node.risk,
                "requires": &node.requires,
            })),
            verifiers: request.verify,
        };

        let task_id = task_ids
            .get(&node.id)
            .ok_or_else(|| format!("missing preallocated task id for node '{}'", node.id))?;
        tasks::insert_task_with_identity(&tx, task_id, Some(&graph_run_id), Some(&node.id), &task)?;
    }

    tx.commit()
        .map_err(|e| format!("commit graph submission: {e}"))?;

    Ok(DurableSubmission {
        graph_run_id,
        graph_id: compiled.spec.id.clone(),
        fingerprint: compiled.fingerprint.clone(),
        approval_digest,
        task_ids,
    })
}

fn validate_durable_graph(compiled: &CompiledGraph) -> Result<(), String> {
    for node in &compiled.spec.nodes {
        if node.kind != NodeKind::Agent {
            continue;
        }
        let ExecutorSpec::Provider {
            capability,
            provider,
            ..
        } = node
            .executor
            .as_ref()
            .ok_or_else(|| format!("agent node '{}' has no executor", node.id))?
        else {
            return Err(format!(
                "agent node '{}' must use a provider executor",
                node.id
            ));
        };
        if capability != AGENT_EXECUTE_CAPABILITY {
            return Err(format!(
                "agent node '{}' must request capability '{}', got '{}'",
                node.id, AGENT_EXECUTE_CAPABILITY, capability
            ));
        }
        if provider
            .as_deref()
            .is_some_and(|name| name != "claude_code")
        {
            return Err(format!(
                "durable supervisor V1 currently executes Claude Code agents; node '{}' prefers unsupported provider '{}'",
                node.id,
                provider.as_deref().unwrap_or_default()
            ));
        }
        let request = agent_request(node)?;
        if request.role.is_some() {
            return Err(format!(
                "durable supervisor V1 requires direct-spawn agents; node '{}' sets a role/mailbox route that can bypass workspace isolation",
                node.id
            ));
        }
        if request.model.is_some() {
            return Err(format!(
                "durable supervisor V1 does not yet enforce per-node model selection; remove request.model from node '{}'",
                node.id
            ));
        }
        if request.budget_usd.is_some() {
            return Err(format!(
                "durable supervisor V1 does not yet enforce per-node agent budgets; remove request.budget_usd from node '{}'",
                node.id
            ));
        }
        if request.timeout_min.is_some() {
            return Err(format!(
                "durable supervisor V1 does not yet enforce per-node wall-clock timeouts; remove request.timeout_min from node '{}'",
                node.id
            ));
        }
        if node.authority.git_commit || node.authority.git_push {
            return Err(format!(
                "durable supervisor V1 disables general Git mutation inside agent sessions; node '{}' must set git_commit=false and git_push=false",
                node.id
            ));
        }

        if node.workspace.mode != WorkspaceMode::Worktree {
            return Err(format!(
                "durable supervisor V1 requires workspace.mode='worktree' for agent node '{}'",
                node.id
            ));
        }
        for path in node
            .authority
            .filesystem_read
            .iter()
            .chain(node.authority.filesystem_write.iter())
        {
            if std::path::Path::new(path).is_absolute() {
                return Err(format!(
                    "durable agent node '{}' has absolute filesystem authority '{}'; V1 worktree scopes must be relative",
                    node.id, path
                ));
            }
        }
    }
    Ok(())
}

fn agent_request(node: &NodeSpec) -> Result<AgentTaskRequest, String> {
    let ExecutorSpec::Provider { request, .. } = node
        .executor
        .as_ref()
        .ok_or_else(|| format!("agent node '{}' has no executor", node.id))?
    else {
        return Err(format!(
            "agent node '{}' must use a provider executor",
            node.id
        ));
    };

    let request: AgentTaskRequest = serde_json::from_value(request.clone())
        .map_err(|e| format!("agent node '{}' request: {e}", node.id))?;
    if request.cwd.trim().is_empty() {
        return Err(format!(
            "agent node '{}' request.cwd must not be empty",
            node.id
        ));
    }
    if request.prompt.trim().is_empty() {
        return Err(format!(
            "agent node '{}' request.prompt must not be empty",
            node.id
        ));
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::compiler;
    use crate::graph::schema::{
        Authority, EdgeEndpoint, EdgeSpec, GraphPolicy, GraphSpec, RetryPolicy, RiskLevel,
    };

    fn agent(id: &str) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind: NodeKind::Agent,
            artifact_paths: vec![],
            description: None,
            requires: vec![AGENT_EXECUTE_CAPABILITY.into()],
            authority: Authority::default(),
            workspace: crate::graph::schema::WorkspaceSpec {
                mode: WorkspaceMode::Worktree,
            },
            risk: RiskLevel::Reversible,
            retry: RetryPolicy { max_attempts: 2 },
            inputs: vec![],
            outputs: vec![],
            executor: Some(ExecutorSpec::Provider {
                capability: AGENT_EXECUTE_CAPABILITY.into(),
                provider: Some("claude_code".into()),
                request: serde_json::json!({
                    "cwd": ".",
                    "prompt": format!("implement {id}"),
                    "verify": [{"kind": "run", "command": "cargo test"}]
                }),
            }),
        }
    }

    #[test]
    fn durable_submission_uses_task_ids_for_dependencies() {
        let a = agent("a");
        let b = agent("b");
        let compiled = compiler::compile(GraphSpec {
            version: 1,
            id: "durable-test".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![a, b],
            edges: vec![EdgeSpec {
                from: EdgeEndpoint {
                    node: "a".into(),
                    port: None,
                },
                to: EdgeEndpoint {
                    node: "b".into(),
                    port: None,
                },
                dependency_only: true,
            }],
        })
        .unwrap();

        let mut conn = store::open_memory();
        let submission = submit_with_connection(&mut conn, &compiled).unwrap();

        let a_id = &submission.task_ids["a"];
        let b_id = &submission.task_ids["b"];
        let a_task = tasks::get_task(&conn, a_id).unwrap().unwrap();
        let b_task = tasks::get_task(&conn, b_id).unwrap().unwrap();

        assert_eq!(
            a_task.graph_run_id.as_deref(),
            Some(submission.graph_run_id.as_str())
        );
        assert_eq!(a_task.graph_node_id.as_deref(), Some("a"));
        assert!(a_task.depends_on.is_empty());
        assert_eq!(b_task.depends_on, vec![a_id.clone()]);
        assert_eq!(b_task.graph_node_id.as_deref(), Some("b"));

        let run_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_runs WHERE id = ?1 AND graph_id = ?2",
                params![submission.graph_run_id, "durable-test"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_count, 1);

        let status = status_with_connection(&conn, &submission.graph_run_id).unwrap();
        assert_eq!(status.effective_state, "PENDING");
        assert_eq!(status.outcome, Outcome::Unknown);
        assert_eq!(status.nodes.len(), 2);

        tasks::transition(
            &mut conn,
            a_id,
            tasks::TaskState::Pending,
            tasks::TaskState::Done,
            "test-done",
        )
        .unwrap();
        tasks::transition(
            &mut conn,
            b_id,
            tasks::TaskState::Pending,
            tasks::TaskState::Done,
            "test-done",
        )
        .unwrap();
        let status = status_with_connection(&conn, &submission.graph_run_id).unwrap();
        assert_eq!(status.effective_state, "DONE");
        assert_eq!(status.stored_state, "DONE");
        assert_eq!(status.outcome, Outcome::Pass);
    }

    #[test]
    fn needs_human_with_failed_verifier_maps_to_fail() {
        assert_eq!(
            task_outcome(
                tasks::TaskState::NeedsHuman,
                Some(&("run".into(), "FAIL".into()))
            ),
            Outcome::Fail
        );
        assert_eq!(
            task_outcome(tasks::TaskState::NeedsHuman, None),
            Outcome::Unknown
        );
        assert_eq!(
            task_outcome(tasks::TaskState::Cancelled, None),
            Outcome::Error
        );
    }

    #[test]
    fn durable_v1_rejects_mailbox_model_and_budget_options() {
        let mut node = agent("direct");
        if let Some(ExecutorSpec::Provider { request, .. }) = node.executor.as_mut() {
            *request = serde_json::json!({
                "cwd": ".",
                "prompt": "inspect",
                "role": "backend"
            });
        }
        let compiled = compiler::compile(GraphSpec {
            version: 1,
            id: "role-rejected".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![node],
            edges: vec![],
        })
        .unwrap();
        let mut conn = store::open_memory();
        assert!(
            submit_with_connection(&mut conn, &compiled)
                .unwrap_err()
                .contains("direct-spawn")
        );

        let mut node = agent("model");
        if let Some(ExecutorSpec::Provider { request, .. }) = node.executor.as_mut() {
            *request = serde_json::json!({
                "cwd": ".",
                "prompt": "inspect",
                "model": "sonnet"
            });
        }
        let compiled = compiler::compile(GraphSpec {
            version: 1,
            id: "model-rejected".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![node],
            edges: vec![],
        })
        .unwrap();
        let mut conn = store::open_memory();
        assert!(
            submit_with_connection(&mut conn, &compiled)
                .unwrap_err()
                .contains("model selection")
        );
    }

    #[test]
    fn durable_v1_rejects_git_mutation_authority() {
        let mut node = agent("git");
        node.authority.git_commit = true;
        let compiled = compiler::compile(GraphSpec {
            version: 1,
            id: "git-rejected".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![node],
            edges: vec![],
        })
        .unwrap();
        let mut conn = store::open_memory();
        assert!(
            submit_with_connection(&mut conn, &compiled)
                .unwrap_err()
                .contains("git_commit=false")
        );
    }

    #[test]
    fn durable_writable_agents_require_worktree_and_relative_scope() {
        let mut node = agent("writer");
        node.workspace.mode = WorkspaceMode::Shared;
        node.authority.filesystem_write = vec!["src/**".into()];
        let compiled = compiler::compile(GraphSpec {
            version: 1,
            id: "writer".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![node.clone()],
            edges: vec![],
        })
        .unwrap();
        let mut conn = store::open_memory();
        let err = submit_with_connection(&mut conn, &compiled).unwrap_err();
        assert!(err.contains("workspace.mode='worktree'"));

        node.workspace.mode = WorkspaceMode::Worktree;
        node.authority.filesystem_write = vec!["/tmp/outside".into()];
        let compiled = compiler::compile(GraphSpec {
            version: 1,
            id: "writer-absolute".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![node],
            edges: vec![],
        })
        .unwrap();
        let mut conn = store::open_memory();
        let err = submit_with_connection(&mut conn, &compiled).unwrap_err();
        assert!(err.contains("absolute filesystem authority"));
    }

    #[test]
    fn durable_submission_rejects_agent_executor_disguised_as_check() {
        let mut node = agent("a");
        node.kind = NodeKind::Check;
        node.workspace.mode = WorkspaceMode::Shared;
        let compiled = compiler::compile(GraphSpec {
            version: 1,
            id: "bad".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![node],
            edges: vec![],
        })
        .unwrap();
        let mut conn = store::open_memory();
        let err = submit_with_connection(&mut conn, &compiled).unwrap_err();
        assert!(err.contains("agent.execute requires an agent node"));
    }
}
