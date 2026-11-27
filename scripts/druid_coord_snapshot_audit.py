#!/usr/bin/env python3
"""Read-only, content-redacted audit of an OFFLINE coordinator SQLite snapshot.

This does not create a backup, open the live profile DB, migrate, or authorize
headless work. Supply a quiesced, single-file copy with no WAL/SHM sidecars.
"""

import argparse
from contextlib import closing
import hashlib
import json
from pathlib import Path
import sqlite3
import stat
import sys


SCHEMA_VERSION = 8  # src/coord/store.rs::EXPECTED_COORD_SCHEMA_VERSION
TABLE_COLUMNS = {
    "tasks": {"id", "graph_run_id", "graph_node_id"},
    "task_attempts": {"id", "task_id", "attempt_num"},
    "task_transitions": {"id", "task_id"},
    "graph_runs": {"id", "fingerprint"},
    "graph_snapshots": {"run_id", "digest", "snapshot_json"},
    "graph_headless_claims": {
        "attempt_id", "task_id", "attempt_num", "run_id", "node_id",
        "snapshot_digest", "from_transition_id",
    },
}


def _offline_file(path):
    """Reject missing, linked, or WAL-backed inputs before immutable SQLite open."""
    try:
        info = path.lstat()
    except OSError:
        raise ValueError("snapshot_missing_or_unreadable") from None
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError("snapshot_not_single_regular_file")
    if any(Path(str(path) + suffix).exists() for suffix in ("-wal", "-shm")):
        raise ValueError("snapshot_has_wal_or_shm_sidecar")
    return info


def audit(snapshot):
    """Return a redacted result; no SQL writes and no snapshot content output."""
    path = Path(snapshot).absolute()
    before = _offline_file(path)
    result = {
        "format_version": 1,
        "audit_kind": "offline_snapshot_only",
        "expected_schema_version": SCHEMA_VERSION,
        "schema_version": None,
        "counts": {},
        "failures": {},
        "pass": False,
    }
    # immutable=1 avoids SQLite creating shared-memory sidecars. It is only
    # safe on a quiesced, single-file snapshot, never an active WAL database.
    uri = path.as_uri() + "?mode=ro&immutable=1"
    try:
        with closing(sqlite3.connect(uri, uri=True)) as db:
            db.execute("PRAGMA query_only=ON")
            version = db.execute("PRAGMA user_version").fetchone()[0]
            result["schema_version"] = version
            result["failures"]["schema_version"] = int(version != SCHEMA_VERSION)
            integrity = db.execute("PRAGMA quick_check").fetchall()
            result["failures"]["quick_check"] = int(integrity != [("ok",)])
            result["failures"]["foreign_keys"] = sum(
                1 for _ in db.execute("PRAGMA foreign_key_check")
            )

            present = {
                row[0] for row in db.execute(
                    "SELECT name FROM sqlite_master WHERE type='table'"
                )
            }
            missing = set(TABLE_COLUMNS) - present
            result["failures"]["missing_tables"] = len(missing)
            if missing:
                return result
            missing_columns = 0
            for table, required in TABLE_COLUMNS.items():
                columns = {row[1] for row in db.execute(f"PRAGMA table_info({table})")}
                missing_columns += len(required - columns)
            result["failures"]["missing_columns"] = missing_columns
            if missing_columns:
                return result

            for table in TABLE_COLUMNS:
                result["counts"][table] = db.execute(
                    f"SELECT COUNT(*) FROM {table}"
                ).fetchone()[0]

            bad_digests = 0
            bad_payloads = 0
            bad_fingerprints = 0
            for digest, data, fingerprint in db.execute(
                "SELECT s.digest, s.snapshot_json, r.fingerprint "
                "FROM graph_snapshots s LEFT JOIN graph_runs r ON r.id=s.run_id"
            ):
                if not isinstance(data, str) or hashlib.sha256(
                    data.encode("utf-8")
                ).hexdigest() != digest:
                    bad_digests += 1
                try:
                    payload = json.loads(data)
                    compiled = payload["compiled"]
                    if payload["version"] != 1 or not isinstance(compiled, dict):
                        raise ValueError("unsupported snapshot")
                    if compiled.get("fingerprint") != fingerprint:
                        bad_fingerprints += 1
                except (TypeError, ValueError, KeyError):
                    bad_payloads += 1
            result["failures"]["snapshot_digests"] = bad_digests
            result["failures"]["snapshot_payloads"] = bad_payloads
            result["failures"]["run_fingerprints"] = bad_fingerprints
            result["failures"]["graph_tasks_without_snapshots"] = db.execute(
                "SELECT COUNT(*) FROM tasks t "
                "LEFT JOIN graph_snapshots s ON s.run_id=t.graph_run_id "
                "WHERE (t.graph_run_id IS NULL AND t.graph_node_id IS NOT NULL) "
                "OR (t.graph_run_id IS NOT NULL AND "
                "(t.graph_node_id IS NULL OR s.run_id IS NULL))"
            ).fetchone()[0]
            result["failures"]["claim_binding_mismatches"] = db.execute(
                "SELECT COUNT(*) FROM graph_headless_claims c "
                "LEFT JOIN task_attempts a ON a.id=c.attempt_id "
                "LEFT JOIN tasks t ON t.id=c.task_id "
                "LEFT JOIN graph_snapshots s ON s.run_id=c.run_id "
                "LEFT JOIN task_transitions x ON x.id=c.from_transition_id "
                "WHERE a.id IS NULL OR t.id IS NULL OR s.run_id IS NULL "
                "OR x.id IS NULL OR a.task_id IS NOT c.task_id "
                "OR a.attempt_num IS NOT c.attempt_num "
                "OR t.graph_run_id IS NOT c.run_id "
                "OR t.graph_node_id IS NOT c.node_id "
                "OR s.digest IS NOT c.snapshot_digest "
                "OR x.task_id IS NOT c.task_id"
            ).fetchone()[0]
    except sqlite3.Error:
        result["failures"]["sqlite_read"] = 1
        return result

    after = _offline_file(path)
    result["failures"]["snapshot_changed_during_audit"] = int(
        (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    )
    result["pass"] = not any(result["failures"].values())
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snapshot", required=True, type=Path,
                        help="existing, quiesced single-file coord DB copy; never a live DB")
    args = parser.parse_args(argv)
    try:
        result = audit(args.snapshot)
    except ValueError as error:
        result = {"format_version": 1, "audit_kind": "offline_snapshot_only",
                  "pass": False, "failures": {str(error): 1}}
    print(json.dumps(result, sort_keys=True))
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
