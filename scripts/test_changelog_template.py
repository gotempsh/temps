#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for the changelog body template in cliff.toml.

Each test builds a throwaway repository of empty commits and renders it with
git-cliff using the repository's own cliff.toml. git-cliff is taken from the
GIT_CLIFF command (default ``git-cliff``), so the suite also runs through a
wrapper, e.g. ``GIT_CLIFF="bunx git-cliff@2.14.2"``.
"""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GIT_CLIFF = shlex.split(os.environ.get("GIT_CLIFF", "git-cliff"))

GIT_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "Changelog Test",
    "GIT_AUTHOR_EMAIL": "changelog-test@example.com",
    "GIT_COMMITTER_NAME": "Changelog Test",
    "GIT_COMMITTER_EMAIL": "changelog-test@example.com",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}


class ChangelogTemplateTests(unittest.TestCase):
    def setUp(self) -> None:
        if shutil.which(GIT_CLIFF[0]) is None:
            self.fail(f"git-cliff not found: set GIT_CLIFF (tried {GIT_CLIFF[0]!r})")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        shutil.copy(ROOT / "cliff.toml", self.repo / "cliff.toml")
        self.git("init", "-q")
        # A subject cliff.toml skips, so it never appears among the entries.
        self.commit("chore(deps): initial commit")

    def git(self, *args: str) -> None:
        subprocess.run(["git", *args], cwd=self.repo, env=GIT_ENV, check=True)

    def commit(self, subject: str, body: str | None = None) -> None:
        args = ["commit", "-q", "--allow-empty", "-m", subject]
        if body is not None:
            args += ["-m", body]
        self.git(*args)

    def render(self, *args: str) -> list[str]:
        result = subprocess.run(
            [*GIT_CLIFF, "--config", "cliff.toml", "--strip", "all", *args],
            cwd=self.repo,
            env=GIT_ENV,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, f"git-cliff failed in {self.repo}:\n{result.stderr}")
        return result.stdout.splitlines()

    def entries(self, *args: str) -> list[str]:
        return [line for line in self.render("--unreleased", *args) if line.startswith("- ")]

    def test_identical_copies_are_listed_once(self) -> None:
        self.commit("feat(api): add the thing")
        self.commit("feat(api): add the thing")

        self.assertEqual(self.entries().count("- **api:** Add the thing"), 1)

    def test_breaking_copy_after_plain_copy_keeps_marker_and_note(self) -> None:
        self.commit("feat(mcp): expose the tools")
        self.commit("feat(mcp)!: expose the tools", "BREAKING CHANGE: rename the old tool to the new one")

        entries = self.entries()
        self.assertIn("- **mcp:** Expose the tools", entries)
        self.assertIn("- **mcp:** Expose the tools [**BREAKING**]: rename the old tool to the new one", entries)

    def test_breaking_copy_before_plain_copy_lists_both(self) -> None:
        self.commit("feat(mcp)!: expose the tools", "BREAKING CHANGE: rename the old tool to the new one")
        self.commit("feat(mcp): expose the tools")

        entries = self.entries()
        self.assertIn("- **mcp:** Expose the tools [**BREAKING**]: rename the old tool to the new one", entries)
        self.assertIn("- **mcp:** Expose the tools", entries)

    def test_breaking_copies_with_different_notes_are_both_listed(self) -> None:
        self.commit("feat(mcp)!: expose the tools", "BREAKING CHANGE: first note")
        self.commit("feat(mcp)!: expose the tools", "BREAKING CHANGE: second note")

        entries = self.entries()
        self.assertIn("- **mcp:** Expose the tools [**BREAKING**]: first note", entries)
        self.assertIn("- **mcp:** Expose the tools [**BREAKING**]: second note", entries)

    def test_identical_breaking_copies_are_listed_once(self) -> None:
        self.commit("feat(mcp)!: expose the tools", "BREAKING CHANGE: rename the old tool")
        self.commit("feat(mcp)!: expose the tools", "BREAKING CHANGE: rename the old tool")

        self.assertEqual(self.entries(), ["- **mcp:** Expose the tools [**BREAKING**]: rename the old tool"])

    def test_wrapped_breaking_note_stays_on_one_line(self) -> None:
        self.commit(
            "feat(release)!: separate the channels",
            "BREAKING CHANGE: hosts on the beta tag stop receiving\nnightly builds; pull the nightly tag instead.",
        )

        self.assertEqual(
            self.entries(),
            [
                "- **release:** Separate the channels [**BREAKING**]: hosts on the beta tag "
                "stop receiving nightly builds; pull the nightly tag instead."
            ],
        )

    def test_bang_only_breaking_commit_shows_marker_without_note(self) -> None:
        self.commit("feat(api)!: drop the legacy route")

        self.assertEqual(self.entries(), ["- **api:** Drop the legacy route [**BREAKING**]"])

    def test_same_text_in_different_scopes_is_listed_per_scope(self) -> None:
        self.commit("fix(web): handle an empty list")
        self.commit("fix(cli): handle an empty list")
        self.commit("fix: handle an empty list")

        self.assertEqual(
            self.entries(),
            ["- **web:** Handle an empty list", "- **cli:** Handle an empty list", "- Handle an empty list"],
        )

    def test_same_entry_in_two_releases_is_listed_in_each(self) -> None:
        self.commit("fix(api): retry the upload")
        self.git("tag", "v0.0.1")
        self.commit("fix(api): retry the upload")
        self.git("tag", "v0.0.2")

        lines = self.render()
        self.assertEqual(lines.count("- **api:** Retry the upload"), 2)

    def test_package_and_libraries_only_tags_are_not_release_sections(self) -> None:
        self.commit("feat(api): first server release")
        self.git("tag", "v0.1.0-beta.63")
        self.commit("fix(cli): ship the login fix")
        # A libraries-only release and the package tags it creates reserve a
        # number but are not server releases: their commits stay unreleased.
        self.git("tag", "libraries/v0.1.0-beta.64")
        self.git("tag", "sdk/cli/v0.2.0-beta.2")
        self.commit("fix(api): retry the upload")

        lines = self.render()
        headings = [line for line in lines if line.startswith("## ")]
        self.assertEqual(len(headings), 2, headings)
        self.assertIn("## [Unreleased]", headings)
        self.assertTrue(headings[1].startswith("## [0.1.0-beta.63]"), headings)
        unreleased = lines[: lines.index(headings[1])]
        self.assertIn("- **cli:** Ship the login fix", unreleased)
        self.assertIn("- **api:** Retry the upload", unreleased)


if __name__ == "__main__":
    sys.exit(unittest.main())
