# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("sign-release-image.sh")


class ImageSigningTests(unittest.TestCase):
    def run_signer(self, channel="beta", digest=None, fail_sign=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = root / "calls"
            docker = root / "docker"
            docker.write_text('#!/bin/sh\nprintf "%s\\n" "$DIGEST"\n')
            cosign = root / "cosign"
            cosign.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\nexit "$SIGN_EXIT"\n')
            docker.chmod(0o700)
            cosign.chmod(0o700)
            env = dict(os.environ, PATH=directory + os.pathsep + os.environ["PATH"],
                       IMAGE_REPOSITORY="ghcr.io/example/temps", IMAGE_VERSION="1.0.0-beta.1",
                       IMAGE_CHANNEL=channel, DIGEST=digest if digest is not None else "sha256:" + "a" * 64,
                       CALLS=str(calls), SIGN_EXIT="1" if fail_sign else "0")
            result = subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True)
            return result.returncode, calls.read_text().splitlines() if calls.exists() else []

    def test_version_and_channel_are_signed_by_digest(self):
        code, calls = self.run_signer()
        self.assertEqual(code, 0)
        self.assertEqual(calls, ["sign --yes ghcr.io/example/temps@sha256:" + "a" * 64] * 2)

    def test_release_without_floating_channel_signs_only_version(self):
        code, calls = self.run_signer(channel="")
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 1)

    def test_bad_digest_prevents_signing(self):
        for digest in ["", "latest", "sha256:abc", "sha256:" + "g" * 64]:
            with self.subTest(digest=digest):
                code, calls = self.run_signer(digest=digest)
                self.assertNotEqual(code, 0)
                self.assertEqual(calls, [])

    def test_sign_failure_fails_the_job(self):
        code, calls = self.run_signer(fail_sign=True)
        self.assertNotEqual(code, 0)
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
