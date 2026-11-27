//! Inert wire contract for a future root-owned local execution broker.
//!
//! The existing coord supervisor remains the only scheduler. This module
//! authenticates a local peer and validates an *intent*, not authority. It
//! never opens a ledger, reads policy, claims an attempt, or launches a worker.
//! A future broker must perform those operations behind a root-owned boundary
//! and may not treat any field here as a permit.

// The protocol is deliberately not called by production code yet. Keep
// dead-code warnings local to this staged boundary until the protected host
// implementation can consume it safely.
#![allow(dead_code)]

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::Deserialize;

const MAX_FRAME_BYTES: usize = 512;
const PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrokerIntent {
    version: u8,
    action: BrokerAction,
    task_id: String,
    expected_attempt: u32,
    snapshot_digest: String,
}

/// Exact human-approved admission data for one graph task. This is only a
/// parsed record: a future root service must load it through a protected,
/// race-resistant owner/mode-checked path and separately check revocation.
/// Neither a client-supplied copy nor this parser grants authority.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProtectedAdmission {
    version: u8,
    task_id: String,
    snapshot_digest: String,
    policy_sha256: String,
    repository_remote: String,
    expires_at_unix_ms: u128,
    max_attempts: u32,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BrokerAction {
    ClaimAndLaunch,
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Validate the *contents* of a protected admission against the requested
/// task and the independently accepted policy. Does not read the file, check
/// its owner, inspect the coordination DB, or claim/launch an attempt.
pub(crate) fn validate_protected_admission(
    bytes: &[u8],
    intent: &BrokerIntent,
    policy_sha256: &str,
    repository_remote: &str,
    now_unix_ms: u128,
) -> Result<ProtectedAdmission, String> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err("protected admission size is invalid".into());
    }
    let admission: ProtectedAdmission =
        serde_json::from_slice(bytes).map_err(|_| "invalid protected admission")?;
    if admission.version != PROTOCOL_VERSION
        || admission.task_id != intent.task_id
        || admission.snapshot_digest != intent.snapshot_digest
        || !is_lower_sha256(&admission.policy_sha256)
        || !is_lower_sha256(policy_sha256)
        || admission.policy_sha256 != policy_sha256
        || admission.repository_remote != repository_remote
        || admission.repository_remote.is_empty()
        || now_unix_ms >= admission.expires_at_unix_ms
        || admission.max_attempts == 0
        || admission.max_attempts > 6
        || intent.expected_attempt > admission.max_attempts
    {
        return Err("protected admission does not authorize this exact intent".into());
    }
    Ok(admission)
}

/// Read peer identity from the kernel, never from request JSON or an
/// environment variable. The future service must additionally use a
/// root-owned socket and a protected, fixed allowed UID.
fn peer_uid(stream: &UnixStream) -> Result<u32, String> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `stream` owns a valid socket FD; `credentials` and `length`
    // point to initialized writable storage of the declared sizes.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if status != 0 || length as usize != std::mem::size_of::<libc::ucred>() {
        return Err("local broker peer credentials unavailable".into());
    }
    Ok(credentials.uid)
}

/// Authenticate before parsing; a caller cannot use a forged JSON UID to
/// discover broker policy. Framing and peer authorization are necessary but
/// not sufficient to authorize any privileged effect.
pub(crate) fn validate_intent(
    stream: &UnixStream,
    frame: &[u8],
    allowed_uid: u32,
) -> Result<BrokerIntent, String> {
    if peer_uid(stream)? != allowed_uid {
        return Err("local broker peer is not authorized".into());
    }
    if frame.is_empty() || frame.len() > MAX_FRAME_BYTES {
        return Err("local broker request size is invalid".into());
    }
    let request: BrokerIntent =
        serde_json::from_slice(frame).map_err(|_| "invalid local broker request")?;
    if request.version != PROTOCOL_VERSION
        || request.expected_attempt == 0
        || request.expected_attempt > 6
        || !request.task_id.starts_with("task_")
        || !(6..=128).contains(&request.task_id.len())
        || !request
            .task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || !is_lower_sha256(&request.snapshot_digest)
    {
        return Err("local broker request fields are invalid".into());
    }
    Ok(request)
}

/// One request per connection: a two-byte big-endian length followed by a
/// bounded JSON object. A read timeout prevents a connected client from
/// holding a future service thread forever with a partial frame.
pub(crate) fn read_intent(
    stream: &mut UnixStream,
    allowed_uid: u32,
) -> Result<BrokerIntent, String> {
    if peer_uid(stream)? != allowed_uid {
        return Err("local broker peer is not authorized".into());
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "local broker read timeout unavailable")?;
    let mut header = [0u8; 2];
    stream
        .read_exact(&mut header)
        .map_err(|_| "local broker request header is incomplete")?;
    let length = usize::from(u16::from_be_bytes(header));
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err("local broker request size is invalid".into());
    }
    let mut frame = vec![0u8; length];
    stream
        .read_exact(&mut frame)
        .map_err(|_| "local broker request body is incomplete")?;
    validate_intent(stream, &frame, allowed_uid)
}

