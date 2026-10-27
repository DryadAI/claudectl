# DRUID agent deliverable evidence — PR #5

## Contract

Master work items may declare `artifact_paths` for agent nodes. The compiler
lowers those paths into the canonical graph IR; the durable snapshot therefore
binds the declarations to one run and node. Durable master status reads only the
recorded, deterministic Git worktree for that task and emits a timestamped
manifest with each relative path, byte length and SHA-256.

Agent work remains planned if any declared file is missing, unsafe, unavailable
or larger than 64 MiB. Absolute paths, traversal, non-portable separators,
symlinks and directories are rejected or reported unsafe. A linked verifier for
agent work must be a direct dependent shell check with `workspace.mode: worktree`;
the existing graph executor runs that check in the same agent worktree. Its
durable evidence includes the artifact manifest observed before and after the
command. If a declared artifact changes during the check, the check cannot pass.
Later content changes invalidate the verified projection.

The current manifest is part of the read-only status response; verifier-time
manifests are preserved with the existing durable evidence record. Save the
returned JSON when an immutable copy is needed. Hashes identify observed file
contents; they do not establish who created them or sign the run. Filesystem
observation is a local evidence view, not an authorization boundary against
concurrent local mutation.

## Local validation

Tests use temporary Git repositories/worktrees and temporary/in-memory SQLite.
They do not launch an agent or touch the user's worktree root, production ledger,
cluster, deployment or recovery services.

## Validation results

- `cargo check --all-targets` — passed.
- `cargo check --all-targets --no-default-features --features hive` — passed.
- `cargo test` — passed: 900 library, 914 binary, 78 integration, 16 master-plan,
  and 8 unit tests (1,916 total).
- Targeted artifact-attestation end-to-end test — passed, including post-check
  content drift demotion and missing-file demotion.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo clippy --all-targets --no-default-features --features hive -- -D warnings`
  — passed.
- `cargo fmt --all -- --check`, `git diff --check`, and generated-schema drift
  test — passed.

No agent was launched; no production ledger, cluster, deployment or recovery
service was touched. This change is ready for PR review; merge remains a human
decision.
