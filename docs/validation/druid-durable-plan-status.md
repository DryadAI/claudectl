# Durable master-plan status validation — 2026-10-26

Base: merged PR #3, `c6e241d923ae0029f2fb0eee5b1aaa61d1875a9a`.
Branch: `codex/druid-durable-plan-status`.
Environment: Linux, rustc/cargo 1.99.1.

## Result

`graph master status plan.json --run RUN_ID` projects one stored durable run into
planned/implemented/verified work states without executing work or changing the
ledger. It embeds the durable results so its evidence pointers are resolvable in
the returned JSON. The existing local `--report` path retains its behavior.

| Final gate | Default | Minimal (`--no-default-features --features hive`) |
| --- | --- | --- |
| `cargo check --all-targets` | PASS | PASS |
| `cargo clippy --all-targets -- -D warnings` | PASS | PASS |
| `cargo test --all-targets` | PASS: 1,909 executions | PASS: 1,253 executions |
| `cargo fmt --all -- --check` | PASS | Same tree |
| `git diff --check` | PASS | Same tree |

Default totals: 897 library, 911 binary, 78 integration, 15 master-plan and 8 unit
tests. Minimal totals: 571 library, 582 binary, 78 integration, 14 master-plan and
8 unit tests. These are executions; library and binary suites overlap.

Build settings: `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, retained from the earlier disk-space mitigation.
Temporary local logs: `/tmp/claudectl-pr4-{check,clippy,tests}-{default,hive}.log`.

## New regression evidence

Eight durable projection tests cover:

1. Real short shell checks advancing planned → implemented → independently
   verified, waiting/approved gates, absent provider UNKNOWN, and resolvable links.
2. Reopening a file-backed ledger read-only with zero SQL changes and byte-for-byte
   unchanged database contents after projection.
3. Rejection of other plan revisions, incomplete task sets, altered metadata or
   dependencies, and missing execution snapshots.
4. No promotion of uncertain claimed attempts; cancellation demotes affected work
   and descendants without discarding historical evidence.
5. Rejection of altered commands, exit codes, attempts, authority or outcomes
   presented as successful execution evidence.
6. Derived provider PASS remains planned rather than becoming verified.
7. Simulated agent DONE remains distinct from an observed planned deliverable,
   even after the independent check succeeds.
8. Rejection of approval-digest drift and DONE nodes whose evidence is missing.

A store test verifies read-only opening cannot create a missing ledger/directory,
migrate an older schema, write tables, or alter a future-version database. A CLI
test verifies `--run` and `--report` conflict before any file/database access.
The nine module tests run in both library and binary suites; the CLI test runs
once, adding 19 executions relative to PR #3.

## Boundaries

Tests use temporary/in-memory databases and synthetic plans with harmless local
shell commands. Agent completion is simulated; no live agent, provider service,
production ledger, daemon, cluster, deployment or recovery operation was used.
No new schema migration, scheduler, executor or approval mechanism was added.

This is a conservative evidence view, not authorization or independent attestation
of repository contents, agent deliverables, actor identity or production health.
The original unsigned-approval and unsandboxed-shell boundaries remain unchanged.

GitHub Actions was enabled and workflows were listed as active, but the repository
reported zero workflow runs during this validation. Remote CI is not claimed as
passing; the table above records actual local gates only.
