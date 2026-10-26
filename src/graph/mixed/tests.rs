use super::*;
use crate::coord::{
    store,
    supervisor::{Action, ObservedSession, Sensors, Supervisor},
};
use crate::graph::{
    compiler,
    durable::{self, DurableSubmission},
    provider::{Provider, ProviderContext, ProviderResult},
    schema::GraphSpec,
};
use serde_json::{Value, json};

struct Quiet;
impl Sensors for Quiet {
    fn last_hook_event_id(&self) -> i64 {
        0
    }
    fn observed_sessions(&self) -> Vec<ObservedSession> {
        vec![]
    }
}

#[test]
fn published_mixed_example_compiles_and_submits_without_executing() {
    let graph = compiler::compile(
        serde_json::from_str(include_str!(
            "../../../examples/graph-durable-mixed-smoke.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let mut conn = store::open_memory();
    let submission = submit(&mut conn, &graph);
    assert_eq!(submission.task_ids.len(), 5);
    assert!(
        tasks::list_tasks(&conn, None)
            .unwrap()
            .iter()
            .all(|t| t.state == TaskState::Pending)
    );
}

#[test]
fn master_compiler_output_hands_off_to_the_same_durable_ledger() {
    let master = super::super::master::compile(
        serde_json::from_str(include_str!("../../../examples/druid-master-plan-v1.json")).unwrap(),
    )
    .unwrap();
    let mut conn = store::open_memory();
    let submission = submit(&mut conn, &master.execution);
    assert_eq!(submission.graph_id, master.execution.spec.id);
    assert_eq!(submission.fingerprint, master.execution.fingerprint);
    assert_eq!(submission.task_ids.len(), master.execution.spec.nodes.len());
    let status = durable::status_with_connection(&conn, &submission.graph_run_id).unwrap();
    assert_eq!(status.effective_state, "PENDING");
    assert_eq!(status.outcome, Outcome::Unknown);
    assert!(
        status
            .nodes
            .iter()
            .all(|n| n.evidence.is_empty() && n.approval.is_none())
    );
}

fn shell(id: &str, command: &str) -> Value {
    json!({"id":id,"kind":"check","requires":["shell.execute"],"executor":{"kind":"shell","command":command},"retry":{"max_attempts":2}})
}
fn gate() -> Value {
    json!({"id":"approve","kind":"approval","requires":["human.approval"]})
}
fn provider(id: &str) -> Value {
    json!({"id":id,"kind":"tool","requires":["test.read"],"executor":{"kind":"provider","capability":"test.read"},"retry":{"max_attempts":2}})
}
fn graph(nodes: Vec<Value>, edges: &[(&str, &str)]) -> CompiledGraph {
    let spec: GraphSpec = serde_json::from_value(json!({"version":1,"id":"mixed-tests","nodes":nodes,"edges":edges.iter().map(|(a,b)| json!({"from":{"node":a},"to":{"node":b},"dependency_only":true})).collect::<Vec<_>>()})).unwrap();
    compiler::compile(spec).unwrap()
}
fn submit(conn: &mut Connection, graph: &CompiledGraph) -> DurableSubmission {
    durable::submit_with_options(conn, graph, true).unwrap()
}
fn state(conn: &Connection, task: &str) -> TaskState {
    tasks::get_task(conn, task).unwrap().unwrap().state
}
fn tick(conn: &Connection) -> Vec<Action> {
    Supervisor::with_defaults().tick(conn, &Quiet).unwrap()
}
fn run(conn: &mut Connection, task: &str, base: &Path) {
    let expected = tasks::attempt_count(conn, task).unwrap() + 1;
    execute_with_registry(conn, task, expected, &ProviderRegistry::new(), base).unwrap();
}
fn decision(run: &DurableSubmission, approved: bool) -> ApprovalDecision {
    ApprovalDecision {
        digest: run.approval_digest.clone(),
        approved,
        actor: "test-operator".into(),
        reason: "reviewed scoped test plan".into(),
        decided_at: crate::logger::timestamp_now(),
    }
}
fn decide_test(conn: &mut Connection, run: &DurableSubmission, approved: bool) {
    decide_with_connection(conn, &run.graph_run_id, "approve", &decision(run, approved)).unwrap();
}

#[test]
fn mixed_pipeline_uses_supervisor_and_holds_downstream_until_exact_approval() {
    let mut conn = store::open_memory();
    let g = graph(
        vec![
            shell("discover", "printf discovery"),
            gate(),
            shell("verify", "printf verified"),
        ],
        &[("discover", "approve"), ("approve", "verify")],
    );
    let run_id = submit(&mut conn, &g);
    let ids = &run_id.task_ids;
    let base = tempfile::tempdir().unwrap();
    assert_eq!(
        tick(&conn),
        vec![Action::ExecuteGraphNode {
            task_id: ids["discover"].clone(),
            expected_attempt: 1
        }]
    );
    assert!(
        decide_with_connection(
            &mut conn,
            &run_id.graph_run_id,
            "approve",
            &decision(&run_id, true)
        )
        .is_err()
    );
    run(&mut conn, &ids["discover"], base.path());
    assert_eq!(
        tick(&conn),
        vec![Action::AwaitGraphApproval {
            task_id: ids["approve"].clone()
        }]
    );
    await_approval(&mut conn, &ids["approve"]).unwrap();
    assert!(tick(&conn).is_empty());
    let status = durable::status_with_connection(&conn, &run_id.graph_run_id).unwrap();
    assert_eq!(status.stored_state, "AWAITING_APPROVAL");
    assert_eq!(status.effective_state, "AWAITING_APPROVAL");
    assert_eq!(status.outcome, Outcome::Unknown);
    assert_eq!(state(&conn, &ids["verify"]), TaskState::Pending);
    decide_test(&mut conn, &run_id, true);
    assert_eq!(
        tick(&conn),
        vec![Action::ExecuteGraphNode {
            task_id: ids["verify"].clone(),
            expected_attempt: 1
        }]
    );
    run(&mut conn, &ids["verify"], base.path());
    let status = durable::status_with_connection(&conn, &run_id.graph_run_id).unwrap();
    assert_eq!(status.outcome, Outcome::Pass);
    assert_eq!(status.effective_state, "DONE");
    assert_eq!(
        evidence(&conn, &ids["discover"]).unwrap()[0]
            .stdout
            .as_deref(),
        Some("discovery")
    );
    assert_eq!(
        approval(&conn, &ids["approve"]).unwrap().unwrap().actor,
        "test-operator"
    );
    assert!(tick(&conn).is_empty());
}

#[test]
fn approval_rejects_wrong_run_digest_node_early_empty_and_duplicate_decisions() {
    let mut conn = store::open_memory();
    let g = graph(vec![gate(), provider("after")], &[("approve", "after")]);
    let first = submit(&mut conn, &g);
    let other = submit(&mut conn, &g);
    assert_ne!(first.approval_digest, other.approval_digest);
    await_approval(&mut conn, &first.task_ids["approve"]).unwrap();
    for (node, mut d) in [
        ("approve", decision(&other, true)),
        ("after", decision(&first, true)),
        ("missing", decision(&first, true)),
        ("approve", decision(&first, true)),
    ] {
        if node == "approve" && d.digest == first.approval_digest {
            d.actor.clear();
        }
        assert!(decide_with_connection(&mut conn, &first.graph_run_id, node, &d).is_err());
    }
    assert!(
        approval(&conn, &first.task_ids["approve"])
            .unwrap()
            .is_none()
    );
    decide_test(&mut conn, &first, true);
    assert!(
        decide_with_connection(
            &mut conn,
            &first.graph_run_id,
            "approve",
            &decision(&first, true)
        )
        .is_err()
    );
}

#[test]
fn rejected_gate_blocks_dependents_and_generic_completion_cannot_override_it() {
    let mut conn = store::open_memory();
    let r = submit(
        &mut conn,
        &graph(vec![gate(), provider("after")], &[("approve", "after")]),
    );
    let id = &r.task_ids["approve"];
    await_approval(&mut conn, id).unwrap();
    assert!(
        tasks::transition(
            &mut conn,
            id,
            TaskState::AwaitingApproval,
            TaskState::Done,
            "operator-approve"
        )
        .is_err()
    );
    decide_test(&mut conn, &r, false);
    assert!(
        tasks::transition(
            &mut conn,
            id,
            TaskState::NeedsHuman,
            TaskState::Done,
            "operator-approve"
        )
        .is_err()
    );
    assert!(
        tasks::transition(
            &mut conn,
            id,
            TaskState::NeedsHuman,
            TaskState::Pending,
            "operator-retry"
        )
        .is_err()
    );
    assert!(
        matches!(&tick(&conn)[0], Action::EscalateHuman { task_id, .. } if task_id == &r.task_ids["after"])
    );
    assert_eq!(
        durable::status_with_connection(&conn, &r.graph_run_id)
            .unwrap()
            .outcome,
        Outcome::Fail
    );
}

#[test]
fn duplicate_dispatch_runs_shell_once_and_completed_node_survives_reopen() {
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &graph(vec![shell("once", "printf once")], &[]));
    let base = tempfile::tempdir().unwrap();
    run(&mut conn, &r.task_ids["once"], base.path());
    let db = base.path().join("ledger.db");
    conn.execute("VACUUM INTO ?1", [db.to_str().unwrap()])
        .unwrap();
    drop(conn);
    let mut conn = Connection::open(db).unwrap();
    assert!(tick(&conn).is_empty());
    run(&mut conn, &r.task_ids["once"], base.path());
    assert_eq!(tasks::attempt_count(&conn, &r.task_ids["once"]).unwrap(), 1);
    assert_eq!(evidence(&conn, &r.task_ids["once"]).unwrap().len(), 1);
}

#[test]
fn crash_after_claim_stays_unknown_after_reopen_without_replay() {
    let mut conn = store::open_memory();
    let r = submit(
        &mut conn,
        &graph(vec![shell("uncertain", "printf never-run")], &[]),
    );
    let id = &r.task_ids["uncertain"];
    assert!(claim(&mut conn, id, 1).unwrap().is_some());
    let base = tempfile::tempdir().unwrap();
    let db = base.path().join("ledger.db");
    conn.execute("VACUUM INTO ?1", [db.to_str().unwrap()])
        .unwrap();
    drop(conn);
    let mut conn = Connection::open(db).unwrap();
    assert!(tick(&conn).is_empty());
    assert!(claim(&mut conn, id, 1).unwrap().is_none());
    assert_eq!(tasks::attempt_count(&conn, id).unwrap(), 1);
    assert!(evidence(&conn, id).unwrap().is_empty());
    assert_eq!(
        durable::status_with_connection(&conn, &r.graph_run_id)
            .unwrap()
            .outcome,
        Outcome::Unknown
    );
    assert!(
        tasks::transition(
            &mut conn,
            id,
            TaskState::Running,
            TaskState::Pending,
            "retry"
        )
        .is_err()
    );
    tasks::transition(
        &mut conn,
        id,
        TaskState::Running,
        TaskState::Cancelled,
        "operator-cancel",
    )
    .unwrap();
    assert!(claim(&mut conn, id, 1).unwrap().is_none());
}

#[test]
fn concurrent_connections_can_claim_only_one_attempt() {
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &graph(vec![provider("once")], &[]));
    let base = tempfile::tempdir().unwrap();
    let db = base.path().join("ledger.db");
    conn.execute("VACUUM INTO ?1", [db.to_str().unwrap()])
        .unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let worker = || {
            let mut conn = Connection::open(&db).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            barrier.wait();
            claim(&mut conn, &r.task_ids["once"], 1).unwrap().is_some()
        };
        let a = scope.spawn(worker);
        let b = scope.spawn(worker);
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|claimed| **claimed).count(), 1);
}

