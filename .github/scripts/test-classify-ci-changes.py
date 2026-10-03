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
import json
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


class WorkflowWiringTests(unittest.TestCase):
    """rust-tests.yml must still report every required check on a docs-only PR.

    A job skipped through `if:` reports success, so skipping is safe for an
    ordinary required job. A matrix job skipped that way is never expanded:
    `Unit Tests (unit-a)` would never report and branch protection would block
    the pull request forever. These tests work out which jobs run when
    `code == 'false'` and fail when a required check would go missing, or when
    a gated job would be skipped because the classifier itself failed.
    """

    WORKFLOW = REPO_ROOT / ".github" / "workflows" / "rust-tests.yml"
    GATE = "needs.changes.outputs.code != 'false'"
    DOCS_ONLY = "needs.changes.outputs.code == 'false'"
    # The required status checks on `main` that this workflow produces. Keep in
    # step with branch protection; adding a name here only makes the test stricter.
    REQUIRED_CHECKS = {
        "Cargo Check",
        "Formatting",
        "Web TypeScript Check",
        "OpenAPI Spec Format",
        "Unit Tests (unit-a)",
        "Unit Tests (unit-b)",
        "Unit Tests (unit-integration)",
    }
    # Matrix jobs that may be skipped outright on a docs-only PR. None of their
    # checks is required. A new matrix job has to be added here or made to run
    # with its steps skipped, the way unit-tests does.
    SKIPPABLE_MATRIX_JOBS = {"integration-tests"}
    MATRIX_REFERENCE = re.compile(r"\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}")

    @classmethod
    def setUpClass(cls) -> None:
        # Ruby's YAML ships on every runner and already parses workflows in
        # test-nightly-release-workflows.sh; Python has no YAML in the stdlib.
        dumped = subprocess.run(
            [
                "ruby",
                "-ryaml",
                "-rjson",
                "-e",
                "puts JSON.generate(YAML.safe_load(File.read(ARGV[0]), aliases: true)['jobs'])",
                str(cls.WORKFLOW),
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        cls.jobs = json.loads(dumped)

    @staticmethod
    def needs(job: dict) -> list[str]:
        value = job.get("needs", [])
        return [value] if isinstance(value, str) else list(value)

    @staticmethod
    def is_matrix(job: dict) -> bool:
        return "matrix" in job.get("strategy", {})

    def docs_only_outcome(self) -> dict[str, str]:
        """Return "runs" or "skipped" for every job when `code == 'false'`."""
        outcome: dict[str, str] = {}

        def resolve(job_id: str) -> str:
            if job_id in outcome:
                return outcome[job_id]
            job = self.jobs[job_id]
            condition = str(job.get("if", ""))
            deps = [resolve(dep) for dep in self.needs(job)]
            if self.GATE in condition:
                result = "skipped"
            elif self.DOCS_ONLY in condition:
                result = "runs"
            elif condition:
                self.fail(f"job {job_id} has a condition this test cannot evaluate: {condition}")
            else:
                # No condition means the implicit success(): any skipped
                # dependency skips the job too.
                result = "skipped" if "skipped" in deps else "runs"
            outcome[job_id] = result
            return result

        for job_id in self.jobs:
            resolve(job_id)
        return outcome

    def check_names(self, job_id: str) -> list[str]:
        job = self.jobs[job_id]
        name = str(job.get("name", job_id))
        include = job.get("strategy", {}).get("matrix", {}).get("include")
        if not include:
            return [name]
        return [
            self.MATRIX_REFERENCE.sub(lambda m, entry=entry: str(entry[m.group(1)]), name)
            for entry in include
        ]

    def test_changes_job_exports_code(self) -> None:
        changes = self.jobs["changes"]
        self.assertEqual(changes["outputs"]["code"], "${{ steps.classify.outputs.code }}")
        self.assertNotIn("needs", changes)

    def test_gated_jobs_still_run_when_the_classifier_fails(self) -> None:
        # A failed `changes` job leaves `code` empty. Only `!= 'false'` under
        # `!cancelled()` turns that into "run"; `== 'true'` or the implicit
        # success() would skip the job and report it green.
        for job_id, job in self.jobs.items():
            condition = str(job.get("if", ""))
            if "needs.changes" not in condition:
                continue
            with self.subTest(job=job_id):
                self.assertIn("changes", self.needs(job))
                self.assertIn("!cancelled()", condition)
                self.assertNotIn("code == 'true'", condition)

    def test_required_checks_report_on_a_docs_only_pr(self) -> None:
        outcome = self.docs_only_outcome()
        produced = {check: job_id for job_id in self.jobs for check in self.check_names(job_id)}
        for check in sorted(self.REQUIRED_CHECKS):
            with self.subTest(check=check):
                self.assertIn(check, produced, f"no job in rust-tests.yml produces {check!r}")
                job_id = produced[check]
                if self.is_matrix(self.jobs[job_id]):
                    self.assertEqual(
                        outcome[job_id],
                        "runs",
                        f"{job_id} is a matrix job; skipping it on a docs-only PR means "
                        f"{check!r} never reports",
                    )

    def test_matrix_jobs_skipped_on_a_docs_only_pr_are_not_required(self) -> None:
        outcome = self.docs_only_outcome()
        skipped = {
            job_id
            for job_id, job in self.jobs.items()
            if self.is_matrix(job) and outcome[job_id] == "skipped"
        }
        self.assertEqual(skipped, self.SKIPPABLE_MATRIX_JOBS)

    def test_matrix_jobs_kept_on_a_docs_only_pr_skip_every_step(self) -> None:
        # Such a job runs although the builds it consumes were skipped, so any
        # ungated step would try to download an artifact that does not exist.
        for job_id, job in self.jobs.items():
            if not self.is_matrix(job) or self.DOCS_ONLY not in str(job.get("if", "")):
                continue
            with self.subTest(job=job_id):
                conditions = [str(step.get("if", "")) for step in job["steps"]]
                self.assertIn(self.DOCS_ONLY, conditions)
                ungated = [
                    step.get("name", "<unnamed>")
                    for step, condition in zip(job["steps"], conditions)
                    if condition not in (self.GATE, self.DOCS_ONLY)
                ]
                self.assertEqual(ungated, [])


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
