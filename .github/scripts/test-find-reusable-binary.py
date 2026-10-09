#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for find-reusable-binary.py, which lets a pull request skip compiling `temps`.

Two ways for it to go wrong, and both merge something untested. It can pick a
binary built from different Rust source than the pull request -- so these
tests pin that a commit is accepted only when nothing but frontend files
differ. Or it can pick a binary nobody vouched for, from a fork or a pull
request run, which would then ship in the image E2E tests -- so they pin the
provenance filter too. Everything else must fall back to building.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("find_reusable_binary", SCRIPTS / "find-reusable-binary.py")
assert spec is not None and spec.loader is not None
finder = importlib.util.module_from_spec(spec)
# dataclasses resolve annotations through sys.modules.
sys.modules[spec.name] = finder
spec.loader.exec_module(finder)

REPO = "example/temps"
ARTIFACT = "temps-binary-musl-fast"


def run(run_id: int, sha: str, **overrides: Any) -> dict[str, Any]:
    value: dict[str, Any] = {
        "id": run_id,
        "event": "push",
        "head_branch": "main",
        "head_sha": sha,
        "path": ".github/workflows/rust-tests.yml",
        "repository": {"full_name": REPO},
        "head_repository": {"full_name": REPO},
        "created_at": f"2026-10-0{run_id % 9 + 1}T00:00:00Z",
    }
    value.update(overrides)
    return value


def artifact(run_id: int, sha: str, **overrides: Any) -> dict[str, Any]:
    value: dict[str, Any] = {
        "name": ARTIFACT,
        "expired": False,
        "workflow_run": {
            "id": run_id,
            "head_sha": sha,
            "head_branch": "main",
            "repository_id": 1,
            "head_repository_id": 1,
        },
    }
    value.update(overrides)
    return value


class FakeApi:
    """Serves the two endpoints the finder calls from in-memory fixtures."""

    def __init__(self, runs: dict[str, list[dict]], artifacts: dict[int, list[dict]]) -> None:
        self.runs = runs
        self.artifacts = artifacts
        self.urls: list[str] = []

    def __call__(self, url: str) -> dict[str, Any]:
        self.urls.append(url)
        if "/workflows/rust-tests.yml/runs?" in url:
            sha = url.split("head_sha=")[1].split("&")[0]
            return {"workflow_runs": self.runs.get(sha, [])}
        if "/artifacts?" in url:
            run_id = int(url.split("/runs/")[1].split("/")[0])
            return {"artifacts": self.artifacts.get(run_id, [])}
        raise AssertionError(f"unexpected URL {url}")


class ProvenanceTests(unittest.TestCase):
    def test_only_main_push_runs_of_this_workflow_and_repository_are_trusted(self) -> None:
        sha = "a" * 40
        trusted = run(1, sha)
        rejected = [
            run(2, sha, event="pull_request"),
            run(3, sha, event="workflow_dispatch"),
            run(4, sha, head_branch="feature"),
            run(5, sha, path=".github/workflows/release.yml"),
            run(6, sha, head_repository={"full_name": "someone/fork"}),
            run(7, sha, repository={"full_name": "someone/fork"}),
            run(8, "b" * 40),
            run(9, sha, id="9"),
        ]
        self.assertEqual(finder.trusted_runs([*rejected, trusted], REPO, sha), [trusted])

    def test_trusted_runs_are_newest_first(self) -> None:
        sha = "a" * 40
        older = run(1, sha, created_at="2026-10-01T00:00:00Z")
        newer = run(2, sha, created_at="2026-10-02T00:00:00Z")
        self.assertEqual(finder.trusted_runs([older, newer], REPO, sha), [newer, older])

    def test_artifact_must_be_unexpired_and_from_that_main_run(self) -> None:
        sha = "a" * 40
        good = artifact(1, sha)
        self.assertIs(finder.usable_artifact([good], ARTIFACT, 1, sha), good)
        fork_run = artifact(1, sha)
        fork_run["workflow_run"]["head_repository_id"] = 2
        no_repository = artifact(1, sha)
        no_repository["workflow_run"]["repository_id"] = None
        no_repository["workflow_run"]["head_repository_id"] = None
        no_expiry = artifact(1, sha)
        del no_expiry["expired"]
        for bad in (
            artifact(1, sha, expired=True),
            artifact(1, sha, name="temps-binary-other"),
            artifact(2, sha),
            artifact(1, "b" * 40),
            fork_run,
            no_repository,
            no_expiry,
        ):
            with self.subTest(artifact=bad):
                self.assertIsNone(finder.usable_artifact([bad], ARTIFACT, 1, sha))