#[test]
fn failure_retries_are_bounded_and_unknown_never_retries() {
    let mut conn = store::open_memory();
    let r = submit(
        &mut conn,
        &graph(vec![shell("fail", "exit 7"), provider("missing")], &[]),
    );
    let base = tempfile::tempdir().unwrap();
    let id = &r.task_ids["fail"];
    run(&mut conn, id, base.path());
    assert_eq!(state(&conn, id), TaskState::Retrying);
    // A delayed copy of the first action must not consume the second attempt.
    execute_with_registry(&mut conn, id, 1, &ProviderRegistry::new(), base.path()).unwrap();
    assert_eq!(tasks::attempt_count(&conn, id).unwrap(), 1);
    assert!(tick(&conn).iter().any(|a| a
        == &Action::ExecuteGraphNode {
            task_id: id.clone(),
            expected_attempt: 2
        }));
    run(&mut conn, id, base.path());
    run(&mut conn, id, base.path());
    assert_eq!(state(&conn, id), TaskState::NeedsHuman);
    assert_eq!(evidence(&conn, id).unwrap().len(), 2);
    run(&mut conn, &r.task_ids["missing"], base.path());
    assert_eq!(state(&conn, &r.task_ids["missing"]), TaskState::NeedsHuman);
    assert_eq!(
        evidence(&conn, &r.task_ids["missing"]).unwrap()[0].outcome,
        Outcome::Unknown
    );
    assert!(tick(&conn).is_empty());
    assert!(
        tasks::transition(
            &mut conn,
            id,
            TaskState::NeedsHuman,
            TaskState::Done,
            "operator-approve"
        )
        .is_err()
    );
}

