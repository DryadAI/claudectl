use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::coord::session_policy::{self, GraphSessionPolicy};

#[derive(Debug, Default, Deserialize)]
struct HookPayload {
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
    #[serde(default, alias = "toolName")]
    tool_name: Option<String>,
    #[serde(default, alias = "toolInput")]
    tool_input: Value,
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Allow,
    Deny(String),
}

pub fn authorize_stdin() -> io::Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let Ok(payload) = serde_json::from_str::<HookPayload>(&raw) else {
        return Ok(());
    };
    let (Some(session_id), Some(tool_name)) =
        (payload.session_id.as_deref(), payload.tool_name.as_deref())
    else {
        return Ok(());
    };

    let policy = match session_policy::read_strict(session_id) {
        Ok(Some(policy)) => Some(policy),
        Ok(None) => match std::env::current_dir()
            .map_err(io::Error::other)
            .and_then(|cwd| {
                session_policy::read_workspace_strict(&cwd).map_err(io::Error::other)
            })
        {
            Ok(policy) => policy,
            Err(err) => {
                emit_deny(&format!(
                    "graph workspace authority policy could not be read safely: {err}"
                ))?;
                return Ok(());
            }
        },
        Err(err) => {
            emit_deny(&format!(
                "graph authority policy could not be read safely: {err}"
            ))?;
            return Ok(());
        }
    };
    let Some(policy) = policy else {
        return Ok(());
    };
    let Some(graph) = policy.graph.as_ref() else {
        return Ok(());
    };

    match authorize(graph, tool_name, &payload.tool_input) {
        Decision::Allow => Ok(()),
        Decision::Deny(reason) => emit_deny(&reason),
    }
}

fn authorize(graph: &GraphSessionPolicy, tool_name: &str, input: &Value) -> Decision {
    match tool_name {
        // Durable graph agents deliberately do not receive a general shell.
        // Deterministic shell work belongs in verifier/check nodes where exit
        // codes and evidence are captured by claudectl.
        "Bash" => Decision::Deny(
            "Bash is disabled for durable graph agents; use structured file tools. Tests and checks run through claudectl verifiers.".into(),
        ),
        "Write" | "Edit" | "NotebookEdit" => {
            authorize_file_tool(graph, input, "file_path", &graph.filesystem_write, "write")
        }
        "Read" => authorize_file_tool(
            graph,
            input,
            "file_path",
            &graph.filesystem_read,
            "read",
        ),
        "Glob" | "Grep" => {
            let path = input.get("path").and_then(Value::as_str).unwrap_or(".");
            authorize_path(graph, path, &graph.filesystem_read, "read")
        }
        "WebFetch" | "WebSearch" => {
            if graph.network {
                Decision::Allow
            } else {
                Decision::Deny("network access is not authorized for this graph node".into())
            }
        }
        // Purely in-session bookkeeping; no filesystem/network authority.
        "TodoWrite" => Decision::Allow,
        other => Decision::Deny(format!(
            "tool '{other}' is not in the durable graph agent allowlist"
        )),
    }
}

fn authorize_file_tool(
    graph: &GraphSessionPolicy,
    input: &Value,
    key: &str,
    scopes: &[String],
    operation: &str,
) -> Decision {
    let Some(path) = input.get(key).and_then(Value::as_str) else {
        return Decision::Deny(format!(
            "{operation} tool input is missing required path field '{key}'"
        ));
    };
    authorize_path(graph, path, scopes, operation)
}

fn authorize_path(
    graph: &GraphSessionPolicy,
    requested: &str,
    scopes: &[String],
    operation: &str,
) -> Decision {
    let workspace = match std::fs::canonicalize(&graph.workspace_root) {
        Ok(path) => path,
        Err(err) => {
            return Decision::Deny(format!(
                "cannot resolve graph workspace '{}': {err}",
                graph.workspace_root
            ));
        }
    };

    let raw = Path::new(requested);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace.join(raw)
    };
    let Some(normalized) = normalize_lexical(&joined) else {
        return Decision::Deny(format!(
            "{operation} path '{requested}' escapes the graph workspace"
        ));
    };
    if !normalized.starts_with(&workspace) {
        return Decision::Deny(format!(
            "{operation} path '{requested}' is outside the graph workspace"
        ));
    }

    if let Some(existing) = nearest_existing_ancestor(&normalized) {
        match std::fs::canonicalize(existing) {
            Ok(canonical) if canonical.starts_with(&workspace) => {}
            Ok(_) => {
                return Decision::Deny(format!(
                    "{operation} path '{requested}' resolves through a symlink outside the graph workspace"
                ));
            }
            Err(err) => {
                return Decision::Deny(format!(
                    "cannot validate {operation} path '{requested}': {err}"
                ));
            }
        }
    }

    let relative = normalized
        .strip_prefix(&workspace)
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");

    if scopes.iter().any(|scope| scope_matches(scope, &relative)) {
        Decision::Allow
    } else {
        Decision::Deny(format!(
            "{operation} path '{requested}' is outside declared {operation} authority"
        ))
    }
}

fn scope_matches(scope: &str, relative: &str) -> bool {
    let scope = scope.trim().trim_start_matches("./").replace('\\', "/");
    let relative = relative.trim_start_matches("./");

    if scope == "**" {
        return true;
    }
    if let Some(prefix) = scope.strip_suffix("/**") {
        let prefix = prefix.trim_end_matches('/');
        return relative == prefix || relative.starts_with(&format!("{prefix}/"));
    }
    relative == scope
}

fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Some(out)
}

fn nearest_existing_ancestor(path: &Path) -> Option<&Path> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

fn emit_deny(reason: &str) -> io::Result<()> {
    let output = serde_json::json!({
        "decision": "deny",
        "reason": format!("claudectl graph policy: {reason}"),
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
            "continueOnBlock": true,
            "systemMessage": format!("claudectl graph policy blocked this call: {reason}")
        }
    });
    println!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(root: &Path) -> GraphSessionPolicy {
        GraphSessionPolicy {
            workspace_root: root.display().to_string(),
            filesystem_read: vec!["src/**".into(), "README.md".into()],
            filesystem_write: vec!["src/**".into()],
            network: false,
            git_commit: false,
            git_push: false,
        }
    }

    #[test]
    fn graph_policy_denies_bash_and_network_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy(dir.path());
        assert!(matches!(
            authorize(&p, "Bash", &serde_json::json!({"command":"ls"})),
            Decision::Deny(_)
        ));
        assert!(matches!(
            authorize(&p, "WebFetch", &serde_json::json!({"url":"https://example.com"})),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn graph_policy_allows_scoped_write_and_denies_escape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let p = policy(dir.path());

        assert_eq!(
            authorize(
                &p,
                "Write",
                &serde_json::json!({"file_path":"src/new.rs"})
            ),
            Decision::Allow
        );
        assert!(matches!(
            authorize(
                &p,
                "Write",
                &serde_json::json!({"file_path":"../outside.rs"})
            ),
            Decision::Deny(_)
        ));
        assert!(matches!(
            authorize(
                &p,
                "Write",
                &serde_json::json!({"file_path":"README.md"})
            ),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn scope_matching_supports_exact_and_recursive_directory() {
        assert!(scope_matches("src/**", "src/lib.rs"));
        assert!(scope_matches("src/**", "src"));
        assert!(scope_matches("README.md", "README.md"));
        assert!(!scope_matches("README.md", "docs/README.md"));
    }
}
