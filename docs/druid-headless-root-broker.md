# DRUID Precision root broker — staged request contract

Status: **protocol only; no broker service, socket, root process, claim, or
worker activation**. The owner approved a root-owned local broker architecture
on 2026-11-27. That approval does not install a service or approve executing a
headless model. The ordinary durable graph submission and actuator remain
closed to the headless provider.

The existing coordination supervisor remains the scheduler and source of
dependency, retry, and approval decisions. A future broker must not poll the
graph or create a second scheduling loop. It may accept only an explicit action
from that supervisor, check its own protected authority, and reconcile exactly
one attempt.

## Current code

`graph::headless_broker` defines an inert local protocol. A request is a
two-byte big-endian length and at most 512 bytes of JSON:

```json
{"version":1,"action":"claim_and_launch","task_id":"task_123_456_0","expected_attempt":1,"snapshot_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
```

The reader obtains peer UID from Linux `SO_PEERCRED` **before** reading the
frame, applies a two-second read timeout, and rejects unknown fields. The
request cannot supply paths, commands, policy, network route, budget,
credentials, or an approval. The UID argument in this inert module is not a
production configuration source: a future root service must pin the permitted
supervisor UID in protected host configuration and use a root-owned socket.
The task ID, attempt, and digest are lookup keys, **not authority**. The
`dispatch_inert` endpoint always refuses execution.

An additional, still-unwired parser checks an exact protected admission
record against the request, independently accepted policy SHA-256, repository
remote, expiry and maximum attempt. That parser does **not** prove that the
record came from an owner-approved path; a JSON copy supplied by the caller
has no authority. A future broker must load, authenticate and revoke the
protected admission itself before using its contents. A protected admission
is narrower than a standing policy: the policy authorizes a class of bounded
development operations, while admission binds one task and graph snapshot.
The source also stages a Linux file-descriptor reader for an absolute,
root-owned admission path. It walks from `/` with `openat(O_NOFOLLOW)`, checks
each directory's owner and writable bits, then requires a root-owned, private
regular file and reads no more than 4 KiB from the verified descriptor. This
avoids a metadata-check/path-read swap for **that admission file only**. It
does not yet authenticate the existing standing-policy/revocation loader,
which remains a separate review gate before a root broker may use it.

## Required design gate before wiring

The current coordination DB is opened through `coord::store::open()`, whose
path follows the invoking user's profile and whose opener may create the
directory and migrate the database. A root broker must not open that
user-controlled path as root. Before any connection to `claim_headless`:

1. Choose a protected ledger ownership/location and a migration strategy that
   preserves the existing supervisor's atomic transitions. Review SQLite DB,
   WAL, SHM, parent-directory and symlink ownership. The recommended design
   is a root-owned parent directory containing a DB writable only by a
   dedicated, non-worker coordinator identity. The operator CLI would use a
   narrow local API instead of writing SQLite directly. The existing
   supervisor logic remains the **one scheduler**, relocated to that trusted
   coordinator; a separate root broker has no tick loop. This is a proposal,
   not an implemented service or migration. Directly giving root the current
   operator-owned DB, merely changing its filename, or making a root-owned DB
   writable by the operator would not establish a protected boundary.
2. Pin broker policy/revocation files, worktree root, socket path, peer UID,
   unit template and resource bounds outside graph JSON and outside any
   user-writable tree. Root independently reloads/checks the active policy,
   exact stored snapshot digest and attempt state before an external effect.
3. Define the crash window between durable claim and transient unit creation.
   A claimed but uncertain attempt must not auto-replay. Reconcile the exact
   unit/cgroup, record evidence, or enter `NeedsHuman`.
4. Bind a worker result to its attempt, perform independent verification, and
   preserve `Verifying` versus `Done`. Prove timeout/cancel cleanup, egress,
   resource limits, identity and first-party subscription authentication on a
   disposable host. No paid-API fallback.

Only after this review may a separately approved installation add a root-owned
socket/service and activate a protected headless lane. A source test or a PR
does not establish host readiness, reboot persistence, or live worker proof.

## Proposed migration sequence (not executed)

1. Inventory the actual Precision coordinator processes, DB path, ownership,
   schema version and any active writers. Back up the SQLite database with its
   own backup API while writers are quiesced; do not use a live file copy of
   WAL-mode state as a migration proof.
2. Create a new protected parent, coordinator identity and root broker identity
   boundary. Deny the worker and ordinary operator direct DB access. Reject
   symlinks, path traversal, world/group-writable ancestors and DB/WAL/SHM
   state, and prove ownership before every privileged open. A path preflight
   alone is insufficient if the file can be swapped before SQLite opens it.
3. Migrate the exact schema in a disposable clone. Verify row counts, foreign
   keys, graph snapshot digests, task transitions, headless claim uniqueness
   and reopen behavior. Stop on any discrepancy; retain the old DB as a
   recoverable rollback artifact rather than deleting it.
4. Put the existing supervisor scheduler behind the coordinator service and
   make the CLI a client. First prove legacy task behavior unchanged; only
   then wire the broker's explicit headless intent. The broker must compare
   the protected admission with stored graph/task/attempt and current policy
   before claiming, and must treat claim-without-unit as uncertain.
5. Independently prove one attempt, verifier transitions, cancel/timeout,
   reboot reconciliation and denial of bypass from an unprivileged user and
   `druid-worker` on a disposable host. Production cutover and activation
   require separate owner approval and a documented rollback.
