#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for ci-affected-crates.py, against fixture workspaces and this one."""

from __future__ import annotations

import importlib.util
import tempfile
import textwrap
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent.parent

spec = importlib.util.spec_from_file_location("ci_affected_crates", SCRIPTS / "ci-affected-crates.py")
assert spec is not None and spec.loader is not None
affected = importlib.util.module_from_spec(spec)
spec.loader.exec_module(affected)


def write(root: Path, path: str, text: str) -> None:
    target = root / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(textwrap.dedent(text), encoding="utf-8")


def crate(root: Path, name: str, deps: str = "", source: str = "") -> None:
    write(root, f"crates/{name}/Cargo.toml", f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n{deps}')
    write(root, f"crates/{name}/src/lib.rs", source or "pub fn f() {}\n")


class FixtureWorkspaceTests(unittest.TestCase):
    """base <- mid (dev-dependency) <- top; reader includes a file of base's."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = self.root = Path(self.temp.name)
        write(
            root,
            "Cargo.toml",
            """
            [workspace]
            members = ["crates/base", "crates/mid", "crates/top", "crates/reader",
                       "crates/alone", "crates/inherits"]

            [workspace.dependencies]
            base = { path = "crates/base" }
            """,
        )
        crate(root, "base")
        write(root, "crates/base/data/fixture.json", "{}\n")
        crate(root, "mid", '[dev-dependencies]\nbase = { path = "../base" }\n')
        crate(root, "top", '[target.\'cfg(unix)\'.dependencies]\nmid = { path = "../mid" }\n')
        crate(
            root,
            "reader",
            source='const F: &str = include_str!("../../base/data/fixture.json");\n'
            'const UI: &str = include_str!("../../../web/src/App.tsx");\n'
            '// path-traversal test inputs, not reads:\n'
            'const BAD: [&str; 3] = ["../", "../..", "../outside"];\n',
        )
        write(root, "web/src/App.tsx", "export {}\n")
        crate(root, "alone")
        crate(root, "inherits", "[build-dependencies]\nbase = { workspace = true }\n")
        self.workspace = affected.load_workspace(root)

    def crates(self, *paths: str):
        return affected.affected_crates(self.workspace, paths)

    def test_a_change_reaches_its_crate_and_every_dependent(self) -> None:
        self.assertEqual(self.crates("crates/base/src/lib.rs"), {"base", "mid", "top", "inherits"})

    def test_a_leaf_change_reaches_only_its_crate(self) -> None:
        self.assertEqual(self.crates("crates/top/src/lib.rs"), {"top"})
        self.assertEqual(self.crates("crates/alone/Cargo.toml"), {"alone"})

    def test_a_file_read_from_another_crate_reaches_the_reader(self) -> None:
        self.assertEqual(
            self.crates("crates/base/data/fixture.json"),
            {"base", "mid", "top", "inherits", "reader"},
        )

    def test_frontend_files_reach_only_their_readers(self) -> None:
        self.assertEqual(self.crates("web/src/App.tsx"), {"reader"})
        self.assertEqual(self.crates("web/src/pages/Other.tsx"), set())
        self.assertEqual(self.crates("docs/page.mdx"), set())

    def test_traversal_literals_are_not_reads(self) -> None:
        self.assertEqual(self.crates("crates/alone/src/lib.rs"), {"alone"})
        self.assertNotIn("crates", self.workspace.readers)

    def test_anything_outside_the_members_reaches_everything(self) -> None:
        for path in ("Cargo.lock", "Cargo.toml", ".cargo/config.toml", ".config/nextest.toml",
                     ".github/workflows/rust-tests.yml", "scripts/x.sh", "crates/patches/x/lib.rs"):
            with self.subTest(path=path):
                self.assertIsNone(self.crates("crates/top/src/lib.rs", path))

    def test_an_empty_diff_reaches_everything(self) -> None:
        self.assertIsNone(self.crates())

    def test_filterset(self) -> None:
        self.assertEqual(affected.filterset(None), "all()")
        self.assertEqual(affected.filterset(set()), "none()")
        self.assertEqual(affected.filterset({"top", "mid"}), "package(=mid) | package(=top)")

    def test_an_unreadable_manifest_is_a_workspace_error(self) -> None:
        write(self.root, "crates/alone/Cargo.toml", "[package\n")
        with self.assertRaises(affected.WorkspaceError):
            affected.load_workspace(self.root)

    def test_no_workspace_means_every_crate_and_the_docker_tests(self) -> None:
        self.assertEqual(
            affected.outputs(None, ["crates/top/src/lib.rs"]),
            {"tests": "all()", "docker_tests": "true"},
        )


class ThisWorkspaceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workspace = affected.load_workspace(REPO_ROOT)

    def test_every_member_is_loaded(self) -> None:
        self.assertIn("temps-cli", self.workspace.dirs)
        for crate_name in affected.DOCKER_TESTS_CRATES:
            self.assertIn(crate_name, self.workspace.dirs)

    def test_a_proxy_change_skips_the_docker_tests(self) -> None:
        result = affected.outputs(self.workspace, ["crates/temps-proxy/src/lib.rs"])
        self.assertEqual(result["docker_tests"], "false")
        self.assertIn("package(=temps-proxy)", result["tests"])
        self.assertIn("package(=temps-cli)", result["tests"])

    def test_a_providers_change_runs_the_docker_tests(self) -> None:
        result = affected.outputs(self.workspace, ["crates/temps-providers/src/lib.rs"])
        self.assertEqual(result["docker_tests"], "true")

    def test_cross_crate_includes_are_found(self) -> None:
        # temps-agents embeds the pty agent's sources into a sandbox image.
        reached = affected.affected_crates(self.workspace, ["crates/temps-pty-agent/src/main.rs"])
        self.assertIn("temps-agents", reached)
        reached = affected.affected_crates(self.workspace, ["web/src/App.tsx"])
        self.assertIn("temps-ai-chat", reached)


if __name__ == "__main__":
    unittest.main()
