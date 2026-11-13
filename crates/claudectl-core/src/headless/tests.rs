use super::*;
use std::os::unix::fs::PermissionsExt;

static FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fixture {
    _directory: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn fixture(script: &str) -> (Fixture, WorkerConfig) {
    // Parallel fork/exec can temporarily inherit another fixture's writable
    // script fd and cause ETXTBSY. Serialize fixture construction/lifetimes,
    // not production launches; production must never retry ambiguous launches.
    let guard = FIXTURES.lock().unwrap_or_else(|error| error.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("fake-worker");
    let sandbox = dir.path().join("fake-sandbox");
    std::fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
    // Emulates only Bubblewrap's final exec for transport tests.  It is never
    // used as an isolation claim; real activation requires /usr/bin/bwrap.
    std::fs::write(
        &sandbox,
        format!(
            "#!/bin/sh\nwhile [ \"$1\" != '{}' ]; do shift; done\nexec \"$@\"\n",
            exe.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&sandbox, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = WorkerConfig {
        executable: exe.clone(),
        workspace: dir.path().to_owned(),
        environment: BTreeMap::new(),
        timeout: Duration::from_secs(2),
        max_output_bytes: 65536,
        max_turns: 3,
        budget_usd: 1.0,
        network: NetworkMode::Deny,
        sandbox: SandboxConfig {
            // Tests inspect the launch contract only; fake-worker is never a
            // usable sandbox outside this disposable fixture.
            executable: sandbox,
            runtime_mounts: vec![PathBuf::from("/usr")],
            worker_home: PathBuf::from("/tmp/druid-worker-home"),
        },
    };
    (
        Fixture {
            _directory: dir,
            _guard: guard,
        },
        config,
    )
}

fn success() -> &'static str {
    r#"{"type":"result","subtype":"success","is_error":false,"permission_denials":[],"session_id":"fixture-1","result":"candidate only","total_cost_usd":0.01}"#
}

#[test]
fn fixed_argv_and_no_inherited_environment() {
    let (_dir, config) = fixture("exit 0");
    let command = config.command().unwrap();
    let args: Vec<_> = command.get_args().map(|v| v.to_str().unwrap()).collect();
    assert!(args.contains(&"--unshare-all"));
    assert!(args.contains(&"--bare"));
    assert!(args.contains(&"dontAsk"));
    assert!(args.contains(&"--no-session-persistence"));
    assert!(
        !args
            .iter()
            .any(|v| v.contains("skip-permissions") || v.contains("Bash"))
    );
    assert_eq!(command.get_envs().count(), 0);
    assert!(args.contains(&"--tmpfs"));
    assert!(args.contains(&"/work"));
}

#[test]
fn subscription_proxy_mode_is_exact_and_never_accepts_api_keys() {
    let (_dir, mut config) = fixture("exit 0");
    config.environment.insert("ANTHROPIC_API_KEY".into(), "forbidden".into());
    assert!(config.command().is_err());
    config.environment.clear();
    config.network = NetworkMode::RootOwnedLoopbackProxy {
        url: "http://127.0.0.1:18443".into(),
    };
    for (key, value) in [
        ("HTTP_PROXY", "http://127.0.0.1:18443"),
        ("HTTPS_PROXY", "http://127.0.0.1:18443"),
        ("NO_PROXY", ""),
        ("CLAUDE_CODE_PROXY_RESOLVES_HOSTS", "1"),
        ("DISABLE_AUTOUPDATER", "1"),
    ] {
        config.environment.insert(key.into(), value.into());
    }
    let command = config.command().unwrap();
    let args: Vec<_> = command.get_args().map(|value| value.to_str().unwrap()).collect();
    assert!(args.contains(&"--share-net"));
    config.environment.insert("NO_PROXY".into(), "localhost".into());
    assert!(config.command().is_err());
}

#[test]
fn delivers_prompt_on_stdin_and_parses_result() {
    let (_dir, config) = fixture(&format!("/bin/cat > received; printf '%s' '{}'", success()));
    let result = run(
        &config,
        "not a shell command: $(touch injected)",
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.text, "candidate only");
    assert_eq!(result.session_id, "fixture-1");
    assert_eq!(result.reported_cost_usd, 0.01);
    assert!(!config.workspace.join("injected").exists());
    assert_eq!(
        std::fs::read_to_string(config.workspace.join("received")).unwrap(),
        "not a shell command: $(touch injected)"
    );
}

#[test]
fn cancellation_before_spawn_has_no_side_effect() {
    let (_dir, config) = fixture("touch launched");
    assert!(
        run(&config, "x", &AtomicBool::new(true))
            .err()
            .unwrap()
            .contains("cancelled")
    );
    assert!(!config.workspace.join("launched").exists());
}

#[test]
fn cancellation_while_running_returns_promptly() {
    let (_dir, config) = fixture("/bin/cat >/dev/null; /bin/sleep 20");
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            cancel.store(true, Ordering::SeqCst);
        });
        assert!(
            run(&config, "x", &cancel)
                .err()
                .unwrap()
                .contains("cancelled")
        );
    });
}