#[test]
fn failed_evidence_commit_rolls_back_result_and_does_not_replay() {
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &graph(vec![shell("work", "printf ok")], &[]));
    let id = &r.task_ids["work"];
    let claim = claim(&mut conn, id, 1).unwrap().unwrap();
    let base = tempfile::tempdir().unwrap();
    let record = super::super::runner::execute_once(
        &claim.node,
        &claim.fingerprint,
        claim.attempt,
        &ProviderRegistry::new(),
        base.path(),
    )
    .unwrap();
    conn.execute_batch("CREATE TRIGGER reject_result BEFORE UPDATE OF state ON tasks WHEN NEW.state='DONE' BEGIN SELECT RAISE(ABORT, 'result commit failed'); END;").unwrap();
    assert!(finish(&mut conn, &claim, &record).is_err());
    assert_eq!(state(&conn, id), TaskState::Running);
    assert!(evidence(&conn, id).unwrap().is_empty());
    assert!(tick(&conn).is_empty());
    let ended: Option<String> = conn
        .query_row(
            "SELECT ended_at FROM task_attempts WHERE id=?1",
            [&claim.attempt_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ended.is_none());
}

#[test]
fn provider_error_retries_to_budget_but_never_reports_success() {
    struct Broken;
    impl Provider for Broken {
        fn name(&self) -> &str {
            "broken"
        }
        fn supports(&self, _: &str) -> bool {
            true
        }
        fn execute(
            &self,
            _: &NodeSpec,
            _: &str,
            _: &Value,
            _: &ProviderContext<'_>,
        ) -> Result<ProviderResult, String> {
            Err("injected provider error".into())
        }
    }
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &graph(vec![provider("read")], &[]));
    let base = tempfile::tempdir().unwrap();
    let broken = Broken;
    let mut registry = ProviderRegistry::new();
    registry.register(&broken);
    for attempt in 1..=2 {
        execute_with_registry(
            &mut conn,
            &r.task_ids["read"],
            attempt,
            &registry,
            base.path(),
        )
        .unwrap();
    }
    assert_eq!(state(&conn, &r.task_ids["read"]), TaskState::NeedsHuman);
    assert_eq!(
        durable::status_with_connection(&conn, &r.graph_run_id)
            .unwrap()
            .outcome,
        Outcome::Error
    );
}

