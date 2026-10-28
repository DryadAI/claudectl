#![cfg(all(feature = "coord", unix))]
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_claudectl"))
}
fn init(path: &Path) {
    let out = cli().args(["profile", "init"]).arg(path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn hook(root: &Path, cwd: &Path, input: &str) -> Output {
    let mut child = cli()
        .env("CLAUDECTL_PROFILE", root)
        .current_dir(cwd)
        .args(["graph", "authorize-tool"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
fn denied(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn creates_private_profile_without_copying_credentials_and_refuses_reuse() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile with spaces");
    init(&root);
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!root.join(".claude/.credentials.json").exists());
    assert!(
        cli()
            .args(["profile", "check"])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        !cli()
            .args(["profile", "init"])
            .arg(&root)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn missing_and_malformed_authority_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    init(&root);
    for input in [
        "not json",
        "{}",
        r#"{"session_id":"test","tool_name":"Read","tool_input":{"file_path":"a"}}"#,
        r#"{"session_id":"../escape","tool_name":"Read"}"#,
    ] {
        denied(hook(&root, temp.path(), input));
    }
}

#[test]
fn real_hook_allows_scoped_edit_and_denies_shell_escape_and_network() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    init(&root);
    let policy_dir = root.join(".claudectl/coord/session-policy");
    fs::create_dir_all(&policy_dir).unwrap();
    fs::write(policy_dir.join("test.json"), serde_json::to_vec(&json!({
        "task_id":"test", "approve_mode":"force_manual", "written_at":"2026-10-28T00:00:00Z",
        "graph":{"workspace_root":temp.path(),"filesystem_read":["**"],"filesystem_write":["allowed.txt"],"network":false,"git_commit":false,"git_push":false}
    })).unwrap()).unwrap();
    let allowed = hook(&root, temp.path(), &json!({"session_id":"test","tool_name":"Write","tool_input":{"file_path":temp.path().join("allowed.txt"),"content":"test"}}).to_string());
    assert!(allowed.status.success());
    assert!(allowed.stdout.is_empty());
    for (tool, input) in [
        (
            "Write",
            json!({"file_path":temp.path().join("outside.txt")}),
        ),
        ("Bash", json!({"command":"true"})),
        ("WebFetch", json!({"url":"https://example.org"})),
    ] {
        denied(hook(
            &root,
            temp.path(),
            &json!({"session_id":"test","tool_name":tool,"tool_input":input}).to_string(),
        ));
    }
}

#[test]
fn telemetry_stays_in_selected_profile() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    init(&first);
    init(&second);
    let mut child = cli()
        .env("CLAUDECTL_PROFILE", &first)
        .args(["ingest", "--hook", "SessionStart"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"isolated"}"#)
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert!(first.join(".claudectl/coord/coord.db").exists());
    assert!(!second.join(".claudectl/coord/coord.db").exists());
}

#[test]
fn altered_settings_and_identity_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    init(&root);
    let settings = root.join(".claude/settings.json");
    let original = fs::read(&settings).unwrap();
    fs::write(&settings, b"{}").unwrap();
    assert!(!hook(&root, temp.path(), "{}").status.success());
    fs::write(&settings, original).unwrap();
    let path = root.join("profile.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["sha256"] = json!("wrong");
    fs::write(path, value.to_string()).unwrap();
    assert!(!hook(&root, temp.path(), "{}").status.success());
}

#[test]
fn unsupported_commands_and_symlink_profiles_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    init(&root);
    assert!(
        !cli()
            .env("CLAUDECTL_PROFILE", &root)
            .arg("--init")
            .output()
            .unwrap()
            .status
            .success()
    );
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    assert!(!hook(&alias, temp.path(), "{}").status.success());
}
