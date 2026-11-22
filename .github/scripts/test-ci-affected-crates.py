#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for ci-affected-crates.py, against fixture workspaces and this one."""

from __future__ import annotations

import importlib.util
import re
import subprocess
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
    """base <- mid (dev-dependency) <- top; base <- inherits (build) <- user;
    reader includes a file of base's."""

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
                       "crates/alone", "crates/inherits", "crates/user"]

            [workspace.dependencies]
            base = { path = "crates/base" }
            serde = "1.0.1"
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
        crate(root, "user", '[dependencies]\ninherits = { path = "../inherits" }\nserde = { workspace = true }\n')
        self.workspace = affected.load_workspace(root)

    def crates(self, *paths: str):
        return affected.affected_crates(self.workspace, paths)

    def test_a_change_reaches_its_crate_and_every_dependent(self) -> None:
        # inherits links base (build) and user links inherits: transitive.
        # mid only dev-depends on base, so its tests run but top, which links
        # mid's library and never base, does not.
        self.assertEqual(
            self.crates("crates/base/src/lib.rs"), {"base", "mid", "inherits", "user"}
        )

    def test_a_dev_dependency_reaches_only_the_dependents_tests(self) -> None:
        self.assertEqual(self.workspace.dev_dependencies["mid"], {"base"})
        self.assertNotIn("base", self.workspace.dependencies["mid"])
        self.assertEqual(self.crates("crates/mid/src/lib.rs"), {"mid", "top"})

    def test_a_leaf_change_reaches_only_its_crate(self) -> None:
        self.assertEqual(self.crates("crates/top/src/lib.rs"), {"top"})
        self.assertEqual(self.crates("crates/alone/Cargo.toml"), {"alone"})

    def test_a_file_read_from_another_crate_reaches_the_reader(self) -> None:
        self.assertEqual(
            self.crates("crates/base/data/fixture.json"),
            {"base", "mid", "inherits", "user", "reader"},
        )

    def test_frontend_files_reach_only_their_readers(self) -> None:
        self.assertEqual(self.crates("web/src/App.tsx"), {"reader"})
        self.assertEqual(self.crates("web/src/pages/Other.tsx"), set())
        self.assertEqual(self.crates("docs/page.mdx"), set())

    def test_traversal_literals_are_not_reads(self) -> None:
        self.assertEqual(self.crates("crates/alone/src/lib.rs"), {"alone"})
        self.assertNotIn("crates", self.workspace.readers)

    def test_anything_outside_the_members_reaches_everything(self) -> None:
        # Without revisions to compare, the lockfile and root manifest are
        # unplaceable too. `scripts/` and `sdks/` are read at test time through
        # paths built at runtime, so they stay here.
        for path in ("Cargo.lock", "Cargo.toml", ".cargo/config.toml", ".config/nextest.toml",
                     ".github/workflows/rust-tests.yml", "scripts/x.sh", "sdks/go/workflows/x.go",
                     "crates/patches/x/lib.rs", ".github/actions/setup/action.yml"):
            with self.subTest(path=path):
                self.assertIsNone(self.crates("crates/top/src/lib.rs", path))

    def test_ci_only_files_reach_only_their_readers(self) -> None:
        for path in (".github/workflows/release.yml", ".github/scripts/release_select.py",
                     ".github/releases/packages.json", "telemetry-api/src/index.ts"):
            with self.subTest(path=path):
                self.assertEqual(self.crates("crates/top/src/lib.rs", path), {"top"})
                self.assertEqual(self.crates(path), set())

    LOCK = """
        version = 4

        [[package]]
        name = "base"
        version = "0.1.0"
        dependencies = ["tokio"]

        [[package]]
        name = "mid"
        version = "0.1.0"
        dependencies = ["base"]

        [[package]]
        name = "top"
        version = "0.1.0"
        dependencies = ["mid", "serde 1.0.1"]

        [[package]]
        name = "serde"
        version = "1.0.1"
        source = "registry+https://github.com/rust-lang/crates.io-index"
        checksum = "aaaa"

        [[package]]
        name = "tokio"
        version = "1.0.0"
        source = "registry+https://github.com/rust-lang/crates.io-index"
        checksum = "bbbb"
        dependencies = ["serde 1.0.1"]
        """

    def lock(self, **replacements: str) -> str:
        text = textwrap.dedent(self.LOCK)
        for old, new in replacements.items():
            text = text.replace(old.replace("_", " "), new)
        return text

    def test_a_lockfile_bump_reaches_the_crates_that_use_the_package(self) -> None:
        bumped = self.lock().replace('name = "tokio"\nversion = "1.0.0"', 'name = "tokio"\nversion = "1.0.1"')
        bumped = bumped.replace('dependencies = ["tokio"]', 'dependencies = ["tokio 1.0.1"]')
        # tokio is used by base only; mid and top reach it through base.
        self.assertEqual(
            affected.lockfile_crates(self.workspace, self.lock(), bumped), {"base", "mid", "top"}
        )
        serde = self.lock().replace('checksum = "aaaa"', 'checksum = "cccc"')
        # serde is used by top directly and by tokio, which base uses.
        self.assertEqual(
            affected.lockfile_crates(self.workspace, self.lock(), serde), {"base", "mid", "top"}
        )

    def test_a_dependency_added_to_one_crate_reaches_only_it(self) -> None:
        added = self.lock().replace(
            'name = "mid"\nversion = "0.1.0"\ndependencies = ["base"]',
            'name = "mid"\nversion = "0.1.0"\ndependencies = ["base", "itoa"]',
        ) + '\n[[package]]\nname = "itoa"\nversion = "1.0.0"\nsource = "registry+x"\nchecksum = "dddd"\n'
        # mid's own entry changed; top depends on mid, so it is reached too.
        self.assertEqual(affected.lockfile_crates(self.workspace, self.lock(), added), {"mid", "top"})
        self.assertEqual(affected.lockfile_crates(self.workspace, self.lock(), self.lock()), set())

    def test_same_name_and_version_from_two_sources_are_compared_separately(self) -> None:
        # Cargo allows one name and version from two sources; a change to the first
        # entry must not be hidden by an unchanged second one.
        git_serde = (
            '\n[[package]]\nname = "serde"\nversion = "1.0.1"\n'
            'source = "git+https://example.invalid/serde#0000"\n'
        )
        before = self.lock() + git_serde
        after = before.replace('checksum = "aaaa"', 'checksum = "cccc"')
        self.assertEqual(affected.lockfile_crates(self.workspace, before, after), {"base", "mid", "top"})

    def test_an_unparseable_or_missing_lockfile_reaches_everything(self) -> None:
        self.assertIsNone(affected.lockfile_crates(self.workspace, self.lock(), "[[package]\n"))
        self.assertIsNone(affected.lockfile_crates(self.workspace, None, self.lock()))

    def test_a_workspace_dependency_bump_reaches_the_crates_that_inherit_it(self) -> None:
        old = (self.root / "Cargo.toml").read_text()
        new = old.replace('serde = "1.0.1"', 'serde = "1.0.2"')
        self.assertEqual(affected.root_manifest_crates(self.workspace, old, new), {"user"})
        moved = old.replace('base = { path = "crates/base" }', 'base = { path = "crates/base", features = ["x"] }')
        self.assertEqual(affected.root_manifest_crates(self.workspace, old, moved), {"inherits"})

    def test_any_other_root_manifest_change_reaches_everything(self) -> None:
        old = (self.root / "Cargo.toml").read_text()
        for new in (
            old.replace('"crates/user"]', '"crates/user", "crates/new"]'),
            old + "\n[patch.crates-io]\nserde = { path = \"crates/patches/serde\" }\n",
            old + "\n[profile.dev]\nopt-level = 1\n",
            "[workspace\n",
        ):
            with self.subTest(new=new[-40:]):
                self.assertIsNone(affected.root_manifest_crates(self.workspace, old, new))
        self.assertIsNone(affected.root_manifest_crates(self.workspace, None, old))

    def test_the_lockfile_is_compared_between_the_two_revisions(self) -> None:
        def git(*args: str) -> str:
            return subprocess.run(
                ["git", *args], cwd=self.root, check=True, capture_output=True, text=True,
                env={"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
                     "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com",
                     "HOME": str(self.root), "PATH": "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin"},
            ).stdout.strip()

        git("init", "-q")
        write(self.root, "Cargo.lock", self.lock())
        git("add", "-A")
        git("commit", "-qm", "base")
        base = git("rev-parse", "HEAD")
        write(self.root, "Cargo.lock", self.lock().replace('checksum = "aaaa"', 'checksum = "cccc"'))
        write(self.root, "crates/alone/src/lib.rs", "pub fn g() {}\n")
        git("commit", "-qam", "bump")
        head = git("rev-parse", "HEAD")
        # serde reaches base, mid and top in the lockfile; inherits and user
        # then link base.
        self.assertEqual(
            affected.affected_crates(self.workspace, ["Cargo.lock", "crates/alone/src/lib.rs"], base, head),
            {"alone", "base", "mid", "top", "inherits", "user"},
        )
        # A revision that does not exist cannot be compared: everything.
        self.assertIsNone(affected.affected_crates(self.workspace, ["Cargo.lock"], base, "no-such-rev"))

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

    # Literals that name a CI-only path without reading it, and why.
    NOT_READS = {
        ("crates/temps-cli/src/commands/upgrade/release_signature.rs", ".github/workflows/release.yml"):
            "the Sigstore signing identity, compared as a string; the file is never opened",
    }

    def test_no_crate_references_a_ci_only_path(self) -> None:
        # CI_ONLY_PREFIXES reach only readers found by the literal scan. A
        # crate that names one of those paths some other way (a runtime join,
        # format!) would silently lose its tests, so any literal naming an
        # existing path under them fails here, unless load_workspace already
        # records the crate as its reader (an include_str! of a `../` path).
        # URLs and paths inside a user's repository do not exist in this
        # checkout, so they do not count.
        literal = re.compile(r'"([^"\n]*)"')
        found: list[str] = []
        for name, directory in self.workspace.dirs.items():
            for source in (REPO_ROOT / directory).rglob("*.rs"):
                if "target" in source.parts:
                    continue
                for value in literal.findall(source.read_text(encoding="utf-8", errors="replace")):
                    path = re.sub(r"^(/|\.\./)+", "", value).rstrip("/")
                    if not path or not (REPO_ROOT / path).exists():
                        continue
                    relative = source.relative_to(REPO_ROOT).as_posix()
                    if (relative, value) in self.NOT_READS or name in self.workspace.readers_of(path):
                        continue
                    if any(path + "/" == prefix or path.startswith(prefix) for prefix in affected.CI_ONLY_PREFIXES):
                        found.append(f"{relative}: {value}")
        self.assertEqual(found, [], "move the path out of CI_ONLY_PREFIXES or stop reading it")

    def test_ci_only_prefixes_exist(self) -> None:
        for prefix in affected.CI_ONLY_PREFIXES:
            with self.subTest(prefix=prefix):
                self.assertTrue((REPO_ROOT / prefix).is_dir(), prefix)
        for path in affected.REACHES_EVERY_CRATE:
            self.assertTrue((REPO_ROOT / path).is_file(), path)

    def test_a_ci_only_file_a_crate_includes_still_reaches_it(self) -> None:
        reached = affected.affected_crates(self.workspace, [".github/workflows/dependency-scan.yml"])
        self.assertIn("temps-core", reached)
        self.assertEqual(affected.affected_crates(self.workspace, [".github/workflows/release.yml"]), set())

    def test_cross_crate_includes_are_found(self) -> None:
        # temps-sandbox-providers embeds the pty agent's sources into a sandbox image.
        reached = affected.affected_crates(self.workspace, ["crates/temps-pty-agent/src/main.rs"])
        self.assertIn("temps-sandbox-providers", reached)
        reached = affected.affected_crates(self.workspace, ["web/src/App.tsx"])
        self.assertIn("temps-ai-chat", reached)


if __name__ == "__main__":
    unittest.main()
