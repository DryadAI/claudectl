# DRUID coordinator offline snapshot audit (source-only draft)

This is a **pre-cutover diagnostic**, not a migration, backup, deployment, or
authorization. It does not open the current profile's `coord.db` by default and
never calls `coord::store::open()` (which can create directories and migrate).

The operator must first arrange a consistent, quiesced **single-file copy** of
the coordinator database outside any repository. Do not copy a live WAL-mode
database with `cp`: uncheckpointed transactions may be in `-wal`. The copy or
backup process and its consistency proof are separate work. Do not run this
against Precision's active database, and do not commit the copy or its output.

```sh
python3 scripts/druid_coord_snapshot_audit.py --snapshot /path/to/offline/coord-snapshot.sqlite
```

The command emits one redacted JSON line and exits 0 only if all checks pass;
otherwise it exits 1. It rejects missing files, symlinks/hardlinks, and adjacent
`-wal`/`-shm` files. SQLite opens the supplied copy with `mode=ro`, `immutable=1`,
and `query_only=ON`; the script never creates, migrates, or repairs a database.

Checks: exact schema v8 and required tables/columns; SQLite `quick_check` and
foreign-key check; table row counts; SHA-256 of the exact persisted snapshot JSON;
snapshot payload version and compiled fingerprint against `graph_runs`; graph
task/snapshot presence; and claim bindings to task, attempt, transition, run,
node, and snapshot digest. No prompts, snapshot JSON, policy, worktree path,
secret, or individual row IDs are emitted. A pass is **not** proof that the
snapshot is complete, freshly captured, root-owned, or safe for a broker to use.

The protected-ledger design still needs an independently reviewed snapshot and
cutover procedure, root-owned path/parent/WAL handling, service identity proof,
and explicit activation approval. This audit does not relax those gates.

Synthetic test evidence:

```sh
python3 -m unittest discover -s tests -p test_druid_coord_snapshot_audit.py -v
```
