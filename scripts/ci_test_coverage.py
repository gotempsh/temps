#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Fail when a Rust test binary is run by no CI job.

CI selects tests with nextest filtersets written by hand, so a test nobody
listed never runs and nothing says so: a whole binary can go red for months
while every check stays green. This script reads the selections from the
workflows instead of retyping them and compares them with the workspace.

`--check` (the default; what CI runs) needs only `cargo metadata --no-deps`, so
it compiles nothing and takes about a second. It works on whole binaries:

  - a target counts when its source tree contains a test attribute, cfg-gated
    or not, because a static scan cannot tell which cfgs CI enables;
  - a filterset `test(..)` predicate is unknown without test names, so it may
    match: a binary is covered when some selection may pick a test from it;
  - a test file gated by `#![cfg(feature = "docker-tests")]` has no tests in the
    default archive, so only a selection running the docker-tests archive covers it.

It fails when:

  - a binary with tests is selected by nothing and is not on the allow-list
    (`scripts/ci-test-coverage-allowlist.toml`, each entry with its reason);
  - an allow-list entry is stale: it has no reason, names no binary, or names a
    binary that a selection now covers;
  - a selection names a package or binary that does not exist, or selects
    nothing, so a renamed crate cannot silently empty a group.

`--measure LIST.json [--docker-tests-list LIST.json]` needs the output of
`cargo nextest list --workspace --all-targets --message-format json`, which
means a full test build, and reports per crate and binary how many tests each
selection really picks. It is the tool for a human; `--check` is the guard.
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from fnmatch import fnmatchcase
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RUST_TESTS_WORKFLOW = Path(".github/workflows/rust-tests.yml")
WORKFLOWS_DIR = Path(".github/workflows")
ALLOWLIST = Path("scripts/ci-test-coverage-allowlist.toml")

LIBRARY_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}
# `--all-targets` builds these in test mode whatever their `test` flag says.
ALWAYS_BUILT_KINDS = {"example", "bench"}
TEST_ATTRIBUTE = re.compile(r"#\[\s*(?:[A-Za-z_]\w*::)*(?:test|rstest|test_case|test_log)\b")
MODULE_DECLARATION = re.compile(
    r'(?:#\[path\s*=\s*"(?P<path>[^"]+)"\]\s*)?(?:pub(?:\([^)]*\))?\s+)?\bmod\s+(?P<name>\w+)\s*;'
)


# A whole-file gate on the one feature CI builds a separate archive for.
DOCKER_TESTS_GATE = re.compile(r'^#!\[cfg\(feature\s*=\s*"docker-tests"\)\]', re.MULTILINE)


class CoverageError(Exception):
    """The inputs could not be read; never treated as "no gaps"."""


# ---------------------------------------------------------------------------
# The nextest filterset language
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class TestBinary:
    """A nextest test binary."""

    package: str
    name: str
    kind: str
    # Whether the binary only has tests in the docker-tests archive.
    docker_tests_only: bool = False

    @property
    def binary_id(self) -> str:
        if self.kind == "lib":
            return self.package
        if self.kind == "test":
            return f"{self.package}::{self.name}"
        return f"{self.package}::{self.kind}/{self.name}"


TOKEN = re.compile(
    r"""\s*(?:
        (?P<predicate>[a-z_]+)\(\s*(?P<argument>(?:/(?:[^/\\]|\\.)*/|[^()]*?))\s*\)
      | (?P<operator>and\b|or\b|not\b|&|\||\+|!|-)
      | (?P<open>\()
      | (?P<close>\))
    )""",
    re.VERBOSE,
)
DEFAULT_MATCHER = {"test": "~", "package": "#", "binary": "#", "binary_id": "#", "deps": "#", "rdeps": "#", "kind": "="}


def matches(matcher_text: str, default: str, value: str) -> bool:
    kind, text = default, matcher_text
    if matcher_text[:1] in {"=", "~", "#"}:
        kind, text = matcher_text[0], matcher_text[1:]
    elif matcher_text[:1] == "/" and matcher_text[-1:] == "/" and len(matcher_text) > 1:
        return re.search(matcher_text[1:-1], value) is not None
    if kind == "=":
        return value == text
    if kind == "~":
        return text in value
    return fnmatchcase(value, text)