#[test]
fn unsupported_typed_transport_and_subgraphs_are_rejected() {
    let mut conn = store::open_memory();
    let mut g = graph(vec![provider("read"), gate()], &[("read", "approve")]);
    g.spec.edges[0].dependency_only = false;
    g.spec.edges[0].from.port = Some("result".into());
    g.spec.edges[0].to.port = Some("result".into());
    let port = super::super::schema::PortSpec {
        name: "result".into(),
        data_type: "Json".into(),
    };
    g.spec.nodes[0].outputs.push(port.clone());
    g.spec.nodes[1].inputs.push(port);
    let typed = compiler::compile(g.spec).unwrap();
    assert!(
        durable::submit_with_options(&mut conn, &typed, false)
            .unwrap_err()
            .contains("dependency-only edges")
    );
    let subgraph = json!({"id":"nested","kind":"subgraph","requires":["test.read"],"executor":{"kind":"provider","capability":"test.read"}});
    assert!(
        durable::submit_with_options(&mut conn, &graph(vec![subgraph], &[]), false)
            .unwrap_err()
            .contains("subgraph execution")
    );
}

#[test]
fn cancellation_preserves_late_evidence_without_releasing_dependencies() {
    let mut conn = store::open_memory();
    let r = submit(
        &mut conn,
        &graph(
            vec![shell("work", "printf ok"), gate()],
            &[("work", "approve")],
        ),
    );
    let id = &r.task_ids["work"];
    let claim = claim(&mut conn, id, 1).unwrap().unwrap();
    tasks::transition(
        &mut conn,
        id,
        TaskState::Running,
        TaskState::Cancelled,
        "operator-cancel",
    )
    .unwrap();
    let base = tempfile::tempdir().unwrap();
    let record = super::super::runner::execute_once(
        &claim.node,
        &claim.fingerprint,
        claim.attempt,
        &ProviderRegistry::new(),
        base.path(),
    )
    .unwrap();
    finish(&mut conn, &claim, &record).unwrap();
    assert_eq!(state(&conn, id), TaskState::Cancelled);
    assert_eq!(evidence(&conn, id).unwrap()[0].outcome, Outcome::Pass);
    assert_eq!(
        durable::status_with_connection(&conn, &r.graph_run_id)
            .unwrap()
            .outcome,
        Outcome::Error
    );
    assert!(matches!(&tick(&conn)[0], Action::EscalateHuman { .. }));
}

struct ReadProvider;
impl Provider for ReadProvider {
    fn name(&self) -> &str {
        "read-test"
    }
    fn supports(&self, capability: &str) -> bool {
        capability == "test.read"
    }
    fn execute(
        &self,
        _: &NodeSpec,
        _: &str,
        _: &Value,
        context: &ProviderContext<'_>,
    ) -> Result<ProviderResult, String> {
        let mut result = ProviderResult::pass_observed("mock provider observation");
        result
            .artifacts
            .push(context.run_dir.join("artifact.json").display().to_string());
        Ok(result)
    }
}

#[test]
fn registered_provider_uses_existing_adapter_and_persists_confidence_and_links() {
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &graph(vec![provider("read")], &[]));
    let p = ReadProvider;
    let mut registry = ProviderRegistry::new();
    registry.register(&p);
    let base = tempfile::tempdir().unwrap();
    execute_with_registry(&mut conn, &r.task_ids["read"], 1, &registry, base.path()).unwrap();
    let records = evidence(&conn, &r.task_ids["read"]).unwrap();
    assert_eq!(
        records[0].confidence,
        super::super::runner::EvidenceConfidence::Observed
    );
    assert_eq!(records[0].artifacts.len(), 1);
    assert_eq!(state(&conn, &r.task_ids["read"]), TaskState::Done);
}

