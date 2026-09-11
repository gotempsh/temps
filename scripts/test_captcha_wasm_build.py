#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise the WASM drift gate against real git repositories."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class CaptchaWasmBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/verify-captcha-wasm.sh", self.repo / "scripts")
        self.pkg = self.repo / "crates/temps-captcha-wasm/pkg"
        self.pkg.mkdir(parents=True)
        self.wasm = self.pkg / "temps_captcha_wasm_bg.wasm"
        self.wasm.write_bytes(b"\x00asm\x01\x00\x00\x00")
        self.git("init", "-q")
        self.git("add", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                 "-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True)

    def verify(self):
        return subprocess.run(
            ["bash", str(self.repo / "scripts/verify-captcha-wasm.sh")],
            cwd=self.pkg, text=True, capture_output=True,
        )

    def test_clean_package_passes_with_unrelated_changes(self):
        (self.repo / "unrelated.txt").write_text("local work")
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_modified_wasm_fails_with_canonical_rebuild_instructions(self):
        self.wasm.write_bytes(b"different host output")
        result = self.verify()
        self.assertEqual(result.returncode, 1)
        self.assertIn("Linux/AMD64", result.stdout)
        self.assertIn("bash scripts/rebuild-captcha-wasm.sh", result.stdout)
        self.assertIn(self.wasm.name, result.stdout)

    def test_deleted_package_file_fails(self):
        self.wasm.unlink()
        self.assertEqual(self.verify().returncode, 1)

    def test_untracked_package_file_fails(self):
        (self.pkg / "unexpected.js").write_text("changed generated output")
        self.assertEqual(self.verify().returncode, 1)

    def test_git_failure_cannot_report_a_match(self):
        shutil.rmtree(self.repo / ".git")
        result = self.verify()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("matches the canonical rebuild", result.stdout)
        self.assertIn("Cannot inspect", result.stderr)

    def test_ci_rebuilds_before_application_without_byte_comparison(self):
        workflow = (ROOT / ".github/workflows/rust-tests.yml").read_text()
        self.assertNotIn("scripts/verify-captcha-wasm.sh", workflow)
        self.assertLess(workflow.index("name: Rebuild CAPTCHA WASM in toolchain container"),
                        workflow.index("cargo build --profile fast --bin temps"))

    def test_rebuild_uses_amd64_for_image_and_container(self):
        script = (ROOT / "scripts/rebuild-captcha-wasm.sh").read_text()
        self.assertIn("docker build --platform linux/amd64", script)
        self.assertIn("docker run --rm --platform linux/amd64", script)


if __name__ == "__main__":
    unittest.main()
