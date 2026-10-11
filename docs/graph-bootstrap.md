# Graph Bootstrap V1

This is an intentionally narrow seed runner for `claudectl`. It is not a production-safe sandbox or a fully durable graph orchestrator.

## Commands

```sh
claudectl graph validate examples/graph-bootstrap-v1.json
claudectl graph plan examples/graph-bootstrap-v1.json
claudectl graph run examples/graph-bootstrap-v1.json
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
- Deterministic topological waves; **V1 executes waves sequentially**.
- Bounded retries; PASS, FAIL, UNKNOWN, ERROR outcomes.
- Missing providers return UNKNOWN, never PASS.
- REA is a runtime capability provider for `reverse.capabilities.discover`, `reverse.javascript.analyze`, and `reverse.native.analyze`; the canonical graph remains provider-independent.
- Interactive approval uses a stable *non-cryptographic* graph fingerprint. It is **not** authorization to merge, deploy, or push.
- Shell commands run on the host with the caller's permissions. Declared filesystem/network/git authority is **not yet enforced at runtime**. Graph policy is currently a preflight declaration check only. Run only trusted graph files.
- No crash-safe resume, no timeout enforcement, no sandbox, no Git worktree isolation, no true provider registry, no typed runtime artifact transfer, no plan/environment digest.

## Next hardening milestones

1. Replace host shell execution with worktree/container-scoped execution and enforce capability/authority manifests at the executor boundary.
2. Add persistent run ledger, interrupted-step reconciliation, bounded command timeouts, and explicit cancellation.
3. Add runtime typed artifact passing, node output verification, and provenance.
4. Integrate the existing supervisor/bus/brain as provider adapters without duplicating their state machines.
5. Expand REA capability adapters only where its CLI contracts are stable, and validate provider request schemas per capability.
6. Introduce a cryptographic digest binding graph, repo state, policy, and intended side effects before consequential approval.
7. Implement the interview UI on top of the same graph core.

## Safety

The bootstrap graph runs only local checks (Cargo commands). The REA examples are separate and opt-in. Use reverse-engineering capabilities only for targets you own, are authorized to inspect, or are otherwise permitted to analyze. The graph layer does not bypass authentication, DRM, licensing controls, or access restrictions. Nothing in these examples deploys, pushes, or merges.