#[test]
fn snapshot_corruption_missing_snapshot_and_dependency_mutation_fail_closed() {
    for mutation in [
        "UPDATE graph_snapshots SET snapshot_json='{}'",
        "DELETE FROM graph_snapshots",
        "UPDATE tasks SET depends_on='[\"unknown\"]'",
        "UPDATE tasks SET cwd='/different'",
        "UPDATE tasks SET state='UNRECOGNIZED'",
    ] {
        let mut conn = store::open_memory();
        let r = submit(&mut conn, &graph(vec![provider("read")], &[]));
        conn.execute(mutation, []).unwrap();
        assert!(
            claim(&mut conn, &r.task_ids["read"], 1).is_err(),
            "{mutation}"
        );
        assert_eq!(tasks::attempt_count(&conn, &r.task_ids["read"]).unwrap(), 0);
    }
}

#[test]
fn submission_is_atomic_and_requires_explicit_shell_opt_in() {
    let mut conn = store::open_memory();
    let g = graph(vec![shell("read", "printf ok")], &[]);
    assert!(
        durable::submit_with_options(&mut conn, &g, false)
            .unwrap_err()
            .contains("--allow-unsandboxed-shell")
    );
    assert!(tasks::list_tasks(&conn, None).unwrap().is_empty());
    conn.execute_batch("CREATE TRIGGER fail_tasks BEFORE INSERT ON tasks BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
    assert!(durable::submit_with_options(&mut conn, &g, true).is_err());
    for table in ["graph_runs", "graph_snapshots", "tasks"] {
        let count: u32 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn forged_compiled_dependencies_are_rejected_before_submission() {
    let mut conn = store::open_memory();
    let mut g = graph(vec![gate(), provider("read")], &[("approve", "read")]);
    g.dependencies.get_mut("read").unwrap().clear();
    assert!(
        durable::submit_with_options(&mut conn, &g, false)
            .unwrap_err()
            .contains("canonical compilation")
    );
}

#[test]
fn shared_shell_cwd_is_frozen_and_provider_relative_paths_are_rejected() {
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &graph(vec![shell("read", "printf ok")], &[]));
    let (_, saved) = snapshot(&conn, &r.graph_run_id).unwrap().unwrap();
    assert!(Path::new(&saved.nodes["read"].cwd).is_absolute());
    let mut node = provider("read");
    node["executor"]["request"] = json!({"target":"relative.js"});
    assert!(
        durable::submit_with_options(&mut conn, &graph(vec![node], &[]), false)
            .unwrap_err()
            .contains("target must be absolute")
    );
    let mut node = provider("read");
    node["authority"] = json!({"filesystem_read":["./**"]});
    assert!(
        durable::submit_with_options(&mut conn, &graph(vec![node], &[]), false)
            .unwrap_err()
            .contains("scopes must be absolute")
    );
}

#[test]
fn unsupported_workspaces_and_recoverable_operations_without_gate_are_rejected() {
    let mut conn = store::open_memory();
    let mut node = provider("read");
    node["workspace"] = json!({"mode":"worktree"});
    assert!(durable::submit_with_options(&mut conn, &graph(vec![node], &[]), false).is_err());
    let mut node = provider("risky");
    node["risk"] = json!("recoverable");
    assert!(
        durable::submit_with_options(&mut conn, &graph(vec![node.clone()], &[]), false)
            .unwrap_err()
            .contains("upstream approval")
    );
    submit(
        &mut conn,
        &graph(vec![gate(), node], &[("approve", "risky")]),
    );
}

#[test]
fn agent_nodes_still_route_through_existing_session_supervisor() {
    let mut conn = store::open_memory();
    let agent = json!({"id":"agent","kind":"agent","requires":["agent.execute"],"workspace":{"mode":"worktree"},"executor":{"kind":"provider","capability":"agent.execute","request":{"cwd":".","prompt":"scoped test work"}}});
    let r = submit(
        &mut conn,
        &graph(vec![agent, gate()], &[("agent", "approve")]),
    );
    assert!(
        matches!(&tick(&conn)[0], Action::Spawn { task_id, .. } if task_id == &r.task_ids["agent"])
    );
    // Simulate the existing agent completion path, never launch a real session.
    tasks::transition(
        &mut conn,
        &r.task_ids["agent"],
        TaskState::Pending,
        TaskState::Done,
        "test-agent-completed",
    )
    .unwrap();
    assert_eq!(
        tick(&conn),
        vec![Action::AwaitGraphApproval {
            task_id: r.task_ids["approve"].clone()
        }]
    );
}

#[test]
fn generic_agent_completion_cannot_skip_a_waiting_graph_approval() {
    let mut conn = store::open_memory();
    let agent = json!({"id":"agent","kind":"agent","requires":["agent.execute"],"workspace":{"mode":"worktree"},"executor":{"kind":"provider","capability":"agent.execute","request":{"cwd":".","prompt":"scoped test work"}}});
    let r = submit(
        &mut conn,
        &graph(vec![gate(), agent], &[("approve", "agent")]),
    );
    let id = &r.task_ids["agent"];
    assert!(
        tasks::transition(
            &mut conn,
            id,
            TaskState::Pending,
            TaskState::Done,
            "operator-approve"
        )
        .is_err()
    );
    assert!(
        tasks::transition(
            &mut conn,
            id,
            TaskState::Pending,
            TaskState::Running,
            "spawned"
        )
        .is_err()
    );
    assert_eq!(state(&conn, id), TaskState::Pending);
}

fn master_fixture() -> super::super::master::CompiledMasterPlan {
    let mut source: super::super::master::MasterPlan =
        serde_json::from_str(include_str!("../../../examples/druid-master-plan-v1.json")).unwrap();
    for node in &mut source.graph.nodes {
        if let Some(ExecutorSpec::Shell { command, .. }) = &mut node.executor {
            *command = "printf 'local projection test'".into();
        }
    }
    super::super::master::compile(source).unwrap()
}

fn run_master_checks(
    conn: &mut Connection,
    bundle: &super::super::master::CompiledMasterPlan,
    submission: &DurableSubmission,
    base: &Path,
) {
    for wave in &bundle.execution.waves {
        for id in wave {
            let node = bundle
                .execution
                .spec
                .nodes
                .iter()
                .find(|n| &n.id == id)
                .unwrap();
            if matches!(node.executor, Some(ExecutorSpec::Shell { .. })) {
                run(conn, &submission.task_ids[id], base);
            }
        }
    }
}

#[test]
fn durable_projection_tracks_planned_implemented_verified_with_resolvable_evidence() {
    use super::super::master::{WorkState, status_from_durable};
    let mut conn = store::open_memory();
    let bundle = master_fixture();
    let r = submit(&mut conn, &bundle.execution);
    let initial = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert!(
        initial
            .plan
            .work
            .values()
            .all(|w| w.state == WorkState::Planned && w.evidence.is_empty())
    );
    assert!(initial.plan.run_dir.is_none());
    let base = tempfile::tempdir().unwrap();
    run(&mut conn, &r.task_ids["discovery"], base.path());
    let partial = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(partial.plan.work["discovery"].state, WorkState::Implemented);
    assert!(
        partial.plan.work["discovery"]
            .blockers
            .contains(&"verification".to_string())
    );
    run_master_checks(&mut conn, &bundle, &r, base.path());
    await_approval(&mut conn, &r.task_ids["approval"]).unwrap();
    let verified = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(
        verified.plan.work["implementation"].state,
        WorkState::Verified
    );
    assert_eq!(verified.plan.work["approval"].state, WorkState::Planned);
    assert_eq!(verified.durable.effective_state, "AWAITING_APPROVAL");
    decide_with_connection(&mut conn, &r.graph_run_id, "approval", &decision(&r, true)).unwrap();
    run(&mut conn, &r.task_ids["deployment"], base.path()); // Unconfigured provider: UNKNOWN.
    let final_status = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(
        final_status.plan.work["approval"].state,
        WorkState::Implemented
    );
    assert_eq!(
        final_status.plan.work["deployment"].state,
        WorkState::Planned
    );
    assert_eq!(
        final_status.plan.work["deployment"].outcome,
        Outcome::Unknown
    );
    let json = serde_json::to_value(&final_status).unwrap();
    for work in final_status.plan.work.values() {
        for pointer in &work.evidence {
            assert!(
                json.pointer(pointer).is_some(),
                "unresolved evidence pointer: {pointer}"
            );
        }
    }
    assert_eq!(
        final_status.durable.approval_digest.as_deref(),
        Some(r.approval_digest.as_str())
    );
}

#[test]
fn durable_projection_preserves_failed_retry_before_observed_success() {
    use super::super::master::{WorkState, status_from_durable};
    let base = tempfile::tempdir().unwrap();
    let mut source = master_fixture().source;
    source.graph.policy.max_attempts_per_node = 2;
    let discovery = source
        .graph
        .nodes
        .iter_mut()
        .find(|n| n.id == "discovery")
        .unwrap();
    discovery.retry.max_attempts = 2;
    discovery.executor = Some(ExecutorSpec::Shell {
        command: "test -f ready".into(),
        cwd: Some(base.path().to_string_lossy().into_owned()),
    });
    let bundle = super::super::master::compile(source).unwrap();
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &bundle.execution);
    let id = &r.task_ids["discovery"];

    run(&mut conn, id, base.path());
    assert_eq!(state(&conn, id), TaskState::Retrying);
    let failed = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(failed.plan.work["discovery"].state, WorkState::Planned);
    assert!(!failed.plan.work["discovery"].evidence.is_empty());

    // A claimed retry without a result must not borrow the earlier evidence.
    let second = claim(&mut conn, id, 2).unwrap().unwrap();
    let uncertain = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(uncertain.plan.work["discovery"].state, WorkState::Planned);
    std::fs::write(base.path().join("ready"), "test fixture").unwrap();
    let record = super::super::runner::execute_once(
        &second.node,
        &second.fingerprint,
        second.attempt,
        &ProviderRegistry::new(),
        base.path(),
    )
    .unwrap();
    finish(&mut conn, &second, &record).unwrap();
    let implemented = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(
        implemented.plan.work["discovery"].state,
        WorkState::Implemented
    );
    run_master_checks(&mut conn, &bundle, &r, base.path());
    let verified = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(verified.plan.work["discovery"].state, WorkState::Verified);
    let result = verified
        .durable
        .nodes
        .iter()
        .find(|n| n.task_id == *id)
        .unwrap();
    assert_eq!(result.attempts, 2);
    assert_eq!(result.evidence.len(), 2);
    assert_eq!(result.evidence[0].outcome, Outcome::Fail);
    assert_eq!(result.evidence[1].outcome, Outcome::Pass);
}

#[test]
fn durable_projection_reads_readonly_reopened_database_without_mutation() {
    let mut conn = store::open_memory();
    let bundle = master_fixture();
    let r = submit(&mut conn, &bundle.execution);
    let base = tempfile::tempdir().unwrap();
    run(&mut conn, &r.task_ids["discovery"], base.path());
    let db = base.path().join("readonly.db");
    conn.execute("VACUUM INTO ?1", [db.to_str().unwrap()])
        .unwrap();
    drop(conn);
    let before = std::fs::read(&db).unwrap();
    let mut read_only =
        Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let result =
        super::super::master::status_from_durable(&mut read_only, &bundle, &r.graph_run_id)
            .unwrap();
    assert_eq!(
        result.plan.work["discovery"].state,
        super::super::master::WorkState::Implemented
    );
    assert_eq!(read_only.total_changes(), 0);
    drop(read_only);
    assert_eq!(std::fs::read(db).unwrap(), before);
}

#[test]
fn durable_projection_rejects_other_revision_missing_tasks_and_metadata_drift() {
    let bundle = master_fixture();
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &bundle.execution);
    let mut changed = bundle.source.clone();
    changed.requirements[0].description.push_str(" modified");
    let other = super::super::master::compile(changed).unwrap();
    assert!(
        super::super::master::status_from_durable(&mut conn, &other, &r.graph_run_id)
            .unwrap_err()
            .contains("different master plan")
    );
    for sql in [
        "UPDATE tasks SET graph_run_id='another-run' WHERE graph_node_id='discovery'",
        "UPDATE graph_runs SET fingerprint='forged'",
        "UPDATE tasks SET depends_on='[\"foreign-task\"]'",
        "DELETE FROM graph_snapshots",
    ] {
        let mut conn = store::open_memory();
        let r = submit(&mut conn, &bundle.execution);
        conn.execute(sql, []).unwrap();
        assert!(
            super::super::master::status_from_durable(&mut conn, &bundle, &r.graph_run_id).is_err(),
            "{sql}"
        );
    }
}

