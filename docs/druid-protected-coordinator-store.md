# Proposed protected coordinator read store

Status: **source-only**. There is no protected coordinator account, host path,
service, socket, CLI client, scheduler relocation or broker activation. This
extends the request contract in `druid-protected-coordinator-read-contract.md`
with a guarded SQLite read opener and two explicit status projections.

The proposed opener requires the process effective UID to equal a non-root,
dedicated coordinator UID. The DB path must be absolute and end in
`coord/coord.db`. From `/`, it opens every ancestor with `openat(O_NOFOLLOW)`;
ancestors must be root-owned and not group/other writable. The final coord
directory, DB and any WAL/SHM sidecars must be private to the coordinator
UID. The DB and sidecars must be single-link regular files. SQLite uses
read-only plus `SQLITE_OPEN_NOFOLLOW`, and the expected schema version must
match exactly. The layout is checked again after open. A source test uses a
disposable synthetic anchor; it does **not** prove these properties for any
host path.

The `ProtectedReadStore` connection is private. Its only current projections
select task state/graph-run ID or graph state/snapshot digest. They do not
select prompts, policies, worktree paths or evidence payloads. A request ID
is still just a lookup key. The module has no listener or production caller;
the staged protocol dispatcher remains inert.

The task projection rejects malformed or oversized graph-run IDs. Responses
have an explicit two-byte length prefix and 512-byte JSON limit; this
encoding is source-only and writes to no socket. The staged parser now
authorizes each peer operation against a policy, but no UIDs or service are
configured.

This is not a complete race-free host guarantee: a process sharing the
coordinator UID could swap a file between validation and SQLite open. The
identity must be dedicated to one service with no untrusted same-UID process.
Actual parent, account, socket ownership, host-pinned peer policy,
complete caller migration, crash/restart, backup/rollback and independent
disposable-host proof remain required before any cutover. Never point root at
the existing operator-profile ledger as a shortcut.
