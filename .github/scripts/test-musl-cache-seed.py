#!/usr/bin/env python3
"""Regression tests and workflow contract for musl cache inheritance."""

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
SCRIPT = ROOT / ".github/scripts/musl-cache-seed.py"
WORKFLOW = ROOT / ".github/workflows/rust-tests.yml"
ARCHIVE_SCRIPT = ROOT / ".github/scripts/musl-cache-archive.sh"
SPEC = importlib.util.spec_from_file_location("musl_cache_seed", SCRIPT)
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
    def test_newest_main_seed_wins(self) -> None:
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

    def test_rejects_pr_expired_and_unrelated_artifacts(self) -> None:
        current = MODULE.SEED_NAME
        candidates = MODULE.eligible_artifacts(
            [
                artifact(current, 1, "2026-08-01T00:00:00Z", branch="feature"),
                artifact(current, 2, "2026-08-01T00:00:00Z", expired=True),
                artifact("unrelated", 3, "2026-08-01T00:00:00Z"),
                artifact(
                    f"{MODULE.SEED_NAME}\nbad",
                    4,
                    "2026-08-01T00:00:00Z",
                ),
                {
                    "name": current,
                    "created_at": "2026-08-01T00:00:00Z",
                    "workflow_run": {},
                },
                {
                    **artifact(current, 5, "2026-08-01T00:00:00Z"),
                    "workflow_run": {
                        "id": 5,
                        "head_branch": "main",
                        "repository_id": 100,
                        "head_repository_id": 200,
                    },
                },
            ],
            current,
            {1, 2, 3, 4},
        )
        self.assertEqual(candidates, [])

    def test_refreshes_missing_or_expiring_exact_seed(self) -> None:
        now = datetime(2026, 8, 15, tzinfo=timezone.utc)
        current = MODULE.SEED_NAME
        unrelated = artifact("old-musl-seed", 1, "2026-08-01T00:00:00Z")
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
        self.assertTrue(MODULE.should_publish([unrelated], current, {1}, now))
        self.assertTrue(MODULE.should_publish([expiring], current, {2}, now))
        self.assertFalse(MODULE.should_publish([fresh], current, {3}, now))

    def test_rejects_seed_from_untrusted_workflow_run(self) -> None:
        current = MODULE.SEED_NAME
        candidates = MODULE.eligible_artifacts(
            [artifact(current, 99, "2026-08-01T00:00:00Z")],
            current,
            {100},
        )
        self.assertEqual(candidates, [])

    def test_only_successful_rust_tests_push_runs_are_trusted(self) -> None:
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
        ) as github_json:
            artifacts, trusted = MODULE.github_seed_data(
                "gotempsh/temps", "test-token", current
            )

        self.assertEqual(len(artifacts), 1)
        self.assertEqual(trusted, {10})
        self.assertIn("name=temps-musl-fast-seed-v1", github_json.call_args_list[0].args[0])
        self.assertIn("rust-tests.yml/runs", github_json.call_args_list[1].args[0])

    def test_api_failure_builds_cold_without_publishing(self) -> None:
        current = MODULE.SEED_NAME
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
                self.assertEqual(MODULE.find_seed(current), 0)
            self.assertEqual(
                output_path.read_text(encoding="utf-8").splitlines(),
                ["artifact-name=", "run-id=", "publish-current=false"],
            )


class WorkflowContractTests(unittest.TestCase):
    def test_binary_build_inherits_and_publishes_main_seed(self) -> None:
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
        workflow = json.loads(parsed)
        job = workflow["jobs"]["build-binary"]
        steps = {step["name"]: step for step in job["steps"]}
        total_miss = (
            "steps.musl-cache.outputs.cache-hit == '' && "
            "steps.musl-seed.outputs.run-id != ''"
        )

        self.assertEqual(workflow["permissions"], {"contents": "read"})
        self.assertEqual(
            job["permissions"], {"contents": "read", "actions": "read"}
        )
        self.assertEqual(steps["Find inherited musl cache seed"]["id"], "musl-seed")
        self.assertIn(
            "python3 .github/scripts/musl-cache-seed.py",
            steps["Find inherited musl cache seed"]["run"],
        )
        self.assertEqual(steps["Download inherited musl cache seed"]["if"], total_miss)
        self.assertEqual(steps["Unpack inherited musl cache seed"]["if"], total_miss)
        download = steps["Download inherited musl cache seed"]["with"]
        self.assertEqual(download["path"], "ci-musl-seed")
        self.assertEqual(download["run-id"], "${{ steps.musl-seed.outputs.run-id }}")
        self.assertEqual(download["github-token"], "${{ secrets.GITHUB_TOKEN }}")

        build_command = steps["Build temps binary in toolchain container"]["run"]
        self.assertIn("cargo clean --profile fast --package temps-cli", build_command)
        self.assertIn("git rev-parse --short HEAD", build_command)
        publish_condition = (
            "github.ref == 'refs/heads/main' && "
            "steps.musl-seed.outputs.publish-current == 'true'"
        )
        self.assertEqual(steps["Package inherited musl cache seed"]["if"], publish_condition)
        self.assertEqual(steps["Publish inherited musl cache seed"]["if"], publish_condition)
        self.assertEqual(
            steps["Publish inherited musl cache seed"]["with"]["compression-level"], 0
        )
        self.assertNotIn("actions: write", parsed)

    def test_archive_preserves_hidden_files_and_executable_mode(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "source"
            restored = root / "restored"
            archive_path = root / "seed.tar.zst"
            fingerprint = source / "target/fast/.fingerprint/example"
            executable = source / "target/fast/build/example/build-script-build"
            symlink = source / "target/fast/build/example/build-script-link"
            fingerprint.parent.mkdir(parents=True)
            executable.parent.mkdir(parents=True)
            fingerprint.write_text("fingerprint", encoding="utf-8")
            executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            executable.chmod(0o755)
            symlink.symlink_to("build-script-build")

            subprocess.run(
                ["bash", str(ARCHIVE_SCRIPT), "pack", str(source), str(archive_path)],
                check=True,
            )
            subprocess.run(
                ["bash", str(ARCHIVE_SCRIPT), "unpack", str(archive_path), str(restored)],
                check=True,
            )

            self.assertEqual(
                (restored / "target/fast/.fingerprint/example").read_text(encoding="utf-8"),
                "fingerprint",
            )
            self.assertTrue(
                os.access(
                    restored / "target/fast/build/example/build-script-build", os.X_OK
                )
            )
            self.assertTrue(
                (restored / "target/fast/build/example/build-script-link").is_file()
            )

    def test_archive_rejects_parent_path(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / "unsafe.tar"
            restored = root / "restored"
            with tarfile.open(archive_path, "w") as archive:
                member = tarfile.TarInfo("../escape")
                member.size = 0
                archive.addfile(member)

            result = subprocess.run(
                ["bash", str(ARCHIVE_SCRIPT), "unpack", str(archive_path), str(restored)],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "escape").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
