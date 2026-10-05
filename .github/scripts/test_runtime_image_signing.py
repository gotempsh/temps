# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Published runtime digests must fail closed before keyless signing."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ["release.yml", "daemon-images.yml", "sandbox-images-beta.yml", "mariadb-walg-image.yml"]

class RuntimeImageSigningTests(unittest.TestCase):
    def test_published_digest_steps_reject_invalid_inputs_and_sign_immutable_refs(self):
        count = 0
        for workflow in WORKFLOWS:
            source = (ROOT / ".github/workflows" / workflow).read_text()
            steps = re.findall(r"      - name: Sign published runtime image digest\n(.*?)(?=\n      - name:|\n  [a-z]|\Z)", source, re.S)
            for step in steps:
                count += 1
                self.assertIn("        if:", step, workflow)
                self.assertIn("IMAGE_DIGEST: ${{ steps.", step, workflow)
                script = step.split("        run: |\n", 1)[1]
                script = "\n".join(line[10:] for line in script.splitlines() if line.startswith("          "))
                with tempfile.TemporaryDirectory() as directory:
                    calls = Path(directory) / "calls"
                    cosign = Path(directory) / "cosign"
                    cosign.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\nexit "$SIGN_EXIT"\n')
                    cosign.chmod(0o700)
                    env = dict(os.environ, PATH=directory + os.pathsep + os.environ["PATH"], CALLS=str(calls), IMAGE_REPOSITORY="ghcr.io/example/runtime", SIGN_EXIT="0")
                    for digest in ["", "sha256:bad", "tag:latest", "sha256:" + "a" * 64]:
                        env["IMAGE_DIGEST"] = digest
                        result = subprocess.run(["bash", "-euc", script], env=env, capture_output=True, text=True)
                        if len(digest) != 71:
                            self.assertNotEqual(result.returncode, 0, workflow)
                            self.assertFalse(calls.exists(), workflow)
                        else:
                            self.assertEqual(result.returncode, 0, result.stderr)
                            self.assertEqual(calls.read_text().strip(), "sign --yes ghcr.io/example/runtime@" + digest)
                    env["SIGN_EXIT"] = "1"
                    result = subprocess.run(["bash", "-euc", script], env=env, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, workflow)
        self.assertEqual(count, 6)

if __name__ == "__main__":
    unittest.main()