#[test]
fn deadline_includes_worker_that_does_not_read_stdin() {
    let (_dir, mut config) = fixture("/bin/sleep 20");
    config.timeout = Duration::from_millis(60);
    let start = Instant::now();
    assert!(
        run(&config, &"x".repeat(32768), &AtomicBool::new(false))
            .err()
            .unwrap()
            .contains("timed out")
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn stdout_and_stderr_floods_are_bounded() {
    for redirect in ["", " >&2"] {
        let (_dir, mut config) = fixture(&format!(
            "/bin/cat >/dev/null; while :; do printf '0123456789abcdef'{redirect}; done"
        ));
        config.max_output_bytes = 1024;
        assert!(
            run(&config, "x", &AtomicBool::new(false))
                .err()
                .unwrap()
                .contains("output limit")
        );
    }
}

#[test]
fn nonzero_exit_and_invalid_json_never_pass() {
    for script in [
        "/bin/cat >/dev/null; exit 3",
        " /bin/cat >/dev/null; printf 'secret non-json output'",
    ] {
        let (_dir, config) = fixture(script);
        let error = run(&config, "x", &AtomicBool::new(false)).err().unwrap();
        assert!(!error.contains("secret"));
    }
}

#[test]
fn result_contract_fails_closed() {
    for (key, value) in [
        ("is_error", serde_json::json!(true)),
        ("subtype", serde_json::json!("error_max_turns")),
        (
            "permission_denials",
            serde_json::json!([{"tool_name":"Bash"}]),
        ),
        ("session_id", serde_json::json!("../../escape")),
        ("total_cost_usd", serde_json::json!(2)),
        ("total_cost_usd", serde_json::json!(-1)),
        ("result", serde_json::Value::Null),
    ] {
        let mut v: serde_json::Value = serde_json::from_str(success()).unwrap();
        v[key] = value;
        assert!(parse_result(&serde_json::to_vec(&v).unwrap(), 1.0).is_err());
    }
    assert!(parse_result(b"{}", 1.0).is_err());
}

#[test]
fn configuration_limits_and_environment_are_closed() {
    let (_dir, mut config) = fixture("exit 0");
    config
        .environment
        .insert("LD_PRELOAD".into(), "untrusted".into());
    assert!(config.command().is_err());
    config.environment.clear();
    config.budget_usd = f64::NAN;
    assert!(config.command().is_err());
    config.budget_usd = 1.0;
    config.executable = "claude".into();
    assert!(config.command().is_err());
}

#[test]
fn sandbox_rejects_home_mounts_and_missing_runtime() {
    let (_dir, mut config) = fixture("exit 0");
    config.sandbox.runtime_mounts = vec![PathBuf::from("/home/katalyst")];
    assert!(config.command().is_err());
    config.sandbox.runtime_mounts.clear();
    assert!(config.command().is_err());
    config.sandbox.runtime_mounts = vec![PathBuf::from("/usr")];
    config.sandbox.worker_home = PathBuf::from("/home/katalyst/worker");
    assert!(config.command().is_err());
}

#[test]
fn prompt_limits_and_missing_binary_fail_closed() {
    let (_dir, mut config) = fixture("exit 0");
    assert!(run(&config, "", &AtomicBool::new(false)).is_err());
    assert!(run(&config, &"x".repeat(32769), &AtomicBool::new(false)).is_err());
    config.executable = config.workspace.join("missing");
    assert!(run(&config, "x", &AtomicBool::new(false)).is_err());
}

#[test]
fn duplicate_security_fields_are_rejected() {
    let duplicate = success().replace("\"is_error\":false", "\"is_error\":true,\"is_error\":false");
    assert!(parse_result(duplicate.as_bytes(), 1.0).is_err());
}

#[test]
fn timeout_stops_same_group_descendants() {
    let (_dir, mut config) = fixture(
        "/bin/cat >/dev/null; (/bin/sleep 0.2; printf leaked > descendant-survived) & wait",
    );
    config.timeout = Duration::from_millis(60);
    assert!(run(&config, "x", &AtomicBool::new(false)).is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!config.workspace.join("descendant-survived").exists());
}

#[test]
fn drains_stderr_without_deadlocking_and_does_not_inherit_login_environment() {
    let (_dir, config) = fixture(&format!(
        "/bin/cat >/dev/null; /usr/bin/env > environment; /usr/bin/head -c 32768 /dev/zero >&2; printf '%s' '{}'",
        success()
    ));
    let result = run(&config, "x", &AtomicBool::new(false));
    assert!(result.is_ok(), "transport failed: {:?}", result.err());
    let env = std::fs::read_to_string(config.workspace.join("environment")).unwrap();
    for key in [
        "HOME=",
        "SSH_AUTH_SOCK=",
        "ANTHROPIC_API_KEY=",
        "CLAUDECTL_PROFILE=",
    ] {
        assert!(!env.lines().any(|line| line.starts_with(key)));
    }
}
