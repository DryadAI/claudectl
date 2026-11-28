# Proposed protected coordinator read store

Status: **source-only**. There is no protected coordinator account, host path,
service, socket, CLI client, scheduler relocation or broker activation. This
extends the request contract in `druid-protected-coordinator-read-contract.md`
with a guarded SQLite read opener and two explicit status projections.

The staged `prepare_staged_response` helper composes the existing authenticated,
operation-authorized request parser, the narrow store projection and the bounded
response encoder for **one already-connected Unix stream and one already-opened
store**. It returns bytes to its caller. The staged `serve_one_staged_read`
helper owns one already-accepted connection, writes one bounded response with
a two-second write timeout and closes it on success or denial. Neither helper
binds or listens, chooses a host path, constructs host policy or changes the
inert dispatcher. Tests use only an in-process Unix socket pair and synthetic
SQLite layout. A second request on the same connection is never executed;
clients must not pipeline requests (unread extra input can reset the socket).

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
have an explicit two-byte length prefix and 512-byte JSON limit. The staged
one-request helper writes only after peer and operation authorization, but
only to a socket pair supplied by its caller; no listener, UID or host service
is configured.

This is not a complete race-free host guarantee: a process sharing the
coordinator UID could swap a file between validation and SQLite open. The
identity must be dedicated to one service with no untrusted same-UID process.
The source checks before and after open do not close this race, and a
successful namespace proof with separate UIDs cannot establish same-UID
noninterference. Host service isolation and adversarial same-UID testing are
independent gates.
Actual parent, account, socket ownership, host-pinned peer policy,
complete caller migration, crash/restart, backup/rollback and independent
disposable-host proof remain required before any cutover. Never point root at
the existing operator-profile ledger as a shortcut.

After owner approval of the host *design*, the disposable namespace harness in
`scripts/verify_druid_protected_namespace.sh` exercised this opener under
separate synthetic process UIDs and a tmpfs-backed `/var/lib` layout. It
passed the listed positive/negative cases on 2026-11-28. This advances the
source boundary beyond unit fixtures; it does not prove actual Precision
installation, service/socket behavior or the documented same-UID path-swap
race. The production dispatcher remains inert.
