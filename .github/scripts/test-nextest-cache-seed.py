#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests and workflow contracts for nextest target inheritance."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/nextest-cache-seed.py"
WORKFLOW = ROOT / ".github/workflows/rust-tests.yml"
ARCHIVE_SCRIPT = ROOT / ".github/scripts/nextest-cache-archive.sh"
SPEC = importlib.util.spec_from_file_location("nextest_cache_seed", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def artifact(
    name: str,
    run_id: int,
    created_at: str,
    expires_at: str = "2026-08-30T00:00:00Z",
    branch: str = "main",
    expired: bool = False,
) -> dict:
    return {
        "name": name,
        "created_at": created_at,
        "expires_at": expires_at,
        "expired": expired,
        "workflow_run": {
            "id": run_id,
            "head_branch": branch,
            "repository_id": 100,
            "head_repository_id": 100,
        },
    }


class SeedSelectionTests(unittest.TestCase):
    def test_newest_trusted_main_seed_wins(self) -> None:
        current = MODULE.SEED_NAME
        candidates = MODULE.eligible_artifacts(
            [
                artifact(current, 10, "2026-08-01T00:00:00Z"),
                artifact(current, 11, "2026-08-02T00:00:00Z"),
            ],
            current,
            {10, 11},
        )
        self.assertEqual(candidates[0]["workflow_run"]["id"], 11)

    def test_rejects_pr_expired_unrelated_fork_and_untrusted_artifacts(self) -> None:
        current = MODULE.SEED_NAME
        fork = artifact(current, 5, "2026-08-01T00:00:00Z")
        fork["workflow_run"]["head_repository_id"] = 200
        candidates = MODULE.eligible_artifacts(
            [
                artifact(current, 1, "2026-08-01T00:00:00Z", branch="feature"),
                artifact(current, 2, "2026-08-01T00:00:00Z", expired=True),
                artifact("unrelated", 3, "2026-08-01T00:00:00Z"),
                artifact(current, 4, "2026-08-01T00:00:00Z"),
                fork,
            ],
            current,
            {1, 2, 3, 5},
        )
        self.assertEqual(candidates, [])

    def test_refreshes_missing_or_expiring_seed(self) -> None:
        now = datetime(2026, 8, 15, tzinfo=timezone.utc)
        current = MODULE.SEED_NAME
        expiring = artifact(
            current,
            2,
            "2026-08-14T00:00:00Z",
            expires_at="2026-08-16T00:00:00Z",
        )
        fresh = artifact(
            current,
            3,
            "2026-08-14T00:00:00Z",
            expires_at="2026-08-25T00:00:00Z",
        )
        self.assertTrue(MODULE.should_publish([], current, set(), now))
        self.assertTrue(MODULE.should_publish([expiring], current, {2}, now))
        self.assertFalse(MODULE.should_publish([fresh], current, {3}, now))

    def test_only_successful_rust_tests_main_pushes_are_trusted(self) -> None:
        current = MODULE.SEED_NAME
        artifacts_payload = {"artifacts": [artifact(current, 10, "2026-08-01T00:00:00Z")]}

        def run(run_id: int, path: str, event: str = "push") -> dict:
            return {
                "id": run_id,
                "event": event,
                "head_branch": "main",
                "conclusion": "success",
                "path": path,
                "repository": {"full_name": "gotempsh/temps"},
                "head_repository": {"full_name": "gotempsh/temps"},
            }

        runs_payload = {
            "workflow_runs": [
                run(10, ".github/workflows/rust-tests.yml"),
                run(11, ".github/workflows/unrelated.yml"),
                run(12, ".github/workflows/rust-tests.yml", event="pull_request"),
            ]
        }
        with mock.patch.object(
            MODULE, "github_json", side_effect=[artifacts_payload, runs_payload]
        ):
            artifacts, trusted = MODULE.github_seed_data(
                "gotempsh/temps", "test-token", current
            )
        self.assertEqual(len(artifacts), 1)
        self.assertEqual(trusted, {10})

    def test_api_failure_builds_cold_without_publishing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            output_path = Path(temp_dir) / "outputs"
            environment = {
                "GITHUB_REPOSITORY": "gotempsh/temps",
                "GH_TOKEN": "test-token",
                "GITHUB_OUTPUT": str(output_path),
            }
            with mock.patch.dict(os.environ, environment, clear=False), mock.patch.object(
                MODULE, "github_seed_data", side_effect=OSError("offline")
            ):
                self.assertEqual(MODULE.find_seed(MODULE.SEED_NAME), 0)
            self.assertEqual(
                output_path.read_text(encoding="utf-8").splitlines(),
                ["artifact-name=", "run-id=", "publish-current=false"],
            )


class WorkflowContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        parsed = subprocess.check_output(
            [
                "ruby",
                "-ryaml",
                "-rjson",
                "-e",
                "puts JSON.generate(YAML.safe_load(File.read(ARGV[0]), aliases: true))",
                str(WORKFLOW),
            ],
            text=True,
        )
        cls.raw = WORKFLOW.read_text(encoding="utf-8")
        cls.workflow = json.loads(parsed)
        cls.job = cls.workflow["jobs"]["build-test-binaries"]
        cls.steps = {step["name"]: step for step in cls.job["steps"]}

    def test_build_inherits_and_publishes_trusted_seed(self) -> None:
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertEqual(self.job["permissions"], {"contents": "read", "actions": "read"})
        self.assertEqual(self.steps["Find inherited nextest target seed"]["id"], "nextest-seed")
        download = self.steps["Download inherited nextest target seed"]
        self.assertEqual(download["if"], "steps.nextest-seed.outputs.run-id != ''")
        self.assertTrue(download["continue-on-error"])
        self.assertEqual(download["with"]["run-id"], "${{ steps.nextest-seed.outputs.run-id }}")
        self.assertEqual(download["with"]["github-token"], "${{ secrets.GITHUB_TOKEN }}")
        publish_if = (
            "github.ref == 'refs/heads/main' && "
            "steps.nextest-seed.outputs.publish-current == 'true'"
        )
        self.assertEqual(self.steps["Package nextest target seed"]["if"], publish_if)
        self.assertEqual(self.steps["Publish nextest target seed"]["if"], publish_if)
        self.assertEqual(
            self.steps["Publish nextest target seed"]["with"]["compression-level"], 0
        )
        self.assertNotIn("actions: write", self.raw)

    def test_archives_are_always_rebuilt_with_expected_scopes(self) -> None:
        default = self.steps["Archive default-feature test binaries"]["run"]
        docker = self.steps["Archive docker-tests test binaries"]["run"]
        self.assertIn("cargo nextest archive --workspace --all-targets", default)
        self.assertIn("--package temps-providers --package temps-backup", docker)
        self.assertNotIn("--workspace", docker)
        self.assertIn("temps-providers/docker-tests,temps-backup/docker-tests", docker)
        self.assertIn("cargo clean --profile dev --package temps-cli", self.steps["Invalidate commit-stamped temps-cli artifacts"]["run"])
        self.assertNotIn("if", self.steps["Archive default-feature test binaries"])
        self.assertNotIn("if", self.steps["Archive docker-tests test binaries"])
        self.assertEqual(self.steps["Upload default-feature archive"]["with"]["compression-level"], 0)
        self.assertEqual(self.steps["Upload docker-tests archive"]["with"]["compression-level"], 0)

    def test_dependency_cache_remains_main_save_guarded(self) -> None:
        cache = self.steps["Cache dependencies"]
        self.assertEqual(cache["with"]["save-if"], "${{ github.ref == 'refs/heads/main' }}")
        self.assertNotIn("cache-workspace-crates", cache["with"])

    def test_archive_preserves_hidden_files_executables_and_dereferences_links(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "source"
            restored = root / "restored"
            archive_path = root / "seed.tar.zst"
            fingerprint = source / "target/debug/.fingerprint/example"
            executable = source / "target/debug/build/example/build-script-build"
            symlink = source / "target/debug/build/example/build-script-link"
            fingerprint.parent.mkdir(parents=True)
            executable.parent.mkdir(parents=True)
            fingerprint.write_text("fingerprint", encoding="utf-8")
            executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            executable.chmod(0o755)
            symlink.symlink_to("build-script-build")
            subprocess.run(["bash", str(ARCHIVE_SCRIPT), "pack", str(source), str(archive_path)], check=True)
            subprocess.run(["bash", str(ARCHIVE_SCRIPT), "unpack", str(archive_path), str(restored)], check=True)
            self.assertEqual((restored / "target/debug/.fingerprint/example").read_text(encoding="utf-8"), "fingerprint")
            self.assertTrue(os.access(restored / "target/debug/build/example/build-script-build", os.X_OK))
            self.assertTrue((restored / "target/debug/build/example/build-script-link").is_file())
            self.assertFalse((restored / "target/debug/build/example/build-script-link").is_symlink())

    def test_archive_rejects_parent_and_unexpected_paths(self) -> None:
        for member_name in ("../escape", "cargo-registry/index", "target/release/binary"):
            with self.subTest(member_name=member_name), tempfile.TemporaryDirectory() as temp_dir:
                root = Path(temp_dir)
                archive_path = root / "unsafe.tar"
                with tarfile.open(archive_path, "w") as archive:
                    member = tarfile.TarInfo(member_name)
                    member.size = 0
                    archive.addfile(member)
                result = subprocess.run(
                    ["bash", str(ARCHIVE_SCRIPT), "unpack", str(archive_path), str(root / "restored")],
                    check=False,
                    capture_output=True,
                    text=True,
                )
                self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
