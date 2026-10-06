# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise publication failures with a fake cosign, without signing externally."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("sign-release-binaries.sh")
PLATFORMS = ["linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64"]
IDENTITY = "https://github.com/example/temps/.github/workflows/release.yml@refs/tags/v1.0.0"


class BinarySigningTests(unittest.TestCase):
    def run_signer(self, missing=None, fail_command="", identity=IDENTITY):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release = root / "release"
            release.mkdir()
            for platform in PLATFORMS:
                if platform != missing:
                    (release / f"temps-{platform}.tar.gz").write_bytes(b"archive")
            calls = root / "calls"
            cosign = root / "cosign"
            cosign.write_text(
                '#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\n'
                '[ "$1" != "$FAIL_COMMAND" ]\n'
            )
            cosign.chmod(0o700)
            environment = dict(os.environ, PATH=temporary + os.pathsep + os.environ["PATH"],
                               CALLS=str(calls), FAIL_COMMAND=fail_command,
                               CERTIFICATE_IDENTITY=identity)
            result = subprocess.run(["bash", str(SCRIPT), str(release)], env=environment,
                                    capture_output=True, text=True)
            return result.returncode, calls.read_text().splitlines() if calls.exists() else []

    def test_every_platform_signed_and_verified_with_exact_identity(self):
        code, calls = self.run_signer()
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 8)
        for index, platform in enumerate(PLATFORMS):
            archive = f"temps-{platform}.tar.gz"
            self.assertTrue(calls[index * 2].startswith("sign-blob --yes --bundle "))
            self.assertIn(archive + ".sigstore.json", calls[index * 2])
            verification = calls[index * 2 + 1]
            self.assertTrue(verification.startswith("verify-blob --bundle "))
            self.assertIn(f"--certificate-identity {IDENTITY}", verification)
            self.assertIn("--certificate-oidc-issuer https://token.actions.githubusercontent.com", verification)
            self.assertTrue(verification.endswith(archive))

    def test_missing_platform_prevents_any_signing(self):
        for platform in PLATFORMS:
            with self.subTest(platform=platform):
                code, calls = self.run_signer(missing=platform)
                self.assertNotEqual(code, 0)
                self.assertEqual(calls, [])

    def test_signing_and_verification_failures_stop_publication(self):
        for command, count in [("sign-blob", 1), ("verify-blob", 2)]:
            with self.subTest(command=command):
                code, calls = self.run_signer(fail_command=command)
                self.assertNotEqual(code, 0)
                self.assertEqual(len(calls), count)

    def test_missing_identity_prevents_signing(self):
        code, calls = self.run_signer(identity="")
        self.assertNotEqual(code, 0)
        self.assertEqual(calls, [])

    def test_workflow_signs_before_publishing_and_uploads_bundles(self):
        workflow = SCRIPT.parents[1] / "workflows" / "release.yml"
        source = workflow.read_text()
        self.assertLess(source.index("run: bash .github/scripts/sign-release-binaries.sh release"),
                        source.index('gh release upload "$RELEASE_TAG"'))
        self.assertIn("release/temps-*.tar.gz.sigstore.json", source)
        self.assertIn("CERTIFICATE_IDENTITY: https://github.com/${{ github.repository }}/.github/workflows/release.yml@${{ github.ref }}", source)


if __name__ == "__main__":
    unittest.main()
