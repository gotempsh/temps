#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for the musl build cache pruner and its cache guard.

The pruner only deletes on `main` when Cargo.lock or the Dockerfile changes,
so a regression there would otherwise surface as repeated expensive rebuilds
long after the change that caused it. These tests build a small offline
workspace twice across a version bump, which leaves a stale generation of
every unit behind, and check that `--apply` removes exactly that generation
and leaves a build cargo still considers fresh.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
PRUNE = SCRIPTS / "prune-cargo-build-cache.py"

WORKSPACE = {
    "Cargo.toml": """\
[workspace]
members = ["app", "util"]
resolver = "2"

[profile.fast]
inherits = "release"
codegen-units = 16
incremental = true
""",
    "app/Cargo.toml": """\
[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
util = { path = "../util" }
""",
    "app/build.rs": 'fn main() { println!("cargo:rerun-if-changed=build.rs"); }\n',
    "app/src/main.rs": 'fn main() { println!("{}", util::answer()); }\n',
    "util/Cargo.toml": """\
[package]
name = "util"
version = "0.1.0"
edition = "2021"
""",
    "util/src/lib.rs": "pub fn answer() -> u32 { 42 }\n",
}


def load_guard_module():
    spec = importlib.util.spec_from_file_location(
        "check_cache_save_guards", SCRIPTS / "check-cache-save-guards.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def cargo_build(workspace: Path, target: Path) -> list[dict]:
    result = subprocess.run(
        [
            "cargo", "build", "--offline", "--profile", "fast",
            "--message-format=json-render-diagnostics",
        ],
        cwd=workspace,
        env={**os.environ, "CARGO_TARGET_DIR": str(target)},
        check=True,
        capture_output=True,
        text=True,
    )
    return [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]


def prune(messages: list[dict], target: Path, *extra: str) -> subprocess.CompletedProcess:
    log = target.parent / "messages.json"
    log.write_text("".join(json.dumps(m) + "\n" for m in messages))
    return subprocess.run(
        [
            sys.executable, str(PRUNE),
            "--messages", str(log),
            "--target-dir", str(target),
            "--container-target", str(target),
            *extra,
        ],
        check=True,
        capture_output=True,
        text=True,
    )


def entries(target: Path, sub: str, stem: str) -> list[str]:
    return sorted(p.name for p in (target / "fast" / sub).glob(f"{stem}-*"))


@unittest.skipUnless(shutil.which("cargo"), "cargo is not installed")
class PruneTargetTests(unittest.TestCase):
    def setUp(self) -> None:
        # Resolved so the target path matches what cargo reports (macOS
        # temp dirs live behind the /var -> /private/var symlink).
        self.tmp = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.workspace = self.tmp / "ws"
        self.target = self.tmp / "target"
        for relative, content in WORKSPACE.items():
            path = self.workspace / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        cargo_build(self.workspace, self.target)
        # A version bump changes util's package id, and with it the cargo
        # hash of util and everything depending on it: the same thing a
        # Cargo.lock bump does to the real workspace.
        manifest = self.workspace / "util/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('"0.1.0"', '"0.2.0"'))
        self.messages = cargo_build(self.workspace, self.target)

    def test_apply_removes_only_the_stale_generation(self) -> None:
        self.assertEqual(len(entries(self.target, "deps", "libutil")), 4)  # 2x rlib+rmeta
        self.assertEqual(len(entries(self.target, "incremental", "util")), 2)

        prune(self.messages, self.target, "--apply")

        live = {
            name.rsplit("-", 1)[1].split(".")[0]
            for name in entries(self.target, "deps", "libutil")
        }
        self.assertEqual(len(live), 1, entries(self.target, "deps", "libutil"))
        self.assertEqual(len(entries(self.target, "deps", "libutil")), 2)
        # The binary is only reported by its uplifted name; the hashed copy
        # must be matched back to it (hard link on Linux, copy on macOS).
        self.assertEqual(len([n for n in entries(self.target, "deps", "app") if "." not in n]), 1)
        self.assertEqual(len(entries(self.target, "incremental", "util")), 1)
        self.assertEqual(len(entries(self.target, "incremental", "app")), 1)
        self.assertTrue(entries(self.target, "incremental", "build_script_build"))

        rebuilt = [
            m["target"]["name"]
            for m in cargo_build(self.workspace, self.target)
            if m["reason"] == "compiler-artifact" and not m["fresh"]
        ]
        self.assertEqual(rebuilt, [], "pruning removed build output still in use")

    def test_report_only_deletes_nothing(self) -> None:
        before = sorted(p.name for p in (self.target / "fast" / "deps").iterdir())
        output = prune(self.messages, self.target).stdout
        self.assertIn("Would remove", output)
        after = sorted(p.name for p in (self.target / "fast" / "deps").iterdir())
        self.assertEqual(before, after)

    def test_incomplete_message_log_deletes_nothing(self) -> None:
        before = sorted(p.name for p in (self.target / "fast" / "deps").iterdir())
        truncated = [m for m in self.messages if m["reason"] != "build-finished"]
        output = prune(truncated, self.target, "--apply").stdout
        self.assertIn("not pruning", output)
        after = sorted(p.name for p in (self.target / "fast" / "deps").iterdir())
        self.assertEqual(before, after)

    def test_registry_keeps_only_locked_crates(self) -> None:
        registry = self.tmp / "registry"
        for kept, name in ((True, "serde-1.0.200"), (False, "serde-1.0.199")):
            (registry / "cache/index.crates.io-1").mkdir(parents=True, exist_ok=True)
            (registry / "cache/index.crates.io-1" / f"{name}.crate").write_text(name)
            (registry / "src/index.crates.io-1" / name).mkdir(parents=True)
        lockfile = self.tmp / "Cargo.lock"
        lockfile.write_text(
            'version = 4\n\n[[package]]\nname = "serde"\nversion = "1.0.200"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
        )

        prune(self.messages, self.target, "--apply",
              "--registry", str(registry), "--lockfile", str(lockfile))

        self.assertEqual(
            sorted(p.name for p in (registry / "cache/index.crates.io-1").iterdir()),
            ["serde-1.0.200.crate"],
        )
        self.assertEqual(
            sorted(p.name for p in (registry / "src/index.crates.io-1").iterdir()),
            ["serde-1.0.200"],
        )


class SetupBunGuardTests(unittest.TestCase):
    guard = load_guard_module()

    def violations(self, no_cache: str) -> list[int]:
        workflow = (
            "steps:\n"
            "  - name: Install Bun\n"
            "    uses: oven-sh/setup-bun@v2\n"
            "    with:\n"
            "      bun-version: latest\n"
            f"{no_cache}"
        )
        return self.guard.find_unguarded_self_saving_steps(workflow)

    def test_accepts_main_only_or_disabled_cache(self) -> None:
        self.assertEqual(self.violations("      no-cache: ${{ github.ref != 'refs/heads/main' }}\n"), [])
        self.assertEqual(self.violations("      no-cache: true\n"), [])

    def test_rejects_missing_false_or_inverted_values(self) -> None:
        for value in (
            "",
            "      no-cache: false\n",
            "      no-cache: ${{ github.ref == 'refs/heads/main' }}\n",
        ):
            with self.subTest(value=value):
                self.assertEqual(self.violations(value), [3])


if __name__ == "__main__":
    unittest.main()
