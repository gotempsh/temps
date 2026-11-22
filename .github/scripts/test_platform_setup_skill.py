#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for the temps-platform-setup installer verification.

The skill verifies `deploy.sh` against the `deploy.sh.sha256` file temps.sh
publishes beside it. These tests run the exact Bash block from SKILL.md with
`curl` replaced by a stub that serves local fixtures, and guard against a
hardcoded installer digest coming back: a pinned value goes stale on the next
installer change and makes the skill refuse every install.
"""

from __future__ import annotations

import hashlib
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path


SKILL_DIR = Path(__file__).resolve().parents[2] / "skills" / "temps-platform-setup"
SKILL_MD = SKILL_DIR / "SKILL.md"
CHECKSUM_URL = "https://temps.sh/deploy.sh.sha256"
BASH_BLOCK = re.compile(r"```bash\n(.*?)```", re.DOTALL)
SHA256_LITERAL = re.compile(r"(?<![0-9A-Fa-f])[0-9A-Fa-f]{64}(?![0-9A-Fa-f])")
INSTALLER = b"#!/usr/bin/env bash\necho 'installer fixture'\n"

# Serves https://temps.sh/<name> from $TEMPS_FIXTURES/<name>, logs each URL,
# and fails like `curl --fail` when a fixture is absent. It also rejects any
# download that drops the strict transport flags the skill requires.
CURL_STUB = r"""
curl() {
  local args=" $* " url='' out=''
  for flag in --fail --show-error "--proto =https" --tlsv1.2 "--proto-redir =https"; do
    [[ $args == *" $flag "* ]] || { echo "curl stub: missing $flag" >&2; return 2; }
  done
  while (($#)); do
    case $1 in
      --output) out=$2; shift 2 ;;
      https://*) url=$1; shift ;;
      *) shift ;;
    esac
  done
  printf '%s\n' "$url" >> "$TEMPS_CURL_LOG"
  [[ $url == https://temps.sh/* && -n $out ]] || return 2
  [[ -f $TEMPS_FIXTURES/${url#https://temps.sh/} ]] || return 22
  cp "$TEMPS_FIXTURES/${url#https://temps.sh/}" "$out"
}
"""


def provisioning_block() -> str:
    """The SKILL.md Bash block that downloads and verifies the installer."""
    blocks = [
        block
        for block in BASH_BLOCK.findall(SKILL_MD.read_text(encoding="utf-8"))
        if CHECKSUM_URL in block
    ]
    if len(blocks) != 1:
        raise AssertionError(
            f"expected exactly one bash block in {SKILL_MD} downloading "
            f"{CHECKSUM_URL}, found {len(blocks)}"
        )
    return blocks[0]


def checksum_line(content: bytes) -> bytes:
    return hashlib.sha256(content).hexdigest().encode() + b"  deploy.sh\n"


class InstallerVerificationTests(unittest.TestCase):
    def run_block(
        self, checksum: bytes | None, installer: bytes = INSTALLER
    ) -> tuple[subprocess.CompletedProcess[str], list[str]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            scratch = root / "tmp"
            fixtures.mkdir()
            scratch.mkdir()
            (fixtures / "deploy.sh").write_bytes(installer)
            if checksum is not None:
                (fixtures / "deploy.sh.sha256").write_bytes(checksum)
            curl_log = root / "curl.log"
            curl_log.touch()

            script = (
                CURL_STUB
                + provisioning_block()
                + 'printf "%s" "$expected_deploy_sha256"\n'
            )
            env = {
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "TMPDIR": str(scratch),
                "TEMPS_FIXTURES": str(fixtures),
                "TEMPS_CURL_LOG": str(curl_log),
            }
            result = subprocess.run(
                ["bash", "--noprofile", "--norc", "-c", script],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            return result, curl_log.read_text(encoding="utf-8").split()

    def assert_refused(self, checksum: bytes | None, message: str) -> list[str]:
        result, downloads = self.run_block(checksum)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(f"Refusing installer: {message}", result.stderr)
        self.assertEqual(result.stdout, "")
        return downloads

    def test_accepts_installer_matching_published_checksum(self) -> None:
        result, downloads = self.run_block(checksum_line(INSTALLER))

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, hashlib.sha256(INSTALLER).hexdigest())
        self.assertEqual(downloads, [CHECKSUM_URL, "https://temps.sh/deploy.sh"])

    def test_refuses_installer_that_differs_from_published_checksum(self) -> None:
        self.assert_refused(
            checksum_line(b"#!/usr/bin/env bash\necho 'another installer'\n"),
            "published checksum mismatch",
        )

    def test_refuses_malformed_checksum_before_downloading_installer(self) -> None:
        digest = hashlib.sha256(INSTALLER).hexdigest().encode()
        malformed = {
            "empty": b"",
            "digest only": digest + b"\n",
            "uppercase digest": digest.upper() + b"  deploy.sh\n",
            "single space": digest + b" deploy.sh\n",
            "binary-mode marker": digest + b" *deploy.sh\n",
            "other file name": digest + b"  install.sh\n",
            "path prefix": digest + b"  ./deploy.sh\n",
            "missing newline": digest + b"  deploy.sh",
            "carriage return": digest + b"  deploy.sh\r\n",
            "trailing blank line": digest + b"  deploy.sh\n\n",
            "second entry": digest + b"  deploy.sh\n" + digest + b"  deploy.sh\n",
            "leading whitespace": b" " + digest + b"  deploy.sh\n",
            "short digest": digest[:63] + b"  deploy.sh\n",
            "long digest": digest + b"0  deploy.sh\n",
            "html error page": b"<!doctype html><title>Not Found</title>\n",
        }
        for name, checksum in malformed.items():
            with self.subTest(name):
                downloads = self.assert_refused(
                    checksum, 'published checksum is not one "<sha256>  deploy.sh" line'
                )
                self.assertEqual(downloads, [CHECKSUM_URL])

    def test_refuses_when_checksum_is_not_published(self) -> None:
        downloads = self.assert_refused(
            None, 'published checksum is not one "<sha256>  deploy.sh" line'
        )
        self.assertEqual(downloads, [CHECKSUM_URL])


class SkillContractTests(unittest.TestCase):
    def test_skill_hardcodes_no_installer_digest(self) -> None:
        for path in sorted(SKILL_DIR.rglob("*")):
            if not path.is_file():
                continue
            text = path.read_text(encoding="utf-8")
            with self.subTest(str(path.relative_to(SKILL_DIR))):
                self.assertIsNone(
                    SHA256_LITERAL.search(text),
                    f"{path} hardcodes a SHA-256 digest; verify against "
                    f"{CHECKSUM_URL} instead of pinning one",
                )
                self.assertNotRegex(text, r"expected_deploy_sha256='")

    def test_transferred_copy_is_checked_against_published_checksum(self) -> None:
        text = SKILL_MD.read_text(encoding="utf-8")
        self.assertIn(
            'test "$remote_deploy_sha256" = "$expected_deploy_sha256"', text
        )


if __name__ == "__main__":
    unittest.main()