class Filterset:
    """A parsed filterset, evaluated with Kleene logic so `None` means "may"."""

    def __init__(self, text: str) -> None:
        self.text = " ".join(text.split())
        self.tokens = self._tokenize(self.text)
        self.position = 0
        self.tree = self._parse_or()
        if self.position != len(self.tokens):
            raise CoverageError(f"unexpected {self.tokens[self.position]} in filterset: {self.text}")

    @staticmethod
    def _tokenize(text: str) -> list[tuple[str, str] | tuple[str, str, str]]:
        tokens: list[tuple[str, str] | tuple[str, str, str]] = []
        position = 0
        while position < len(text):
            if text[position:].strip() == "":
                break
            found = TOKEN.match(text, position)
            if found is None:
                raise CoverageError(f"cannot parse filterset at {text[position:]!r}: {text}")
            position = found.end()
            if found.group("predicate"):
                tokens.append(("predicate", found.group("predicate"), found.group("argument")))
            elif found.group("operator"):
                symbol = found.group("operator")
                tokens.append(("op", {"&": "and", "|": "or", "+": "or", "!": "not"}.get(symbol, symbol)))
            elif found.group("open"):
                tokens.append(("open", "("))
            else:
                tokens.append(("close", ")"))
        return tokens

    def _peek(self, *ops: str) -> bool:
        if self.position >= len(self.tokens):
            return False
        token = self.tokens[self.position]
        return token[0] == "op" and token[1] in ops

    def _parse_or(self):
        left = self._parse_and()
        while self._peek("or"):
            self.position += 1
            left = ("or", left, self._parse_and())
        return left

    def _parse_and(self):
        left = self._parse_unary()
        while self._peek("and", "-"):
            difference = self.tokens[self.position][1] == "-"
            self.position += 1
            right = self._parse_unary()
            left = ("and", left, ("not", right) if difference else right)
        return left

    def _parse_unary(self):
        if self._peek("not"):
            self.position += 1
            return ("not", self._parse_unary())
        if self.position >= len(self.tokens):
            raise CoverageError(f"filterset ends early: {self.text}")
        token = self.tokens[self.position]
        self.position += 1
        if token[0] == "open":
            inner = self._parse_or()
            if self.position >= len(self.tokens) or self.tokens[self.position][0] != "close":
                raise CoverageError(f"unbalanced parentheses in filterset: {self.text}")
            self.position += 1
            return inner
        if token[0] == "predicate":
            return ("predicate", token[1], token[2])
        raise CoverageError(f"unexpected {token} in filterset: {self.text}")

    def named(self, predicate: str) -> list[str]:
        """Every matcher given to `predicate`, for checking they still exist."""

        def walk(node) -> list[str]:
            if node[0] == "predicate":
                return [node[2]] if node[1] == predicate else []
            return [found for child in node[1:] for found in walk(child)]

        return walk(self.tree)

    def evaluate(self, binary: TestBinary, test: str | None = None) -> bool | None:
        """True or False, or None when the answer depends on an unknown test."""

        def walk(node) -> bool | None:
            if node[0] == "predicate":
                return self._predicate(node[1], node[2], binary, test)
            if node[0] == "not":
                value = walk(node[1])
                return None if value is None else not value
            left, right = walk(node[1]), walk(node[2])
            if node[0] == "and":
                if left is False or right is False:
                    return False
                return None if left is None or right is None else True
            if left is True or right is True:
                return True
            return None if left is None or right is None else False

        return walk(self.tree)

    def _predicate(self, name: str, argument: str, binary: TestBinary, test: str | None) -> bool | None:
        default = DEFAULT_MATCHER.get(name, "=")
        if name == "all":
            return True
        if name == "none":
            return False
        if name == "package":
            return matches(argument, default, binary.package)
        if name == "binary":
            return matches(argument, default, binary.name)
        if name == "binary_id":
            return matches(argument, default, binary.binary_id)
        if name == "kind":
            return matches(argument, default, binary.kind)
        if name == "test":
            return None if test is None else matches(argument, default, test)
        raise CoverageError(f"unsupported predicate {name}() in filterset: {self.text}")


# ---------------------------------------------------------------------------
# Reading what CI runs
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Selection:
    """One CI job or matrix group and the tests it selects."""

    name: str
    filterset: Filterset
    archive: str = "default"
    # `cargo test -- --ignored`, or `nextest --run-ignored only`.
    ignored: bool = False