#[test]
fn durable_projection_does_not_promote_uncertain_attempts_or_cancelled_descendants() {
    use super::super::master::{WorkState, status_from_durable};
    let mut conn = store::open_memory();
    let bundle = master_fixture();
    let r = submit(&mut conn, &bundle.execution);
    claim(&mut conn, &r.task_ids["discovery"], 1)
        .unwrap()
        .unwrap();
    let status = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(status.plan.work["discovery"].state, WorkState::Planned);
    assert!(status.plan.work["discovery"].evidence.is_empty());
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &bundle.execution);
    let base = tempfile::tempdir().unwrap();
    run_master_checks(&mut conn, &bundle, &r, base.path());
    tasks::transition(
        &mut conn,
        &r.task_ids["discovery"],
        TaskState::Done,
        TaskState::Cancelled,
        "operator-cancel",
    )
    .unwrap();
    let status = status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert!(
        status
            .plan
            .work
            .values()
            .all(|w| w.state == WorkState::Planned)
    );
    assert_eq!(status.plan.work["discovery"].outcome, Outcome::Error);
    assert!(!status.plan.work["verification"].evidence.is_empty());
}

#[test]
fn durable_projection_rejects_forged_success_evidence() {
    for mutation in 0..5 {
        let mut conn = store::open_memory();
        let bundle = master_fixture();
        let r = submit(&mut conn, &bundle.execution);
        let base = tempfile::tempdir().unwrap();
        run(&mut conn, &r.task_ids["discovery"], base.path());
        let mut record = evidence(&conn, &r.task_ids["discovery"]).unwrap().remove(0);
        match mutation {
            0 => record.command = Some("different command".into()),
            1 => record.exit_code = Some(1),
            2 => record.attempt = 2,
            3 => record
                .declared_authority
                .filesystem_read
                .push("/outside".into()),
            _ => record.outcome = Outcome::Fail,
        }
        conn.execute(
            "UPDATE graph_node_evidence SET record_json=?1",
            [serde_json::to_string(&record).unwrap()],
        )
        .unwrap();
        assert!(
            super::super::master::status_from_durable(&mut conn, &bundle, &r.graph_run_id).is_err()
        );
    }
}

