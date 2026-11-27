//! Staged, read-only wire contract for a future protected coordinator.
//!
//! No listener, service, ledger opener, SQL dispatcher or client is wired to
//! this module. The existing supervisor remains the only scheduler. A future
//! coordinator must pin the permitted peer UID in protected host configuration
//! and open its own protected ledger; a request ID is never authority.

#![allow(dead_code)]

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const VERSION: u8 = 1;
const MAX_REQUEST_BYTES: usize = 256;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadRequest {
    pub(crate) version: u8,
    pub(crate) operation: ReadOperation,
    pub(crate) id: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadOperation {
    TaskState,
    GraphState,
}

/// Deliberately excludes prompts, policy, worktree paths, evidence payloads,
/// admission records and credentials. A future dispatcher must construct
/// this from explicit column projections, not serialize a whole task row.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ReadResponse {
    NotFound,
    TaskState {
        id: String,
        state: String,
        graph_run_id: Option<String>,
    },
    GraphState {
        id: String,
        state: String,
        snapshot_digest: Option<String>,
    },
}

fn peer_uid(stream: &UnixStream) -> Result<u32, String> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `stream` owns a valid socket FD; both pointers reference
    // initialized writable storage with the declared sizes.
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
        return Err("coordinator peer credentials unavailable".into());
    }
    Ok(credentials.uid)
}

pub(crate) fn validate_fields(request: &ReadRequest) -> Result<(), String> {
    let prefix = match request.operation {
        ReadOperation::TaskState => "task_",
        ReadOperation::GraphState => "graph_",
    };
    if request.version != VERSION
        || !request.id.starts_with(prefix)
        || !(prefix.len() + 1..=128).contains(&request.id.len())
        || !request
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("coordinator read request fields are invalid".into());
    }
    Ok(())
}

/// Authenticate the Unix peer before reading a bounded length-prefixed JSON
/// frame. A future service must close the connection after one request and
/// must not obtain `allowed_uid` from the request or an untrusted profile.
pub(crate) fn read_request(
    stream: &mut UnixStream,
    allowed_uid: u32,
) -> Result<ReadRequest, String> {
    if peer_uid(stream)? != allowed_uid {
        return Err("coordinator peer is not authorized".into());
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "coordinator read timeout unavailable")?;
    let mut header = [0u8; 2];
    stream
        .read_exact(&mut header)
        .map_err(|_| "coordinator request header is incomplete")?;
    let length = usize::from(u16::from_be_bytes(header));
    if length == 0 || length > MAX_REQUEST_BYTES {
        return Err("coordinator request size is invalid".into());
    }
    let mut frame = vec![0u8; length];
    stream
        .read_exact(&mut frame)
        .map_err(|_| "coordinator request body is incomplete")?;
    let request: ReadRequest =
        serde_json::from_slice(&frame).map_err(|_| "invalid coordinator read request")?;
    validate_fields(&request)?;
    Ok(request)
}

/// Explicitly closed until protected storage, client authorization, and
/// complete host cutover are reviewed. A parsed request never opens SQLite.
pub(crate) fn dispatch_inert(_request: &ReadRequest) -> Result<ReadResponse, String> {
    Err("protected coordinator read service is not activated".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn request(operation: &str, id: &str) -> Vec<u8> {
        serde_json::json!({"version": 1, "operation": operation, "id": id})
            .to_string()
            .into_bytes()
    }

    fn framed(bytes: &[u8]) -> Vec<u8> {
        let mut result = (bytes.len() as u16).to_be_bytes().to_vec();
        result.extend_from_slice(bytes);
        result
    }

    fn receive(bytes: &[u8], allowed_uid: u32) -> Result<ReadRequest, String> {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(bytes).unwrap();
        read_request(&mut server, allowed_uid)
    }

    #[test]
    fn accepted_request_is_still_inert() {
        let uid = unsafe { libc::geteuid() };
        let task_request = receive(&framed(&request("task_state", "task_123_456")), uid).unwrap();
        assert_eq!(task_request.operation, ReadOperation::TaskState);
        assert!(dispatch_inert(&task_request).is_err());
        let graph = receive(&framed(&request("graph_state", "graph_123_456")), uid).unwrap();
        assert_eq!(graph.operation, ReadOperation::GraphState);
        assert!(dispatch_inert(&graph).is_err());
    }

    #[test]
    fn rejects_wrong_peer_before_reading() {
        let uid = unsafe { libc::geteuid() };
        let (_, mut server) = UnixStream::pair().unwrap();
        let other_uid = uid.checked_add(1).unwrap_or(uid - 1);
        assert_eq!(
            read_request(&mut server, other_uid).unwrap_err(),
            "coordinator peer is not authorized"
        );
    }

    #[test]
    fn rejects_malformed_and_overbroad_requests() {
        let uid = unsafe { libc::geteuid() };
        let bad = [
            serde_json::json!({"version": 2, "operation": "task_state", "id": "task_123"}),
            serde_json::json!({"version": 1, "operation": "claim", "id": "task_123"}),
            serde_json::json!({"version": 1, "operation": "task_state", "id": "graph_123"}),
            serde_json::json!({"version": 1, "operation": "task_state", "id": "task_../etc"}),
            serde_json::json!({"version": 1, "operation": "task_state", "id": "task_123", "prompt": "override"}),
            serde_json::json!({"version": 1, "operation": "graph_state", "id": "graph_123", "approved": true}),
        ];
        for value in bad {
            assert!(receive(&framed(value.to_string().as_bytes()), uid).is_err());
        }
        assert!(receive(&[0, 0], uid).is_err());
        assert!(receive(&[1, 1], uid).is_err());
        assert!(receive(&[0, 8, b'{'], uid).is_err());
    }

    #[test]
    fn response_shape_never_contains_task_payload() {
        let response = ReadResponse::TaskState {
            id: "task_123".into(),
            state: "PENDING".into(),
            graph_run_id: None,
        };
        let json = serde_json::to_value(response).unwrap();
        assert_eq!(json["kind"], "task_state");
        assert_eq!(json.as_object().unwrap().len(), 4);
    }
}
