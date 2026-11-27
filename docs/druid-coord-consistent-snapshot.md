# DRUID coordinator consistent snapshot producer (source-only draft)

This script is an **operator-run backup primitive**, not a migration or
protected-ledger cutover. It is not executed automatically by DRUID. Its
inputs must be explicit; there is no default live database path.

Create a new private directory owned by the invoking operator with mode 0700
outside any repository or synced vault. Then, when separately authorized to
capture the chosen coordinator database:

```sh
python3 scripts/druid_coord_snapshot.py \
  --source /explicit/path/to/coord.db \
  --destination /private/snapshot-directory/coord-snapshot.sqlite
python3 scripts/druid_coord_snapshot_audit.py \
  --snapshot /private/snapshot-directory/coord-snapshot.sqlite
```

The source opens in SQLite read-only mode and the online backup API captures a
consistent committed view, including WAL transactions. SQLite may interact
with the source's WAL shared-memory sidecar; this is **not** a claim of zero
source-filesystem activity. The producer converts its private copy to DELETE
journal mode, verifies no target sidecars and `quick_check=ok`, fsyncs, and
atomically publishes under an absent destination name. It does not overwrite,
create a public destination directory, copy source sidecars, or emit database
contents. Output is one JSON status line. If it reports
`destination_published_durability_unverified`, the file may exist: inspect it
before retrying, never assume failure removed it.

The separate audit checks schema v8, graph digests, fingerprints and claim
bindings. Producer success **does not** imply audit success, root-owned
storage, transfer-safe policy, or approval to use the copy in a privileged
broker. Do not commit, upload, or paste the snapshot: it may contain prompts,
paths, policy and other sensitive coordination data. Source ownership,
protected destination identity/parent, WAL behavior at cutover, CLI access,
rollback and activation still need independent review.

Synthetic tests only:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_druid_coord_snapshot*.py' -v
```