#[test]
fn durable_projection_never_promotes_derived_provider_pass_to_verified() {
    let mut source = master_fixture().source;
    let discovery = &mut source.graph.nodes[0];
    discovery.requires = vec!["test.read".into()];
    discovery.executor = Some(ExecutorSpec::Provider {
        capability: "test.read".into(),
        provider: None,
        request: json!({}),
    });
    discovery.authority.filesystem_read.clear();
    let bundle = super::super::master::compile(source).unwrap();
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &bundle.execution);
    let claim = claim(&mut conn, &r.task_ids["discovery"], 1)
        .unwrap()
        .unwrap();
    let provider = ReadProvider;
    let mut registry = ProviderRegistry::new();
    registry.register(&provider);
    let base = tempfile::tempdir().unwrap();
    let mut record = super::super::runner::execute_once(
        &claim.node,
        &claim.fingerprint,
        claim.attempt,
        &registry,
        base.path(),
    )
    .unwrap();
    record.confidence = super::super::runner::EvidenceConfidence::Derived;
    finish(&mut conn, &claim, &record).unwrap();
    run_master_checks(&mut conn, &bundle, &r, base.path());
    let status =
        super::super::master::status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(status.plan.work["discovery"].outcome, Outcome::Pass);
    assert_eq!(
        status.plan.work["discovery"].state,
        super::super::master::WorkState::Planned
    );
}

