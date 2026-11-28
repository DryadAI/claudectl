//! Proposed coordinator-owned, read-only SQLite opener.
//!
//! No service or production caller exists. A future service must take its
//! absolute path and dedicated UID from reviewed protected host configuration,
//! never from a peer request. The root broker must not use this opener.

#![allow(dead_code)]

use std::ffi::{OsStr, OsString, c_int};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::protected_read::{
    ReadOperation, ReadPeerPolicy, ReadRequest, ReadResponse, frame_response, read_request,
    validate_fields,
};
use super::{store, tasks::TaskState};

/// The connection is private so a caller cannot supply an arbitrary
/// operator-owned SQLite connection to the read projection.
pub(crate) struct ProtectedReadStore {
    conn: Connection,
}

/// Prepare one response from an already-connected peer and an already-opened
/// protected store. This source-only helper does not write the frame, bind a
/// socket, choose a host path or activate the inert dispatcher. The separate
/// staged handler owns one already-accepted connection.
pub(crate) fn prepare_staged_response(
    stream: &mut UnixStream,
    policy: &ReadPeerPolicy,
    store: &ProtectedReadStore,
) -> Result<Vec<u8>, String> {
    let request = read_request(stream, policy)?;
    let response = store.query(&request)?;
    frame_response(&response)
}

/// Handle exactly one request on an already-accepted Unix connection. The
/// owned stream is dropped on every return path, including denial and write
/// failure. This does not create a listener or select a host identity/path.
/// A future service must still bound the number of simultaneous connections.
pub(crate) fn serve_one_staged_read(
    mut stream: UnixStream,
    policy: &ReadPeerPolicy,
    store: &ProtectedReadStore,
) -> Result<(), String> {
    let frame = prepare_staged_response(&mut stream, policy, store)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "coordinator write timeout unavailable")?;
    stream
        .write_all(&frame)
        .map_err(|_| "coordinator response write failed".into())
}

