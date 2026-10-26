# Graph Bootstrap V1

The graph layer now has two execution paths over one canonical, framework-independent JSON IR:

- **local seed/provider runtime** for deterministic commands, checks, approvals, and providers such as REA;
- **durable supervisor runtime** for agent, command/check, provider and approval DAGs with persisted dependency ordering, attempts and evidence.

This is still a draft bootstrap. [Mixed durable graphs](druid-durable-mixed-graphs.md) now have SHA-256 run-bound approval snapshots. Automatic integration/merge, graph timeouts, signed approvals and repository-state attestation remain unimplemented.

The [DRUID master planner](druid-master-plans.md) now compiles versioned lifecycle
plans into this same IR, with requirements, SHA-256 content identity and evidence
projections. Its digest is not a signed authorization or a change to runtime
approval enforcement.

## Operator flow

Refresh the current hook/plugin assets and DB migrations after installing this branch:

```sh
claudectl init upgrade
```

Run the existing headless control loop in one terminal:

```sh
claudectl --headless
```

Then submit and inspect a durable graph from another terminal:

```sh
claudectl graph validate examples/graph-durable-worktree-smoke.json
claudectl graph plan examples/graph-durable-worktree-smoke.json
claudectl graph submit examples/graph-durable-worktree-smoke.json

# use the graph_run_id printed by submit
claudectl graph status <graph-run-id>
```

A read-only two-node dependency example is also available:

```sh
claudectl graph submit examples/graph-durable-agent-smoke.json
```

The source repository must be a **clean Git checkout**, and durable V1 requires each agent request's `cwd` to resolve to the repository root.

## Local seed/provider runtime

```sh
claudectl graph validate examples/graph-bootstrap-v1.json
claudectl graph plan examples/graph-bootstrap-v1.json
claudectl graph run examples/graph-bootstrap-v1.json
```

The local runner supports shell/check nodes, bounded retries, interactive approval, provider nodes, evidence JSONL, and PASS / FAIL / UNKNOWN / ERROR outcomes. It executes topological waves synchronously and does not have crash-safe resume.

REA is registered as a runtime provider for:

- `reverse.capabilities.discover`
- `reverse.javascript.analyze`
- `reverse.native.analyze`

Safe provider discovery:

```sh
claudectl graph run examples/graph-rea-capabilities.json
```

When REA is installed, its full JSON result is streamed to a run artifact. Missing REA returns `UNKNOWN`, never a fabricated `PASS`. REA target paths must be covered by the node's declared read authority.

## Durable supervisor contract

`graph submit` supports [mixed graphs](druid-durable-mixed-graphs.md) through the same supervisor. Agent nodes still require capability `agent.execute`, with provider omitted or set to `claude_code`. Non-agent shell execution requires explicit unsandboxed opt-in. Subgraphs and typed artifact transport are rejected.

Submission is atomic:

1. A `graph_runs` row receives a run-scoped ID and graph fingerprint.
2. Every node receives a preallocated durable supervisor task ID.
3. Dependency edges are rewritten from graph node IDs to those durable task IDs.
4. Tasks are inserted with `graph_run_id` and `graph_node_id` in one SQLite transaction.
5. The supervisor releases a PENDING task only after all dependency task IDs reach `DONE`.
6. Missing, `CANCELLED`, or `NEEDS_HUMAN` dependencies fail closed to operator escalation.
7. Task transitions refresh the parent graph-run state transactionally.

Human-authored supervisor TOML may still use dependency names; the CLI resolves them to preallocated task IDs before insertion. The ledger itself never relies on global task-name matching.

## Durable worktree isolation

All durable V1 agents require:

```json
"workspace": { "mode": "worktree" }
```

The actuator materializes a stable per-node worktree under:

```text
~/.claudectl/worktrees/<repo-hash>/<graph-run>/<node>/
```

Properties:

- a new workspace is created from the source repository's exact `HEAD`;
- the source repository must be clean before first materialization;
- retries and daemon restarts reuse the same worktree;
- the selected workspace path is persisted on the supervisor task;
- the worktree `HEAD` is recorded on each task attempt as execution evidence;
- verifiers execute in the materialized worktree, not the shared source checkout;
- worktrees are intentionally retained after completion for inspection/evidence; automatic cleanup is not implemented yet.

## Durable authority enforcement

Before Claude launches, the actuator writes a workspace-scoped graph policy. After discovery, the same policy is also bound to the real Claude session ID.

The installed Claude Code `PreToolUse` hook calls the hidden:

```sh
claudectl graph authorize-tool
```

Ordinary non-graph sessions have no graph policy and are unaffected.

Durable graph sessions currently use a deliberately small structured-tool allowlist:

