# DRUID verified worktree integration — PR #6

## Contract

The canonical graph adds an `integration` node with an `artifact_collect`
executor. Each source must be a declared path from a direct agent dependency,
name a direct worktree shell verifier, and also depend on that verifier. Before
collection, durable execution requires an observed PASS with matching command,
exit code, authority, attempt, file hash, and byte count. Agent source worktrees
must share one repository and the integration worktree must share their base Git
revision.

Only regular, non-symlink declared files are read. Each file is capped at 64 MiB
and the complete collection at 256 MiB. Destinations must be relative, non-
overlapping, and covered by the integration node's `filesystem_write` scope.
Writes use create-new publication and never overwrite differing content; an
identical prior copy is safe to retry. The integration node writes only to its
own run-specific worktree. It does not merge, commit, push, alter the source
checkout, or deploy. Downstream checks can independently verify the staged tree.

## Validation evidence

- End-to-end temporary-Git test: agent artifact → durable verifier → hash-bound
  integration into a separate worktree; source checkout remained unchanged.
- Compiler tests reject undeclared/non-direct sources, missing verifier edges,
  and destinations outside write authority.
- Collector tests cover idempotent retries, conflicting destination content,
  verifier-hash drift, and source symlink rejection.
- Published smoke graph compiles/submits to an in-memory ledger without
  executing an agent. CLI `graph validate` and `graph plan` both passed.
- Default `cargo test --all-targets`: 1,924 passed (904 lib, 918 bin, 78
  integration, 16 master-plan, 8 unit).
- Hive-only `cargo test --all-targets --no-default-features --features hive`:
  1,256 passed (572 lib, 583 bin, 78 integration, 15 master-plan, 8 unit).
- Strict clippy passed for default and hive-only configurations.
- Formatting, `git diff --check`, and generated-schema drift test passed.

No agent was launched and no production ledger, Git checkout, cluster,
deployment, or recovery service was changed. This feature provides an isolated
staging tree only; human review and any eventual Git merge remain outside the
executor.
