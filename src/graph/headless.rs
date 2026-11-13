//! Development adapter for an explicitly injected, host-configured worker.
//! Not registered by the CLI or durable supervisor until OS isolation and the
//! authenticated approval bridge are provisioned. No terminal-launch fallback.
use std::sync::atomic::AtomicBool;

use claudectl_core::headless::{WorkerConfig, run};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::provider::{Provider, ProviderConfidence, ProviderContext, ProviderResult};
use super::schema::{NetworkAuthority, NodeKind, NodeSpec, Outcome, WorkspaceMode};
use super::standing_policy::PolicyPermit;

pub const CAPABILITY: &str = "agent.execute";

pub struct HeadlessProvider<'a> {
    /// Trusted host configuration; caller must establish OS sandbox and route.
    /// This object cannot be enabled by a graph JSON property.
    pub worker: &'a WorkerConfig,
    pub cancelled: &'a AtomicBool,
    /// Created by the protected supervisor-side standing-policy loader. A
    /// worker cannot synthesize this permit from graph JSON or its worktree.
    pub permit: &'a PolicyPermit,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    cwd: String,
    prompt: String,
}

impl Provider for HeadlessProvider<'_> {
    fn name(&self) -> &str {
        "claude_code_headless"
    }
    fn supports(&self, capability: &str) -> bool {
        capability == CAPABILITY
    }

    fn execute(
        &self,
        node: &NodeSpec,
        capability: &str,
        request: &Value,
        _context: &ProviderContext<'_>,
    ) -> Result<ProviderResult, String> {
        if capability != CAPABILITY
            || node.id != self.permit.task_id
            || node.kind != NodeKind::Agent
            || node.workspace.mode != WorkspaceMode::Worktree
            || node.authority.git_commit
            || node.authority.git_push
            || node.authority.network != NetworkAuthority::Denied
            || node.authority.filesystem_read != ["**"]
            || node.authority.filesystem_write != ["**"]
        {
            return Err("headless adapter requires agent/worktree, whole-workspace file authority, and no Git/network-tool authority".into());
        }
        let request: Request = serde_json::from_value(request.clone())
            .map_err(|_| "invalid headless request; only cwd and prompt are supported")?;
        // The host selects one exact pre-isolated workspace, not a user-selected
        // arbitrary path. Canonical equality is a binding check, NOT a sandbox.
        let cwd = std::fs::canonicalize(&request.cwd).map_err(|_| "invalid worker cwd")?;
        if cwd != self.worker.workspace || !std::path::Path::new(&request.cwd).is_absolute() {
            return Err("headless request does not match host-bound workspace".into());
        }
        let result = run(self.worker, &request.prompt, self.cancelled)?;
        Ok(ProviderResult {
            outcome: Outcome::Pass,
            confidence: ProviderConfidence::Derived,
            stdout: None,
            stderr: None,
            // Never put arbitrary model text or stderr in the ledger. A separate
            // verifier must inspect the candidate workspace before acceptance.
            message: Some(format!(
                "headless worker completed; session={}; reported_cost_usd={}; result_sha256={:x}; candidate is NOT independently verified",
                result.session_id,
                result.reported_cost_usd,
                Sha256::digest(result.text.as_bytes())
            )),
            artifacts: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::standing_policy::PolicyPermit;
    use super::super::{compiler::compile, provider::ProviderRegistry, runner, schema::GraphSpec};
    use super::*;
    use claudectl_core::headless::{NetworkMode, SandboxConfig};
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Duration};

    #[test]
    fn existing_graph_runner_executes_fake_headless_worker_and_records_derived_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("fake-claude");
        std::fs::write(&exe, "#!/bin/sh\n/bin/cat >/dev/null\nprintf '%s' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"permission_denials\":[],\"session_id\":\"fake-1\",\"result\":\"untrusted prose\",\"total_cost_usd\":0.01}'\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let worker = WorkerConfig {
            executable: exe.clone(),
            workspace: dir.path().canonicalize().unwrap(),
            environment: BTreeMap::new(),
            timeout: Duration::from_secs(2),
            max_output_bytes: 4096,
            max_turns: 2,
            budget_usd: 1.0,
            network: NetworkMode::Deny,
            sandbox: SandboxConfig {
                executable: exe.clone(),
                runtime_mounts: vec![std::path::PathBuf::from("/usr")],
                worker_home: std::path::PathBuf::from("/tmp/druid-worker-home"),
            },
        };
        let cancel = AtomicBool::new(false);
        let adapter = HeadlessProvider {
            worker: &worker,
            cancelled: &cancel,
            permit: &PolicyPermit {
                policy_id: "test".into(),
                task_id: "candidate".into(),
                repository_remote: "https://github.com/DryadAI/claudectl".into(),
            },
        };
        let mut registry = ProviderRegistry::new();
        registry.register(&adapter);
        let graph: GraphSpec = serde_json::from_value(serde_json::json!({
            "version":1,"id":"headless-contract","nodes":[{
                "id":"candidate","kind":"agent","requires":["agent.execute"],
                "workspace":{"mode":"worktree"},
                "authority":{"filesystem_read":["**"],"filesystem_write":["**"]},
                "executor":{"kind":"provider","capability":"agent.execute","provider":"claude_code_headless",
                    "request":{"cwd":worker.workspace,"prompt":"produce candidate"}}
            }],"edges":[]
        })).unwrap();
        let compiled = compile(graph).unwrap();
        let report =
            runner::run_with_registry(&compiled, &registry, &dir.path().join("evidence")).unwrap();
        assert!(report.succeeded());
        let evidence =
            std::fs::read_to_string(std::path::Path::new(&report.run_dir).join("evidence.jsonl"))
                .unwrap();
        assert!(evidence.contains("derived"));
        assert!(evidence.contains("NOT independently verified"));
        assert!(!evidence.contains("untrusted prose"));

        let node = &compiled.spec.nodes[0];
        let context = ProviderContext {
            run_dir: dir.path(),
            attempt: 1,
        };
        for request in [
            serde_json::json!({"cwd":"/","prompt":"x"}),
            serde_json::json!({"cwd":worker.workspace,"prompt":"x","args":["--dangerously-skip-permissions"]}),
        ] {
            assert!(
                adapter
                    .execute(node, CAPABILITY, &request, &context)
                    .is_err()
            );
        }
        let mut denied = node.clone();
        denied.authority.git_push = true;
        assert!(
            adapter
                .execute(&denied, CAPABILITY, &serde_json::json!({}), &context)
                .is_err()
        );
        let mut wrong_task = node.clone();
        wrong_task.id = "worker-self-approved".into();
        assert!(
            adapter
                .execute(
                    &wrong_task,
                    CAPABILITY,
                    &serde_json::json!({"cwd":worker.workspace,"prompt":"x"}),
                    &context
                )
                .is_err()
        );
        let empty = ProviderRegistry::new();
        let report =
            runner::run_with_registry(&compiled, &empty, &dir.path().join("unconfigured")).unwrap();
        assert!(!report.succeeded());
    }
}