def _matrix_entries(text: str) -> list[dict[str, str]]:
    """The `- group:` entries of a workflow, with their `filter` and `archive`.

    A line parser so CI needs no PyYAML. It fails closed in `selections`: a
    group without a filter is an error, not an empty selection.
    """
    entries: list[dict[str, str]] = []
    lines = text.splitlines()
    index = 0
    current: dict[str, str] | None = None
    item_indent = 0
    while index < len(lines):
        line = lines[index]
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        index += 1
        if stripped.startswith("- group:"):
            current = {"group": stripped.split(":", 1)[1].strip()}
            entries.append(current)
            item_indent = indent
            continue
        if current is None or not stripped or stripped.startswith("#"):
            continue
        if indent <= item_indent:
            current = None
            continue
        key, separator, value = stripped.partition(":")
        if not separator or key not in {"filter", "archive"}:
            continue
        value = value.strip()
        if value in {">-", ">", "|", "|-"}:
            block: list[str] = []
            while index < len(lines):
                following = lines[index]
                if following.strip() and len(following) - len(following.lstrip()) <= indent:
                    break
                block.append(following.strip())
                index += 1
            value = " ".join(part for part in block if part)
        current[key] = value
    return entries


def _commands(text: str) -> str:
    """The text with comment lines dropped and continuation lines joined."""
    kept = [line for line in text.splitlines() if not line.lstrip().startswith("#")]
    return re.sub(r"\\\n\s*", " ", "\n".join(kept))


def _inline_nextest_runs(text: str) -> list[tuple[str, bool]]:
    """Literal `cargo nextest run ... -E "<filterset>"`, skipping templated ones."""
    runs: list[tuple[str, bool]] = []
    for found in re.finditer(r"cargo nextest run\b(?P<args>[^\n]*)", _commands(text)):
        args = found.group("args")
        expression = re.search(r"""-E\s+(?:"([^"]+)"|'([^']+)')""", args)
        if expression is None:
            continue
        filterset = expression.group(1) or expression.group(2)
        if "${{" in filterset or "$filter" in filterset:
            continue
        runs.append((filterset, "--run-ignored only" in args))
    return runs


def _cargo_test_runs(text: str) -> list[tuple[str, bool]]:
    """`cargo test -p X [--lib|--test Y]` lines, as filtersets."""
    runs: list[tuple[str, bool]] = []
    for found in re.finditer(r"\bcargo test\b(?P<args>[^\n|;&]*)", _commands(text)):
        try:
            words = shlex.split(found.group("args"))
        except ValueError:
            continue
        before, _, after = _partition_words(words)
        packages = _option_values(before, "-p", "--package")
        tests = _option_values(before, "--test")
        if not packages or "--no-run" in before:
            continue
        for package in packages:
            scope = f"package(={package})"
            if "--lib" in before:
                runs.append((f"{scope} and kind(=lib)", "--ignored" in after))
            if tests:
                alternatives = " or ".join(f"binary(={test})" for test in tests)
                runs.append((f"{scope} and ({alternatives})", "--ignored" in after))
            if "--lib" not in before and not tests:
                runs.append((scope, "--ignored" in after))
    return runs


def _partition_words(words: list[str]) -> tuple[list[str], str, list[str]]:
    if "--" in words:
        split = words.index("--")
        return words[:split], "--", words[split + 1 :]
    return words, "", []


def _option_values(words: list[str], *names: str) -> list[str]:
    return [words[index + 1] for index, word in enumerate(words[:-1]) if word in names]


def selections(root: Path) -> list[Selection]:
    """Every test selection the workflows make, read from the workflows."""
    workflow = root / RUST_TESTS_WORKFLOW
    if not workflow.is_file():
        raise CoverageError(f"workflow not found: {workflow}")
    found: list[Selection] = []
    groups = _matrix_entries(workflow.read_text(encoding="utf-8"))
    if not groups:
        raise CoverageError(f"no test groups found in {workflow}; has its matrix format changed?")
    for group in groups:
        if "filter" not in group:
            raise CoverageError(f"group {group['group']} in {workflow} has no filter")
        found.append(Selection(group["group"], Filterset(group["filter"]), group.get("archive", "default")))
    for source in sorted((root / WORKFLOWS_DIR).glob("*.yml")):
        text = source.read_text(encoding="utf-8")
        # The docker-tests archive only holds temps-providers and temps-backup,
        # the only crates selected this way with that archive.
        for filterset, ignored in _inline_nextest_runs(text):
            archive = "docker-tests" if "test-binaries-docker-tests" in text and "temps-backup" in filterset else "default"
            found.append(Selection(f"{source.stem}: {filterset}", Filterset(filterset), archive, ignored))
        for filterset, ignored in _cargo_test_runs(text):
            found.append(Selection(f"{source.stem}: {filterset}", Filterset(filterset), "default", ignored))
    return found


