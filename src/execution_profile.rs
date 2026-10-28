//! Opt-in Linux/Unix graph profiles. State isolation, not an OS sandbox.
use clap::Subcommand;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Create a new private directory. Refuses existing paths; never copies login data.
    Init { path: PathBuf },
    /// Check pinned binary and safety settings; does not claim live agent readiness.
    Check { path: PathBuf },
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}
fn digest(path: &Path) -> io::Result<String> {
    use io::Read;
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let len = file.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hasher.update(&buffer[..len]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn settings(root: &Path, binary: &Path) -> Value {
    let invocation = format!(
        "env CLAUDECTL_PROFILE={} {}",
        quote(&root.to_string_lossy()),
        quote(&binary.to_string_lossy())
    );
    let mut hooks = serde_json::Map::new();
    for event in ["PreToolUse", "PostToolUse", "SessionStart", "Stop"] {
        let mut entries = vec![];
        if event == "PreToolUse" {
            // Claude only treats exit 2 as a blocking hook error. Preserve denial
            // even if the pinned executable disappears or fails to start.
            entries.push(json!({"type":"command", "command":format!("{invocation} graph authorize-tool || exit 2"), "timeout":10, "onFailure":"block"}));
        }
        entries.push(json!({"type":"command", "command":format!("{invocation} ingest --hook {event}"), "timeout":10}));
        hooks.insert(event.into(), json!([{"matcher":"*", "hooks":entries}]));
    }
    json!({"hooks":hooks, "permissions":{"defaultMode":"default"}})
}

#[cfg(unix)]
fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}
#[cfg(not(unix))]
fn private_dir(_path: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "isolated profiles currently require Unix private directories",
    ))
}

pub fn dispatch(command: &ProfileCommand) -> io::Result<()> {
    match command {
        ProfileCommand::Init { path } => {
            if !path.is_absolute() {
                return Err(io::Error::other("profile path must be absolute"));
            }
            // create, never create_all: existing files/directories/symlinks fail.
            private_dir(path)?;
            let root = path.canonicalize()?;
            for dir in [".claude", ".claudectl", "logs"] {
                private_dir(&root.join(dir))?;
            }
            private_dir(&root.join(".claude/sessions"))?;
            let binary = std::env::current_exe()?.canonicalize()?;
            fs::write(
                root.join(".claude/settings.json"),
                serde_json::to_vec_pretty(&settings(&root, &binary))?,
            )?;
            fs::write(
                root.join("profile.json"),
                serde_json::to_vec_pretty(
                    &json!({"version":1,"root":root,"binary":binary,"sha256":digest(&binary)?}),
                )?,
            )?;
            println!(
                "Created {}. Login is separate; no credentials copied. Live hook proof is still required.",
                root.display()
            );
            Ok(())
        }
        ProfileCommand::Check { path } => {
            validate(path)?;
            println!(
                "Profile settings and binary pin verified. This is not live-agent verification."
            );
            Ok(())
        }
    }
}

pub fn validate_active() -> io::Result<PathBuf> {
    let root =
        claudectl_core::profile::root().ok_or_else(|| io::Error::other("no active profile"))?;
    validate(&root)?;
    Ok(root)
}

fn validate(root: &Path) -> io::Result<()> {
    if !root.is_absolute() || root.canonicalize()? != root {
        return Err(io::Error::other(
            "profile must be a canonical absolute path",
        ));
    }
    for name in ["", ".claude", ".claudectl", "logs", ".claude/sessions"] {
        let metadata = fs::symlink_metadata(root.join(name))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::other("profile directories cannot be symlinks"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::other(
                    "profile directories must be private (0700)",
                ));
            }
        }
    }
    let manifest: Value = serde_json::from_slice(&fs::read(root.join("profile.json"))?)?;
    let binary = std::env::current_exe()?.canonicalize()?;
    if manifest["version"] != 1
        || manifest["root"] != json!(root)
        || manifest["binary"] != json!(binary)
        || manifest["sha256"] != digest(&binary)?
    {
        return Err(io::Error::other(
            "profile binary or identity changed; create a new profile",
        ));
    }
    let actual: Value = serde_json::from_slice(&fs::read(root.join(".claude/settings.json"))?)?;
    if actual != settings(root, &binary) {
        return Err(io::Error::other("profile safety settings changed"));
    }
    Ok(())
}

/// Launch adapter only: dependencies, retries, evidence and scheduling remain
/// owned by the existing supervisor. No terminal server or shared session reuse.
pub fn spawn(cwd: &Path, prompt: &str) -> io::Result<String> {
    let root = validate_active()?;
    let policy =
        crate::coord::session_policy::read_workspace_strict(cwd).map_err(io::Error::other)?;
    if policy.as_ref().and_then(|p| p.graph.as_ref()).is_none() {
        return Err(io::Error::other(
            "isolated agent requires graph workspace authority",
        ));
    }
    // Linux supplies collision-resistant UUIDs without adding another dependency.
    let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_owned();
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("logs").join(format!("{id}.log")))?;
    let mut child = agent_command(&root, cwd, prompt, &id)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .spawn()?;
    let record = json!({"pid":child.id(), "sessionId":id, "cwd":cwd,
        "startedAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()});
    if let Err(err) = fs::write(
        root.join(".claude/sessions").join(format!("{id}.json")),
        serde_json::to_vec(&record)?,
    ) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(id)
}

fn agent_command(root: &Path, cwd: &Path, prompt: &str, id: &str) -> Command {
    let mut command = Command::new("claude");
    command
        .current_dir(cwd)
        .env_clear()
        .envs(
            ["HOME", "PATH", "USER", "LANG", "TERM", "TMPDIR"]
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
        )
        .env("CLAUDE_CONFIG_DIR", root.join(".claude"))
        .env("CLAUDECTL_PROFILE", root)
        .env_remove("CLAUDE_CODE_SIMPLE")
        .args([
            "--setting-sources",
            "user",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--session-id",
            id,
            "--permission-mode",
            "default",
            "--allowedTools",
            "Read,Write,Edit,Glob,Grep,TodoWrite",
            "-p",
            prompt,
        ]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_uses_profile_settings_without_permission_bypass_or_shell() {
        let command = agent_command(
            Path::new("/private/profile"),
            Path::new("/repo"),
            "literal $(prompt)",
            "id",
        );
        assert_eq!(command.get_program(), "claude");
        assert_eq!(command.get_current_dir(), Some(Path::new("/repo")));
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(args.windows(2).any(|a| a == ["--setting-sources", "user"]));
        assert!(args.contains(&"--strict-mcp-config"));
        assert!(
            !args
                .iter()
                .any(|a| a.contains("skip-permissions") || *a == "--bare")
        );
        assert_eq!(args.last(), Some(&"literal $(prompt)"));
        let env: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new("CLAUDE_CONFIG_DIR")],
            Some(std::ffi::OsStr::new("/private/profile/.claude"))
        );
        assert!(!env.contains_key(std::ffi::OsStr::new("ANTHROPIC_API_KEY")));
    }
    #[test]
    fn authority_wrapper_blocks_executable_failure_and_quotes_paths() {
        let value = settings(Path::new("/tmp/a'b"), Path::new("/missing binary"));
        let command = value["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(command.contains("'\"'\"'"));
        let result = Command::new("sh").args(["-c", command]).output().unwrap();
        assert_eq!(result.status.code(), Some(2));
    }
}
