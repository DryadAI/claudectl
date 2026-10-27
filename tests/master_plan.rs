use std::fs;
use std::process::{Command, Stdio};

use claudectl::graph::master::{self, CompiledMasterPlan, MasterPlan, Phase, WorkState};
use claudectl::graph::runner::{EvidenceConfidence, EvidenceRecord, NodeExecution, RunReport};
use claudectl::graph::schema::{
    EdgeEndpoint, EdgeSpec, ExecutorSpec, NetworkAuthority, NodeKind, Outcome, WorkspaceMode,
};

fn example() -> MasterPlan {
    serde_json::from_str(include_str!("../examples/druid-master-plan-v1.json")).unwrap()
}

#[cfg(feature = "coord")]
#[test]
fn cli_rejects_ambiguous_local_and_durable_evidence_sources() {
    let output = Command::new(env!("CARGO_BIN_EXE_claudectl"))
        .args([
            "graph",
            "master",
            "status",
            "does-not-exist.json",
            "--run",
            "test-run",
            "--report",
            "does-not-exist-report.json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
}

fn edge(from: &str, to: &str) -> EdgeSpec {
    EdgeSpec {
        from: EdgeEndpoint {
            node: from.into(),
            port: None,
        },
        to: EdgeEndpoint {
            node: to.into(),
            port: None,
        },
        dependency_only: true,
    }
}

// Synthetic reports are used only to exercise the projector's rejection rules.
// The CLI test below also feeds it evidence from an actual local runtime run.
fn observed_report(bundle: &CompiledMasterPlan) -> RunReport {
    RunReport {
        graph_id: bundle.execution.spec.id.clone(),
        graph_fingerprint: bundle.execution.fingerprint.clone(),
        run_dir: "/synthetic-test-evidence".into(),
        nodes: bundle
            .execution
            .spec
            .nodes
            .iter()
            .map(|node| {
                let (command, exit_code) = match &node.executor {
                    Some(ExecutorSpec::Shell { command, .. }) => (Some(command.clone()), Some(0)),
                    _ => (None, None),
                };
                (
                    node.id.clone(),
                    NodeExecution {
                        outcome: Outcome::Pass,
                        attempts: 1,
                        evidence: vec![EvidenceRecord {
                            node_id: node.id.clone(),
                            attempt: 1,
                            outcome: Outcome::Pass,
                            confidence: EvidenceConfidence::Observed,
                            started_at_ms: 1,
                            duration_ms: 1,
                            declared_authority: node.authority.clone(),
                            command,
                            exit_code,
                            stdout: None,
                            stderr: None,
                            message: None,
                            artifacts: vec![],
                            artifact_attestations: vec![],
                        }],
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn deterministic_compilation_reuses_runtime_ir_and_starts_planned() {
    let a = master::compile(example()).unwrap();
    let b = master::compile(example()).unwrap();
    assert_eq!(a.digest, b.digest);
    assert_eq!(
        serde_json::to_value(&a).unwrap(),
        serde_json::to_value(&b).unwrap()
    );
    let lowered = claudectl::graph::compiler::compile(a.execution.spec.clone()).unwrap();
    assert_eq!(lowered.waves, a.execution.waves);
    assert_eq!(lowered.dependencies, a.execution.dependencies);
    assert_eq!(lowered.fingerprint, a.execution.fingerprint);
    assert_eq!(a.execution.waves.len(), 10);
    assert!(
        master::status(&a, None)
            .unwrap()
            .work
            .values()
            .all(|w| w.state == WorkState::Planned && w.evidence.is_empty())
    );
}

#[test]
fn provenance_requirement_and_authority_changes_invalidate_old_evidence() {
    let first = master::compile(example()).unwrap();
    let report = observed_report(&first);
    for change in 0..5 {
        let mut source = example();
        match change {
            0 => source.requirements[0].description.push_str(" revised"),
            1 => source.source.commit = "a".repeat(40),
            2 => source.graph.nodes[0]
                .authority
                .filesystem_read
                .push("docs".into()),
            3 => source.work[0]
                .deliverables
                .push("additional-artifact".into()),
            _ => {
                source.revision = 2;
                source.previous_digest = Some(first.digest.clone());
            }
        }
        let next = master::compile(source).unwrap();
        assert_ne!(first.digest, next.digest);
        assert_ne!(first.execution.spec.id, next.execution.spec.id);
        assert!(
            master::status(&next, Some(&report))
                .unwrap_err()
                .contains("different plan")
        );
    }
}

#[test]
fn rejects_bad_versions_provenance_and_revision_links() {
    for change in 0..7 {
        let mut source = example();
        match change {
            0 => source.version = 2,
            1 => source.revision = 0,
            2 => source.revision = 2,
            3 => source.previous_digest = Some(format!("sha256:{}", "a".repeat(64))),
            4 => source.source.commit = "main".into(),
            5 => source.source.references.clear(),
            _ => source.source.repository.clear(),
        }
        assert!(master::compile(source).is_err(), "case {change}");
    }
}

#[test]
fn rejects_traceability_gaps_and_duplicates() {
    for change in 0..8 {
        let mut source = example();
        match change {
            0 => {
                source.work.pop();
            }
            1 => source.work.push(source.work[0].clone()),
            2 => source.work[0].requirements = vec!["unknown".into()],
            3 => source.work[0].deliverables.clear(),
            4 => source.requirements.push(source.requirements[0].clone()),
            5 => source.work[1].requirements = source.work[0].requirements.clone(),
            6 => source.work[0].node = "missing".into(),
            _ => source.work[1].phase = Phase::Discovery,
        }
        assert!(master::compile(source).is_err(), "case {change}");
    }
}

#[test]
fn agent_artifact_paths_lower_into_the_canonical_graph_and_reject_escape_paths() {
    let mut source = example();
    let node = source
        .graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "implementation")
        .unwrap();
    node.kind = NodeKind::Agent;
    node.requires = vec!["agent.execute".into()];
    node.workspace.mode = WorkspaceMode::Worktree;
    node.executor = Some(ExecutorSpec::Provider {
        capability: "agent.execute".into(),
        provider: None,
        request: serde_json::json!({"cwd":".","prompt":"implement scoped changes"}),
    });
    source
        .graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "verification")
        .unwrap()
        .workspace
        .mode = WorkspaceMode::Worktree;
    source.work[5].artifact_paths = vec!["src/feature.rs".into(), "docs/feature.md".into()];
    let compiled = master::compile(source.clone()).unwrap();
    let node = compiled
        .execution
        .spec
        .nodes
        .iter()
        .find(|node| node.id == "implementation")
        .unwrap();
    assert_eq!(node.artifact_paths, source.work[5].artifact_paths);
    master::validate_bundle(&compiled).unwrap();

    for path in ["../outside", "/etc/passwd", "src\\outside", "."] {
        let mut invalid = source.clone();
        invalid.work[5].artifact_paths = vec![path.into()];
        assert!(master::compile(invalid).is_err(), "accepted {path:?}");
    }
}

#[test]
fn preserves_canonical_graph_validation_and_fail_closed_policy() {
    for change in 0..8 {
        let mut source = example();
        match change {
            0 => source.graph.edges.push(edge("recovery", "discovery")),
            1 => source.graph.edges.push(edge("missing", "discovery")),
            2 => source.graph.nodes[0].requires.clear(),
            3 => source.graph.nodes[0].authority.network = NetworkAuthority::Allowed,
            4 => source.graph.nodes[0].authority.git_push = true,
            5 => source.graph.nodes[0].retry.max_attempts = 100,
            6 => source.graph.edges[0].dependency_only = false,
            _ => source.graph.nodes.push(source.graph.nodes[0].clone()),
        }
        assert!(master::compile(source).is_err(), "case {change}");
    }
}

#[test]
fn rejects_phase_bypass_fake_verification_and_approval_bypass() {
    for change in 0..8 {
        let mut source = example();
        match change {
            0 => source.work[5].verify_with.clear(),
            1 => source.work[5].verify_with = vec!["implementation".into()],
            2 => source.work[5].verify_with = vec!["inventory".into()],
            3 => source.work[5].verify_with = vec!["missing".into()],
            4 => source
                .graph
                .edges
                .retain(|e| !(e.from.node == "approval" && e.to.node == "recovery")),
            5 => {
                source.graph.edges.retain(|e| e.to.node != "approval");
                source.graph.edges.push(edge("implementation", "approval"));
            }
            6 => {
                source.graph.edges.retain(|e| e.to.node != "specification");
                source.graph.edges.push(edge("discovery", "specification"));
            }
            _ => {
                source.graph.nodes[7].executor = Some(ExecutorSpec::Shell {
                    command: "true".into(),
                    cwd: None,
                })
            }
        }
        assert!(master::compile(source).is_err(), "case {change}");
    }
}

#[test]
fn approval_must_wait_for_every_implementation_verifier() {
    let mut source = example();
    let mut additional = source.graph.nodes[6].clone();
    additional.id = "additional-check".into();
    source.graph.nodes.push(additional);
    let mut additional = source.work[6].clone();
    additional.node = "additional-check".into();
    source.work.push(additional);
    source.work[5].verify_with.push("additional-check".into());
    source
        .graph
        .edges
        .push(edge("implementation", "additional-check"));
    assert!(
        master::compile(source.clone())
            .unwrap_err()
            .contains("after all implementation verifiers")
    );
    source
        .graph
        .edges
        .push(edge("additional-check", "approval"));
    assert!(master::compile(source).is_ok());
}

#[test]
fn bundle_tampering_is_detected_by_recompilation() {
    for change in 0..5 {
        let mut bundle = master::compile(example()).unwrap();
        match change {
            0 => bundle.digest.push('0'),
            1 => bundle.execution.waves.reverse(),
            2 => bundle.execution.spec.policy.allow_network = true,
            3 => bundle.source.requirements[0]
                .description
                .push_str(" altered"),
            _ => bundle.execution.dependencies.clear(),
        }
        assert!(master::status(&bundle, None).is_err(), "case {change}");
    }
}

#[test]
fn only_observed_independent_evidence_promotes_to_verified() {
    let bundle = master::compile(example()).unwrap();
    let mut report = observed_report(&bundle);
    let result = master::status(&bundle, Some(&report)).unwrap();
    assert_eq!(result.work["implementation"].state, WorkState::Verified);
    assert_eq!(result.work["verification"].state, WorkState::Implemented);
    assert_eq!(
        result.work["implementation"].evidence,
        vec![
            "/nodes/implementation/evidence",
            "/nodes/verification/evidence"
        ]
    );
    report.nodes.get_mut("verification").unwrap().evidence[0].confidence =
        EvidenceConfidence::Inferred;
    assert_eq!(
        master::status(&bundle, Some(&report)).unwrap().work["implementation"].state,
        WorkState::Implemented
    );
    report.nodes.get_mut("implementation").unwrap().evidence[0].confidence =
        EvidenceConfidence::Unknown;
    assert_eq!(
        master::status(&bundle, Some(&report)).unwrap().work["implementation"].state,
        WorkState::Planned
    );
}

#[test]
fn malformed_stale_and_impossible_reports_are_rejected() {
    let bundle = master::compile(example()).unwrap();
    for change in 0..11 {
        let mut report = observed_report(&bundle);
        match change {
            0 => report.graph_fingerprint.push('x'),
            1 => {
                report.nodes.remove("recovery");
            }
            2 => report
                .nodes
                .get_mut("implementation")
                .unwrap()
                .evidence
                .clear(),
            3 => {
                report.nodes.get_mut("implementation").unwrap().evidence[0].node_id = "other".into()
            }
            4 => report.nodes.get_mut("implementation").unwrap().attempts = 0,
            5 => report.nodes.get_mut("implementation").unwrap().evidence[0].attempt = 3,
            6 => report.nodes.get_mut("implementation").unwrap().evidence[0].exit_code = Some(1),
            7 => {
                report.nodes.get_mut("implementation").unwrap().evidence[0].command =
                    Some("different".into())
            }
            8 => {
                report.nodes.get_mut("implementation").unwrap().evidence[0]
                    .declared_authority
                    .git_push = true
            }
            9 => report.nodes.get_mut("implementation").unwrap().outcome = Outcome::Fail,
            _ => {
                let node = report.nodes.get_mut("implementation").unwrap();
                node.outcome = Outcome::Fail;
                node.evidence[0].outcome = Outcome::Fail;
            }
        }
        assert!(
            master::status(&bundle, Some(&report)).is_err(),
            "case {change}"
        );
    }
}

#[test]
fn input_rejects_unknown_fields_including_claimed_completion() {
    let mut source = serde_json::to_value(example()).unwrap();
    source["work"][0]["state"] = "verified".into();
    assert!(serde_json::from_value::<MasterPlan>(source).is_err());
}

fn cli(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_claudectl"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn cli_compiles_runs_existing_runtime_and_links_real_evidence_without_approval() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = example();
    // Harmless fixture replaces repository checks with deterministic commands.
    // Approval remains interactive with closed stdin; deployment is never run.
    for node in &mut source.graph.nodes {
        if let Some(ExecutorSpec::Shell { command, .. }) = &mut node.executor {
            *command = "printf 'observed fixture check\\n'".into();
        }
    }
    fs::write(
        dir.path().join("source.json"),
        serde_json::to_vec(&source).unwrap(),
    )
    .unwrap();
    let output = cli(
        dir.path(),
        &[
            "graph",
            "master",
            "compile",
            "source.json",
            "--out",
            "compiled",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !cli(
            dir.path(),
            &[
                "graph",
                "master",
                "compile",
                "source.json",
                "--out",
                "compiled"
            ]
        )
        .status
        .success()
    );
    let graph_path = "compiled/graph.json";
    assert!(
        cli(dir.path(), &["graph", "validate", graph_path])
            .status
            .success()
    );
    let output = cli(dir.path(), &["--json", "graph", "run", graph_path]);
    assert!(!output.status.success());
    let report: RunReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.nodes["approval"].outcome, Outcome::Fail);
    assert_eq!(report.nodes["deployment"].outcome, Outcome::Unknown);
    assert_eq!(report.nodes["deployment"].attempts, 0);
    assert_eq!(report.nodes["recovery"].attempts, 0);
    let summary = format!("{}/summary.json", report.run_dir);
    assert!(std::path::Path::new(&summary).is_file());
    let result = cli(
        dir.path(),
        &[
            "graph",
            "master",
            "status",
            "compiled/plan.json",
            "--report",
            &summary,
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(status["work"]["implementation"]["state"], "verified");
    assert_eq!(status["work"]["deployment"]["state"], "planned");
    assert_eq!(status["work"]["approval"]["outcome"], "FAIL");
    let report_json = serde_json::to_value(&report).unwrap();
    for item in status["work"].as_object().unwrap().values() {
        for link in item["evidence"].as_array().unwrap() {
            assert!(report_json.pointer(link.as_str().unwrap()).is_some());
        }
    }
}

#[test]
fn failed_verifier_and_missing_provider_block_the_real_runtime() {
    for unavailable in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut source = example();
        for node in &mut source.graph.nodes {
            if let Some(ExecutorSpec::Shell { command, .. }) = &mut node.executor {
                *command = "true".into();
            }
        }
        if unavailable {
            let node = &mut source.graph.nodes[3];
            node.requires = vec!["unavailable.capability".into()];
            node.executor = Some(ExecutorSpec::Provider {
                capability: "unavailable.capability".into(),
                provider: Some("not-installed".into()),
                request: serde_json::Value::Null,
            });
        } else {
            source.graph.nodes[6].executor = Some(ExecutorSpec::Shell {
                command: "exit 7".into(),
                cwd: None,
            });
        }
        let bundle = master::compile(source).unwrap();
        fs::write(
            dir.path().join("graph.json"),
            serde_json::to_vec(&bundle.execution.spec).unwrap(),
        )
        .unwrap();
        let output = cli(dir.path(), &["--json", "graph", "run", "graph.json"]);
        assert!(!output.status.success());
        let report: RunReport = serde_json::from_slice(&output.stdout).unwrap();
        let status = master::status(&bundle, Some(&report)).unwrap();
        assert_eq!(report.nodes["approval"].attempts, 0);
        assert_eq!(report.nodes["deployment"].attempts, 0);
        assert_eq!(report.nodes["recovery"].attempts, 0);
        if unavailable {
            assert_eq!(report.nodes["dependencies"].outcome, Outcome::Unknown);
            assert_eq!(status.work["implementation"].state, WorkState::Planned);
        } else {
            assert_eq!(report.nodes["verification"].outcome, Outcome::Fail);
            assert_eq!(status.work["implementation"].state, WorkState::Implemented);
        }
    }
}

#[test]
fn schemas_are_generated_from_rust_contracts() {
    let input: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/druid-master-plan-v1.schema.json")).unwrap();
    let compiled: serde_json::Value = serde_json::from_str(include_str!(
        "../schemas/druid-compiled-plan-v1.schema.json"
    ))
    .unwrap();
    assert_eq!(
        input,
        serde_json::to_value(schemars::schema_for!(MasterPlan)).unwrap()
    );
    assert_eq!(
        compiled,
        serde_json::to_value(schemars::schema_for!(CompiledMasterPlan)).unwrap()
    );
}