def load_allowlist(root: Path) -> dict[str, str]:
    path = root / ALLOWLIST
    if not path.is_file():
        raise CoverageError(f"allow-list not found: {path}")
    try:
        parsed = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as error:
        raise CoverageError(f"cannot parse {path}: {error}") from error
    allowed: dict[str, str] = {}
    for entry in parsed.get("binary", []):
        binary_id = entry.get("id", "")
        if binary_id in allowed:
            raise CoverageError(f"{binary_id} is listed twice in {path}")
        allowed[binary_id] = entry.get("reason", "")
    return allowed


# ---------------------------------------------------------------------------
# --check: whole binaries, from the manifests
# ---------------------------------------------------------------------------


def load_metadata(root: Path) -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise CoverageError(f"cargo metadata failed in {root}:\n{result.stderr.strip()}")
    return json.loads(result.stdout)


def _module_files(entry: Path) -> set[Path]:
    """The files of a target's module tree, starting from its root file.

    Errs towards more files: a `mod x;` inside an inline module does not sit
    where the file's own modules do, so an unresolved one is searched for.
    """
    seen: set[Path] = set()
    pending = [(entry, entry.parent)]
    while pending:
        path, module_dir = pending.pop()
        if path in seen or not path.is_file():
            continue
        seen.add(path)
        for found in MODULE_DECLARATION.finditer(path.read_text(encoding="utf-8", errors="replace")):
            name = found.group("name")
            if found.group("path"):
                candidates = [path.parent / found.group("path")]
            else:
                candidates = [module_dir / f"{name}.rs", module_dir / name / "mod.rs"]
            resolved = next((candidate for candidate in candidates if candidate.is_file()), None)
            if resolved is None:
                nested = sorted(module_dir.rglob(f"{name}.rs")) + sorted(module_dir.rglob(f"{name}/mod.rs"))
                resolved = nested[0] if nested else None
            if resolved is None:
                continue
            child_dir = resolved.parent if resolved.name == "mod.rs" else resolved.parent / resolved.stem
            pending.append((resolved, child_dir))
    return seen


def has_tests(entry: Path) -> bool:
    return any(
        TEST_ATTRIBUTE.search(path.read_text(encoding="utf-8", errors="replace"))
        for path in _module_files(entry)
    )


def binaries_from_metadata(metadata: dict) -> list[TestBinary]:
    binaries: list[TestBinary] = []
    members = set(metadata["workspace_members"])
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        for target in package["targets"]:
            kind = "lib" if target["kind"][0] in LIBRARY_KINDS else target["kind"][0]
            if kind not in ALWAYS_BUILT_KINDS and not target.get("test", False):
                continue
            if kind == "custom-build" or not has_tests(Path(target["src_path"])):
                continue
            name = target["name"].replace("-", "_") if kind == "lib" else target["name"]
            source = Path(target["src_path"]).read_text(encoding="utf-8", errors="replace")
            binaries.append(TestBinary(package["name"], name, kind, DOCKER_TESTS_GATE.search(source) is not None))
    return binaries


def may_select(selection: Selection, binary: TestBinary) -> bool:
    if binary.docker_tests_only and selection.archive != "docker-tests":
        return False
    return selection.filterset.evaluate(binary) is not False


def check(root: Path, metadata: dict | None = None) -> list[str]:
    """Every problem found; an empty list means CI runs every binary."""
    metadata = metadata if metadata is not None else load_metadata(root)
    binaries = binaries_from_metadata(metadata)
    all_selections = selections(root)
    allowed = load_allowlist(root)
    problems: list[str] = []

    packages = {package["name"] for package in metadata["packages"]}
    target_names = {target["name"] for package in metadata["packages"] for target in package["targets"]}
    target_names |= {name.replace("-", "_") for name in target_names}
    for selection in all_selections:
        for matcher in selection.filterset.named("package"):
            if not any(matches(matcher, DEFAULT_MATCHER["package"], name) for name in packages):
                problems.append(f"{selection.name}: package({matcher}) matches no workspace package")
        for matcher in selection.filterset.named("binary"):
            if not any(matches(matcher, DEFAULT_MATCHER["binary"], name) for name in target_names):
                problems.append(f"{selection.name}: binary({matcher}) matches no target")
        if not any(may_select(selection, binary) for binary in binaries):
            problems.append(f"{selection.name}: selects no test binary, so it would never start")

    known = {binary.binary_id for binary in binaries}
    selected = {
        binary.binary_id
        for binary in binaries
        if any(may_select(selection, binary) for selection in all_selections)
    }
    for binary_id, reason in sorted(allowed.items()):
        if not reason.strip():
            problems.append(f"allow-list entry {binary_id} has no reason")
        if binary_id not in known:
            problems.append(f"allow-list entry {binary_id} names no test binary with tests; remove it")
        elif binary_id in selected:
            problems.append(f"allow-list entry {binary_id} is run by a CI group now; remove it")
    for binary_id in sorted(known - selected - set(allowed)):
        problems.append(
            f"{binary_id} is run by no CI group. Add it to a group in {RUST_TESTS_WORKFLOW}, "
            f"or to {ALLOWLIST} with the reason it cannot run there."
        )
    return problems


