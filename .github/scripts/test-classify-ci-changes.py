#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for classify-ci-changes.py, the docs-only skip for rust-tests.yml.

A classifier that calls code "documentation" lets an untested change merge
with every required check green, so most of these tests pin the fail-closed
cases. The last group is a contract over the repository itself: a script under
`scripts/` or `.github/` that reads a page under `docs/` makes that page test
input, and it must then be listed in TEST_INPUT_DOCS so editing it still runs
the jobs that read it.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent.parent
CLASSIFIER = SCRIPTS / "classify-ci-changes.py"

spec = importlib.util.spec_from_file_location("classify_ci_changes", CLASSIFIER)
assert spec is not None and spec.loader is not None
classify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(classify)


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


class ClassificationTests(unittest.TestCase):
    def test_docs_and_root_markdown_are_documentation(self) -> None:
        self.assertIsNone(
            classify.first_code_path(
                ["docs/concepts/page.mdx", "docs/images/diagram.png", "README.md", "CHANGELOG.md"]
            )
        )

    def test_any_non_documentation_file_requires_full_ci(self) -> None:
        self.assertEqual(
            classify.first_code_path(["docs/concepts/page.mdx", "crates/temps-core/src/lib.rs"]),
            "crates/temps-core/src/lib.rs",
        )

    def test_nested_markdown_is_not_documentation(self) -> None:
        # Embedded into the binary via include_dir!, and fixtures carry READMEs.
        for path in (
            "crates/temps-core/templates/README.md",
            "crates/temps-deployments/tests/fixtures/simple-nodejs/README.md",
            "skills/temps/SKILL.md",
        ):
            with self.subTest(path=path):
                self.assertEqual(classify.first_code_path([path]), path)

    def test_pages_read_by_ci_require_full_ci(self) -> None:
        for path in sorted(classify.TEST_INPUT_DOCS):
            with self.subTest(path=path):
                self.assertEqual(classify.first_code_path([path]), path)

    def test_paths_that_only_resemble_docs_require_full_ci(self) -> None:
        for path in ("docs", "docs-site/index.md", "web/docs/design/README.md", "README.md/x"):
            with self.subTest(path=path):
                self.assertEqual(classify.first_code_path([path]), path)

    def test_empty_change_set_requires_full_ci(self) -> None:
        self.assertEqual(classify.first_code_path([]), "<empty diff>")
        self.assertEqual(classify.first_code_path([""]), "<empty diff>")


class GitDiffTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        git(self.repo, "init", "--quiet", "--initial-branch=main")
        git(self.repo, "config", "user.email", "ci@example.test")
        git(self.repo, "config", "user.name", "CI")
        (self.repo / "crates").mkdir()
        (self.repo / "docs").mkdir()
        (self.repo / "crates" / "lib.rs").write_text("pub fn f() {}\n" * 20)
        (self.repo / "docs" / "page.mdx").write_text("# Page\n")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "--quiet", "-m", "base")
        self.base = git(self.repo, "rev-parse", "HEAD").strip()
        self.cwd = Path.cwd()
        os.chdir(self.repo)

    def tearDown(self) -> None:
        os.chdir(self.cwd)
        self.tmp.cleanup()

    def commit(self, message: str) -> str:
        git(self.repo, "add", "--all")
        git(self.repo, "commit", "--quiet", "-m", message)
        return git(self.repo, "rev-parse", "HEAD").strip()

    def run_main(self, base: str, head: str) -> tuple[int, str]:
        output = self.repo / "github_output"
        output.write_text("")
        previous = os.environ.get("GITHUB_OUTPUT")
        os.environ["GITHUB_OUTPUT"] = str(output)
        try:
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                status = classify.main(["--base", base, "--head", head])
        finally:
            if previous is None:
                del os.environ["GITHUB_OUTPUT"]
            else:
                os.environ["GITHUB_OUTPUT"] = previous
        return status, output.read_text()

    def test_moving_code_into_docs_still_reports_the_source_file(self) -> None:
        git(self.repo, "mv", "crates/lib.rs", "docs/lib.md")
        head = self.commit("move")
        paths = classify.changed_paths(self.base, head)
        self.assertIn("crates/lib.rs", paths)
        self.assertEqual(classify.first_code_path(paths), "crates/lib.rs")

    def test_docs_only_commit_writes_code_false(self) -> None:
        (self.repo / "docs" / "page.mdx").write_text("# Page\n\nMore.\n")
        head = self.commit("docs")
        self.assertEqual(self.run_main(self.base, head), (0, "code=false\n"))

    def test_code_commit_writes_code_true(self) -> None:
        (self.repo / "crates" / "lib.rs").write_text("pub fn g() {}\n")
        head = self.commit("code")
        self.assertEqual(self.run_main(self.base, head), (0, "code=true\n"))

    def test_unreadable_diff_fails_and_leaves_output_unset(self) -> None:
        # rust-tests.yml runs every job unless `code` is exactly "false".
        self.assertEqual(self.run_main(self.base, "no-such-revision"), (1, ""))


class RepositoryContractTests(unittest.TestCase):
    """Pages under docs/ that CI reads must be listed in TEST_INPUT_DOCS."""

    EXCLUDED = {
        ".github/scripts/classify-ci-changes.py",
        ".github/scripts/test-classify-ci-changes.py",
    }
    # A docs/ path not preceded by anything that would make it part of a longer
    # name. URLs are removed first, so temps.sh/docs/... links do not count.
    DOCS_REFERENCE = re.compile(r"(?<![A-Za-z0-9_.-])docs/[A-Za-z0-9_./-]*[A-Za-z0-9_]")
    URL = re.compile(r"https?://\S+")

    def tracked(self) -> list[str]:
        return [p for p in git(REPO_ROOT, "ls-files", "-z").split("\0") if p]

    def docs_read_by_ci(self) -> dict[str, set[str]]:
        tracked = self.tracked()
        docs_files = {p for p in tracked if p.startswith("docs/")}
        readers: dict[str, set[str]] = {}
        for path in tracked:
            if not path.startswith(("scripts/", ".github/")) or path in self.EXCLUDED:
                continue
            try:
                text = (REPO_ROOT / path).read_text(encoding="utf-8")
            except (UnicodeDecodeError, FileNotFoundError):
                continue
            for match in self.DOCS_REFERENCE.finditer(self.URL.sub(" ", text)):
                reference = match.group(0)
                prefix = reference.rstrip("/") + "/"
                for doc in docs_files:
                    if doc == reference or doc.startswith(prefix):
                        readers.setdefault(doc, set()).add(path)
        return readers

    def test_docs_read_by_ci_scripts_are_test_inputs(self) -> None:
        unlisted = {
            doc: sorted(readers)
            for doc, readers in self.docs_read_by_ci().items()
            if classify.is_documentation(doc)
        }
        self.assertEqual(
            unlisted,
            {},
            "these docs pages are read by CI scripts but would let a pull request "
            "skip CI; add them to TEST_INPUT_DOCS in classify-ci-changes.py",
        )

    def test_test_input_docs_exist(self) -> None:
        tracked = set(self.tracked())
        self.assertEqual(sorted(classify.TEST_INPUT_DOCS - tracked), [])


if __name__ == "__main__":
    unittest.main()
