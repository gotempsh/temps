#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for ci_test_coverage.py."""

from __future__ import annotations

import re
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import ci_test_coverage as coverage  # noqa: E402

LIB = coverage.TestBinary("temps-a", "temps_a", "lib")
IT = coverage.TestBinary("temps-a", "it_one", "test")


def workflow(*groups: str) -> str:
    body = "\n".join(textwrap.indent(textwrap.dedent(group), " " * 10) for group in groups)
    return f"jobs:\n  unit:\n    strategy:\n      matrix:\n        include:\n{body}\n    steps:\n      - run: echo\n"


class Workspace:
    """A workspace on disk, with the metadata `cargo metadata` would report."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.packages: list[dict] = []
        (root / ".github/workflows").mkdir(parents=True)
        (root / "scripts").mkdir()
        self.write_allowlist("")

    def write_workflow(self, text: str, name: str = "rust-tests.yml") -> None:
        (self.root / ".github/workflows" / name).write_text(text, encoding="utf-8")

    def write_allowlist(self, text: str) -> None:
        (self.root / coverage.ALLOWLIST).write_text(textwrap.dedent(text), encoding="utf-8")

    def add_package(self, name: str, targets: list[tuple[str, str, str]]) -> None:
        """Each target is (kind, target name, Rust source)."""
        crate = self.root / "crates" / name
        entries = []
        for kind, target, source in targets:
            relative = {"lib": "src/lib.rs", "bin": "src/main.rs"}.get(kind, f"{kind}s/{target}.rs")
            path = crate / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(textwrap.dedent(source), encoding="utf-8")
            entries.append({"kind": [kind], "name": target, "src_path": str(path), "test": kind != "example"})
        self.packages.append({"id": name, "name": name, "targets": entries})

    def check(self) -> list[str]:
        metadata = {"packages": self.packages, "workspace_members": [package["id"] for package in self.packages]}
        return coverage.check(self.root, metadata)


TEST_SOURCE = "#[test]\nfn works() {}\n"


class FiltersetTests(unittest.TestCase):
    def test_package_kind_and_binary_predicates(self) -> None:
        selection = coverage.Filterset("(package(temps-a) or package(temps-b)) and kind(=lib)")
        self.assertIs(selection.evaluate(LIB), True)
        self.assertIs(selection.evaluate(IT), False)
        self.assertIs(coverage.Filterset("binary(it_one)").evaluate(IT), True)
        self.assertIs(coverage.Filterset("binary_id(=temps-a::it_one)").evaluate(IT), True)

    def test_a_test_predicate_may_match_until_a_name_is_known(self) -> None:
        selection = coverage.Filterset("package(temps-a) and not test(/_real_api$/)")
        self.assertIsNone(selection.evaluate(LIB))
        self.assertIs(selection.evaluate(LIB, "calls_real_api"), False)
        self.assertIs(selection.evaluate(LIB, "calls_mock"), True)

    def test_an_unknown_test_cannot_rescue_the_wrong_package(self) -> None:
        self.assertIs(coverage.Filterset("package(other) and test(x)").evaluate(LIB), False)
        self.assertIs(coverage.Filterset("package(temps-a) or test(x)").evaluate(LIB), True)

    def test_matcher_prefixes_and_defaults(self) -> None:
        self.assertIs(coverage.Filterset("package(temps-*)").evaluate(LIB), True)
        self.assertIs(coverage.Filterset("package(=temps-)").evaluate(LIB), False)
        self.assertIs(coverage.Filterset("package(~mps-a)").evaluate(LIB), True)
        self.assertIs(coverage.Filterset("test(bc)").evaluate(LIB, "abcd"), True)
        self.assertIs(coverage.Filterset("test(=bc)").evaluate(LIB, "abcd"), False)

    def test_operator_spellings_and_precedence(self) -> None:
        self.assertIs(coverage.Filterset("package(temps-a) & !kind(=test)").evaluate(LIB), True)
        self.assertIs(coverage.Filterset("package(temps-a) - kind(=lib)").evaluate(LIB), False)
        self.assertIs(coverage.Filterset("none() and none() or all()").evaluate(LIB), True)

    def test_a_malformed_filterset_is_an_error_not_an_empty_selection(self) -> None:
        for text in ("package(x", "package(x) and", "(package(x)", "frobnicate(x)"):
            with self.assertRaises(coverage.CoverageError, msg=text):
                coverage.Filterset(text).evaluate(LIB)


class WorkflowParsingTests(unittest.TestCase):
    def test_folded_filters_and_archives_are_read(self) -> None:
        text = workflow(
            """\
            - group: unit-a
              # A comment naming package(nothing).
              filter: >-
                (package(temps-a) or
                package(temps-b))
                and kind(=lib)
              test_threads: "2"
            - group: docker
              archive: docker-tests
              filter: package(temps-c) and kind(=lib)
            """
        )
        entries = coverage._matrix_entries(text)
        self.assertEqual([entry["group"] for entry in entries], ["unit-a", "docker"])
        self.assertEqual(entries[0]["filter"], "(package(temps-a) or package(temps-b)) and kind(=lib)")
        self.assertEqual(entries[1]["archive"], "docker-tests")

    def test_cargo_test_lines_become_selections(self) -> None:
        text = textwrap.dedent(
            """\
            # cargo test -p from-a-comment
            run: cargo test -p temps-a --lib
            run: cargo test --locked -p temps-b --test it_one -- --nocapture
            run: TEMPS_ONLY="$x" cargo test -p temps-c --test starters -- --ignored
            run: cargo test --no-run --workspace --all-targets
            run: cargo test --no-run --package temps-d
            """
        )
        self.assertEqual(
            coverage._cargo_test_runs(text),
            [
                ("package(=temps-a) and kind(=lib)", False),
                ("package(=temps-b) and (binary(=it_one))", False),
                ("package(=temps-c) and (binary(=starters))", True),
            ],
        )

    def test_only_literal_nextest_filters_are_read(self) -> None:
        text = textwrap.dedent(
            """\
            cargo nextest run --archive-file a.tar.zst -E '(${{ matrix.filter }}) and (x)'
            bash -lc 'cargo nextest run -E "package(temps-a) and test(pitr)" --test-threads=1'
            """
        )
        self.assertEqual(coverage._inline_nextest_runs(text), [("package(temps-a) and test(pitr)", False)])


class ScanTests(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)

    def write(self, relative: str, text: str) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def test_tests_are_found_through_the_module_tree(self) -> None:
        lib = self.write("src/lib.rs", "pub mod service;\n")
        self.write("src/service/mod.rs", "#[cfg(test)]\nmod tests;\n")
        self.write("src/service/tests.rs", "#[tokio::test]\nasync fn works() {}\n")
        self.assertTrue(coverage.has_tests(lib))

    def test_a_sibling_outside_the_module_tree_does_not_count(self) -> None:
        lib = self.write("src/lib.rs", "pub fn a() {}\n")
        self.write("src/main.rs", TEST_SOURCE)
        self.assertFalse(coverage.has_tests(lib))

    def test_a_path_attribute_and_a_module_inside_an_inline_module_are_followed(self) -> None:
        lib = self.write("src/lib.rs", '#[path = "odd/place.rs"]\nmod place;\nmod outer { mod nested; }\n')
        self.write("src/odd/place.rs", "fn a() {}\n")
        self.write("src/outer/nested.rs", TEST_SOURCE)
        self.assertTrue(coverage.has_tests(lib))

    def test_plain_code_mentioning_test_is_not_a_test(self) -> None:
        lib = self.write("src/lib.rs", 'fn test() { let _ = "test"; }\n#[derive(Debug)]\nstruct Test;\n')
        self.assertFalse(coverage.has_tests(lib))


class CheckTests(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.workspace = Workspace(Path(temp.name))
        self.workspace.add_package("temps-a", [("lib", "temps-a", TEST_SOURCE), ("test", "it_one", TEST_SOURCE)])

    def use_filter(self, text: str) -> None:
        self.workspace.write_workflow(workflow(f"- group: unit\n  filter: {text}\n"))

    def test_every_binary_selected_passes(self) -> None:
        self.use_filter("package(temps-a)")
        self.assertEqual(self.workspace.check(), [])

    def test_a_binary_no_group_selects_fails_and_says_which(self) -> None:
        self.use_filter("package(temps-a) and kind(=lib)")
        problems = self.workspace.check()
        self.assertEqual(len(problems), 1)
        self.assertIn("temps-a::it_one is run by no CI group", problems[0])

    def test_a_library_without_tests_needs_no_group(self) -> None:
        self.workspace.add_package("temps-b", [("lib", "temps-b", "pub fn a() {}\n")])
        self.use_filter("package(temps-a)")
        self.assertEqual(self.workspace.check(), [])

    def test_a_group_excluding_some_tests_by_name_still_covers_the_binary(self) -> None:
        self.use_filter("package(temps-a) and not test(/_real_api$/)")
        self.assertEqual(self.workspace.check(), [])

    def test_a_run_in_another_workflow_counts(self) -> None:
        self.use_filter("package(temps-a) and kind(=lib)")
        self.workspace.write_workflow("run: cargo test -p temps-a --test it_one\n", "other.yml")
        self.assertEqual(self.workspace.check(), [])

    def test_a_docker_tests_gated_file_needs_the_docker_tests_archive(self) -> None:
        self.workspace.add_package("temps-c", [("test", "it_gated", '#![cfg(feature = "docker-tests")]\n' + TEST_SOURCE)])
        self.use_filter("package(temps-a) or package(temps-c)")
        self.assertIn("temps-c::it_gated is run by no CI group", self.workspace.check()[0])
        self.workspace.write_workflow(
            workflow(
                "- group: unit\n  filter: package(temps-a)\n",
                "- group: docker\n  archive: docker-tests\n  filter: package(temps-c)\n",
            )
        )
        self.assertEqual(self.workspace.check(), [])

    def test_an_allow_listed_binary_passes(self) -> None:
        self.use_filter("package(temps-a) and kind(=lib)")
        self.workspace.write_allowlist(
            """\
            [[binary]]
            id = "temps-a::it_one"
            reason = "needs a live service"
            """
        )
        self.assertEqual(self.workspace.check(), [])

    def test_an_allow_list_entry_without_a_reason_fails(self) -> None:
        self.use_filter("package(temps-a) and kind(=lib)")
        self.workspace.write_allowlist('[[binary]]\nid = "temps-a::it_one"\nreason = " "\n')
        self.assertIn("allow-list entry temps-a::it_one has no reason", self.workspace.check())

    def test_an_allow_list_entry_for_a_covered_binary_fails(self) -> None:
        self.use_filter("package(temps-a)")
        self.workspace.write_allowlist('[[binary]]\nid = "temps-a::it_one"\nreason = "stale"\n')
        self.assertIn("allow-list entry temps-a::it_one is run by a CI group now; remove it", self.workspace.check())

    def test_an_allow_list_entry_for_a_missing_binary_fails(self) -> None:
        self.use_filter("package(temps-a)")
        self.workspace.write_allowlist('[[binary]]\nid = "temps-gone::it"\nreason = "stale"\n')
        self.assertIn("allow-list entry temps-gone::it names no test binary with tests; remove it", self.workspace.check())

    def test_a_filter_naming_a_missing_package_or_binary_fails(self) -> None:
        self.use_filter("package(temps-a) or package(temps-renamed) or binary(it_gone)")
        problems = self.workspace.check()
        self.assertIn("unit: package(temps-renamed) matches no workspace package", problems)
        self.assertIn("unit: binary(it_gone) matches no target", problems)

    def test_a_group_that_selects_nothing_fails(self) -> None:
        self.workspace.write_workflow(
            workflow(
                "- group: unit\n  filter: package(temps-a)\n",
                "- group: empty\n  filter: package(temps-a) and kind(=bin)\n",
            )
        )
        self.assertIn("empty: selects no test binary, so it would never start", self.workspace.check())

    def test_a_group_without_a_filter_fails_closed(self) -> None:
        self.workspace.write_workflow(workflow("- group: unit\n  test_threads: 2\n"))
        with self.assertRaisesRegex(coverage.CoverageError, "group unit .* has no filter"):
            self.workspace.check()

    def test_a_workflow_with_no_groups_fails_closed(self) -> None:
        self.workspace.write_workflow("jobs: {}\n")
        with self.assertRaisesRegex(coverage.CoverageError, "no test groups"):
            self.workspace.check()

    def test_an_unreadable_allow_list_fails_closed(self) -> None:
        self.use_filter("package(temps-a)")
        self.workspace.write_allowlist("[[binary]\n")
        with self.assertRaisesRegex(coverage.CoverageError, "cannot parse"):
            self.workspace.check()


class MeasureTests(unittest.TestCase):
    @staticmethod
    def suite(tests: dict[str, bool]) -> dict:
        return {
            "temps-a": {
                "package-name": "temps-a",
                "binary-name": "temps_a",
                "kind": "lib",
                "testcases": {name: {"ignored": ignored} for name, ignored in tests.items()},
            }
        }

    def test_a_test_excluded_by_name_is_not_covered(self) -> None:
        selection = coverage.Selection("unit", coverage.Filterset("package(temps-a) and not test(live)"))
        [row] = coverage.measure([selection], {"default": self.suite({"mock": False, "live": False})})
        self.assertEqual(row.covered, 1)
        self.assertEqual(row.uncovered, ["live"])

    def test_an_ignored_test_needs_a_selection_asking_for_ignored_tests(self) -> None:
        plain = coverage.Selection("unit", coverage.Filterset("package(temps-a)"))
        [row] = coverage.measure([plain], {"default": self.suite({"slow": True})})
        self.assertEqual((row.covered, row.uncovered), (0, ["slow"]))
        ignored = coverage.Selection("slow-job", coverage.Filterset("package(temps-a)"), ignored=True)
        [row] = coverage.measure([plain, ignored], {"default": self.suite({"slow": True})})
        self.assertEqual((row.covered, row.uncovered), (1, []))

    def test_a_selection_only_sees_the_archive_it_runs(self) -> None:
        docker = coverage.Selection("docker", coverage.Filterset("package(temps-a)"), "docker-tests")
        lists = {"default": self.suite({"plain": False}), "docker-tests": self.suite({"plain": False, "gated": False})}
        [row] = coverage.measure([docker], lists)
        self.assertEqual(row.uncovered, [])
        self.assertEqual(sorted(row.tests), ["gated", "plain"])
        [row] = coverage.measure([docker], {"default": self.suite({"plain": False})})
        self.assertEqual(row.uncovered, ["plain"])


def pull_request_branches(workflow_text: str) -> list[str] | None:
    """The workflow's `on.pull_request.branches`, or None when it has no filter."""
    lines = [line.split(" #")[0].rstrip() for line in workflow_text.splitlines()]
    lines = [line for line in lines if line.strip() and not line.lstrip().startswith("#")]
    start = lines.index("on:") + 1
    event_indent: int | None = None
    in_pull_request = False
    for index in range(start, len(lines)):
        line = lines[index]
        indent = len(line) - len(line.lstrip())
        if indent == 0:
            break
        event_indent = event_indent if event_indent is not None else indent
        if indent == event_indent:
            if in_pull_request:
                break
            in_pull_request = line.strip().rstrip(":") == "pull_request"
        elif in_pull_request and line.strip().startswith("branches:"):
            value = line.split(":", 1)[1].strip()
            if value:
                return [item.strip().strip("'\"") for item in value.strip("[]").split(",")]
            items = []
            for item in lines[index + 1 :]:
                if not item.strip().startswith("- "):
                    break
                items.append(item.strip()[2:].strip().strip("'\""))
            return items
    return None


