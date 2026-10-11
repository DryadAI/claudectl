pub mod compiler;
pub mod provider;
pub mod runner;
pub mod schema;

use std::io;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum GraphCommand {
    /// Validate graph syntax, typed edges, policy, risk gates, and acyclicity.
    Validate {
        /// Path to a graph JSON file.
        path: String,
    },
    /// Compile a graph and print its deterministic execution waves.
    Plan {
        /// Path to a graph JSON file.
        path: String,
    },
    /// Execute a graph with the minimal local seed runner.
    ///
    /// The seed runner supports shell/check nodes and interactive approval.
    /// Provider-backed agent/tool/subgraph nodes remain UNKNOWN until a provider
    /// adapter (for example REA) is registered.
    Run {
        /// Path to a graph JSON file.
        path: String,
    },
}

pub fn dispatch(command: &GraphCommand, json: bool) -> io::Result<()> {
    match command {
        GraphCommand::Validate { path } => {
            let compiled = load_and_compile(path)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "valid",
                        "graph_id": compiled.spec.id,
                        "fingerprint": compiled.fingerprint,
                        "waves": compiled.waves,
                    })
                );
            } else {
                println!("Graph '{}' is valid.", compiled.spec.id);
                println!("Fingerprint: {}", compiled.fingerprint);
                println!("Execution waves: {}", compiled.waves.len());
            }
            Ok(())
        }
        GraphCommand::Plan { path } => {
            let compiled = load_and_compile(path)?;
            if json {
                let output = serde_json::to_string_pretty(&compiled)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                println!("{output}");
            } else {
                println!("Graph: {}", compiled.spec.id);
                if let Some(goal) = &compiled.spec.goal {
                    println!("Goal: {goal}");
                }
                println!("Fingerprint: {}", compiled.fingerprint);
                for (index, wave) in compiled.waves.iter().enumerate() {
                    println!("Wave {}: {}", index + 1, wave.join(", "));
                }
            }
            Ok(())
        }
        GraphCommand::Run { path } => {
            let compiled = load_and_compile(path)?;
            if !json {
                println!(
                    "Running graph '{}' ({} waves, {})",
                    compiled.spec.id,
                    compiled.waves.len(),
                    compiled.fingerprint
                );
            }
            let report = runner::run(&compiled)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                );
            } else {
                for (node_id, result) in &report.nodes {
                    println!(
                        "  {:<8} {} ({} attempt{})",
                        format!("{:?}", result.outcome).to_ascii_uppercase(),
                        node_id,
                        result.attempts,
                        if result.attempts == 1 { "" } else { "s" }
                    );
                }
                println!("Evidence: {}/evidence.jsonl", report.run_dir);
                println!("Summary:  {}/summary.json", report.run_dir);
            }

            if report.succeeded() {
                Ok(())
            } else {
                Err(io::Error::other(
                    "graph stopped with FAIL, UNKNOWN, or ERROR outcome",
                ))
            }
        }
    }
}

fn load_and_compile(path: &str) -> io::Result<compiler::CompiledGraph> {
    let content = std::fs::read_to_string(path)?;
    let spec: schema::GraphSpec = serde_json::from_str(&content)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("graph JSON: {e}")))?;
    compiler::compile(spec)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("graph compile: {e}")))
}
