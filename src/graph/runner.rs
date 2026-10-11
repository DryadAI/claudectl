use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::compiler::CompiledGraph;
use super::schema::{Authority, ExecutorSpec, NodeKind, NodeSpec, Outcome};

const OUTPUT_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    Observed,
    Derived,
    Inferred,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub node_id: String,
    pub attempt: u32,
    pub outcome: Outcome,
    pub confidence: EvidenceConfidence,
    pub started_at_ms: u128,
    pub duration_ms: u128,
    pub declared_authority: Authority,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeExecution {
    pub outcome: Outcome,
    pub attempts: u32,
    pub evidence: Vec<EvidenceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub graph_id: String,
    pub graph_fingerprint: String,
    pub run_dir: String,
    pub nodes: BTreeMap<String, NodeExecution>,
}

impl RunReport {
    pub fn succeeded(&self) -> bool {
        self.nodes
            .values()
            .all(|node| node.outcome == Outcome::Pass)
    }
}

pub fn run(compiled: &CompiledGraph) -> io::Result<RunReport> {
    let run_dir = create_run_dir()?;
    let evidence_path = run_dir.join("evidence.jsonl");
    let mut results: BTreeMap<String, NodeExecution> = BTreeMap::new();

    for wave in &compiled.waves {
        // Seed-runner invariant: scheduling is graph-aware but intentionally
        // synchronous. A later executor can run a wave concurrently without
        // changing the canonical graph format.
        for node_id in wave {
            let node = compiled
                .spec
                .nodes
                .iter()
                .find(|node| &node.id == node_id)
                .expect("compiled graph references known node");

            if let Some(blocker) = first_non_passing_dependency(node_id, compiled, &results) {
                let evidence = EvidenceRecord {
                    node_id: node.id.clone(),
                    attempt: 0,
                    outcome: Outcome::Unknown,
                    confidence: EvidenceConfidence::Unknown,
                    started_at_ms: now_epoch_ms(),
                    duration_ms: 0,
                    declared_authority: node.authority.clone(),
                    command: None,
                    exit_code: None,
                    stdout: None,
                    stderr: None,
                    message: Some(format!(
                        "not executed because dependency '{blocker}' did not PASS"
                    )),
                };
                append_evidence(&evidence_path, &evidence)?;
                results.insert(
                    node.id.clone(),
                    NodeExecution {
                        outcome: Outcome::Unknown,
                        attempts: 0,
                        evidence: vec![evidence],
                    },
                );
                continue;
            }

            let unsupported = unsupported_capabilities(node);
            if !unsupported.is_empty() {
                let evidence = EvidenceRecord {
                    node_id: node.id.clone(),
                    attempt: 0,
                    outcome: Outcome::Unknown,
                    confidence: EvidenceConfidence::Unknown,
                    started_at_ms: now_epoch_ms(),
                    duration_ms: 0,
                    declared_authority: node.authority.clone(),
                    command: None,
                    exit_code: None,
                    stdout: None,
                    stderr: None,
                    message: Some(format!(
                        "seed runner has no provider for capabilities: {}",
                        unsupported.join(", ")
                    )),
                };
                append_evidence(&evidence_path, &evidence)?;
                results.insert(
                    node.id.clone(),
                    NodeExecution {
                        outcome: Outcome::Unknown,
                        attempts: 0,
                        evidence: vec![evidence],
                    },
                );
                continue;
            }

            let mut records = Vec::new();
            let mut final_outcome = Outcome::Error;
            let max_attempts = node.retry.max_attempts;

            for attempt in 1..=max_attempts {
                let record = execute_once(node, &compiled.fingerprint, attempt)?;
                final_outcome = record.outcome;
                append_evidence(&evidence_path, &record)?;
                records.push(record);

                if matches!(final_outcome, Outcome::Pass | Outcome::Unknown) {
                    break;
                }
            }

            results.insert(
                node.id.clone(),
                NodeExecution {
                    outcome: final_outcome,
                    attempts: records.len() as u32,
                    evidence: records,
                },
            );
        }
    }

    let report = RunReport {
        graph_id: compiled.spec.id.clone(),
        graph_fingerprint: compiled.fingerprint.clone(),
        run_dir: run_dir.display().to_string(),
        nodes: results,
    };
    let summary = serde_json::to_string_pretty(&report)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(run_dir.join("summary.json"), summary)?;
    Ok(report)
}

