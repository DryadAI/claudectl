# DRUID durable headless bridge — implementation contract

Status: proposed design, not implemented or authorized for activation. This
document describes the next reviewed code change after the resource-boundary
candidate. No worker, model, transient unit, or protected policy is enabled by
this document.

## Current source boundary

- `graph::durable::validate_durable_graph` rejects
  `claude_code_headless` before inserting any task. This remains the safe
  default until host-side authorization and a separate action lane exist.
- `coord::supervisor::tick` currently emits legacy `Spawn`/`Retry` for agent
  tasks. `coord::actuator::apply` now rejects non-legacy graph providers on
  session actions; it must not be weakened to make headless work.
- `graph::mixed::claim_at` refuses agent nodes. Its `finish` transitions a
  passing deterministic provider result directly to `Done`. Calling that
  executor for a headless agent would incorrectly equate model completion
  with independent verification.
- The non-durable `HeadlessProvider` transport produces a derived completion
  hash, not proof that candidate code or artifacts meet specifications.

## Required transition contract

1. A protected supervisor configuration, never graph JSON, selects the
   headless lane. Admission binds a canonical graph revision, task/node ID,
   repository remote, exact isolated worktree, route, resource limits and
   standing-policy permit. Ordinary `submit` remains fail closed. A graph
   cannot mint or broaden its own permit.
2. The existing supervisor performs dependency and upstream human-approval
   checks and emits a distinct headless action. It must never emit legacy
   `Spawn`, mailbox assignment or `Retry` for such a node. No second scheduler
   or independent retry loop is allowed.
3. Before any external effect, the existing coord transaction claims one
   attempt and records `Running` with the graph/task/attempt identity.
   Duplicate or stale actions cannot launch another worker. A crash after
   claim but before a recorded result is uncertain: do not automatically
   replay it. Reconcile the exact transient unit and cgroup, or escalate to
   human review.
4. The host-bound worker executes only in the permitted worktree and returns
   bounded, untrusted result metadata. The result and artifact hashes are
   recorded against that attempt; arbitrary model prose and secrets are not
   stored in the evidence ledger.
5. A transport success moves to `Verifying`, not `Done`. An independent
   verifier checks declared deliverables in the same immutable attempt
   context. Only a recorded pass can move to `Done`; a failure consumes the
   protected retry budget or moves to `NeedsHuman`. Approval is never inferred
   from model output or an old chat message.
6. Cancel, timeout and supervisor loss require demonstrable unit/cgroup
   termination or an explicit uncertain state. Deployment, production writes
   and policy activation remain separate human-controlled gates.

## Acceptance tests before activation

- A headless graph submitted through the ordinary API still creates zero
  tasks; an authorized host path binds the exact canonical plan and permit.
- One pending task creates one attempt despite duplicate ticks/actions;
  dependencies and human approval block launch until satisfied.
- Provider, route, worktree, repository, authority, limit or retry mismatch
  rejects before a worker starts. No legacy session action accepts headless.
- Model success with no independent verifier cannot reach `Done`; a passing
  verifier records evidence, while failure and exhaustion follow the existing
  retry/`NeedsHuman` ledger.
- Crash after claim, after spawn, and before evidence commit never silently
  replays work. Cancellation and timeout leave no live child or stale slot.
- Disposable-host proof observes cgroup CPU/memory/task limits, UID/groups/
  capabilities, egress denial, lock contention and orphan cleanup. First-party
  subscription authentication is verified without paid API fallback.

Until these tests and the protected host deployment are reviewed, the
headless provider stays unregistered and DRUID durable headless execution is
**unavailable**.
