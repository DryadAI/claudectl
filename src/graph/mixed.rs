//! Durable non-agent nodes use supervisor tasks, never a second scheduler.
//! Claim before execution; a missing result is uncertain, never replayable.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compiler::CompiledGraph;
use super::provider::ProviderRegistry;
use super::runner::EvidenceRecord;
use super::schema::{ExecutorSpec, NodeKind, NodeSpec, Outcome, RiskLevel, WorkspaceMode};
use crate::coord::tasks::{self, TaskRow, TaskState};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedNode {
    node: NodeSpec,
    pub cwd: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    version: u32,
    compiled: CompiledGraph,
    nodes: BTreeMap<String, PreparedNode>,
    task_ids: BTreeMap<String, String>,
    allow_unsandboxed_shell: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub digest: String,
    pub approved: bool,
    pub actor: String,
    pub reason: String,
    pub decided_at: String,
}

pub(crate) fn prepare(
    compiled: &CompiledGraph,
    allow_shell: bool,
) -> Result<BTreeMap<String, PreparedNode>, String> {
    if compiled.spec.edges.iter().any(|e| !e.dependency_only) {
        return Err(
            "durable execution supports dependency-only edges, not artifact transfer".into(),
        );
    }
    let base = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut nodes = BTreeMap::new();
    for original in &compiled.spec.nodes {
        let mut node = original.clone();
        let mut cwd = base.clone();
        if node.kind == NodeKind::Subgraph {
            return Err("durable subgraph execution is not supported".into());
        }
        if node.kind != NodeKind::Agent && node.workspace.mode != WorkspaceMode::Shared {
            return Err(format!(
                "non-agent node '{}' requires shared workspace; worktree handoff is not supported",
                node.id
            ));
        }
        if node.kind != NodeKind::Approval
            && matches!(node.risk, RiskLevel::Recoverable | RiskLevel::Irreversible)
            && !has_approval_ancestor(compiled, &node.id)
        {
            return Err(format!(
                "risky durable node '{}' requires an upstream approval",
                node.id
            ));
        }
        match node.kind {
            NodeKind::Agent => {
                let Some(ExecutorSpec::Provider { request, .. }) = &node.executor else {
                    return Err("agent executor missing".into());
                };
                cwd = std::fs::canonicalize(
                    request
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .ok_or("agent cwd missing")?,
                )
                .map_err(|e| format!("resolve agent cwd: {e}"))?;
            }
            NodeKind::Approval => {
                if node.executor.is_some() || node.requires.iter().any(|r| r != "human.approval") {
                    return Err(format!(
                        "approval '{}' must use only human.approval and no executor",
                        node.id
                    ));
                }
            }
            _ => match node.executor.as_mut() {
                Some(ExecutorSpec::Shell {
                    cwd: execution_cwd, ..
                }) => {
                    if !allow_shell {
                        return Err("shell nodes require --allow-unsandboxed-shell; declared authority is NOT a shell sandbox".into());
                    }
                    cwd = std::fs::canonicalize(
                        execution_cwd.as_deref().map(Path::new).unwrap_or(&base),
                    )
                    .map_err(|e| format!("resolve shell cwd: {e}"))?;
                    if !cwd.is_dir() {
                        return Err("shell cwd must be a directory".into());
                    }
                    *execution_cwd = Some(cwd.to_string_lossy().into_owned());
                }
                Some(ExecutorSpec::Provider {
                    capability,
                    request,
                    ..
                }) => {
                    if capability == super::durable::AGENT_EXECUTE_CAPABILITY {
                        return Err("agent.execute requires an agent node and its worktree authority checks".into());
                    }
                    // REA resolves filesystem targets itself. Require absolute inputs so a
                    // daemon's cwd cannot change their meaning; do not rewrite arbitrary
                    // provider request payloads or widen declared authority.
                    for scope in node
                        .authority
                        .filesystem_read
                        .iter()
                        .chain(&node.authority.filesystem_write)
                    {
                        if !Path::new(scope).is_absolute() {
                            return Err(
                                "durable provider filesystem scopes must be absolute".into()
                            );
                        }
                    }
                    if let Some(target) = request.get("target").and_then(|v| v.as_str())
                        && !Path::new(target).is_absolute()
                    {
                        return Err("durable provider target must be absolute".into());
                    }
                }
                None => return Err(format!("node '{}' has no executor", node.id)),
            },
        }
        nodes.insert(
            node.id.clone(),
            PreparedNode {
                node,
                cwd: cwd.to_string_lossy().into_owned(),
            },
        );
    }
    Ok(nodes)
}

