# Durable mixed graph validation — 2026-10-26 UTC

Base: merged PR #2, `0267504345f6267ef415b8d612f7feeb5a43ee23`.
Branch: `codex/druid-durable-mixed-graphs`.
Host: Linux; rustc 1.99.1 / cargo 1.99.1.

## Final local gates

| Gate | Default features | `--no-default-features --features hive` |
| --- | --- | --- |
| `cargo check --all-targets` | PASS | PASS |
| `cargo clippy --all-targets -- -D warnings` | PASS | PASS |
| `cargo test --all-targets` | PASS, 1,890 executions | PASS, 1,253 executions |
| `cargo fmt --all -- --check` | PASS | Same source tree |
| `git diff --check` | PASS | Same source tree |

Default totals: 888 library, 902 binary, 78 integration, 14 master-plan and 8 unit
tests. Minimal totals: 571 library, 582 binary, 78 integration, 14 master-plan and
8 unit tests. Library and binary exercise overlapping modules: these are test
executions, not counts of unique test cases.

There are 21 new mixed-graph tests plus one actuator guard test (each runs in the
default library and binary suites). The existing schema-version test now also
asserts migration cannot overwrite a future version stamp.

The final matrix ran with `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`, and
`CARGO_PROFILE_TEST_DEBUG=0`. An earlier build failed with local disk exhaustion;
only this repository's generated `target` outputs were cleaned, then the complete
matrix was rerun successfully with reduced build-output size. No source, ledger
or unrelated user data was removed.

Local command logs: `/tmp/claudectl-pr3-{check,clippy,tests}-{default,hive}.log`.
These temporary host logs are not repository artifacts; this record and the
repeatable tests are the reviewable evidence. Remote CI is separate and must be
checked on the PR; no remote result is claimed here.

## Executed acceptance coverage

- Master compiler output submits atomically into the same durable task ledger;
  submitted work remains PENDING/UNKNOWN with no fabricated evidence.
- Command → approval → check progression uses the existing supervisor. Waiting
  approvals hold downstream work without failure propagation.
- Wrong digest/run/node, early, empty-actor and duplicate decisions are rejected.
  Rejection blocks descendants and generic completion/retry cannot override it.
- Agent actions cannot execute approval nodes; generic agent completion cannot
  skip an upstream approval. Agent routing remains on the existing spawn path.
- Concurrent database connections claim one attempt; delayed actions cannot
  consume later retries. Completed nodes remain completed after database reopen.
- Crash-after-claim and injected result-commit failure preserve UNKNOWN and
  prevent replay. Failed result transactions roll back evidence and completion.
- Cancellation retains late PASS evidence without changing CANCELLED to DONE.
- Recorded FAIL/ERROR retries are bounded; missing providers yield UNKNOWN and
  do not retry. Mock provider confidence and artifact links persist unchanged.
- Corrupt/missing snapshots, mutated dependencies/cwd, unknown task state, forged
  compiled dependencies, unsupported workspaces/subgraphs/typed transport and
  recoverable operations lacking approval fail closed.
- Shell execution requires explicit opt-in and freezes cwd; relative provider
  targets/scopes are rejected to avoid daemon-working-directory reinterpretation.

Real short shell commands executed only in local tests. SQLite stores were
in-memory or temporary file-backed databases. Provider success/error paths were
mocked; no live agent sessions or production REA operations were launched.

## CLI validation (no execution)

`claudectl --json graph validate examples/graph-durable-mixed-smoke.json` passed:

- ID: `druid-durable-mixed-smoke-v1`
- Fingerprint: `fnv1a64:9131e7015feec82c`
- Five waves: inventory → capabilities → agent-marker → review → confirmation.

`claudectl graph master validate examples/druid-master-plan-v1.json` passed:

- Digest: `sha256:2f240ec45c4dcf819987473161439246c9ede1238c8b97d7b6112d08acf3198c`
- Ten phases: discovery, inventory, architecture, dependencies, specification,
  implementation, verification, approval, deployment and recovery.
- The unconfigured deployment/recovery placeholders now declare no unnecessary
  filesystem read grants. No deployment/recovery provider is configured.

## Not established by this validation

No production submission, daemon startup, cluster mutation, deployment, recovery,
live Claude launch, installed-hook integration, worktree fan-in or remote-provider
behavior was validated. The user's separately reported node-05 pilot is not
evidence for this runtime's production execution and grants no rollout authority.

Remaining boundaries are explicit in [the runtime contract](../druid-durable-mixed-graphs.md):
unsandboxed shell opt-in, unsigned local approval assertions, synchronous calls
without timeouts, no process termination on cancellation, no automatic replay of
uncertain attempts, and no durable-to-master report projection. This completes
the scoped mixed-runtime increment, not production deployment certification.