class SearchTests(unittest.TestCase):
    BASE, OLDER, OLDEST = "1" * 40, "2" * 40, "3" * 40
    HEAD = "f" * 40

    def search(self, api: FakeApi, diffs: dict[str, list[str]]):
        logs: list[str] = []
        selection = finder.find_binary(
            repository=REPO,
            head=self.HEAD,
            candidates=[self.BASE, self.OLDER, self.OLDEST],
            artifact_name=ARTIFACT,
            fetch_json=api,
            changed_paths=lambda base, head: diffs[base],
            log=logs.append,
        )
        return selection, logs

    def test_uses_the_base_commit_when_it_has_a_binary(self) -> None:
        api = FakeApi({self.BASE: [run(10, self.BASE)]}, {10: [artifact(10, self.BASE)]})
        selection, _ = self.search(api, {self.BASE: ["web/src/App.tsx"]})
        self.assertEqual(selection, finder.Selection(self.BASE, 10, ARTIFACT))

    def test_walks_back_while_main_only_changed_frontend_files(self) -> None:
        api = FakeApi({self.OLDER: [run(20, self.OLDER)]}, {20: [artifact(20, self.OLDER)]})
        selection, logs = self.search(
            api, {self.OLDER: ["web/src/App.tsx", "apps/temps-cli/src/index.ts", "docs/x.mdx"]}
        )
        self.assertEqual(selection, finder.Selection(self.OLDER, 20, ARTIFACT))
        self.assertIn("no unexpired", logs[0])

    def test_refuses_a_binary_built_from_different_rust_source(self) -> None:
        api = FakeApi(
            {self.OLDER: [run(20, self.OLDER)], self.OLDEST: [run(30, self.OLDEST)]},
            {20: [artifact(20, self.OLDER)], 30: [artifact(30, self.OLDEST)]},
        )
        selection, logs = self.search(
            api, {self.OLDER: ["web/src/App.tsx", "crates/temps-core/src/lib.rs"]}
        )
        self.assertIsNone(selection)
        self.assertIn("crates/temps-core/src/lib.rs differs", logs[-1])
        # It stops there rather than trying an even older binary.
        self.assertFalse(any(self.OLDEST in url for url in api.urls))

    def test_an_empty_diff_is_not_trusted(self) -> None:
        api = FakeApi({self.BASE: [run(10, self.BASE)]}, {10: [artifact(10, self.BASE)]})
        selection, _ = self.search(api, {self.BASE: []})
        self.assertIsNone(selection)

    def test_skips_runs_without_a_usable_artifact(self) -> None:
        api = FakeApi(
            {self.BASE: [run(11, self.BASE, created_at="2026-10-05T00:00:00Z"), run(10, self.BASE)]},
            {11: [artifact(11, self.BASE, expired=True)], 10: [artifact(10, self.BASE)]},
        )
        selection, _ = self.search(api, {self.BASE: ["web/src/App.tsx"]})
        self.assertEqual(selection, finder.Selection(self.BASE, 10, ARTIFACT))

    def test_nothing_found_returns_none(self) -> None:
        selection, logs = self.search(FakeApi({}, {}), {})
        self.assertIsNone(selection)
        self.assertEqual(len(logs), 3)

    def test_malformed_responses_raise(self) -> None:
        with self.assertRaises(ValueError):
            self.search(lambda url: {"message": "Not Found"}, {})  # type: ignore[arg-type]


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


