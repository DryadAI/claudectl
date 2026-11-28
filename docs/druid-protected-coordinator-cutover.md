# DRUID protected coordinator cutover — review contract

Status: **proposed design; no service, protected ledger, socket, migration or
worker activation exists in this change**. This document started from source
`2850a32`; the direct-caller inventory below was refreshed at claudectl
`3c1f5cd` and refines the gate in
`druid-headless-root-broker.md`. It is not a host configuration or permit.

## Why this is a full coordinator migration

The operator-profile SQLite connection is shared, not just used by the
supervisor. Moving only `supervisor tick` would leave direct writers and
readers able to bypass a protected authority boundary:

| Source | Current direct-ledger role | Proposed client boundary |
| --- | --- | --- |
| `coord/supervisor_cli.rs`, `coord/cli.rs` | task/coord commands, status, drain and cancellation | typed operator requests; no SQL or arbitrary task mutation |
| `runtime/actions.rs`, `runtime/coord.rs`, `runtime/orchestrator.rs` | direct cancel/retry/approve writes; TUI/runtime views; interrupt delivery and lease expiry | split owner decisions from read projections and scheduled internal effects; preserve existing supervisor authority |
| `commands.rs` | periodic supervisor tick/actuation, pruning and context-rot interrupts | relocate this *existing* tick into the sole coordinator; define bounded internal effects, never a second scheduler |
| `graph/durable.rs`, `graph/mixed.rs`, `graph/mod.rs` | durable submission/status, approval and snapshot inspection | submission, status and owner-decision requests bound to exact graph digest |
| `graph/master.rs` | read-only run status | typed read-only status response |
| `bus/mcp.rs` | some handlers open the coordinator DB directly; other calls open the separate bus DB | inventory each MCP handler and require an operation-specific peer policy; do not migrate bus storage by accident |
| `bus/cli.rs` | inspected direct `store::open()` calls use the bus store | keep bus DB separate; recheck if call sites change |
| `ingest.rs` | hook payload append into the coordinator DB | preserve bounded best-effort hook ingest and an authenticated append operation; do not turn an arbitrary payload into authority |
| `init/mod.rs`, `doctor.rs` | coordinator schema migration and schema/health checks | protected, offline migration path and a non-mutating status check; no operator-ledger migration as root |
| `coord/injection.rs`, `coord/exporter.rs`, `coord/pr.rs`, `coord/promotion.rs` | context, metrics, PR summaries and promotion | least-privilege read or named effect requests, as appropriate |
| `coord/supervisor.rs` | scheduler logic receives a connection from callers | relocate the *existing* scheduler into the sole coordinator process |

This is a source-level inventory, **not** a complete runtime/process inventory.
Reproduce the direct-open search on the target revision with
`rg -n 'coord::store::open|store::open\(|store::open_read_only\(' src --glob '*.rs'`,
then resolve each local `store` import before classifying it: the same spelling
can mean the independent bus store. Also trace functions that receive an
already-open coordinator connection (such as the supervisor and actuator).
Any new direct-ledger caller blocks the cutover until classified and migrated.

This is not a claim that each caller is safe to expose unchanged. Each
operation needs a caller identity, authorization rule, input
limit, response redaction rule, idempotency key and denial test before it
crosses a socket. A generic SQL, file-path or shell request is forbidden.

## Proposed trust boundary

1. A root-owned, non-user-writable parent holds a private coordinator DB
   directory. A dedicated non-worker coordinator identity alone may write the
   DB, WAL and SHM. Neither the ordinary operator nor `druid-worker` gets
   direct ledger access. The owner approved the identity/path *design* in
   `druid-protected-coordinator-host-decision.md`; the socket ACL is a
   candidate and unit hardening still needs host review. Nothing is installed.
2. A local coordinator service authenticates each Unix peer from the kernel
   and authorizes a named operation. The operator CLI and existing bus/graph
   integrations become clients. The service owns the existing supervisor
   scheduler; there is no second tick loop. The root broker has no scheduler
   and may only consume an exact, protected coordinator intent.
3. The root broker separately checks root-owned policy, revocations and exact
   admission. A peer's task ID/digest/attempt is a lookup key, not authority.
   It must never open the operator-profile ledger, including read-only, and
   must not accept a caller-provided SQLite connection, path or approval.
4. Until the protected service and all relevant clients are migrated, the
   current operator ledger remains a legacy, non-broker lane. Headless claim
   and launch remain closed. Source-only tests cannot lift this restriction.

## Cutover proof required before enabling any client

1. Inventory actual Precision processes and every open handle to the live
   ledger. Quiesce writers. Capture a consistent private backup with the
   operator-run producer, and audit the **offline** copy. Preserve an
   independently readable rollback snapshot; never copy it into Git.
2. In a disposable environment, create the protected identity and storage,
   migrate the exact snapshot, check schema/rows/FKs/graph digests/claim
   uniqueness, reopen through the new identity, and prove the operator and
   worker cannot directly open DB/WAL/SHM. Check symlinks, parent ownership,
   sidecar modes and SQLite path-swap behavior. A path preflight alone is not
   sufficient.
3. Migrate every direct-ledger caller above, including runtime hooks, TUI,
   initialization, doctor, MCP and metrics.
   Compare legacy behavior for normal tasks, approvals, retries, interrupts,
   cancel and restart. Prove denied/unknown/oversized requests have no side
   effect; prove a compromised client cannot forge an owner decision.
4. Prove the claim-to-unit crash window: a durable claim with no confirmed
   unit enters an uncertain/NeedsHuman path, never automatic replay. Bind a
   result to one attempt and require independent verification before `Done`.
5. Independently prove service restart, reboot persistence, rollback,
   resource/egress controls, exact first-party subscription route and no
   paid-API fallback on a disposable host. Only then seek separate owner
   authorization for production cutover and headless activation.

Open design decisions: exact Unix-socket ACL, service hardening, backup and
rollback procedure, and the write/approval operation set. The identity/path
direction is owner-approved but not installed. No current source endpoint or
SDK implements this contract.

The staged read-only request shape is in
`druid-protected-coordinator-read-contract.md`. Its parser has no listener or ledger
access and cannot be treated as a working coordinator service.
`druid-protected-coordinator-store.md` adds source-only guarded read access
and redacted projections; it still has no production caller or host proof.
`druid-protected-coordinator-host-decision.md` records the owner-approved
Precision identity/path/peer *design*. The source-only namespace proof covers
synthetic roles, not the actual host path, socket, service or same-UID race.
It installs nothing and does not replace complete disposable-host proof or
separate production cutover authorization.