fn first_non_passing_dependency(
    node_id: &str,
    compiled: &CompiledGraph,
    results: &BTreeMap<String, NodeExecution>,
) -> Option<String> {
    compiled
        .dependencies
        .get(node_id)
        .into_iter()
        .flatten()
        .find(|dep| {
            results
                .get(dep.as_str())
                .is_none_or(|result| result.outcome != Outcome::Pass)
        })
        .cloned()
}

fn unsupported_capabilities(node: &NodeSpec) -> Vec<String> {
    let mut seed = BTreeSet::new();
    seed.insert("shell.execute");
    seed.insert("human.approval");

    node.requires
        .iter()
        .filter(|cap| !seed.contains(cap.as_str()))
        .cloned()
        .collect()
}

fn execute_once(node: &NodeSpec, fingerprint: &str, attempt: u32) -> io::Result<EvidenceRecord> {
    let started_at_ms = now_epoch_ms();
    let started = Instant::now();

    if node.kind == NodeKind::Approval {
        let approved = prompt_for_approval(node, fingerprint)?;
        return Ok(EvidenceRecord {
            node_id: node.id.clone(),
            attempt,
            outcome: if approved {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            confidence: EvidenceConfidence::Observed,
            started_at_ms,
            duration_ms: started.elapsed().as_millis(),
            declared_authority: node.authority.clone(),
            command: None,
            exit_code: None,
            stdout: None,
            stderr: None,
            message: Some(if approved {
                format!("human approved exact plan {fingerprint}")
            } else {
                format!("human rejected exact plan {fingerprint}")
            }),
        });
    }

    match node.executor.as_ref() {
        Some(ExecutorSpec::Shell { command, cwd }) => {
            let mut process = shell_command(command);
            if let Some(cwd) = cwd {
                process.current_dir(cwd);
            }
            match process.output() {
                Ok(output) => {
                    let exit_code = output.status.code();
                    Ok(EvidenceRecord {
                        node_id: node.id.clone(),
                        attempt,
                        outcome: if output.status.success() {
                            Outcome::Pass
                        } else {
                            Outcome::Fail
                        },
                        confidence: EvidenceConfidence::Observed,
                        started_at_ms,
                        duration_ms: started.elapsed().as_millis(),
                        declared_authority: node.authority.clone(),
                        command: Some(command.clone()),
                        exit_code,
                        stdout: Some(truncate_output(&String::from_utf8_lossy(&output.stdout))),
                        stderr: Some(truncate_output(&String::from_utf8_lossy(&output.stderr))),
                        message: None,
                    })
                }
                Err(err) => Ok(EvidenceRecord {
                    node_id: node.id.clone(),
                    attempt,
                    outcome: Outcome::Error,
                    confidence: EvidenceConfidence::Observed,
                    started_at_ms,
                    duration_ms: started.elapsed().as_millis(),
                    declared_authority: node.authority.clone(),
                    command: Some(command.clone()),
                    exit_code: None,
                    stdout: None,
                    stderr: None,
                    message: Some(format!("failed to start shell command: {err}")),
                }),
            }
        }
        Some(ExecutorSpec::Provider {
            capability,
            provider,
        }) => Ok(EvidenceRecord {
            node_id: node.id.clone(),
            attempt,
            outcome: Outcome::Unknown,
            confidence: EvidenceConfidence::Unknown,
            started_at_ms,
            duration_ms: started.elapsed().as_millis(),
            declared_authority: node.authority.clone(),
            command: None,
            exit_code: None,
            stdout: None,
            stderr: None,
            message: Some(format!(
                "provider executor is not registered in the seed runner (capability='{capability}', provider='{}')",
                provider.as_deref().unwrap_or("auto")
            )),
        }),
        None => Ok(EvidenceRecord {
            node_id: node.id.clone(),
            attempt,
            outcome: Outcome::Error,
            confidence: EvidenceConfidence::Unknown,
            started_at_ms,
            duration_ms: started.elapsed().as_millis(),
            declared_authority: node.authority.clone(),
            command: None,
            exit_code: None,
            stdout: None,
            stderr: None,
            message: Some("node has no executable provider".into()),
        }),
    }
}

fn prompt_for_approval(node: &NodeSpec, fingerprint: &str) -> io::Result<bool> {
    eprintln!();
    eprintln!("Approval required");
    eprintln!("  node: {}", node.id);
    eprintln!("  risk: {:?}", node.risk);
    eprintln!("  exact plan: {fingerprint}");
    if let Some(description) = &node.description {
        eprintln!("  action: {description}");
    }
    eprint!("Approve this exact plan? [y/N] ");
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn append_evidence(path: &Path, evidence: &EvidenceRecord) -> io::Result<()> {
    let line = serde_json::to_string(evidence)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

fn create_run_dir() -> io::Result<PathBuf> {
    let base = std::env::current_dir()?.join(".claudectl-runs");
    fs::create_dir_all(&base)?;
    let dir = base.join(format!(
        "graph-{}-{}",
        now_epoch_ms(),
        std::process::id()
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn now_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn truncate_output(s: &str) -> String {
    if s.len() <= OUTPUT_MAX_BYTES {
        return s.to_string();
    }
    let mut end = OUTPUT_MAX_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("\n[truncated]");
    out
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut process = Command::new("cmd");
    process.args(["/C", command]);
    process
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut process = Command::new("sh");
    process.args(["-lc", command]);
    process
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::compiler;
    use crate::graph::schema::{
        EdgeEndpoint, EdgeSpec, GraphPolicy, GraphSpec, RetryPolicy, RiskLevel,
    };

    fn provider_graph() -> CompiledGraph {
        compiler::compile(GraphSpec {
            version: 1,
            id: "provider-test".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes: vec![NodeSpec {
                id: "inspect".into(),
                kind: NodeKind::Tool,
                description: None,
                requires: vec![],
                authority: Authority::default(),
                risk: RiskLevel::Low,
                retry: RetryPolicy::default(),
                inputs: vec![],
                outputs: vec![],
                executor: Some(ExecutorSpec::Provider {
                    capability: "rea.binary.inspect".into(),
                    provider: Some("rea".into()),
                }),
            }],
            edges: vec![],
        })
        .unwrap()
    }

    #[test]
    fn provider_without_registration_is_unknown_not_pass() {
        let graph = provider_graph();
        let dir = tempfile::tempdir().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let report = run(&graph).unwrap();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(report.nodes["inspect"].outcome, Outcome::Unknown);
        assert!(!report.succeeded());
    }

    #[test]
    fn dependency_on_unknown_becomes_unknown() {
        let mut graph = provider_graph();
        graph.spec.nodes.push(NodeSpec {
            id: "after".into(),
            kind: NodeKind::Tool,
            description: None,
            requires: vec![],
            authority: Authority::default(),
            risk: RiskLevel::Low,
            retry: RetryPolicy::default(),
            inputs: vec![],
            outputs: vec![],
            executor: Some(ExecutorSpec::Provider {
                capability: "other".into(),
                provider: None,
            }),
        });
        graph.spec.edges.push(EdgeSpec {
            from: EdgeEndpoint {
                node: "inspect".into(),
                port: None,
            },
            to: EdgeEndpoint {
                node: "after".into(),
                port: None,
            },
            dependency_only: true,
        });
        graph = compiler::compile(graph.spec).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let report = run(&graph).unwrap();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(report.nodes["after"].outcome, Outcome::Unknown);
        assert_eq!(report.nodes["after"].attempts, 0);
    }
}
