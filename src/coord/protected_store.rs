//! Proposed coordinator-owned, read-only SQLite opener.
//!
//! No service or production caller exists. A future service must take its
//! absolute path and dedicated UID from reviewed protected host configuration,
//! never from a peer request. The root broker must not use this opener.

#![allow(dead_code)]

use std::ffi::{OsStr, OsString, c_int};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::protected_read::{ReadOperation, ReadRequest, ReadResponse, validate_fields};
use super::{store, tasks::TaskState};

/// The connection is private so a caller cannot supply an arbitrary
/// operator-owned SQLite connection to the read projection.
pub(crate) struct ProtectedReadStore {
    conn: Connection,
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
    use std::os::unix::fs::{PermissionsExt, symlink};

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
