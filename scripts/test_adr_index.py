#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for adr_index.py."""

from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import adr_index  # noqa: E402


def write_adr(root: Path, name: str, body: str) -> None:
    path = root / "docs" / "adr" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(body), encoding="utf-8")


def run(root: Path, *args: str) -> tuple[int, str]:
    stderr = io.StringIO()
    stdout = io.StringIO()
    with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(stdout):
        code = adr_index.main(["--root", str(root), *args])
    return code, stdout.getvalue() + stderr.getvalue()


class AdrIndexTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        # The real renumbering table refers to files that do not exist here.
        patcher = mock.patch.object(adr_index, "RENUMBERED", ())
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(self.tmp.cleanup)
        write_adr(self.root, "001-first.md", "# ADR-001: First\n\n**Status:** Accepted — 2026-01-01\n")
        write_adr(
            self.root,
            "002-second.md",
            """\
            ---
            title: "ADR-002: Second"
            status: Proposed
            ---

            # ADR-002: Second

            **Supersedes in part:** [ADR-001](./001-first.md) §2
            """,
        )

    def test_write_then_check_passes(self) -> None:
        self.assertEqual(run(self.root, "--write")[0], 0)
        code, output = run(self.root, "--check")
        self.assertEqual(code, 0, output)

    def test_missing_index_fails(self) -> None:
        code, output = run(self.root)
        self.assertEqual(code, 1)
        self.assertIn("out of date", output)

    def test_stale_index_fails_after_a_new_adr(self) -> None:
        run(self.root, "--write")
        write_adr(self.root, "003-third.md", "# ADR-003: Third\n\n## Status\n\nProposed\n")
        code, output = run(self.root, "--check")
        self.assertEqual(code, 1)
        self.assertIn("--write", output)

    def test_duplicate_number_fails_and_names_both_files(self) -> None:
        write_adr(self.root, "002-another.md", "# ADR-002: Another\n")
        code, output = run(self.root, "--write")
        self.assertEqual(code, 1)
        self.assertIn("duplicate ADR number 002", output)
        self.assertIn("002-another.md", output)
        self.assertIn("002-second.md", output)

    def test_heading_number_must_match_file_name(self) -> None:
        write_adr(self.root, "003-third.md", "# ADR-004: Third\n")
        code, output = run(self.root, "--check")
        self.assertEqual(code, 1)
        self.assertIn("heading says ADR 004", output)

    def test_file_without_adr_heading_fails(self) -> None:
        write_adr(self.root, "003-notes.md", "# Implementation notes\n")
        code, output = run(self.root, "--check")
        self.assertEqual(code, 1)
        self.assertIn("first heading must be", output)

    def test_bad_file_name_fails(self) -> None:
        write_adr(self.root, "3-Third.md", "# ADR-003: Third\n")
        code, output = run(self.root, "--check")
        self.assertEqual(code, 1)
        self.assertIn("NNN-lowercase-slug.md", output)

    def test_supporting_document_must_belong_to_an_adr(self) -> None:
        write_adr(self.root, "supporting/009-plan.md", "# A plan\n")
        code, output = run(self.root, "--check")
        self.assertEqual(code, 1)
        self.assertIn("ADR 009, which does not exist", output)

    def test_supporting_document_is_listed_under_its_adr(self) -> None:
        write_adr(self.root, "supporting/001-plan.md", "# The plan\n")
        self.assertEqual(run(self.root, "--write")[0], 0)
        index = (self.root / "docs/adr/README.md").read_text(encoding="utf-8")
        self.assertIn("| [001](./001-first.md) | [The plan](./supporting/001-plan.md) |", index)

    def test_relations_are_shown_in_both_directions(self) -> None:
        run(self.root, "--write")
        index = (self.root / "docs/adr/README.md").read_text(encoding="utf-8")
        self.assertIn("| Accepted | superseded in part by [002](./002-second.md) |", index)
        self.assertIn("| Proposed | supersedes in part [001](./001-first.md) |", index)

    def test_renumbered_entry_must_point_at_an_existing_adr(self) -> None:
        with mock.patch.object(adr_index, "RENUMBERED", (("001", "007"),)):
            code, output = run(self.root, "--check")
        self.assertEqual(code, 1)
        self.assertIn("no ADR 007 exists", output)

    def test_next_prints_the_number_after_the_highest(self) -> None:
        code, output = run(self.root, "--next")
        self.assertEqual(code, 0)
        self.assertEqual(output.strip(), "003")


class ParsingTests(unittest.TestCase):
    def test_short_status_keeps_the_leading_clause(self) -> None:
        self.assertEqual(adr_index.short_status("Accepted — 2026-10-20"), "Accepted")
        self.assertEqual(adr_index.short_status("Proposed (needs sign-off)"), "Proposed")
        self.assertEqual(
            adr_index.short_status("Implemented for process/HTTP/TCP; runtime exec remains gated"),
            "Implemented for process/HTTP/TCP",
        )

    def test_relation_kinds(self) -> None:
        self.assertEqual(adr_index.relation_kind("Supersedes:"), "supersedes")
        self.assertEqual(adr_index.relation_kind("Supersedes the query design of:"), "supersedes in part")
        self.assertEqual(adr_index.relation_kind("Superseded by:"), "superseded by")
        self.assertEqual(adr_index.relation_kind("Superseded in part by:"), "superseded in part by")
        self.assertEqual(adr_index.relation_kind("Amends:"), "amends")

    def test_the_repository_index_is_current(self) -> None:
        code, output = run(Path(__file__).resolve().parent.parent, "--check")
        self.assertEqual(code, 0, output)


if __name__ == "__main__":
    unittest.main()
