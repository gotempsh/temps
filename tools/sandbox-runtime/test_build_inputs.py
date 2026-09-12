# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Offline regression checks for the public SDK pin and local image build inputs."""

import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
import unittest

from image_metadata import image_version, metadata


PACKAGE = Path(__file__).resolve().parent
ROOT = PACKAGE.parent.parent


def read_toml(path):
    with path.open("rb") as source:
        return tomllib.load(source)


class BuildInputsTests(unittest.TestCase):
    def publication_environment(self, **overrides):
        return {"FLAVOR": "python", "CHANNEL": "beta", "DRY_RUN": "false", "GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "GITHUB_REPOSITORY": "gotempsh/temps", "GITHUB_SHA": "a" * 40, **overrides}

    def test_image_version_comes_from_runtime_selection(self):
        source = (ROOT / "crates/temps-sandbox/src/services/sandbox_service.rs").read_text()
        version = image_version(source)
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")
        with self.assertRaises(ValueError):
            image_version(source.replace(f"temps-sandbox-all:{version}", "temps-sandbox-all:99.0.0"))
        with self.assertRaises(ValueError):
            image_version("")

    def test_beta_cannot_overwrite_stable_or_legacy_tags(self):
        result = metadata(self.publication_environment(), "0.3.2")
        self.assertEqual(result["publish"], "true")
        self.assertEqual(result["tags"].split(","), [f"ghcr.io/gotempsh/temps-sandbox-python:daemon-{'a' * 40}", "ghcr.io/gotempsh/temps-sandbox-python:0.3.2-beta"])

    def test_stable_requires_stable_release_ref(self):
        result = metadata(self.publication_environment(CHANNEL="stable", GITHUB_REF="refs/tags/v1.2.3"), "0.3.2")
        self.assertIn("temps-sandbox-python:0.3.2", result["tags"].split(",")[1])
        for ref in ("refs/heads/main", "refs/tags/v1.2.3-beta.1"):
            with self.assertRaises(ValueError):
                metadata(self.publication_environment(CHANNEL="stable", GITHUB_REF=ref), "0.3.2")

    def test_untrusted_refs_and_repositories_cannot_publish(self):
        for overrides in ({"GITHUB_REF": "refs/heads/feature"}, {"GITHUB_REPOSITORY": "someone/fork"}, {"DRY_RUN": ""}, {"CHANNEL": "latest"}, {"FLAVOR": "node"}):
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                metadata(self.publication_environment(**overrides), "0.3.2")

    def test_pr_and_dry_run_never_publish(self):
        for overrides in ({"GITHUB_EVENT_NAME": "pull_request", "GITHUB_REF": "refs/pull/1/merge"}, {"DRY_RUN": "true", "GITHUB_REF": "refs/heads/feature"}):
            self.assertEqual(metadata(self.publication_environment(**overrides), "0.3.2")["publish"], "false")

    def test_release_and_beta_use_shared_daemon_workflow(self):
        for name in ("release.yml", "sandbox-images-beta.yml"):
            workflow = (ROOT / ".github/workflows" / name).read_text()
            self.assertIn("uses: ./.github/workflows/daemon-images.yml", workflow)
            self.assertIn("dry_run: ${{ github.event_name == 'workflow_dispatch' && inputs.dry_run == true }}", workflow)
        workflow = (ROOT / ".github/workflows/daemon-images.yml").read_text()
        self.assertIn("flavor: [nodejs, python, all]", workflow)
        self.assertIn("platforms: linux/amd64,linux/arm64", workflow)
        self.assertIn("context: tools/sandbox-runtime", workflow)
        self.assertIn("push: ${{ steps.metadata.outputs.publish == 'true' }}", workflow)
        self.assertNotIn("pull_request_target", workflow)
        self.assertNotIn("  pull_request:", workflow)
        checks = (ROOT / ".github/workflows/daemon-images-check.yml").read_text()
        self.assertNotIn("packages: write", checks)
        self.assertIn("push: false", checks)
        self.assertIn("persist-credentials: false", checks)

    def test_bridge_and_daemon_pin_the_same_public_commit(self):
        bridge = read_toml(ROOT / "crates/temps-ai-agent-cli/Cargo.toml")["dependencies"]["temps-agent-runtime"]
        daemon = read_toml(PACKAGE / "Cargo.toml")["dependencies"]["temps-agent-runtime"]
        self.assertEqual(bridge["git"], "https://github.com/gotempsh/agent-runtime-sdk.git")
        self.assertEqual(daemon["git"], bridge["git"])
        self.assertTrue(re.fullmatch(r"[0-9a-f]{40}", bridge["rev"]))
        self.assertEqual(daemon["rev"], bridge["rev"])
        for dependency in (bridge, daemon):
            self.assertNotIn("path", dependency)
            self.assertNotIn("branch", dependency)
        self.assertFalse(daemon["default-features"])
        self.assertEqual(daemon["features"], ["claude", "codex", "opencode"])

    def test_both_lockfiles_resolve_the_declared_commit(self):
        dependency = read_toml(PACKAGE / "Cargo.toml")["dependencies"]["temps-agent-runtime"]
        expected = f"git+{dependency['git']}?rev={dependency['rev']}#{dependency['rev']}"
        for lockfile in (ROOT / "Cargo.lock", PACKAGE / "Cargo.lock"):
            with self.subTest(lockfile=str(lockfile)):
                packages = [package for package in read_toml(lockfile)["package"] if package["name"] == "temps-agent-runtime"]
                self.assertEqual(len(packages), 1)
                self.assertEqual(packages[0]["source"], expected)

    def test_docker_build_needs_no_external_source_context(self):
        dockerfile = (PACKAGE / "Dockerfile").read_text()
        self.assertNotIn("runtime-src", dockerfile)
        self.assertIn("RUN cargo build --locked --release", dockerfile)
        self.assertIn("COPY Cargo.toml Cargo.lock", dockerfile)

    def invoke_builder(self, arguments):
        with tempfile.TemporaryDirectory(prefix="temps-build-inputs-") as directory:
            temporary = Path(directory)
            docker = temporary / "docker"
            output = temporary / "arguments"
            docker.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$BUILD_ARGS_FILE"\n')
            docker.chmod(0o700)
            environment = {**os.environ, "PATH": f"{temporary}{os.pathsep}{os.environ.get('PATH', '')}", "BUILD_ARGS_FILE": str(output)}
            result = subprocess.run(["bash", str(PACKAGE / "build-local.sh"), *arguments], env=environment, capture_output=True, text=True, check=False)
            return result, output.read_text().splitlines() if output.exists() else None

    def test_builds_each_flavor_with_only_the_package_context(self):
        for flavor in ("nodejs", "python", "all"):
            with self.subTest(flavor=flavor):
                result, arguments = self.invoke_builder(["temps-test:pin-check", flavor])
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(arguments, ["build", "--target", flavor, "-t", "temps-test:pin-check", str(PACKAGE)])

    def test_default_flavor_is_nodejs(self):
        result, arguments = self.invoke_builder(["temps-test:pin-check"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(arguments[2], "nodejs")

    def test_invalid_arguments_fail_before_calling_docker(self):
        for arguments in ([], ["temps-test:pin-check", "invalid"], ["/sdk", "temps-test:pin-check", "nodejs"]):
            with self.subTest(arguments=arguments):
                result, docker_arguments = self.invoke_builder(arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertIsNone(docker_arguments)
                self.assertTrue(result.stderr.strip())


if __name__ == "__main__":
    unittest.main()
