use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::tasks::TaskRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceMode {
    Shared,
    Worktree,
}

pub fn prepare_task_workspace(task: &TaskRow) -> Result<PathBuf, String> {
    let source = Path::new(&task.cwd)
        .canonicalize()
        .map_err(|e| format!("resolve task cwd '{}': {e}", task.cwd))?;

    match workspace_mode(task.policy.as_ref()) {
        WorkspaceMode::Shared => Ok(source),
        WorkspaceMode::Worktree => prepare_worktree(task, &source),
    }
}

fn prepare_worktree(task: &TaskRow, source: &Path) -> Result<PathBuf, String> {
    if let Some(existing) = task.workspace_path.as_deref() {
        let path = PathBuf::from(existing);
        if path.is_dir() && is_git_worktree(&path) {
            return path
                .canonicalize()
                .map_err(|e| format!("resolve existing workspace '{}': {e}", path.display()));
        }
    }

    let graph_run_id = task
        .graph_run_id
        .as_deref()
        .ok_or_else(|| "worktree workspace requires graph_run_id".to_string())?;
    let graph_node_id = task
        .graph_node_id
        .as_deref()
        .ok_or_else(|| "worktree workspace requires graph_node_id".to_string())?;

    let repo_root = git_stdout(source, &["rev-parse", "--show-toplevel"])?;
    let repo_root = PathBuf::from(repo_root.trim());
    if !repo_root.is_dir() {
        return Err(format!(
            "git reported missing repository root '{}'",
            repo_root.display()
        ));
    }
    let canonical_repo_root = repo_root
        .canonicalize()
        .map_err(|e| format!("resolve repository root '{}': {e}", repo_root.display()))?;
    if source != canonical_repo_root {
        return Err(format!(
            "durable worktree V1 requires request.cwd to be the Git repository root ('{}'), got '{}'",
            canonical_repo_root.display(),
            source.display()
        ));
    }

    let target = workspace_root()
        .join(format!("{:016x}", fnv1a64(repo_root.to_string_lossy().as_bytes())))
        .join(sanitize_component(graph_run_id))
        .join(sanitize_component(graph_node_id));

    // Crash-safe reuse: if the worktree already exists but workspace_path
    // was not persisted before the process died, reuse it before consulting
    // the mutable source checkout.
    if target.is_dir() && is_git_worktree(&target) {
        return target
            .canonicalize()
            .map_err(|e| format!("resolve existing worktree '{}': {e}", target.display()));
    }

    if target.exists() {
        return Err(format!(
            "workspace path '{}' exists but is not a Git worktree",
            target.display()
        ));
    }

    let dirty =
        git_stdout(&repo_root, &["status", "--porcelain", "--untracked-files=normal"])?;
    if !dirty.trim().is_empty() {
        return Err(format!(
            "source repository '{}' has uncommitted changes; durable worktree execution requires a clean source checkout",
            repo_root.display()
        ));
    }
    let base_commit = git_stdout(&repo_root, &["rev-parse", "HEAD"])?;
    let base_commit = base_commit.trim().to_string();
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create workspace parent '{}': {e}", parent.display()))?;
    }

    let branch = format!(
        "claudectl/{}/{}",
        sanitize_component(graph_run_id),
        sanitize_component(graph_node_id)
    );
    let target_str = target.to_string_lossy().to_string();

    let create = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args([
            "worktree",
            "add",
            "-b",
            &branch,
            &target_str,
            &base_commit,
        ])
        .output()
        .map_err(|e| format!("start git worktree add: {e}"))?;

    if !create.status.success() {
        // A crash can leave the branch created before the worktree path was
        // fully registered. Retry by attaching the existing branch.
        let reuse = Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .args(["worktree", "add", &target_str, &branch])
            .output()
            .map_err(|e| format!("retry git worktree add: {e}"))?;
        if !reuse.status.success() {
            return Err(format!(
                "create worktree failed: {}; retry failed: {}",
                compact_stderr(&create.stderr),
                compact_stderr(&reuse.stderr)
            ));
        }
    }

    target
        .canonicalize()
        .map_err(|e| format!("resolve new worktree '{}': {e}", target.display()))
}

pub fn workspace_revision(path: &Path) -> Result<String, String> {
    Ok(git_stdout(path, &["rev-parse", "HEAD"])?
        .trim()
        .to_string())
}

fn workspace_mode(policy: Option<&Value>) -> WorkspaceMode {
    match policy
        .and_then(|value| value.pointer("/workspace/mode"))
        .and_then(Value::as_str)
    {
        Some("worktree") => WorkspaceMode::Worktree,
        _ => WorkspaceMode::Shared,
    }
}

fn workspace_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".claudectl").join("worktrees")
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("start git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in '{}': {}",
            args.join(" "),
            cwd.display(),
            compact_stderr(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn is_git_worktree(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "true"
        })
}

fn sanitize_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "task".to_string()
    } else {
        out
    }
}

fn compact_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    if trimmed.len() <= 2048 {
        return trimmed.to_string();
    }
    let mut end = 2048;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [truncated]", &trimmed[..end])
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_to_shared_and_reads_worktree() {
        assert_eq!(workspace_mode(None), WorkspaceMode::Shared);
        assert_eq!(
            workspace_mode(Some(&serde_json::json!({
                "workspace": {"mode": "worktree"}
            }))),
            WorkspaceMode::Worktree
        );
    }

    #[test]
    fn workspace_components_are_path_safe() {
        assert_eq!(sanitize_component("graph/run:1"), "graph-run-1");
        assert_eq!(sanitize_component(""), "task");
    }

    #[test]
    fn repo_hash_is_stable() {
        assert_eq!(fnv1a64(b"/repo/a"), fnv1a64(b"/repo/a"));
        assert_ne!(fnv1a64(b"/repo/a"), fnv1a64(b"/repo/b"));
    }
}
