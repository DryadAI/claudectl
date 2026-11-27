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

## Required design gate before wiring

The current coordination DB is opened through `coord::store::open()`, whose
path follows the invoking user's profile and whose opener may create the
directory and migrate the database. A root broker must not open that
user-controlled path as root. Before any connection to `claim_headless`:

1. Choose a protected ledger ownership/location and a migration strategy that
   preserves the existing supervisor's atomic transitions. Review SQLite DB,
   WAL, SHM, parent-directory and symlink ownership. A root-owned ledger with
   a narrowly mediated user-facing API is the preferred direction; this is a
   proposal, not an implemented migration.
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
