#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for ci-target-cache.py against throwaway git repositories."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent

spec = importlib.util.spec_from_file_location("ci_target_cache", SCRIPTS / "ci-target-cache.py")
assert spec is not None and spec.loader is not None
cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cache)


def run(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
         "-c", "core.hooksPath=/dev/null", *args],
        cwd=root, check=True, capture_output=True, text=True,
    ).stdout.strip()


def write(root: Path, path: str, text: str) -> None:
    target = root / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")


def artifact(name: str, fresh: bool, directory: str | None = None) -> str:
    location = f"path+file:///work/crates/{directory or name}"
    package_id = f"{location}#{name}@0.1.0" if directory else f"{location}#0.1.0"
    return json.dumps({"reason": "compiler-artifact", "package_id": package_id, "fresh": fresh})


class TargetCacheTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = self.root = Path(self.temp.name)
        write(root, "Cargo.toml", '[workspace]\nmembers = ["crates/a", "crates/b"]\n')
        for name in ("a", "b"):
            write(root, f"crates/{name}/Cargo.toml", f'[package]\nname = "{name}"\nversion = "0.1.0"\n')
            write(root, f"crates/{name}/src/lib.rs", "pub fn f() {}\n")
        write(root, "crates/a/fixtures/sample.rs", "// not compiled\n")
        run(root, "init", "-q")
        run(root, "add", ".")
        run(root, "commit", "-qm", "base")
        self.base = run(root, "rev-parse", "HEAD")
        self.target = root / "target"

    def quietly(self, function, *args):
        with contextlib.redirect_stdout(io.StringIO()):
            return function(*args)

    def commit_change(self, path: str) -> None:
        write(self.root, path, "pub fn g() {}\n")
        run(self.root, "commit", "-qam", "change")

    def test_record_then_prepare_restamps_only_changed_files(self) -> None:
        self.quietly(cache.record, self.target, self.root)
        self.assertEqual(cache.recorded_commit(self.target), self.base)
        self.commit_change("crates/a/src/lib.rs")
        self.assertEqual(self.quietly(cache.prepare, self.target, self.root), self.base)
        changed = os.stat(self.root / "crates/a/src/lib.rs").st_mtime
        unchanged = os.stat(self.root / "crates/b/src/lib.rs").st_mtime
        self.assertEqual(unchanged, cache.OLD)
        self.assertGreater(changed, cache.OLD + 10**8)

    def test_no_recorded_commit_leaves_checkout_times(self) -> None:
        before = os.stat(self.root / "crates/b/src/lib.rs").st_mtime
        self.assertIsNone(self.quietly(cache.prepare, self.target, self.root))
        self.assertEqual(os.stat(self.root / "crates/b/src/lib.rs").st_mtime, before)

    def test_a_malformed_or_unknown_commit_leaves_checkout_times(self) -> None:
        self.target.mkdir()
        before = os.stat(self.root / "crates/b/src/lib.rs").st_mtime
        for recorded in ("not-a-sha; rm -rf /", "f" * 40):
            with self.subTest(recorded=recorded):
                (self.target / cache.STAMP).write_text(recorded, encoding="utf-8")
                self.assertIsNone(self.quietly(cache.prepare, self.target, self.root))
                self.assertEqual(os.stat(self.root / "crates/b/src/lib.rs").st_mtime, before)

    def test_rebuilt_packages_reads_both_package_id_forms(self) -> None:
        messages = Path(self.temp.name) / "messages.json"
        messages.write_text(
            "\n".join([
                artifact("a", fresh=False),
                artifact("b", fresh=True, directory="b-dir"),
                json.dumps({"reason": "compiler-artifact", "package_id": "registry+https://x#serde@1.0", "fresh": True}),
                "Compiling a",
            ]),
            encoding="utf-8",
        )
        self.assertEqual(cache.rebuilt_packages([messages]), ({"a"}, {"b"}))

    def test_verify_fails_when_a_changed_crate_was_reused(self) -> None:
        self.commit_change("crates/a/src/lib.rs")
        messages = Path(self.temp.name) / "messages.json"
        messages.write_text("\n".join([artifact("a", True), artifact("b", True)]), encoding="utf-8")
        self.assertEqual(self.quietly(cache.verify, self.base, [messages], self.root), 1)
        messages.write_text("\n".join([artifact("a", False), artifact("b", True)]), encoding="utf-8")
        self.assertEqual(self.quietly(cache.verify, self.base, [messages], self.root), 0)

    def test_verify_ignores_rust_files_cargo_does_not_compile(self) -> None:
        self.commit_change("crates/a/fixtures/sample.rs")
        messages = Path(self.temp.name) / "messages.json"
        messages.write_text("\n".join([artifact("a", True), artifact("b", True)]), encoding="utf-8")
        self.assertEqual(self.quietly(cache.verify, self.base, [messages], self.root), 0)


if __name__ == "__main__":
    unittest.main()