fn has_approval_ancestor(compiled: &CompiledGraph, id: &str) -> bool {
    let mut todo = vec![id.to_owned()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for parent in compiled.dependencies.get(&id).into_iter().flatten() {
            if compiled
                .spec
                .nodes
                .iter()
                .any(|n| n.id == *parent && n.kind == NodeKind::Approval)
            {
                return true;
            }
            todo.push(parent.clone());
        }
    }
    false
}

pub(crate) fn save_snapshot(
    conn: &Connection,
    run: &str,
    compiled: &CompiledGraph,
    nodes: &BTreeMap<String, PreparedNode>,
    task_ids: &BTreeMap<String, String>,
    allow_unsandboxed_shell: bool,
) -> Result<String, String> {
    let snapshot = Snapshot {
        version: 1,
        compiled: compiled.clone(),
        nodes: nodes.clone(),
        task_ids: task_ids.clone(),
        allow_unsandboxed_shell,
    };
    let json = serde_json::to_string(&snapshot).map_err(|e| e.to_string())?;
    let digest = format!("{:x}", Sha256::digest(json.as_bytes()));
    conn.execute(
        "INSERT INTO graph_snapshots(run_id, digest, snapshot_json) VALUES (?1, ?2, ?3)",
        params![run, digest, json],
    )
    .map_err(|e| e.to_string())?;
    Ok(digest)
}

pub(crate) fn snapshot(conn: &Connection, run: &str) -> Result<Option<(String, Snapshot)>, String> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT digest, snapshot_json FROM graph_snapshots WHERE run_id=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((digest, json)) = row else {
        return Ok(None);
    };
    if digest != format!("{:x}", Sha256::digest(json.as_bytes())) {
        return Err("stored graph snapshot digest mismatch".into());
    }
    let snapshot: Snapshot = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    if snapshot.version != 1 {
        return Err("unsupported durable snapshot version".into());
    }
    Ok(Some((digest, snapshot)))
}

fn prepared_task(conn: &Connection, task: &TaskRow) -> Result<Option<PreparedNode>, String> {
    let Some(run) = &task.graph_run_id else {
        return Ok(None);
    };
    let Some((_, snapshot)) = snapshot(conn, run)? else {
        if task
            .policy
            .as_ref()
            .and_then(|p| p.pointer("/graph/snapshot_version"))
            .is_some()
        {
            return Err("graph task is missing its required snapshot".into());
        }
        return Ok(None); // Legacy agent runs only.
    };
    let node_id = task
        .graph_node_id
        .as_ref()
        .ok_or("graph task missing node identity")?;
    if snapshot.task_ids.get(node_id) != Some(&task.id) {
        return Err("graph task identity does not match snapshot".into());
    }
    let expected: Vec<_> = snapshot
        .compiled
        .dependencies
        .get(node_id)
        .into_iter()
        .flatten()
        .map(|id| {
            snapshot
                .task_ids
                .get(id)
                .cloned()
                .ok_or("missing dependency identity")
        })
        .collect::<Result<_, _>>()?;
    if expected != task.depends_on {
        return Err("task dependencies differ from stored graph".into());
    }
    let prepared = snapshot
        .nodes
        .get(node_id)
        .ok_or("graph snapshot missing node")?
        .clone();
    if task.cwd != prepared.cwd {
        return Err("task cwd differs from stored graph".into());
    }
    Ok(Some(prepared))
}

pub(crate) fn node_kind(conn: &Connection, task: &TaskRow) -> Result<Option<NodeKind>, String> {
    Ok(prepared_task(conn, task)?.map(|p| p.node.kind))
}

