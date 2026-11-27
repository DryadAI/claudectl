"""Synthetic-only tests for the offline coordinator audit."""

import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest
import hashlib
import json


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/druid_coord_snapshot_audit.py"
spec = importlib.util.spec_from_file_location("druid_coord_snapshot_audit", SCRIPT)
audit_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit_module)


class SnapshotAuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "snapshot.sqlite"

    def fixture(self):
        db = sqlite3.connect(self.path)
        db.executescript("""
            PRAGMA foreign_keys=ON;
            PRAGMA user_version=9;
            CREATE TABLE graph_runs(id TEXT PRIMARY KEY, fingerprint TEXT);
            CREATE TABLE tasks(id TEXT PRIMARY KEY, graph_run_id TEXT,
                               graph_node_id TEXT);
            CREATE TABLE task_attempts(id TEXT PRIMARY KEY, task_id TEXT REFERENCES tasks(id),
                                       attempt_num INTEGER);
            CREATE TABLE task_transitions(id INTEGER PRIMARY KEY, task_id TEXT REFERENCES tasks(id));
            CREATE TABLE graph_snapshots(run_id TEXT PRIMARY KEY REFERENCES graph_runs(id),
                                         digest TEXT, snapshot_json TEXT);
            CREATE TABLE graph_headless_claims(
                attempt_id TEXT PRIMARY KEY REFERENCES task_attempts(id),
                task_id TEXT REFERENCES tasks(id), attempt_num INTEGER,
                run_id TEXT REFERENCES graph_runs(id), node_id TEXT,
                snapshot_digest TEXT,
                from_transition_id INTEGER REFERENCES task_transitions(id));
        """)
        data = json.dumps({"version": 1, "compiled": {"fingerprint": "fingerprint"},
                           "private_prompt": "must not print"})
        digest = hashlib.sha256(data.encode()).hexdigest()
        db.execute("INSERT INTO graph_runs VALUES ('r', 'fingerprint')")
        db.execute("INSERT INTO tasks VALUES ('t', 'r', 'n')")
        db.execute("INSERT INTO task_attempts VALUES ('a', 't', 1)")
        db.execute("INSERT INTO task_transitions VALUES (1, 't')")
        db.execute("INSERT INTO graph_snapshots VALUES ('r', ?, ?)", (digest, data))
        db.execute("INSERT INTO graph_headless_claims VALUES "
                   "('a', 't', 1, 'r', 'n', ?, 1)", (digest,))
        db.commit()
        db.close()

    def test_valid_and_redacted(self):
        self.fixture()
        before = self.path.read_bytes()
        result = audit_module.audit(self.path)
        self.assertTrue(result["pass"], result)
        self.assertEqual(result["counts"]["graph_headless_claims"], 1)
        self.assertNotIn("must not print", json.dumps(result))
        self.assertEqual(self.path.read_bytes(), before)
        self.assertFalse(Path(str(self.path) + "-shm").exists())

    def test_bad_digest_fails_closed(self):
        self.fixture()
        with sqlite3.connect(self.path) as db:
            db.execute("UPDATE graph_snapshots SET digest='forged'")
        result = audit_module.audit(self.path)
        self.assertFalse(result["pass"])
        self.assertEqual(result["failures"]["snapshot_digests"], 1)
        self.assertEqual(result["failures"]["claim_binding_mismatches"], 1)

    def test_claim_binding_mismatch(self):
        self.fixture()
        with sqlite3.connect(self.path) as db:
            db.execute("UPDATE graph_headless_claims SET node_id='wrong'")
        result = audit_module.audit(self.path)
        self.assertEqual(result["failures"]["claim_binding_mismatches"], 1)

    def test_null_claim_binding_mismatch(self):
        self.fixture()
        with sqlite3.connect(self.path) as db:
            db.execute("UPDATE tasks SET graph_run_id=NULL")
        result = audit_module.audit(self.path)
        self.assertEqual(result["failures"]["claim_binding_mismatches"], 1)
        self.assertEqual(result["failures"]["graph_tasks_without_snapshots"], 1)

    def test_fingerprint_drift(self):
        self.fixture()
        with sqlite3.connect(self.path) as db:
            db.execute("UPDATE graph_runs SET fingerprint='forged'")
        result = audit_module.audit(self.path)
        self.assertEqual(result["failures"]["run_fingerprints"], 1)

    def test_foreign_key_violation(self):
        self.fixture()
        with sqlite3.connect(self.path) as db:
            db.execute("PRAGMA foreign_keys=OFF")
            db.execute("UPDATE task_attempts SET task_id='missing'")
        result = audit_module.audit(self.path)
        self.assertFalse(result["pass"])
        self.assertEqual(result["failures"]["foreign_keys"], 1)
        self.assertEqual(result["failures"]["claim_binding_mismatches"], 1)

    def test_missing_does_not_create(self):
        with self.assertRaisesRegex(ValueError, "snapshot_missing"):
            audit_module.audit(self.path)
        self.assertFalse(self.path.exists())

    def test_sidecar_and_symlink_rejected(self):
        self.fixture()
        sidecar = Path(str(self.path) + "-wal")
        sidecar.touch()
        with self.assertRaisesRegex(ValueError, "snapshot_has_wal"):
            audit_module.audit(self.path)
        sidecar.unlink()
        link = Path(self.temp.name) / "linked.sqlite"
        link.symlink_to(self.path)
        with self.assertRaisesRegex(ValueError, "snapshot_not_single"):
            audit_module.audit(link)

    def test_schema_drift_and_missing_table(self):
        self.fixture()
        with sqlite3.connect(self.path) as db:
            db.execute("PRAGMA user_version=10")
            db.execute("DROP TABLE graph_headless_claims")
        result = audit_module.audit(self.path)
        self.assertFalse(result["pass"])
        self.assertEqual(result["failures"]["schema_version"], 1)
        self.assertEqual(result["failures"]["missing_tables"], 1)


if __name__ == "__main__":
    unittest.main()
