"""Synthetic WAL and fail-closed tests; never touches a live coordinator DB."""

import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/druid_coord_snapshot.py"
spec = importlib.util.spec_from_file_location("druid_coord_snapshot", SCRIPT)
snapshot_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(snapshot_module)


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.source = root / "source.sqlite"
        self.private = root / "private"
        self.private.mkdir(mode=0o700)
        self.target = self.private / "snapshot.sqlite"

    def test_wal_committed_data_in_consistent_copy(self):
        with sqlite3.connect(self.source) as live:
            live.execute("PRAGMA journal_mode=WAL")
            live.execute("CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT)")
            live.execute("INSERT INTO records(value) VALUES ('committed')")
            live.commit()
            # Keep source open in WAL mode while backup runs.
            self.assertTrue(snapshot_module.create_snapshot(self.source, self.target))
        with sqlite3.connect(self.target) as backup:
            self.assertEqual(backup.execute("SELECT value FROM records").fetchone(),
                             ("committed",))
            self.assertEqual(backup.execute("PRAGMA quick_check").fetchone(),
                             ("ok",))
        self.assertEqual(self.target.stat().st_mode & 0o777, 0o600)
        self.assertFalse(Path(str(self.target) + "-wal").exists())
        self.assertFalse(Path(str(self.target) + "-shm").exists())

    def test_no_overwrite(self):
        sqlite3.connect(self.source).close()
        self.target.write_text("preserve")
        with self.assertRaisesRegex(snapshot_module.SnapshotError,
                                    "destination_already_exists"):
            snapshot_module.create_snapshot(self.source, self.target)
        self.assertEqual(self.target.read_text(), "preserve")

    def test_missing_source_does_not_create_target(self):
        with self.assertRaisesRegex(snapshot_module.SnapshotError,
                                    "source_missing_or_unreadable"):
            snapshot_module.create_snapshot(self.source, self.target)
        self.assertFalse(self.target.exists())

    def test_rejects_public_or_linked_destination_parent(self):
        sqlite3.connect(self.source).close()
        public = Path(self.temp.name) / "public"
        public.mkdir(mode=0o755)
        with self.assertRaisesRegex(snapshot_module.SnapshotError,
                                    "destination_parent_not_owner_private"):
            snapshot_module.create_snapshot(self.source, public / "copy.sqlite")
        link = Path(self.temp.name) / "link"
        link.symlink_to(self.private, target_is_directory=True)
        with self.assertRaisesRegex(snapshot_module.SnapshotError,
                                    "destination_parent_not_owner_private"):
            snapshot_module.create_snapshot(self.source, link / "copy.sqlite")

    def test_rejects_source_symlink(self):
        sqlite3.connect(self.source).close()
        link = Path(self.temp.name) / "source-link"
        link.symlink_to(self.source)
        with self.assertRaisesRegex(snapshot_module.SnapshotError,
                                    "source_not_regular_file"):
            snapshot_module.create_snapshot(link, self.target)

    def test_corrupt_source_leaves_no_partial_copy(self):
        self.source.write_bytes(b"not a database")
        with self.assertRaisesRegex(snapshot_module.SnapshotError,
                                    "sqlite_backup_failed"):
            snapshot_module.create_snapshot(self.source, self.target)
        self.assertFalse(self.target.exists())
        self.assertEqual(list(self.private.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
