# DRUID durable headless bridge — implementation contract

Status: staged implementation, **not authorized for activation**. The
supervisor distinguishes the stored `claude_code_headless` provider and emits
`ExecuteHeadlessNode` instead of a legacy session action. The actuator still
rejects that action before an attempt or external effect. A separate, unwired
root-supervisor claim function can atomically record a policy-bound attempt,
but ordinary durable submission still rejects the provider. No worker, model,
transient unit, or protected policy is enabled by this change.

## Current source boundary

- `graph::durable::validate_durable_graph` rejects
  `claude_code_headless` before inserting any task. This remains the safe
  default until protected host admission exists.
- `coord::supervisor::tick` emits a distinct, currently inert headless action
  for a stored headless agent snapshot. `coord::actuator::apply` rejects it;
  session actions still reject non-legacy graph providers. An unknown provider
  fails closed instead of falling into `Spawn` or `Retry`. For agent tasks,
  the prepared node must equal the compiled node in the stored snapshot;
  a mismatched routing record fails closed.
- `graph::mixed::claim_at` refuses agent nodes. Its `finish` transitions a
  passing deterministic provider result directly to `Done`. Calling that
  executor for a headless agent would incorrectly equate model completion
  with independent verification.
- The non-durable `HeadlessProvider` transport produces a derived completion
  hash, not proof that candidate code or artifacts meet specifications.
- Coordination schema v8 records a `graph_headless_claims` row against the
  exact attempt and prior task transition. The production `claim_headless`
  entry point is now explicitly inactive until a protected coordinator ledger
  exists; a private synthetic-test path still exercises the atomic claim.
  That path checks protected active policy, exact snapshot
  digest, canonical compiled graph, satisfied dependencies, isolated recorded
  worktree, repository grant, deterministic verifier declaration, and both
  graph and policy retry ceilings. The task's projected prompt, role,
  model, budget, timeout, retry and verifier fields must equal the compiled
  graph's declaration, and the claim records a SHA-256 digest of the
  accepted protected policy alongside its bounds. The claim and `Running` transition commit
  in one immediate transaction. Duplicate/stale claims return no new attempt;
  an old claim cannot authorize a later retry. This is durable ledger state,
  **not** worker launch, verification, or production proof.

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
