# Graph Bootstrap V1

This is an intentionally narrow seed runner for `claudectl`. It is not a production-safe sandbox or a fully durable graph orchestrator.

## Commands

```sh
claudectl graph validate examples/graph-bootstrap-v1.json
claudectl graph plan examples/graph-bootstrap-v1.json
claudectl graph run examples/graph-bootstrap-v1.json
```

`--json` works with the graph subcommands. Run evidence is written under `.claudectl-runs/graph-*/`.

## Contracts

- Canonical versioned JSON, independent of graph frameworks.
- Dependency-only edges and typed-port edges.
- Deterministic topological waves; **V1 executes waves sequentially**.
- Bounded retries; PASS, FAIL, UNKNOWN, ERROR outcomes.
- Missing providers return UNKNOWN, never PASS. REA is a future provider adapter.
- Interactive approval uses a stable *non-cryptographic* graph fingerprint. It is **not** authorization to merge, deploy, or push.
- Shell commands run on the host with the caller's permissions. Declared filesystem/network/git authority is **not yet enforced at runtime**. Graph policy is currently a preflight declaration check only. Run only trusted graph files.
- No crash-safe resume, no timeout enforcement, no sandbox, no Git worktree isolation, no true provider registry, no typed runtime artifact transfer, no plan/environment digest.

## Next hardening milestones

1. Replace host shell execution with worktree/container-scoped execution and enforce capability/authority manifests at the executor boundary.
2. Add persistent run ledger, interrupted-step reconciliation, bounded command timeouts, and explicit cancellation.
3. Add runtime typed artifact passing, node output verification, and provenance.
4. Integrate the existing supervisor/bus/brain as provider adapters without duplicating their state machines.
5. Add REA as an opt-in observation provider with explicit effects and evidence contracts.
6. Introduce a cryptographic digest binding graph, repo state, policy, and intended side effects before consequential approval.
7. Implement the interview UI on top of the same graph core.

## Safety

This bootstrap graph runs only local checks (Cargo commands). It does not reverse engineer external software, launch an AI agent, deploy, push, or merge.