def branch_accepted(branch: str, patterns: list[str] | None) -> bool:
    """GitHub's branch filter: `*` stops at a slash, `**` does not."""
    if patterns is None:
        return True
    return any(
        re.fullmatch(
            "".join(
                ".*" if part == "**" else "[^/]*" if part == "*" else re.escape(part)
                for part in re.split(r"(\*\*|\*)", pattern)
            ),
            branch,
        )
        for pattern in patterns
    )


class PullRequestFilterTests(unittest.TestCase):
    """Rust Tests must run on pull requests into main, and no others."""

    ACCEPTED = ("main",)
    REJECTED = ("develop", "refactor/crates-refactor-wave3", "main-backup")

    def assert_filter_is_right(self, patterns: list[str] | None) -> None:
        for branch in self.ACCEPTED:
            self.assertTrue(branch_accepted(branch, patterns), f"{branch} rejected by {patterns}")
        for branch in self.REJECTED:
            self.assertFalse(branch_accepted(branch, patterns), f"{branch} accepted by {patterns}")

    def test_rust_tests_runs_on_the_right_pull_requests(self) -> None:
        text = (coverage.REPO_ROOT / coverage.WORKFLOWS_DIR / "rust-tests.yml").read_text(encoding="utf-8")
        self.assert_filter_is_right(pull_request_branches(text))

    def test_a_filter_wider_than_main_is_caught(self) -> None:
        with self.assertRaises(AssertionError):
            self.assert_filter_is_right(["main", "refactor/*"])

    def test_no_filter_is_caught(self) -> None:
        with self.assertRaises(AssertionError):
            self.assert_filter_is_right(None)

    def test_the_filter_is_read_in_every_yaml_form(self) -> None:
        flow = "on:\n  push:\n    branches: [other]\n  pull_request:\n    # note\n    branches: [main, 'a/*']\n"
        self.assertEqual(pull_request_branches(flow), ["main", "a/*"])
        block = "on:\n  pull_request:\n    branches:\n      - main\n      - 'a/*'\n  push:\n"
        self.assertEqual(pull_request_branches(block), ["main", "a/*"])
        self.assertIsNone(pull_request_branches("on:\n  pull_request:\n  push:\n    branches: [main]\njobs:\n"))


class DatabaseGuardTests(unittest.TestCase):
    def test_no_test_skips_on_an_empty_database_url(self) -> None:
        """The migrations group empties this variable, and `is_ok()` calls empty "set"."""
        guard = re.compile(r'var\("TEMPS_TEST_DATABASE_URL"\)\s*\.is_ok\(\)')
        offenders = [
            str(path.relative_to(coverage.REPO_ROOT))
            for path in sorted((coverage.REPO_ROOT / "crates").rglob("*.rs"))
            if guard.search(path.read_text(encoding="utf-8"))
        ]
        self.assertEqual(offenders, [])


if __name__ == "__main__":
    unittest.main()