/// Explicit closed endpoint until protected ledger access, unit/cgroup
/// reconciliation, independent verification, and host proof are implemented.
/// This function cannot consume the validated intent as launch authority.
pub(crate) fn dispatch_inert(_intent: &BrokerIntent) -> Result<(), String> {
    Err("durable headless broker is not activated".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn valid() -> Vec<u8> {
        serde_json::json!({
            "version": 1,
            "action": "claim_and_launch",
            "task_id": "task_123_456_0",
            "expected_attempt": 1,
            "snapshot_digest": "a".repeat(64),
        })
        .to_string()
        .into_bytes()
    }

    fn valid_admission() -> Vec<u8> {
        serde_json::json!({
            "version": 1,
            "task_id": "task_123_456_0",
            "snapshot_digest": "a".repeat(64),
            "policy_sha256": "b".repeat(64),
            "repository_remote": "https://github.com/DryadAI/claudectl",
            "expires_at_unix_ms": 100,
            "max_attempts": 2,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn authenticates_kernel_peer_and_remains_inert() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let uid = unsafe { libc::geteuid() };
        let request = validate_intent(&server, &valid(), uid).unwrap();
        assert_eq!(request.expected_attempt, 1);
        assert!(dispatch_inert(&request).is_err());
        assert!(validate_intent(&client, &valid(), uid.wrapping_add(1)).is_err());
        let frame = valid();
        client
            .write_all(&(frame.len() as u16).to_be_bytes())
            .unwrap();
        client.write_all(&frame).unwrap();
        assert_eq!(read_intent(&mut server, uid).unwrap(), request);
    }

    #[test]
    fn rejects_client_authority_and_malformed_frames() {
        let (_client, server) = UnixStream::pair().unwrap();
        let uid = unsafe { libc::geteuid() };
        for field in [
            "uid",
            "policy_path",
            "worktree_path",
            "repository_remote",
            "route",
            "command",
            "model",
            "budget_usd",
            "approval",
            "credential",
        ] {
            let mut forged: serde_json::Value = serde_json::from_slice(&valid()).unwrap();
            forged[field] = serde_json::json!("client-controlled");
            assert!(
                validate_intent(&server, forged.to_string().as_bytes(), uid).is_err(),
                "{field}"
            );
        }
        for bad in [Vec::new(), vec![b' '; MAX_FRAME_BYTES + 1], b"{}".to_vec()] {
            assert!(validate_intent(&server, &bad, uid).is_err());
        }
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(&(513u16).to_be_bytes()).unwrap();
        assert!(read_intent(&mut server, uid).is_err());
        for (field, value) in [
            ("version", serde_json::json!(2)),
            ("action", serde_json::json!("deploy")),
            ("task_id", serde_json::json!("task_../escape")),
            ("task_id", serde_json::json!("task_")),
            ("task_id", serde_json::json!("task_💥")),
            ("expected_attempt", serde_json::json!(0)),
            ("expected_attempt", serde_json::json!(7)),
            ("snapshot_digest", serde_json::json!("A".repeat(64))),
        ] {
            let mut forged: serde_json::Value = serde_json::from_slice(&valid()).unwrap();
            forged[field] = value;
            assert!(
                validate_intent(&server, forged.to_string().as_bytes(), uid).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn admission_requires_exact_task_snapshot_policy_repository_and_attempt() {
        let (_client, server) = UnixStream::pair().unwrap();
        let uid = unsafe { libc::geteuid() };
        let intent = validate_intent(&server, &valid(), uid).unwrap();
        let policy = "b".repeat(64);
        let repo = "https://github.com/DryadAI/claudectl";
        assert!(
            validate_protected_admission(&valid_admission(), &intent, &policy, repo, 99).is_ok()
        );
        assert!(
            validate_protected_admission(&valid_admission(), &intent, &policy, repo, 100).is_err()
        );
        assert!(
            validate_protected_admission(&valid_admission(), &intent, &"c".repeat(64), repo, 99)
                .is_err()
        );
        assert!(
            validate_protected_admission(&valid_admission(), &intent, &policy, "other", 99)
                .is_err()
        );
        for (field, value) in [
            ("task_id", serde_json::json!("task_other")),
            ("snapshot_digest", serde_json::json!("c".repeat(64))),
            ("policy_sha256", serde_json::json!("B".repeat(64))),
            ("max_attempts", serde_json::json!(0)),
            ("max_attempts", serde_json::json!(7)),
            ("version", serde_json::json!(2)),
            ("approval", serde_json::json!(true)),
        ] {
            let mut forged: serde_json::Value = serde_json::from_slice(&valid_admission()).unwrap();
            forged[field] = value;
            assert!(
                validate_protected_admission(
                    forged.to_string().as_bytes(),
                    &intent,
                    &policy,
                    repo,
                    99
                )
                .is_err(),
                "{field}"
            );
        }
        let mut intent2 = intent;
        intent2.expected_attempt = 3;
        assert!(
            validate_protected_admission(&valid_admission(), &intent2, &policy, repo, 99).is_err()
        );
    }
}