class MainTests(unittest.TestCase):
    """End to end over a real repository: main history plus a PR merge commit."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        git(self.repo, "init", "--quiet", "--initial-branch=main")
        git(self.repo, "config", "user.email", "ci@example.test")
        git(self.repo, "config", "user.name", "CI")
        self.write("crates/lib.rs", "pub fn f() {}\n")
        self.write("web/App.tsx", "export {}\n")
        self.rust_commit = self.commit("rust")
        self.write("web/App.tsx", "export const a = 1\n")
        self.base = self.commit("web on main")
        git(self.repo, "checkout", "--quiet", "-b", "pr")
        self.write("web/App.tsx", "export const a = 2\n")
        self.commit("pr")
        git(self.repo, "checkout", "--quiet", "main")
        git(self.repo, "merge", "--quiet", "--no-ff", "-m", "merge", "pr")
        self.merge = git(self.repo, "rev-parse", "HEAD")
        self.output = self.repo / "github_output"
        self.output.write_text("")
        self.cwd = Path.cwd()
        os.chdir(self.repo)

    def tearDown(self) -> None:
        os.chdir(self.cwd)
        self.tmp.cleanup()

    def write(self, path: str, text: str) -> None:
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, message: str) -> str:
        git(self.repo, "add", "--all")
        git(self.repo, "commit", "--quiet", "-m", message)
        return git(self.repo, "rev-parse", "HEAD")

    def main(self, api, env: dict[str, str] | None = None) -> tuple[int, str]:
        environment = {
            "GITHUB_REPOSITORY": REPO,
            "GH_TOKEN": "token",
            "GITHUB_OUTPUT": str(self.output),
        }
        environment.update(env or {})
        with mock.patch.dict(os.environ, environment), mock.patch.object(
            finder, "github_json", lambda url, token: api(url)
        ), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            status = finder.main(
                ["--base", "HEAD^1", "--head", "HEAD", "--artifact", ARTIFACT]
            )
        return status, self.output.read_text()

    def test_reuses_an_older_main_binary_across_frontend_only_commits(self) -> None:
        api = FakeApi(
            {self.rust_commit: [run(5, self.rust_commit)]},
            {5: [artifact(5, self.rust_commit)]},
        )
        self.assertEqual(
            self.main(api),
            (0, f"sha={self.rust_commit}\nrun-id=5\nartifact-name={ARTIFACT}\n"),
        )

    def test_api_failure_writes_empty_outputs_and_succeeds(self) -> None:
        def broken(url: str) -> dict[str, Any]:
            raise OSError("connection reset")

        self.assertEqual(self.main(broken), (0, "sha=\nrun-id=\nartifact-name=\n"))

    def test_unknown_head_writes_empty_outputs(self) -> None:
        with mock.patch.dict(os.environ, {"GITHUB_REPOSITORY": REPO, "GH_TOKEN": "t", "GITHUB_OUTPUT": str(self.output)}), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            status = finder.main(["--base", "HEAD^1", "--head", "no-such-ref", "--artifact", ARTIFACT])
        self.assertEqual((status, self.output.read_text()), (0, "sha=\nrun-id=\nartifact-name=\n"))

    def test_missing_environment_is_a_usage_error(self) -> None:
        with mock.patch.dict(os.environ, {"GH_TOKEN": ""}), contextlib.redirect_stderr(io.StringIO()):
            status = finder.main(["--base", "HEAD^1", "--head", "HEAD", "--artifact", ARTIFACT])
        self.assertEqual(status, 2)

    def test_rejects_a_malformed_repository(self) -> None:
        status, _ = self.main(FakeApi({}, {}), {"GITHUB_REPOSITORY": "no-owner-separator"})
        self.assertEqual(status, 2)


if __name__ == "__main__":
    unittest.main()
