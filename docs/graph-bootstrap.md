# Graph Bootstrap V1

This is an intentionally narrow seed runner for `claudectl`. It is not a production-safe sandbox or a fully durable graph orchestrator.

## Commands

```sh
claudectl graph validate examples/graph-bootstrap-v1.json
claudectl graph plan examples/graph-bootstrap-v1.json
claudectl graph run examples/graph-bootstrap-v1.json

# Durable Claude Code agent DAG (requires the coord/supervisor feature)
claudectl graph submit examples/graph-durable-agent-smoke.json
claudectl graph status <graph-run-id>
```

`--json` works with the graph subcommands. Run evidence is written under `.claudectl-runs/graph-*/`.

REA provider smoke test (safe discovery only):

```sh
claudectl graph run examples/graph-rea-capabilities.json
```

If `rea` is unavailable, the node returns `UNKNOWN`, never `PASS`. When REA is installed, its full `--json` output is streamed to a run artifact rather than embedded into the JSONL evidence log.

For authorized local JavaScript/Electron analysis, copy and edit `examples/graph-rea-javascript-template.json` so `target` and declared read authority match the application you are permitted to inspect.

## Contracts

- Canonical versioned JSON, independent of graph frameworks.
- Dependency-only edges and typed-port edges.
- Deterministic topological waves; the local seed runner executes waves sequentially.
- Durable agent graphs compile node ids to preallocated supervisor task ids, and dependency edges are stored as those task ids.
- Supervisor PENDING tasks wait until every dependency task is DONE; missing or unsuccessful terminal dependencies escalate to NEEDS_HUMAN.
- Bounded retries; PASS, FAIL, UNKNOWN, ERROR outcomes.
- Missing providers return UNKNOWN, never PASS.
- REA is a runtime capability provider for `reverse.capabilities.discover`, `reverse.javascript.analyze`, and `reverse.native.analyze`; the canonical graph remains provider-independent.
- Interactive approval uses a stable *non-cryptographic* graph fingerprint. It is **not** authorization to merge, deploy, or push.
- Shell commands run on the host with the caller's permissions. Declared filesystem/network/git authority is **not yet enforced at runtime**. Graph policy is currently a preflight declaration check only. Run only trusted graph files.
- The local seed runner has no crash-safe resume or timeout enforcement.
- Durable Claude Code agent nodes use the existing supervisor ledger/reconciler and survive process restart, but mixed durable graphs (shell/tool/approval + agent) are intentionally rejected in V1.
- No sandbox or Git worktree isolation yet; declared authority is persisted into task policy but is not yet enforced for Claude Code agent execution.
- No typed runtime artifact transfer or plan/environment cryptographic digest yet.

## Next hardening milestones

1. Enforce graph-declared filesystem/network/git authority for durable Claude Code agents, preferably through per-node worktrees/containers.
2. Add bounded command/agent timeouts and explicit cancellation to the graph-facing contract.
3. Add runtime typed artifact passing, node output verification, and provenance.
4. Teach the graph runtime to reconcile mixed graphs while continuing to delegate agent durability to the existing supervisor.
5. Expand REA capability adapters only where its CLI contracts are stable, and validate provider request schemas per capability.
6. Introduce a cryptographic digest binding graph, repo state, policy, and intended side effects before consequential approval.
7. Implement the interview UI on top of the same graph core.

## Safety

The bootstrap graph runs only local checks (Cargo commands). The REA examples are separate and opt-in. Use reverse-engineering capabilities only for targets you own, are authorized to inspect, or are otherwise permitted to analyze. The graph layer does not bypass authentication, DRM, licensing controls, or access restrictions. Nothing in these examples deploys, pushes, or merges.


## Durable supervisor bridge

`claudectl graph submit` is intentionally narrower than `graph run` in V1. It accepts only `agent` nodes whose provider capability is `agent.execute` and whose preferred provider is either omitted or `claude_code`.

Submission is atomic:

1. A `graph_runs` row receives a run-scoped id and graph fingerprint.
2. Every node receives a preallocated durable supervisor task id.
3. Edges are rewritten from graph node ids to those task ids.
4. All task rows are inserted in one SQLite transaction with `graph_run_id` and `graph_node_id`.
5. The existing supervisor reconciler releases a task only after all dependency task ids reach `DONE`.
6. Every task transition refreshes the parent graph-run state transactionally.

Human-authored supervisor TOML keeps using dependency names for ergonomics, but the CLI resolves those names to preallocated task ids before insertion. The ledger never relies on global task-name matching.