# ---------------------------------------------------------------------------
# --measure: per test, from `cargo nextest list`
# ---------------------------------------------------------------------------


@dataclass
class BinaryCoverage:
    package: str
    binary_id: str
    # Test name to the selections that run it; empty means nothing does.
    tests: dict[str, set[str]] = field(default_factory=dict)
    ignored: set[str] = field(default_factory=set)

    @property
    def covered(self) -> int:
        return sum(1 for groups in self.tests.values() if groups)

    @property
    def uncovered(self) -> list[str]:
        return sorted(test for test, groups in self.tests.items() if not groups)

    @property
    def groups(self) -> set[str]:
        return {group for groups in self.tests.values() for group in groups}


def load_nextest_list(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))["rust-suites"]
    except (OSError, ValueError, KeyError) as error:
        raise CoverageError(f"cannot read a nextest list from {path}: {error}") from error


def measure(all_selections: list[Selection], lists: dict[str, dict]) -> list[BinaryCoverage]:
    """Per binary, the selections running each test. `lists` is keyed by archive.

    A selection only sees the archive it runs, because a feature-gated test
    exists in one archive and not the other. An ignored test only counts as run
    by a selection that asks for ignored tests.
    """
    merged: dict[str, BinaryCoverage] = {}
    for archive, suites in lists.items():
        relevant = [selection for selection in all_selections if selection.archive == archive]
        for binary_id, suite in suites.items():
            binary = TestBinary(suite["package-name"], suite["binary-name"], suite["kind"])
            coverage = merged.setdefault(binary_id, BinaryCoverage(suite["package-name"], binary_id))
            for test, details in suite["testcases"].items():
                ignored = details.get("ignored", False)
                if ignored:
                    coverage.ignored.add(test)
                coverage.tests.setdefault(test, set()).update(
                    selection.name
                    for selection in relevant
                    if selection.ignored == ignored and selection.filterset.evaluate(binary, test)
                )
    return sorted(merged.values(), key=lambda item: item.binary_id)


def format_measure(rows: list[BinaryCoverage], verbose: bool) -> str:
    lines = [f"{'binary':<62} {'total':>6} {'in CI':>6} {'none':>6} {'ignored':>8}  groups"]
    totals = [0, 0, 0]
    shown = [row for row in rows if row.tests]
    for row in shown:
        total, covered = len(row.tests), row.covered
        totals = [totals[0] + total, totals[1] + covered, totals[2] + len(row.ignored)]
        groups = ", ".join(sorted({name.split(":")[0] for name in row.groups}))
        lines.append(
            f"{row.binary_id:<62} {total:>6} {covered:>6} {total - covered:>6} {len(row.ignored):>8}  {groups}"
        )
        if verbose:
            lines.extend(f"    not run: {test}" for test in row.uncovered)
    lines.append(f"{'TOTAL':<62} {totals[0]:>6} {totals[1]:>6} {totals[0] - totals[1]:>6} {totals[2]:>8}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=REPO_ROOT)
    parser.add_argument("--check", action="store_true", help="the fast whole-binary guard (default)")
    parser.add_argument("--metadata", type=Path, help="read `cargo metadata --no-deps` output from this file")
    parser.add_argument("--measure", type=Path, help="report per test from a `cargo nextest list` JSON file")
    parser.add_argument("--docker-tests-list", type=Path, help="the same list built with the docker-tests features")
    parser.add_argument("--verbose", action="store_true", help="with --measure, name each test that is not run")
    args = parser.parse_args(argv)
    try:
        if args.measure:
            lists = {"default": load_nextest_list(args.measure)}
            if args.docker_tests_list:
                lists["docker-tests"] = load_nextest_list(args.docker_tests_list)
            print(format_measure(measure(selections(args.root), lists), args.verbose))
            return 0
        metadata = json.loads(args.metadata.read_text(encoding="utf-8")) if args.metadata else None
        problems = check(args.root, metadata)
    except CoverageError as error:
        print(f"ci_test_coverage: {error}", file=sys.stderr)
        return 2
    for problem in problems:
        print(f"ci_test_coverage: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("ci_test_coverage: every test binary is run by a CI group or allow-listed with a reason.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
