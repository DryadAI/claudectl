# Coordinator schema v9: legacy claim binding

The coordinator's `user_version=8` does not, by itself, prove that
`graph_headless_claims` has `from_transition_id`. An early v8 ledger may have
been stamped while the table still had the older shape. `CREATE TABLE IF NOT
EXISTS` cannot repair that column. The offline snapshot audit reports this as
`missing_columns=1` and fails closed.

Schema v9 requires the exact prior transition binding. On opening a ledger,
the store checks the table shape. If the field is absent and the claim table is
empty, it transactionally rebuilds only that table and stamps v9. If any claim
exists, it refuses the migration and preserves the rows for manual review;
the missing transition cannot be inferred safely. This migration neither
grants worker authority nor activates headless execution.

Before any live cutover:

1. Quiesce every coordinator writer and capture a consistent, owner-private
   SQLite snapshot using `scripts/druid_coord_snapshot.py`.
2. Audit the **offline** snapshot using
   `scripts/druid_coord_snapshot_audit.py --snapshot <private-copy>`.
3. If it reports v8 with only the legacy claim field missing, verify the claim
   table is empty. Test v9 migration against a separate, disposable copy, then
   repeat the offline audit expecting v9 and all zero failures.
4. Review the exact code revision, operator evidence, and rollback snapshot
   before permitting a live coordinator open with v9. If claims are nonempty,
   stop for manual reconciliation; never drop or synthesize them.

The coordinator directory is owner-only (`0700`) and the DB file is `0600`
when opened by the v9 store. An existing live DB or its WAL/SHM sidecars must
not be copied into a repository. This procedure is not proof of reboot
persistence, root-broker support, worker policy, or production activation.
