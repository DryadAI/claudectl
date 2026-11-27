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

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BrokerAction {
    ClaimAndLaunch,
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
        || request.snapshot_digest.len() != 64
        || !request
            .snapshot_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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
}