#[test]
fn durable_projection_keeps_agent_done_distinct_from_verified_deliverable() {
    let mut source = master_fixture().source;
    let agent = source
        .graph
        .nodes
        .iter_mut()
        .find(|n| n.id == "implementation")
        .unwrap();
    agent.kind = NodeKind::Agent;
    agent.requires = vec!["agent.execute".into()];
    agent.workspace.mode = WorkspaceMode::Worktree;
    agent.executor = Some(ExecutorSpec::Provider {
        capability: "agent.execute".into(),
        provider: None,
        request: json!({"cwd":".","prompt":"test only; no real launch"}),
    });
    let bundle = super::super::master::compile(source).unwrap();
    let mut conn = store::open_memory();
    let r = submit(&mut conn, &bundle.execution);
    let base = tempfile::tempdir().unwrap();
    for wave in &bundle.execution.waves {
        for id in wave {
            if id == "implementation" {
                tasks::transition(
                    &mut conn,
                    &r.task_ids[id],
                    TaskState::Pending,
                    TaskState::Done,
                    "test-agent-done",
                )
                .unwrap();
            } else if matches!(
                bundle
                    .execution
                    .spec
                    .nodes
                    .iter()
                    .find(|n| &n.id == id)
                    .unwrap()
                    .executor,
                Some(ExecutorSpec::Shell { .. })
            ) {
                run(&mut conn, &r.task_ids[id], base.path());
            }
        }
    }
    let status =
        super::super::master::status_from_durable(&mut conn, &bundle, &r.graph_run_id).unwrap();
    assert_eq!(status.plan.work["implementation"].outcome, Outcome::Pass);
    assert_eq!(
        status.plan.work["implementation"].state,
        super::super::master::WorkState::Planned
    );
    assert!(!status.plan.work["implementation"].evidence.is_empty()); // Verifier link, not proof of the deliverable.
}

#[test]
fn durable_projection_rejects_approval_digest_drift_and_unproven_done() {
    let bundle = master_fixture();
    for mutation in [
        "UPDATE graph_approvals SET digest='wrong-run'",
        "DELETE FROM graph_node_evidence",
    ] {
        let mut conn = store::open_memory();
        let r = submit(&mut conn, &bundle.execution);
        let base = tempfile::tempdir().unwrap();
        run_master_checks(&mut conn, &bundle, &r, base.path());
        await_approval(&mut conn, &r.task_ids["approval"]).unwrap();
        decide_with_connection(&mut conn, &r.graph_run_id, "approval", &decision(&r, true))
            .unwrap();
        conn.execute(mutation, []).unwrap();
        assert!(
            super::super::master::status_from_durable(&mut conn, &bundle, &r.graph_run_id).is_err(),
            "{mutation}"
        );
    }
}