pub(crate) fn dependencies_done(conn: &Connection, task: &TaskRow) -> Result<bool, String> {
    for id in &task.depends_on {
        if !tasks::get_task(conn, id)?.is_some_and(|t| t.state == TaskState::Done) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn evidence(conn: &Connection, task: &str) -> Result<Vec<EvidenceRecord>, String> {
    let mut statement = conn.prepare("SELECT e.record_json FROM graph_node_evidence e JOIN task_attempts a ON a.id=e.attempt_id WHERE a.task_id=?1 ORDER BY a.attempt_num").map_err(|e| e.to_string())?;
    statement
        .query_map([task], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .map(|row| {
            serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        })
        .collect()
}

pub(crate) fn approval(conn: &Connection, task: &str) -> Result<Option<ApprovalDecision>, String> {
    conn.query_row(
        "SELECT digest, approved, actor, reason, decided_at FROM graph_approvals WHERE task_id=?1",
        [task],
        |r| {
            Ok(ApprovalDecision {
                digest: r.get(0)?,
                approved: r.get(1)?,
                actor: r.get(2)?,
                reason: r.get(3)?,
                decided_at: r.get(4)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// Enforce graph-only completion paths even through generic operator controls.
pub(crate) fn guard_transition(
    conn: &Connection,
    task_id: &str,
    from: TaskState,
    to: TaskState,
) -> Result<(), String> {
    let Some(task) = tasks::get_task(conn, task_id)? else {
        return Ok(());
    };
    let Some(kind) = node_kind(conn, &task)? else {
        return Ok(());
    };
    if kind == NodeKind::Agent {
        if matches!(
            to,
            TaskState::Assigned
                | TaskState::Running
                | TaskState::Resuming
                | TaskState::Verifying
                | TaskState::Done
        ) && !dependencies_done(conn, &task)?
        {
            return Err("graph agent dependencies are not complete".into());
        }
        return Ok(());
    }
    if to == TaskState::Cancelled {
        return Ok(());
    }
    let valid = if kind == NodeKind::Approval {
        match (from, to) {
            (TaskState::Pending, TaskState::AwaitingApproval) => dependencies_done(conn, &task)?,
            (TaskState::Pending, TaskState::NeedsHuman) => true, // blocked dependency
            (TaskState::AwaitingApproval, TaskState::Done | TaskState::NeedsHuman) => {
                let digest = snapshot(conn, task.graph_run_id.as_deref().ok_or("missing run")?)?
                    .ok_or("missing snapshot")?
                    .0;
                approval(conn, task_id)?
                    .is_some_and(|d| d.digest == digest && d.approved == (to == TaskState::Done))
                    && dependencies_done(conn, &task)?
            }
            _ => false,
        }
    } else {
        match (from, to) {
            (TaskState::Pending | TaskState::Retrying, TaskState::Running) => {
                dependencies_done(conn, &task)?
            }
            (TaskState::Pending | TaskState::Retrying, TaskState::NeedsHuman) => true,
            (TaskState::Running, TaskState::Done | TaskState::Retrying | TaskState::NeedsHuman) => {
                let count = tasks::attempt_count(conn, task_id)?;
                evidence(conn, task_id)?.last().is_some_and(|e| {
                    e.attempt == count
                        && match to {
                            TaskState::Done => e.outcome == Outcome::Pass,
                            TaskState::Retrying => {
                                matches!(e.outcome, Outcome::Fail | Outcome::Error)
                                    && count <= task.max_retries
                            }
                            _ => e.outcome != Outcome::Pass,
                        }
                })
            }
            _ => false,
        }
    };
    if valid {
        Ok(())
    } else {
        Err("graph transition requires its durable evidence/approval path; resubmit a new run to retry an uncertain or terminal node".into())
    }
}

pub(crate) fn await_approval(conn: &mut Connection, task_id: &str) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let task = tasks::get_task(&tx, task_id)?.ok_or("task not found")?;
    if node_kind(&tx, &task)? != Some(NodeKind::Approval) {
        return Err("not an approval node".into());
    }
    if task.state != TaskState::Pending || !dependencies_done(&tx, &task)? {
        return Ok(());
    }
    tasks::transition_in_transaction(
        &tx,
        task_id,
        task.state,
        TaskState::AwaitingApproval,
        "graph-awaiting-approval",
    )?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn decide(
    run: &str,
    node: &str,
    digest: &str,
    approved: bool,
    actor: &str,
    reason: &str,
) -> Result<(), String> {
    let mut conn = crate::coord::store::open()?;
    decide_with_connection(
        &mut conn,
        run,
        node,
        &ApprovalDecision {
            digest: digest.into(),
            approved,
            actor: actor.into(),
            reason: reason.into(),
            decided_at: crate::logger::timestamp_now(),
        },
    )
}

pub(crate) fn decide_with_connection(
    conn: &mut Connection,
    run: &str,
    node: &str,
    decision: &ApprovalDecision,
) -> Result<(), String> {
    if decision.actor.trim().is_empty() || decision.reason.trim().is_empty() {
        return Err("approval requires nonempty actor and reason".into());
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let (digest, snapshot) = snapshot(&tx, run)?.ok_or("run has no approval snapshot")?;
    if decision.digest != digest {
        return Err("approval digest does not match this run".into());
    }
    let task_id = snapshot.task_ids.get(node).ok_or("node not found in run")?;
    let task = tasks::get_task(&tx, task_id)?.ok_or("task not found")?;
    if node_kind(&tx, &task)? != Some(NodeKind::Approval)
        || task.state != TaskState::AwaitingApproval
        || !dependencies_done(&tx, &task)?
    {
        return Err("node is not awaiting approval with completed dependencies".into());
    }
    tx.execute("INSERT INTO graph_approvals(task_id,digest,approved,actor,reason,decided_at) VALUES (?1,?2,?3,?4,?5,?6)", params![task_id, digest, decision.approved, decision.actor, decision.reason, decision.decided_at]).map_err(|e| e.to_string())?;
    tasks::transition_in_transaction(
        &tx,
        task_id,
        task.state,
        if decision.approved {
            TaskState::Done
        } else {
            TaskState::NeedsHuman
        },
        "graph-human-decision",
    )?;
    tx.commit().map_err(|e| e.to_string())
}

struct Claim {
    task: TaskRow,
    node: NodeSpec,
    attempt_id: String,
    attempt: u32,
    fingerprint: String,
}

fn claim(
    conn: &mut Connection,
    task_id: &str,
    expected_attempt: u32,
) -> Result<Option<Claim>, String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let task = tasks::get_task(&tx, task_id)?.ok_or("task not found")?;
    let prepared = prepared_task(&tx, &task)?.ok_or("task has no execution snapshot")?;
    if matches!(prepared.node.kind, NodeKind::Agent | NodeKind::Approval) {
        return Err("node cannot use deterministic execution".into());
    }
    if !matches!(task.state, TaskState::Pending | TaskState::Retrying)
        || !dependencies_done(&tx, &task)?
    {
        return Ok(None);
    }
    let (fingerprint, snapshot) =
        snapshot(&tx, task.graph_run_id.as_deref().ok_or("missing run")?)?
            .ok_or("missing snapshot")?;
    if matches!(prepared.node.executor, Some(ExecutorSpec::Shell { .. }))
        && !snapshot.allow_unsandboxed_shell
    {
        return Err("shell execution was not authorized at submission".into());
    }
    let attempt = tasks::attempt_count(&tx, task_id)? + 1;
    if attempt != expected_attempt {
        return Ok(None);
    }
    if attempt > prepared.node.retry.max_attempts {
        return Err("attempt budget exhausted".into());
    }
    let attempt_id = tasks::insert_attempt(&tx, task_id, attempt, None, None, None)?;
    tasks::transition_in_transaction(
        &tx,
        task_id,
        task.state,
        TaskState::Running,
        "graph-attempt-claimed",
    )?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Some(Claim {
        task,
        node: prepared.node,
        attempt_id,
        attempt,
        fingerprint,
    }))
}

fn finish(conn: &mut Connection, claim: &Claim, record: &EvidenceRecord) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO graph_node_evidence(attempt_id,record_json) VALUES (?1,?2)",
        params![
            claim.attempt_id,
            serde_json::to_string(record).map_err(|e| e.to_string())?
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE task_attempts SET ended_at=?1,outcome=?2 WHERE id=?3",
        params![
            crate::logger::timestamp_now(),
            format!("{:?}", record.outcome).to_uppercase(),
            claim.attempt_id
        ],
    )
    .map_err(|e| e.to_string())?;
    let task = tasks::get_task(&tx, &claim.task.id)?.ok_or("task disappeared")?;
    // Cancellation is not process termination. Preserve late evidence but never
    // turn a cancelled task (and its dependents) into a success.
    if task.state == TaskState::Running {
        let next = match record.outcome {
            Outcome::Pass => TaskState::Done,
            Outcome::Fail | Outcome::Error if claim.attempt < claim.node.retry.max_attempts => {
                TaskState::Retrying
            }
            _ => TaskState::NeedsHuman,
        };
        tasks::transition_in_transaction(&tx, &task.id, task.state, next, "graph-attempt-result")?;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn execute(
    conn: &mut Connection,
    task_id: &str,
    expected_attempt: u32,
) -> Result<(), String> {
    let rea = super::rea::ReaCliProvider;
    let mut registry = ProviderRegistry::new();
    registry.register(&rea);
    let home = std::env::var_os("HOME").ok_or("HOME is required for durable graph evidence")?;
    execute_with_registry(
        conn,
        task_id,
        expected_attempt,
        &registry,
        &PathBuf::from(home).join(".claudectl/coord/graph-evidence"),
    )
}

fn execute_with_registry(
    conn: &mut Connection,
    task_id: &str,
    expected_attempt: u32,
    registry: &ProviderRegistry<'_>,
    base: &Path,
) -> Result<(), String> {
    let Some(claim) = claim(conn, task_id, expected_attempt)? else {
        return Ok(());
    };
    // All path components are internal IDs, never user node labels.
    let run_dir = base.join(&claim.attempt_id);
    std::fs::create_dir_all(&run_dir).map_err(|e| {
        format!(
            "attempt claimed but evidence directory unavailable; execution remains UNKNOWN: {e}"
        )
    })?;
    let record = super::runner::execute_once(
        &claim.node,
        &claim.fingerprint,
        claim.attempt,
        registry,
        &run_dir,
    )
    .map_err(|e| format!("attempt result uncertain; no automatic replay: {e}"))?;
    finish(conn, &claim, &record)
}

#[cfg(test)]
mod tests;
