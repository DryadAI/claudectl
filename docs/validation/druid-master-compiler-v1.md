# DRUID master compiler V1: local validation evidence

Validated 2026-10-20 on Linux with `rustc 1.99.0 (b940084d7 2026-09-28)`
and `cargo 1.99.0 (5f94df478 2026-08-27)`. Repository base:
`4f6dbbd271788ee021584ece002aa072c1b1c75c` (merged PR #1).
GitHub was queried directly: PR #1 was merged on 2026-10-18 and remote main still
pointed to that merge during this validation. This report records local results;
GitHub Actions and platform integration are separate checks.

## Build and test matrix

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --all-targets` | PASS |
| `cargo check --all-targets --no-default-features --features hive` | PASS |
| `cargo clippy --all-targets -- -D warnings` | PASS |
| `cargo clippy --all-targets --no-default-features --features hive -- -D warnings` | PASS |
| `cargo test --all-targets` | PASS: 866 library + 880 binary + 78 integration + 14 master planner + 8 unit |
| `cargo test --all-targets --no-default-features --features hive` | PASS: 571 library + 582 binary + 78 integration + 14 master planner + 8 unit |
| `git diff --check` | PASS |

The totals (1,846 and 1,253) count test executions, not unique test definitions;
the existing project compiles many modules into both its library and binary.
All 14 new planner tests passed in both configurations. No tests were skipped.

The input example and generated compiled bundle also passed independent JSON
Schema validation with the installed Python `jsonschema` CLI. The Rust tests
compare both checked-in schemas against schemas generated from the Rust types.

## Actual example execution

Commands run from the repository root:

```sh
target/debug/claudectl graph master compile examples/druid-master-plan-v1.json --out outputs/pr2-validation
target/debug/claudectl graph validate outputs/pr2-validation/graph.json
target/debug/claudectl --json graph run outputs/pr2-validation/graph.json < /dev/null
target/debug/claudectl graph master status outputs/pr2-validation/plan.json --report outputs/pr2-validation/runtime-report.json
```

The third command's stdout was captured as `runtime-report.json`; the runner also
wrote `summary.json` and `evidence.jsonl` in its run directory. Outputs are retained
locally under ignored `outputs/pr2-validation/` and
`.claudectl-runs/graph-1792533568150-3925871/`. They are not committed because they
contain machine-specific paths. This run used the working tree containing PR #2
changes; its discovery command observed commit `04826ed` before the feature commit.
The example's source commit records the PR #1 baseline, not a checkout attestation.

- Plan digest: `sha256:deaa7a854c71e6071893c56ce689d8e4fe6027bf2b1b5ecaf412e50c93995503`.
- Runtime fingerprint: `fnv1a64:16234c7a5f83405a`.
- Canonical graph validation: PASS, ten execution waves.
- Discovery, inventory, architecture, dependencies, specification,
  implementation and verification: PASS, one attempt each.
- Verification ran the actual `cargo test --test master_plan`: 14 passed.
- Approval: FAIL because stdin was closed; no approval was granted.
- Deployment and recovery: UNKNOWN, **zero attempts** each.
- Overall process exit: 1, expected because the approval gate was declined.
- Evidence projection: implementation verified; approval/deployment/recovery
  remain planned. The verification task itself is implemented, since it has no
  independent verifier and is not allowed to verify itself.

No deployment, recovery operation, durable agent submission, daemon startup or
installation/hook upgrade was performed.

## Bootstrap defects found while enabling the gates

A separate prerequisite commit contains these fixes, plus formatting and
behavior-preserving lint cleanup:

- A `TaskRow` fixture omitted PR #1's three graph/workspace fields and failed to
  compile. The fields now explicitly use `None`.
- A durable authority test expected an obsolete error string. It now checks the
  actual absolute-filesystem-authority rejection.
- Ordinary supervisor retries incorrectly required a Git workspace revision,
  unlike ordinary task spawning. Revision capture now remains conditional on
  graph membership in both paths. The existing retry regression test passes.
- The QR fallback test depended on `qrencode` being absent. It now directly tests
  the extracted fallback renderer, preserving normal QR selection behavior.

## Deliberate boundaries

This validates the compiler, local runner integration and rejection behavior.
It does not verify production infrastructure or new durable mixed-graph execution.
The existing durable bridge still rejects mixed graphs. Deployment/recovery
providers in the example are unconfigured; no provider is added. Local shell
execution retains caller permissions. Digests identify content, while evidence
and source claims remain unsigned and do not authorize execution.
