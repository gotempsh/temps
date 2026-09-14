# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import unittest
import json
import subprocess
from unittest.mock import patch

from release_image_manifest import REPOSITORIES, assemble, record, verify_platforms, verify_registry, promotion_tags


class ReleaseImageManifestTests(unittest.TestCase):
    def records(self):
        return [record(kind, "sha256:" + f"{index:064x}", "a" * 40)
                for index, kind in enumerate(REPOSITORIES)]

    def test_complete_release_uses_exact_digests_not_channel_tags(self):
        manifest = assemble(self.records(), "a" * 40)
        self.assertEqual(len(manifest["images"]), 10)
        for reference in manifest["images"].values():
            self.assertIn("@sha256:", reference)
            self.assertNotIn("-beta", reference)
            self.assertNotIn(":0.3.4", reference)

    def test_missing_image_blocks_release(self):
        for index in range(10):
            entries = self.records()
            del entries[index]
            with self.subTest(index=index), self.assertRaises(ValueError):
                assemble(entries, "a" * 40)

    def test_duplicate_or_other_revision_blocks_release(self):
        with self.assertRaises(ValueError):
            assemble(self.records() * 2, "a" * 40)
        with self.assertRaises(ValueError):
            assemble(self.records(), "b" * 40)

    def test_wrong_repository_and_malformed_digest_rejected(self):
        entries = self.records()
        entries[0]["images"]["daemon_nodejs"] = "example.invalid/image@sha256:" + "a" * 64
        with self.assertRaises(ValueError):
            assemble(entries, "a" * 40)
        for digest in ("latest", "sha256:abc", "sha256:" + "G" * 64, "sha256:" + "a" * 64 + "\n"):
            with self.subTest(digest=digest), self.assertRaises(ValueError):
                record("daemon_nodejs", digest, "a" * 40)

    def test_both_architectures_required_attestations_allowed(self):
        entries = [{"platform": {"os": "linux", "architecture": arch}}
                   for arch in ("amd64", "arm64")]
        verify_platforms({"manifests": entries + [{"platform": {"os": "unknown"}}]})
        for index in (0, 1):
            with self.assertRaises(ValueError):
                verify_platforms({"manifests": [entries[index]]})
        with self.assertRaises(ValueError):
            verify_platforms({})

    def test_promotion_keeps_channels_and_legacy_python_separate(self):
        self.assertEqual(promotion_tags("daemon_python", "stable", "0.3.4", "0.1.0", "0.1.0"),
                         ["ghcr.io/gotempsh/temps-sandbox-python:0.3.4"])
        for kind in REPOSITORIES:
            beta = promotion_tags(kind, "beta", "0.3.4", "0.1.0", "0.1.0")
            self.assertTrue(all(tag.endswith(("-beta", ":beta")) for tag in beta))
            self.assertFalse(any(tag.endswith((":latest", ":stable")) for tag in beta))
        with self.assertRaises(ValueError):
            promotion_tags("daemon_nodejs", "stable", "latest", "0.1.0", "0.1.0")

    def test_registry_checks_exact_digest_and_fails_on_missing_image(self):
        manifest = assemble(self.records(), "a" * 40)
        index = {"manifests": [{"platform": {"os": "linux", "architecture": arch}}
                               for arch in ("amd64", "arm64")]}
        with patch("release_image_manifest.subprocess.run") as run, patch("builtins.print"):
            run.return_value.stdout = json.dumps(index)
            verify_registry(manifest)
            self.assertEqual(run.call_count, 10)
            for call, reference in zip(run.call_args_list, manifest["images"].values()):
                self.assertEqual(call.args[0],
                                 ["docker", "buildx", "imagetools", "inspect", "--raw", reference])
                self.assertTrue(call.kwargs["check"])
            run.side_effect = subprocess.CalledProcessError(1, "docker")
            with self.assertRaises(subprocess.CalledProcessError):
                verify_registry(manifest)

    def test_registry_rejects_single_architecture_image(self):
        manifest = assemble(self.records(), "a" * 40)
        with patch("release_image_manifest.subprocess.run") as run:
            run.return_value.stdout = json.dumps({"manifests": [
                {"platform": {"os": "linux", "architecture": "amd64"}}]})
            with self.assertRaises(ValueError):
                verify_registry(manifest)


if __name__ == "__main__":
    unittest.main()
