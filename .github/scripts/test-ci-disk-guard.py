#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for ci-disk-guard.py."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent

spec = importlib.util.spec_from_file_location("ci_disk_guard", SCRIPTS / "ci-disk-guard.py")
assert spec is not None and spec.loader is not None
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

GIB = guard.GIB


class GuardCase(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.report = self.root / "report"
        self.stop_file = self.root / "stop"
        self.stopped: list[str] = []

    def watch(self, readings: list[int], inspect: list[Path] | None = None) -> bool:
        remaining = iter(readings)

        def usage(_path: Path) -> tuple[int, int]:
            try:
                return next(remaining), 100 * GIB
            except StopIteration:
                # The restore finished: everything above the limit was read.
                self.stop_file.touch()
                return 100 * GIB, 100 * GIB

        return guard.watch(
            Path("/"),
            6 * GIB,
            "tar",
            self.report,
            self.stop_file,
            inspect or [],
            0,
            usage,
            stop=self.stopped.append,
            sleep=lambda _seconds: None,
        )


class WatchTests(GuardCase):
    def test_trips_below_the_limit_and_stops_the_extractor(self) -> None:
        self.assertTrue(self.watch([50 * GIB, 7 * GIB, 5 * GIB]))
        self.assertEqual(self.stopped, ["tar"])
        self.assertIn("fell to 5.00 GiB of 100.00 GiB", self.report.read_text())

    def test_exactly_at_the_limit_does_not_trip(self) -> None:
        self.assertFalse(self.watch([6 * GIB]))
        self.assertEqual(self.stopped, [])
        self.assertFalse(self.report.exists())

    def test_stops_without_tripping_when_the_restore_finished(self) -> None:
        self.stop_file.touch()
        self.assertFalse(self.watch([0]))
        self.assertEqual(self.stopped, [])

    def test_leaves_no_partial_report(self) -> None:
        self.watch([0])
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["report"])


class ReportTests(GuardCase):
    def test_names_what_filled_the_disk(self) -> None:
        target = self.root / "target"
        (target / "debug" / "deps").mkdir(parents=True)
        (target / "debug" / "deps" / "big").write_bytes(b"x" * 1_000_000)
        (target / "debug" / "small").write_bytes(b"x")

        self.watch([0], inspect=[target])

        report = self.report.read_text()
        self.assertIn(str(target / "debug" / "deps" / "big"), report)
        self.assertIn(f"{target}:", report)

    def test_a_missing_directory_is_reported_not_fatal(self) -> None:
        missing = self.root / "missing"
        self.watch([0], inspect=[missing])
        self.assertIn(f"{missing}: does not exist", self.report.read_text())

    def test_tells_a_sparse_file_from_a_real_one(self) -> None:
        target = self.root / "target"
        target.mkdir()
        with open(target / "sparse", "wb") as handle:
            handle.truncate(10 * GIB)

        directories, largest = guard.scan(target)

        self.assertEqual(directories[str(target)][0], 10 * GIB)
        self.assertLess(directories[str(target)][1], GIB)
        self.assertEqual(largest[0][0], 10 * GIB)

    def test_hard_links_count_once(self) -> None:
        target = self.root / "target"
        target.mkdir()
        (target / "first").write_bytes(b"x" * 4096)
        os.link(target / "first", target / "second")

        directories, largest = guard.scan(target)

        self.assertEqual(directories[str(target)][0], 4096)
        self.assertEqual(len(largest), 1)

    def test_deep_directories_roll_up_to_their_ancestor(self) -> None:
        target = self.root / "target"
        deep = target / "a" / "b" / "c" / "d" / "e"
        deep.mkdir(parents=True)
        (deep / "file").write_bytes(b"x" * 10)

        directories, _largest = guard.scan(target)

        self.assertEqual(list(directories), [str(target / "a" / "b" / "c")])


class FinishTests(GuardCase):
    def finish(self) -> tuple[int, str]:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = guard.finish(self.root, self.report, self.stop_file)
        return code, output.getvalue()

    def test_passes_and_reports_free_space_when_not_tripped(self) -> None:
        code, output = self.finish()
        self.assertEqual(code, 0)
        self.assertIn("after the restore", output)
        self.assertTrue(self.stop_file.exists(), "the watcher must be told to exit")

    def test_fails_with_the_report_when_tripped(self) -> None:
        self.report.write_text("what filled the disk\n")
        code, output = self.finish()
        self.assertEqual(code, 1)
        self.assertIn("what filled the disk", output)
        self.assertIn("::error::", output)


if __name__ == "__main__":
    unittest.main()