- `Read` — allowed only inside declared `filesystem_read` scopes.
- `Glob` / `Grep` — allowed only inside declared read scopes.
- `Write` / `Edit` / `NotebookEdit` — allowed only inside declared `filesystem_write` scopes.
- `WebFetch` / `WebSearch` — allowed only when node network authority is explicitly `allowed`.
- `TodoWrite` — allowed.
- `Bash` — denied for durable V1 agents.
- unknown tools — denied.

Path checks are worktree-relative and reject lexical escapes and detected symlink escapes outside the worktree.

Because general shell access is disabled, durable V1 also rejects `git_commit=true` or `git_push=true`. Tests/checks belong in deterministic verifiers, where exit codes and output are recorded as evidence.

The durable compiler also rejects options it cannot yet enforce:

- role/mailbox routing,
- per-node model selection,
- per-node agent budget,
- per-node wall-clock timeout.

Accepting an unenforced safety or execution contract would be worse than rejecting it.

## Hooks and completion

`claudectl init upgrade` refreshes both the embedded plugin assets and the direct Claude Code hook entries. Managed hook installation is idempotent: stale claudectl hooks are replaced while unrelated third-party hooks are preserved.

Lifecycle hooks feed `coord.db` through:

```text
PreToolUse  -> claudectl ingest --hook PreToolUse
PostToolUse -> claudectl ingest --hook PostToolUse
SessionStart -> claudectl ingest --hook SessionStart
Stop        -> claudectl ingest --hook Stop
```

The supervisor correlates a `Stop` event by the latest attempt's unique Claude session ID and emits `RunVerifier`. This avoids treating a vanished process as proof of success.

Verifier behavior:

- shell verifier exit 0 -> PASS;
- non-zero exit -> FAIL;
- verifier backend errors become explicit FAIL evidence rather than leaving the task wedged in `VERIFYING`;
- a failed verifier moves the task to `RETRYING` while retry budget remains;
- retry reuses the same isolated worktree and includes the failed verifier output in the next Claude prompt;
- exhausted retries -> `NEEDS_HUMAN`;
- all verifiers passing -> `DONE`.

## Durable outcome mapping

For agent tasks, `graph status` maps supervisor state back into the graph outcome vocabulary:

- `DONE` -> `PASS`
- `NEEDS_HUMAN` with a latest verifier `FAIL` -> `FAIL`
- other `NEEDS_HUMAN` -> `UNKNOWN`
- `CANCELLED` -> `ERROR`
- pending / assigned / running / verifying / retrying / resuming -> `UNKNOWN`

A durable graph run is `PASS` only when every node is `PASS`.

For mixed non-agent nodes, persisted executor evidence supplies FAIL/ERROR/UNKNOWN
outcomes; rejected approval is FAIL. AWAITING_APPROVAL is UNKNOWN and holds
dependents without failure propagation. Cancellation remains ERROR even if late
executor evidence reports PASS. See the [mixed-runtime contract](druid-durable-mixed-graphs.md).

## Evidence and plan binding

The canonical graph fingerprint is deterministic FNV-1a and is useful for stale-plan/change detection only. It is **not** an authorization digest. New durable runs additionally store a SHA-256 execution snapshot digest for explicit approval decisions; this is not a signature or repository-state attestation.

Current durable evidence includes:

- graph/run/node/task identity,
- dependency task IDs,
- task transitions,
- attempts and real Claude session IDs,
- materialized workspace path,
- worktree revision at attempt start,
- verifier commands/verdicts/output,
- retry history,
- graph-level state/outcome.

## Current limitations

The most important remaining gaps are:

1. **Durable environment integration remains unverified.** [PR #2 local validation](validation/druid-master-compiler-v1.md) covers formatting, check, clippy, tests and the local master-plan example; it does not exercise production agent launches or installed hooks.
2. No automatic fan-in/integration/merge of independent worktrees.
3. Mixed execution is persisted, but interrupted deterministic attempts remain UNKNOWN rather than being replayed. Master status can now read a durable run with `--run`; this does not attest agent deliverables or export a synthetic local-runner report.
4. No enforced wall-clock cancellation/termination contract.
5. No automatic worktree cleanup.
6. No signed authorization binding execution to verified repository state. Master plans now have SHA-256 content identity over the authored graph, source claims and authority; the runtime does not attest those claims or enforce a signed approval digest.
7. No Graph Builder interview/UI over this runtime yet.

## Required validation before merge

```sh
cargo fmt --all -- --check
cargo check --all-targets
cargo check --all-targets --no-default-features --features hive
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features --features hive -- -D warnings
cargo test --all-targets
cargo test --all-targets --no-default-features --features hive
```

Keep the PR draft until these pass.

## Safety

REA/reverse-engineering capabilities should only be used for targets the operator owns, is authorized to inspect, or is otherwise permitted to analyze. Nothing in these examples bypasses authentication, DRM, licensing controls, or access restrictions.

The durable worktree smoke graph is deliberately reversible: it writes one marker file inside an isolated worktree, runs a deterministic verifier, and does not commit, push, deploy, or merge.
