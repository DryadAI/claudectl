#!/usr/bin/env python3
"""Create a consistent private coordinator snapshot using SQLite online backup.

Source and destination are explicit. Never defaults to the active profile DB.
The source is opened for read-only SQL; SQLite may use its WAL shared-memory
sidecar. The destination must be absent in an owner-only private directory.
"""

import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import stat
import sys
import tempfile
import time


class SnapshotError(Exception):
    """A stable, non-sensitive failure code for CLI output."""


def _check_input(path):
    try:
        info = path.lstat()
    except OSError:
        raise SnapshotError("source_missing_or_unreadable") from None
    if not stat.S_ISREG(info.st_mode):
        raise SnapshotError("source_not_regular_file")


def _check_output(path):
    parent = path.parent
    try:
        info = parent.lstat()
    except OSError:
        raise SnapshotError("destination_parent_missing") from None
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid()
            or info.st_mode & 0o077):
        raise SnapshotError("destination_parent_not_owner_private")
    if os.path.lexists(path):
        raise SnapshotError("destination_already_exists")


def create_snapshot(source, destination, timeout_seconds=30):
    """Atomically publish a new single-file SQLite backup; never overwrite."""
    source = Path(source).absolute()
    destination = Path(destination).absolute()
    if timeout_seconds <= 0:
        raise SnapshotError("invalid_timeout")
    _check_input(source)
    _check_output(destination)

    temp_path = None
    published = False
    try:
        fd, name = tempfile.mkstemp(prefix=".druid-coord-", suffix=".sqlite",
                                    dir=destination.parent)
        os.close(fd)
        temp_path = Path(name)
        deadline = time.monotonic() + timeout_seconds

        with closing(sqlite3.connect(source.as_uri() + "?mode=ro", uri=True,
                                     timeout=5)) as live:
            live.execute("PRAGMA query_only=ON")
            with closing(sqlite3.connect(temp_path)) as target:
                def progress(_status, _remaining, _total):
                    if time.monotonic() > deadline:
                        raise SnapshotError("backup_timeout")

                live.backup(target, pages=64, progress=progress, sleep=0.05)

        # Online backup can retain the source's WAL journal mode. Convert the
        # private copy to DELETE mode before publication so the offline audit
        # cannot silently ignore committed data in a target WAL sidecar.
        with closing(sqlite3.connect(temp_path)) as target:
            target.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            mode = target.execute("PRAGMA journal_mode=DELETE").fetchone()[0]
            if mode.lower() != "delete":
                raise SnapshotError("backup_not_single_file")
        if any(Path(str(temp_path) + suffix).exists()
               for suffix in ("-wal", "-shm")):
            raise SnapshotError("backup_has_sidecar")

        # The destination is a standalone, checkpointed copy. Refuse to
        # publish a corrupt backup, but leave content-level drift for the
        # separate offline audit to diagnose.
        with closing(sqlite3.connect(
            temp_path.as_uri() + "?mode=ro&immutable=1", uri=True
        )) as check:
            if check.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
                raise SnapshotError("backup_integrity_failed")
        with open(temp_path, "rb") as snapshot_file:
            os.fsync(snapshot_file.fileno())
        # link() is atomic and refuses an existing destination. Unlinking the
        # temporary name leaves one private, non-overwritten file.
        os.link(temp_path, destination, follow_symlinks=False)
        published = True
        temp_path.unlink()
        temp_path = None
        dir_fd = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    except FileExistsError:
        raise SnapshotError("destination_already_exists") from None
    except sqlite3.Error:
        raise SnapshotError("sqlite_backup_failed") from None
    except OSError:
        if published:
            raise SnapshotError("destination_published_durability_unverified") from None
        raise SnapshotError("filesystem_operation_failed") from None
    finally:
        if temp_path is not None:
            temp_path.unlink(missing_ok=True)
            for suffix in ("-wal", "-shm", "-journal"):
                Path(str(temp_path) + suffix).unlink(missing_ok=True)
    return published


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--destination", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        create_snapshot(args.source, args.destination)
        result = {"format_version": 1, "created": True,
                  "kind": "consistent_private_sqlite_snapshot"}
    except SnapshotError as error:
        result = {"format_version": 1, "created": False,
                  "kind": "consistent_private_sqlite_snapshot", "error": str(error)}
    print(json.dumps(result, sort_keys=True))
    return 0 if result["created"] else 1


if __name__ == "__main__":
    sys.exit(main())
