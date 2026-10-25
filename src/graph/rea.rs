use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use super::provider::{Provider, ProviderConfidence, ProviderContext, ProviderResult};
use super::schema::{NodeSpec, Outcome};

pub const CAP_JAVASCRIPT_ANALYZE: &str = "reverse.javascript.analyze";
pub const CAP_NATIVE_ANALYZE: &str = "reverse.native.analyze";
pub const CAP_DISCOVER: &str = "reverse.capabilities.discover";

pub struct ReaCliProvider;

impl Provider for ReaCliProvider {
    fn name(&self) -> &str {
        "rea"
    }

    fn supports(&self, capability: &str) -> bool {
        matches!(
            capability,
            CAP_JAVASCRIPT_ANALYZE | CAP_NATIVE_ANALYZE | CAP_DISCOVER
        )
    }

    fn execute(
        &self,
        node: &NodeSpec,
        capability: &str,
        request: &Value,
        context: &ProviderContext<'_>,
    ) -> Result<ProviderResult, String> {
        let artifact = context.run_dir.join(format!(
            "{}.attempt-{}.rea.json",
            sanitize(&node.id),
            context.attempt
        ));
        let stdout = File::create(&artifact)
            .map_err(|e| format!("create REA evidence artifact {}: {e}", artifact.display()))?;

        let mut command = Command::new("rea");
        match capability {
            CAP_JAVASCRIPT_ANALYZE => {
                let target = required_string(request, "target")?;
                authorize_read_target(node, target)?;
                command
                    .arg("analyze-javascript-application")
                    .arg(target)
                    .arg("--json");
            }
            CAP_NATIVE_ANALYZE => {
                let target = required_string(request, "target")?;
                authorize_read_target(node, target)?;
                command.arg("analyze").arg(target);
                if let Some(provider_id) = request.get("provider_id").and_then(Value::as_str) {
                    command.arg("--provider").arg(provider_id);
                }
                command.arg("--json");
            }
            CAP_DISCOVER => {
                command.arg("capabilities").arg("--json");
            }
            other => {
                return Ok(ProviderResult::unknown(format!(
                    "REA provider does not support capability '{other}'"
                )));
            }
        }

        let output = match command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::piped())
            .output()
        {
            Ok(output) => output,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let _ = std::fs::remove_file(&artifact);
                return Ok(ProviderResult::unknown(
                    "REA CLI is not installed or not available on PATH",
                ));
            }
            Err(err) => {
                let _ = std::fs::remove_file(&artifact);
                return Err(format!("start REA CLI: {err}"));
            }
        };

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let artifact_path = artifact.display().to_string();

        if output.status.success() {
            Ok(ProviderResult {
                outcome: Outcome::Pass,
                // REA's JSON is itself an evidence-bearing analysis product.
                // claudectl records that it was derived by an external provider
                // rather than pretending the graph runner observed the target.
                confidence: ProviderConfidence::Derived,
                stdout: None,
                stderr: (!stderr.is_empty()).then_some(stderr),
                message: Some(format!(
                    "REA completed capability '{capability}'; full evidence saved to {artifact_path}"
                )),
                artifacts: vec![artifact_path],
            })
        } else {
            Ok(ProviderResult {
                outcome: Outcome::Error,
                confidence: ProviderConfidence::Observed,
                stdout: None,
                stderr: (!stderr.is_empty()).then_some(stderr),
                message: Some(format!(
                    "REA capability '{capability}' exited with status {}",
                    output
                        .status
                        .code()
                        .map_or_else(|| "signal".into(), |code| code.to_string())
                )),
                artifacts: vec![artifact_path],
            })
        }
    }
}

fn authorize_read_target(node: &NodeSpec, target: &str) -> Result<(), String> {
    let target =
        std::fs::canonicalize(target).map_err(|e| format!("resolve REA target '{target}': {e}"))?;

    for rule in &node.authority.filesystem_read {
        if let Some(prefix) = rule.strip_suffix("/**") {
            if canonical_prefix(prefix).is_some_and(|allowed| target.starts_with(allowed)) {
                return Ok(());
            }
        } else if std::fs::canonicalize(rule).is_ok_and(|allowed| allowed == target) {
            return Ok(());
        }
    }

    Err(format!(
        "REA target '{}' is outside declared filesystem_read authority",
        target.display()
    ))
}

fn canonical_prefix(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        return Some(Path::new("/").to_path_buf());
    }
    std::fs::canonicalize(value).ok()
}

fn required_string<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    request
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("REA request requires non-empty string field '{key}'"))
}

fn sanitize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() { "rea".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_capabilities_are_explicit() {
        let provider = ReaCliProvider;
        assert!(provider.supports(CAP_JAVASCRIPT_ANALYZE));
        assert!(provider.supports(CAP_NATIVE_ANALYZE));
        assert!(provider.supports(CAP_DISCOVER));
        assert!(!provider.supports("code.write"));
    }

    #[test]
    fn native_request_requires_target() {
        assert!(required_string(&serde_json::json!({}), "target").is_err());
        assert_eq!(
            required_string(&serde_json::json!({"target": "/tmp/app"}), "target").unwrap(),
            "/tmp/app"
        );
    }

    #[test]
    fn target_must_be_inside_declared_read_authority() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir_all(&app).unwrap();

        let mut node = NodeSpec {
            id: "inspect".into(),
            kind: super::super::schema::NodeKind::Tool,
            description: None,
            requires: vec![CAP_JAVASCRIPT_ANALYZE.into()],
            authority: super::super::schema::Authority::default(),
            workspace: Default::default(),
            risk: super::super::schema::RiskLevel::Low,
            retry: super::super::schema::RetryPolicy::default(),
            inputs: vec![],
            outputs: vec![],
            executor: None,
        };

        assert!(authorize_read_target(&node, app.to_str().unwrap()).is_err());

        node.authority.filesystem_read = vec![format!("{}/**", dir.path().display())];
        assert!(authorize_read_target(&node, app.to_str().unwrap()).is_ok());
    }

    #[test]
    fn artifact_slug_is_path_safe() {
        assert_eq!(sanitize("inspect app/one"), "inspect-app-one");
    }
}
