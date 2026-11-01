//! Bounded Claude CLI transport, NOT an OS sandbox or an approval issuer.
//! Caller owns isolation, credentials, graph identity, and independent verification.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Trusted host configuration, never deserialized from a graph or model response.
/// Do not derive Debug: the explicitly supplied environment may contain credentials.
pub struct WorkerConfig {
    pub executable: PathBuf,
    pub workspace: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub max_turns: u32,
    pub budget_usd: f64,
}

pub struct WorkerResult {
    /// Untrusted model output, not verification evidence.
    pub text: String,
    pub session_id: String,
    pub reported_cost_usd: f64,
}

impl WorkerConfig {
    fn command(&self) -> Result<Command, String> {
        if !self.executable.is_absolute()
            || !self.workspace.is_absolute()
            || !self.workspace.is_dir()
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(3600)
            || !(1..=1_048_576).contains(&self.max_output_bytes)
            || !(1..=100).contains(&self.max_turns)
            || !self.budget_usd.is_finite()
            || self.budget_usd <= 0.0
            || self.budget_usd > 100.0
        {
            return Err("invalid trusted worker configuration".into());
        }
        if self.environment.keys().any(|key| {
            !matches!(
                key.as_str(),
                "HOME" | "PATH" | "TMPDIR" | "ANTHROPIC_API_KEY" | "ANTHROPIC_BASE_URL"
            )
        }) {
            return Err("unsupported worker environment key".into());
        }
        // No implicit PATH lookup, inherited credentials, resume, plugins, MCP, shell,
        // background tasks, or permission bypass. Bare mode requires a compatible CLI.
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .envs(&self.environment)
            .current_dir(&self.workspace)
            .args([
                "--bare",
                "-p",
                "--output-format",
                "json",
                "--no-session-persistence",
                "--tools",
                "Read,Edit,Write",
                "--allowedTools",
                "Read,Edit,Write",
                "--permission-mode",
                "dontAsk",
                "--max-turns",
            ])
            .arg(self.max_turns.to_string())
            .arg("--max-budget-usd")
            .arg(self.budget_usd.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        Ok(command)
    }
}

struct Process(Child, bool);
impl Process {
    fn stop_group(&mut self) {
        if self.1 {
            return;
        }
        self.1 = true;
        // Child was created in its own process group. Detached descendants require
        // an external cgroup/container boundary; this is not hostile-code isolation.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop_group();
        let _ = self.0.wait();
    }
}

fn nonblocking(fd: i32) -> Result<(), String> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err("worker pipe setup failed".into());
        }
    }
    Ok(())
}

/// One attempt only. The existing supervisor owns retries and dependency ordering.
/// Cancellation/timeout errors never return partial model output as success.
pub fn run(
    config: &WorkerConfig,
    prompt: &str,
    cancel: &AtomicBool,
) -> Result<WorkerResult, String> {
    if prompt.trim().is_empty() || prompt.len() > 32_768 {
        return Err("invalid worker prompt size".into());
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("worker cancelled".into());
    }
    let mut process = Process(
        config
            .command()?
            .spawn()
            .map_err(|error| format!("worker spawn failed (os code {:?})", error.raw_os_error()))?,
        false,
    );
    let mut input = process.0.stdin.take();
    let mut stdout = process.0.stdout.take().ok_or("missing worker stdout")?;
    let mut stderr = process.0.stderr.take().ok_or("missing worker stderr")?;
    nonblocking(input.as_ref().ok_or("missing worker stdin")?.as_raw_fd())?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let start = Instant::now();
    let mut written = 0;
    let mut output = Vec::new();
    let mut total = 0;
    let mut eof = [false, false];
    let mut status = None;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("worker cancelled".into());
        }
        if start.elapsed() >= config.timeout {
            return Err("worker timed out".into());
        }
        if let Some(pipe) = input.as_mut() {
            match pipe.write(&prompt.as_bytes()[written..]) {
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err("worker prompt delivery failed".into()),
            }
            if written == prompt.len() {
                input = None;
            }
        }
        for (index, pipe) in [&mut stdout as &mut dyn Read, &mut stderr]
            .into_iter()
            .enumerate()
        {
            let mut bytes = [0u8; 8192];
            match pipe.read(&mut bytes) {
                Ok(0) => eof[index] = true,
                Ok(n) => {
                    total += n;
                    if total > config.max_output_bytes {
                        return Err("worker output limit exceeded".into());
                    }
                    if index == 0 {
                        output.extend_from_slice(&bytes[..n]);
                    }
                    // stderr counts against the cap but is never retained/logged.
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err("worker output read failed".into()),
            }
        }
        if status.is_none() {
            status = process.0.try_wait().map_err(|_| "worker wait failed")?;
            if status.is_some() {
                process.stop_group();
            }
        }
        if let Some(status) = status {
            if !status.success() {
                return Err("worker exited unsuccessfully".into());
            }
            if eof.iter().all(|value| *value) {
                if written != prompt.len() {
                    return Err("incomplete worker prompt".into());
                }
                return parse_result(&output, config.budget_usd);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn parse_result(bytes: &[u8], budget: f64) -> Result<WorkerResult, String> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(rename = "type")]
        kind: String,
        subtype: String,
        is_error: bool,
        permission_denials: Vec<serde_json::Value>,
        session_id: String,
        result: String,
        total_cost_usd: f64,
    }
    let value: Envelope = serde_json::from_slice(bytes).map_err(|_| "invalid worker JSON")?;
    if value.kind != "result"
        || value.subtype != "success"
        || value.is_error
        || !value.permission_denials.is_empty()
    {
        return Err("worker result was not successful or contained permission denials".into());
    }
    let session = value.session_id;
    if session.is_empty()
        || session.len() > 128
        || !session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("invalid worker session id".into());
    }
    let cost = value.total_cost_usd;
    if !cost.is_finite() || cost < 0.0 || cost > budget {
        return Err("invalid or exceeded reported worker budget".into());
    }
    Ok(WorkerResult {
        text: value.result,
        session_id: session,
        reported_cost_usd: cost,
    })
}

#[cfg(test)]
mod tests;
