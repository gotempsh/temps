#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for check-toolchain-pin.py."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("check_toolchain_pin", Path(__file__).with_name("check-toolchain-pin.py"))
assert SPEC and SPEC.loader
pin = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pin)


class ToolchainPinTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / ".github" / "workflows").mkdir(parents=True)
        (self.root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.97.1"\n', encoding="utf-8")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def workflow(self, body: str) -> None:
        (self.root / ".github" / "workflows" / "ci.yml").write_text(body, encoding="utf-8")

    def test_matching_pin_passes(self) -> None:
        self.workflow(
            "steps:\n"
            "  - name: Install\n"
            "    uses: dtolnay/rust-toolchain@abc # stable\n"
            "    with:\n"
            "      toolchain: 1.97.1\n"
            "      components: clippy\n"
            "  - uses: dtolnay/rust-toolchain@nightly\n"
        )
        self.assertEqual(pin.main(self.root), 0)

    def test_floating_stable_fails(self) -> None:
        self.workflow("steps:\n  - uses: dtolnay/rust-toolchain@stable\n  - run: cargo test\n")
        self.assertEqual(pin.main(self.root), 1)

    def test_drifted_version_fails(self) -> None:
        self.workflow("steps:\n  - uses: dtolnay/rust-toolchain@abc\n    with:\n      toolchain: stable\n")
        self.assertEqual(pin.main(self.root), 1)

    def test_toolchain_of_next_step_is_not_borrowed(self) -> None:
        self.workflow(
            "steps:\n"
            "  - uses: dtolnay/rust-toolchain@abc\n"
            "  - uses: other/action@v1\n"
            "    with:\n"
            "      toolchain: 1.97.1\n"
        )
        self.assertEqual(pin.main(self.root), 1)

    def test_dockerfile_must_pin_its_own_compiler_when_different(self) -> None:
        self.workflow("steps: []\n")
        (self.root / "Dockerfile").write_text("FROM rust:1.98.1-alpine AS toolchain\n", encoding="utf-8")
        self.assertEqual(pin.main(self.root), 1)
        (self.root / "Dockerfile").write_text(
            "FROM rust:1.98.1-alpine AS toolchain\nENV RUSTUP_TOOLCHAIN=1.98.1\n", encoding="utf-8"
        )
        self.assertEqual(pin.main(self.root), 0)


if __name__ == "__main__":
    unittest.main()