fn valid_graph_state(state: &str) -> bool {
    matches!(
        state,
        "SUBMITTED"
            | "PENDING"
            | "RUNNING"
            | "AWAITING_APPROVAL"
            | "DONE"
            | "NEEDS_HUMAN"
            | "CANCELLED"
    )
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_graph_id(id: &str) -> bool {
    id.starts_with("graph_")
        && (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

impl ProtectedReadStore {
    /// Only the two staged status projections are available. No task prompt,
    /// policy, worktree, evidence payload or admission is selected.
    pub(crate) fn query(&self, request: &ReadRequest) -> Result<ReadResponse, String> {
        validate_fields(request)?;
        match request.operation {
            ReadOperation::TaskState => {
                let row: Option<(String, Option<String>)> = self
                    .conn
                    .query_row(
                        "SELECT state, graph_run_id FROM tasks WHERE id = ?1",
                        params![request.id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|_| "protected task status query failed")?;
                match row {
                    None => Ok(ReadResponse::NotFound),
                    Some((state, graph_run_id))
                        if TaskState::parse(&state).is_some()
                            && graph_run_id.as_deref().is_none_or(valid_graph_id) =>
                    {
                        Ok(ReadResponse::TaskState {
                            id: request.id.clone(),
                            state,
                            graph_run_id,
                        })
                    }
                    Some(_) => Err("protected task state or graph ID is invalid".into()),
                }
            }
            ReadOperation::GraphState => {
                let row: Option<(String, Option<String>)> = self
                    .conn
                    .query_row(
                        "SELECT r.state, s.digest FROM graph_runs r \
                         LEFT JOIN graph_snapshots s ON s.run_id = r.id WHERE r.id = ?1",
                        params![request.id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|_| "protected graph status query failed")?;
                match row {
                    None => Ok(ReadResponse::NotFound),
                    Some((state, snapshot_digest))
                        if valid_graph_state(&state)
                            && snapshot_digest.as_deref().is_none_or(valid_digest) =>
                    {
                        Ok(ReadResponse::GraphState {
                            id: request.id.clone(),
                            state,
                            snapshot_digest,
                        })
                    }
                    Some(_) => Err("protected graph state or digest is invalid".into()),
                }
            }
        }
    }
}

fn open_child(parent: &File, name: &OsStr, directory: bool) -> Result<Option<File>, String> {
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| "protected coordinator path contains NUL")?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: `parent` owns a directory FD and `name` is NUL-terminated.
    let fd: c_int = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err("protected coordinator path could not be opened safely".into());
    }
    // SAFETY: openat returned a new FD, transferred to File ownership.
    Ok(Some(unsafe { File::from_raw_fd(fd) }))
}

fn required_child(parent: &File, name: &OsStr, directory: bool) -> Result<File, String> {
    open_child(parent, name, directory)?
        .ok_or_else(|| "protected coordinator path is missing".into())
}

fn root_directory(file: &File, root_uid: u32) -> Result<(), String> {
    let meta = file
        .metadata()
        .map_err(|_| "protected directory metadata unavailable")?;
    if !meta.is_dir() || meta.uid() != root_uid || meta.mode() & 0o022 != 0 {
        return Err("protected coordinator ancestor ownership or mode is unsafe".into());
    }
    Ok(())
}

fn private_directory(file: &File, coordinator_uid: u32) -> Result<(), String> {
    let meta = file
        .metadata()
        .map_err(|_| "protected directory metadata unavailable")?;
    if !meta.is_dir() || meta.uid() != coordinator_uid || meta.mode() & 0o077 != 0 {
        return Err("protected coordinator directory ownership or mode is unsafe".into());
    }
    Ok(())
}

fn private_file(file: &File, coordinator_uid: u32) -> Result<(), String> {
    let meta = file
        .metadata()
        .map_err(|_| "protected file metadata unavailable")?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != coordinator_uid
        || meta.mode() & 0o077 != 0
    {
        return Err("protected coordinator file ownership or mode is unsafe".into());
    }
    Ok(())
}

/// FD-walk the proposed protected layout. All ancestors up to the final
/// directory must be root-owned and not group/other writable; the final
/// directory and DB/WAL/SHM must be private to the dedicated coordinator UID.
/// No path component or sidecar symlink is followed. This does not by itself
/// prove that SQLite later opened the same inode; the dedicated UID must not
/// be shared with any untrusted process.
fn inspect_beneath(
    root: &File,
    relative: &Path,
    root_uid: u32,
    coordinator_uid: u32,
) -> Result<(), String> {
    if relative.is_absolute() {
        return Err("protected coordinator path must be relative to anchor".into());
    }
    let names: Vec<&OsStr> = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err("protected coordinator path traversal is forbidden".into()),
        })
        .collect::<Result<_, String>>()?;
    let (file_name, dirs) = names
        .split_last()
        .ok_or("protected coordinator path is empty")?;
    if *file_name != OsStr::new("coord.db")
        || dirs.is_empty()
        || dirs[dirs.len() - 1] != OsStr::new("coord")
    {
        return Err("protected coordinator DB name or parent is invalid".into());
    }
    root_directory(root, root_uid)?;
    let mut parent = root.try_clone().map_err(|_| "protected root unavailable")?;
    for name in &dirs[..dirs.len() - 1] {
        parent = required_child(&parent, name, true)?;
        root_directory(&parent, root_uid)?;
    }
    let db_dir = required_child(&parent, dirs[dirs.len() - 1], true)?;
    private_directory(&db_dir, coordinator_uid)?;
    let db = required_child(&db_dir, file_name, false)?;
    private_file(&db, coordinator_uid)?;
    for suffix in ["-wal", "-shm"] {
        let mut name = OsString::from(*file_name);
        name.push(suffix);
        if let Some(sidecar) = open_child(&db_dir, &name, false)? {
            private_file(&sidecar, coordinator_uid)?;
        }
    }
    Ok(())
}

/// Source-only protected read opener. It refuses root and requires the
/// process to be the designated dedicated identity. The absolute path must
/// be pinned in protected host configuration; no production caller exists.
pub(crate) fn open_read_only(
    path: &Path,
    coordinator_uid: u32,
) -> Result<ProtectedReadStore, String> {
    if coordinator_uid == 0 || unsafe { libc::geteuid() } != coordinator_uid {
        return Err("protected coordinator identity is invalid".into());
    }
    let relative = path
        .strip_prefix("/")
        .map_err(|_| "protected coordinator path must be absolute")?;
    let root = File::open("/").map_err(|_| "protected root unavailable")?;
    open_checked(path, &root, relative, 0, coordinator_uid)
}

fn open_checked(
    path: &Path,
    root: &File,
    relative: &Path,
    root_uid: u32,
    coordinator_uid: u32,
) -> Result<ProtectedReadStore, String> {
    inspect_beneath(root, relative, root_uid, coordinator_uid)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "protected coordinator DB could not be opened read-only")?;
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| "protected coordinator schema version unavailable")?;
    if version != store::EXPECTED_COORD_SCHEMA_VERSION {
        return Err("protected coordinator schema version is not exact".into());
    }
    inspect_beneath(root, relative, root_uid, coordinator_uid)?;
    Ok(ProtectedReadStore { conn })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::RawFd;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;

    /// Invoked only by scripts/verify_druid_protected_namespace.sh inside an
    /// isolated user/mount/network namespace and disposable chroot. The
    /// numeric identities below are *namespace-only*, never host selections.
    #[test]
    fn disposable_namespace_probe() {
        let Some(mode) = std::env::var_os("DRUID_PROTECTED_PROOF_MODE") else {
            return;
        };
        let path = Path::new("/var/lib/druid-coordinator/coord/coord.db");
        const COORDINATOR_UID: u32 = 1001;
        let uid = unsafe { libc::geteuid() };
        match mode.to_str().unwrap() {
            "seed" => {
                assert_eq!(uid, COORDINATOR_UID);
                let conn = store::open().unwrap();
                let version: u32 = conn
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(version, store::EXPECTED_COORD_SCHEMA_VERSION);
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                    .unwrap();
            }
            "coordinator" => {
                assert_eq!(uid, COORDINATOR_UID);
                let protected = open_read_only(path, COORDINATOR_UID).unwrap();
                for (operation, id) in [
                    (ReadOperation::TaskState, "task_missing"),
                    (ReadOperation::GraphState, "graph_missing"),
                ] {
                    let request = ReadRequest {
                        version: 1,
                        operation,
                        id: id.into(),
                    };
                    assert_eq!(protected.query(&request).unwrap(), ReadResponse::NotFound);
                }
            }
            "operator" | "worker" => {
                assert!(matches!(uid, 1000 | 1002));
                assert_eq!(
                    File::open(path).unwrap_err().kind(),
                    std::io::ErrorKind::PermissionDenied
                );
                for suffix in ["-wal", "-shm"] {
                    let sidecar = path.with_file_name(format!("coord.db{suffix}"));
                    assert_eq!(
                        File::open(sidecar).unwrap_err().kind(),
                        std::io::ErrorKind::PermissionDenied
                    );
                }
                assert_eq!(
                    std::fs::rename(path, path.with_extension("swapped"))
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::PermissionDenied
                );
                assert!(open_read_only(path, COORDINATOR_UID).is_err());
            }
            "root" => {
                assert_eq!(uid, 0);
                // Namespace root can bypass DAC, but the guarded opener must
                // still refuse it. This is not host-root isolation.
                assert!(File::open(path).is_ok());
                assert!(open_read_only(path, COORDINATOR_UID).is_err());
            }
            "reject" => {
                assert_eq!(uid, COORDINATOR_UID);
                assert!(open_read_only(path, COORDINATOR_UID).is_err());
            }
            "reject_schema" => {
                assert_eq!(uid, COORDINATOR_UID);
                assert_eq!(
                    open_read_only(path, COORDINATOR_UID).err().as_deref(),
                    Some("protected coordinator schema version is not exact")
                );
            }
            "socket_server" => {
                assert_eq!(uid, COORDINATOR_UID);
                let fd: RawFd = std::env::var("DRUID_PROOF_LISTEN_FD")
                    .unwrap()
                    .parse()
                    .unwrap();
                // SAFETY: the disposable namespace harness passes a live,
                // inherited AF_UNIX listening FD and transfers ownership.
                let listener = unsafe { UnixListener::from_raw_fd(fd) };
                let store = open_read_only(path, COORDINATOR_UID).unwrap();
                let policy = ReadPeerPolicy::new(1000, 1000, COORDINATOR_UID).unwrap();
                let (operator, _) = listener.accept().unwrap();
                serve_one_staged_read(operator, &policy, &store).unwrap();
                let (root, _) = listener.accept().unwrap();
                assert_eq!(
                    serve_one_staged_read(root, &policy, &store).unwrap_err(),
                    "coordinator peer is not authorized"
                );
            }
            other => panic!("unknown disposable proof mode: {other}"),
        }
    }

    fn framed_request(operation: &str, id: &str) -> Vec<u8> {
        let body = serde_json::json!({"version": 1, "operation": operation, "id": id})
            .to_string()
            .into_bytes();
        let mut frame = (body.len() as u16).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        frame
    }

    fn prepare(
        store: &ProtectedReadStore,
        policy: &ReadPeerPolicy,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(bytes).unwrap();
        prepare_staged_response(&mut server, policy, store)
    }

    #[test]
    fn staged_connection_writes_one_frame_then_closes() {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 {
            return;
        }
        let (dir, root, _) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let policy = ReadPeerPolicy::new(uid, uid, uid + 1).unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(&framed_request("task_state", "task_123"))
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        serve_one_staged_read(server, &policy, &store).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        let length = usize::from(u16::from_be_bytes([response[0], response[1]]));
        assert_eq!(response.len(), length + 2);
        let body: serde_json::Value = serde_json::from_slice(&response[2..]).unwrap();
        assert_eq!(body["kind"], "task_state");
    }

    #[test]
    fn staged_connection_denial_closes_without_response() {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 {
            return;
        }
        let (dir, root, _) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let policy = ReadPeerPolicy::new(uid, uid + 1, uid + 2).unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(&framed_request("graph_state", "graph_123"))
            .unwrap();
        assert_eq!(
            serve_one_staged_read(server, &policy, &store).unwrap_err(),
            "coordinator peer is not authorized for operation"
        );
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.is_empty());
    }

    #[test]
    fn staged_response_authenticates_then_projects_only_status() {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 {
            return;
        }
        let (dir, root, _) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let policy = ReadPeerPolicy::new(uid, uid, uid + 1).unwrap();
        for (operation, id, kind) in [
            ("task_state", "task_123", "task_state"),
            ("graph_state", "graph_123", "graph_state"),
            ("task_state", "task_missing", "not_found"),
        ] {
            let frame = prepare(&store, &policy, &framed_request(operation, id)).unwrap();
            assert_eq!(
                usize::from(u16::from_be_bytes([frame[0], frame[1]])),
                frame.len() - 2
            );
            let response: serde_json::Value = serde_json::from_slice(&frame[2..]).unwrap();
            assert_eq!(response["kind"], kind);
            assert!(
                !String::from_utf8(frame[2..].to_vec())
                    .unwrap()
                    .contains("private prompt")
            );
        }
    }

    #[test]
    fn staged_response_rejects_wrong_operation_and_bad_frame() {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 {
            return;
        }
        let (dir, root, _) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let policy = ReadPeerPolicy::new(uid, uid + 1, uid + 2).unwrap();
        assert_eq!(
            prepare(&store, &policy, &framed_request("graph_state", "graph_123")).unwrap_err(),
            "coordinator peer is not authorized for operation"
        );
        assert_eq!(
            prepare(&store, &policy, &[0, 0]).unwrap_err(),
            "coordinator request size is invalid"
        );
    }

    #[test]
    fn staged_response_fails_closed_on_invalid_projection() {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 {
            return;
        }
        let (dir, root, _) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute("UPDATE tasks SET graph_run_id = 'invalid'", [])
            .unwrap();
        drop(writer);
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let policy = ReadPeerPolicy::new(uid, uid, uid + 1).unwrap();
        assert_eq!(
            prepare(&store, &policy, &framed_request("task_state", "task_123")).unwrap_err(),
            "protected task state or graph ID is invalid"
        );
    }

    fn fixture() -> (tempfile::TempDir, File, u32) {
        let dir = tempfile::tempdir().unwrap();
        let uid = unsafe { libc::geteuid() };
        std::fs::create_dir(dir.path().join("protected")).unwrap();
        std::fs::create_dir(dir.path().join("protected/coord")).unwrap();
        let db_path = dir.path().join("protected/coord/coord.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "user_version", store::EXPECTED_COORD_SCHEMA_VERSION)
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, state TEXT NOT NULL, graph_run_id TEXT, prompt TEXT NOT NULL);
             CREATE TABLE graph_runs (id TEXT PRIMARY KEY, state TEXT NOT NULL);
             CREATE TABLE graph_snapshots (run_id TEXT PRIMARY KEY, digest TEXT NOT NULL);
             INSERT INTO tasks VALUES ('task_123', 'PENDING', 'graph_123', 'private prompt');
             INSERT INTO graph_runs VALUES ('graph_123', 'PENDING');
             INSERT INTO graph_snapshots VALUES ('graph_123', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');",
        ).unwrap();
        drop(conn);
        std::fs::set_permissions(
            dir.path().join("protected/coord"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let root = File::open(dir.path()).unwrap();
        (dir, root, uid)
    }

    #[test]
    fn accepts_private_synthetic_layout() {
        let (dir, root, uid) = fixture();
        assert!(inspect_beneath(&root, Path::new("protected/coord/coord.db"), uid, uid).is_ok());
        let path = dir.path().join("protected/coord/coord.db");
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let task = ReadRequest {
            version: 1,
            operation: ReadOperation::TaskState,
            id: "task_123".into(),
        };
        let result = store.query(&task).unwrap();
        let response = serde_json::to_string(&result).unwrap();
        assert!(response.contains("PENDING"));
        assert!(!response.contains("private prompt"));
        let graph = ReadRequest {
            version: 1,
            operation: ReadOperation::GraphState,
            id: "graph_123".into(),
        };
        assert!(matches!(
            store.query(&graph).unwrap(),
            ReadResponse::GraphState { .. }
        ));
        let missing = ReadRequest {
            version: 1,
            operation: ReadOperation::TaskState,
            id: "task_missing".into(),
        };
        assert_eq!(store.query(&missing).unwrap(), ReadResponse::NotFound);
        let invalid = ReadRequest {
            version: 1,
            operation: ReadOperation::TaskState,
            id: "graph_123".into(),
        };
        assert!(store.query(&invalid).is_err());
    }

    #[test]
    fn rejects_unsafe_ancestors_and_identity() {
        let (dir, root, uid) = fixture();
        std::fs::set_permissions(
            dir.path().join("protected"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(inspect_beneath(&root, Path::new("protected/coord/coord.db"), uid, uid).is_err());
        std::fs::set_permissions(
            dir.path().join("protected"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(
            inspect_beneath(
                &root,
                Path::new("protected/coord/coord.db"),
                uid.wrapping_add(1),
                uid
            )
            .is_err()
        );
        assert!(open_read_only(&dir.path().join("protected/coord/coord.db"), 0).is_err());
    }

    #[test]
    fn rejects_linked_database_or_sidecars() {
        let (dir, root, uid) = fixture();
        let db_dir = dir.path().join("protected/coord");
        std::fs::hard_link(db_dir.join("coord.db"), db_dir.join("other.db")).unwrap();
        assert!(inspect_beneath(&root, Path::new("protected/coord/coord.db"), uid, uid).is_err());
        std::fs::remove_file(db_dir.join("other.db")).unwrap();
        symlink("coord.db", db_dir.join("coord.db-wal")).unwrap();
        assert!(inspect_beneath(&root, Path::new("protected/coord/coord.db"), uid, uid).is_err());
        std::fs::remove_file(db_dir.join("coord.db-wal")).unwrap();
        std::fs::write(db_dir.join("coord.db-shm"), b"invalid").unwrap();
        std::fs::set_permissions(
            db_dir.join("coord.db-shm"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(inspect_beneath(&root, Path::new("protected/coord/coord.db"), uid, uid).is_err());
    }

    #[test]
    fn refuses_schema_drift_before_any_projection() {
        let (dir, root, uid) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(
            None,
            "user_version",
            store::EXPECTED_COORD_SCHEMA_VERSION - 1,
        )
        .unwrap();
        drop(conn);
        assert!(
            open_checked(
                &path,
                &root,
                Path::new("protected/coord/coord.db"),
                uid,
                uid
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_corrupt_graph_projection() {
        let (dir, root, uid) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute("UPDATE graph_snapshots SET digest = 'not-a-digest'", [])
            .unwrap();
        drop(writer);
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let graph = ReadRequest {
            version: 1,
            operation: ReadOperation::GraphState,
            id: "graph_123".into(),
        };
        assert!(store.query(&graph).is_err());
    }

    #[test]
    fn refuses_unbounded_graph_id_in_task_projection() {
        let (dir, root, uid) = fixture();
        let path = dir.path().join("protected/coord/coord.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute("UPDATE tasks SET graph_run_id = ?1", ["x".repeat(600)])
            .unwrap();
        drop(writer);
        let store = open_checked(
            &path,
            &root,
            Path::new("protected/coord/coord.db"),
            uid,
            uid,
        )
        .unwrap();
        let task = ReadRequest {
            version: 1,
            operation: ReadOperation::TaskState,
            id: "task_123".into(),
        };
        assert!(store.query(&task).is_err());
    }

    #[test]
    fn rejects_traversal_and_unexpected_name() {
        let (_dir, root, uid) = fixture();
        assert!(
            inspect_beneath(&root, Path::new("protected/../coord/coord.db"), uid, uid).is_err()
        );
        assert!(inspect_beneath(&root, Path::new("protected/coord/other.db"), uid, uid).is_err());
    }
}
