# DRUID protected coordinator cutover — review contract

Status: **proposed design; no service, protected ledger, socket, migration or
worker activation exists in this change**. This document inventories the
source at claudectl `2850a32` and refines the gate in
`druid-headless-root-broker.md`. It is not a host configuration or permit.

## Why this is a full coordinator migration

The operator-profile SQLite connection is shared, not just used by the
supervisor. Moving only `supervisor tick` would leave direct writers and
readers able to bypass a protected authority boundary:

| Source | Current direct-ledger role | Proposed client boundary |
| --- | --- | --- |
| `coord/supervisor_cli.rs`, `coord/cli.rs` | task/coord commands, status, drain and cancellation | typed operator requests; no SQL or arbitrary task mutation |
| `graph/durable.rs`, `graph/mixed.rs`, `graph/mod.rs` | durable submission/status, approval and snapshot inspection | submission, status and owner-decision requests bound to exact graph digest |
| `graph/master.rs` | read-only run status | typed read-only status response |
| `bus/mcp.rs`, `bus/cli.rs` | bus and coordinator state through direct open | separate bus DB stays separate; coordinator operations use typed requests |
| `coord/injection.rs`, `coord/exporter.rs`, `coord/pr.rs`, `coord/promotion.rs` | context, metrics, PR summaries and promotion | least-privilege read or named effect requests, as appropriate |
| `coord/supervisor.rs` | scheduler logic receives a connection from callers | relocate the *existing* scheduler into the sole coordinator process |

This is a source inventory, not a claim that each caller is safe to expose
unchanged. Each operation needs a caller identity, authorization rule, input
limit, response redaction rule, idempotency key and denial test before it
crosses a socket. A generic SQL, file-path or shell request is forbidden.

## Proposed trust boundary

1. A root-owned, non-user-writable parent holds a private coordinator DB
   directory. A dedicated non-worker coordinator identity alone may write the
   DB, WAL and SHM. Neither the ordinary operator nor `druid-worker` gets
   direct ledger access. The exact account, group, absolute paths and unit
   hardening need host review; none is selected or installed here.
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
3. Migrate every direct-ledger caller above, including MCP and metrics.
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

Open design decision: exact protected identity/storage/Unix-socket ownership
and the minimal allowed operation set. The recommended direction is a
dedicated coordinator identity under a root-owned parent with a narrow local
API, but these values require an explicit reviewed host design. No current
source endpoint or SDK implements this contract.

The staged read-only request shape is in
`druid-protected-coordinator-read-contract.md`. It has no listener or ledger
access and cannot be treated as a working coordinator service.
