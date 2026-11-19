#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Report flaky tests from nextest JUnit, and learn which ones to serialise.

Why this exists: CI retries a failing test twice (`--profile ci`,
.config/nextest.toml) so that one unlucky timing does not fail a pull request.
A retry that passes would otherwise be invisible, and the flake would stay in
the suite until it failed three times in a row. This script keeps it visible
and, over time, takes the commonest cause off the table: most flakes here are
tests that share something outside their process (a port, a container name, a
Docker volume) and lose a race when nextest runs them alongside each other.

  report  Annotate every test that only passed on a retry, and list it in the
          job summary. Never fails the job: the test passed.
  learn   Read JUnit reports from many runs and write the tests that were
          flaky in at least --min-runs of them, as JSON.
  apply   Append the learned list to .config/nextest.toml as an override
          putting those tests in the `learned-serial` group (one at a time).

Both the JUnit reports and the learned list are inputs from other runs, so
`apply` accepts only binary IDs and test names made of the characters Rust
and Cargo use. Anything else is dropped, so the worst a bad list can do is run
some tests one at a time.

Ideas from Bun's CI, which learns the opposite list (tests safe to run in
parallel) from its last few hundred builds.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import xml.etree.ElementTree as ElementTree
from collections.abc import Iterable, Iterator
from datetime import datetime, timezone
from pathlib import Path

BINARY_ID = re.compile(r"^[A-Za-z0-9_-]+(?:::[A-Za-z0-9_/-]+)?$")
TEST_NAME = re.compile(r"^[A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*$")
# Bounds the override, and with it the cost of a list gone wrong.
MAX_LEARNED = 200
GROUP = "learned-serial"


def _cases(junit: Path) -> Iterator[tuple[str, str, ElementTree.Element]]:
    """Yield (binary_id, test, testcase element) for every case in a report."""
    root = ElementTree.parse(junit).getroot()
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            binary_id = case.get("classname") or suite.get("name") or ""
            yield binary_id, case.get("name") or "", case


def flaky_tests(junit: Path) -> list[tuple[str, str, int]]:
    """Return (binary_id, test, failed attempts) for tests that passed on a retry."""
    found = []
    for binary_id, test, case in _cases(junit):
        failed_attempts = len(case.findall("flakyFailure")) + len(case.findall("flakyError"))
        if failed_attempts and case.find("failure") is None and case.find("error") is None:
            found.append((binary_id, test, failed_attempts))
    return found


def failed_tests(junit: Path) -> list[tuple[str, str]]:
    """Return (binary_id, test) for tests that failed every attempt."""
    return [
        (binary_id, test)
        for binary_id, test, case in _cases(junit)
        if case.find("failure") is not None or case.find("error") is not None
    ]


def _data(value: str) -> str:
    """Escape a workflow-command message."""
    return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def _property(value: str) -> str:
    """Escape a workflow-command property such as `title`."""
    return _data(value).replace(":", "%3A").replace(",", "%2C")


def report(junit_files: Iterable[Path], group: str) -> int:
    flaky: list[tuple[str, str, int]] = []
    failed: list[tuple[str, str]] = []
    for junit in junit_files:
        if not junit.is_file():
            print(f"No JUnit report at {junit}; the run wrote none (nothing ran, or it never started).")
            continue
        try:
            flaky += flaky_tests(junit)
            failed += failed_tests(junit)
        except (OSError, ElementTree.ParseError) as error:
            print(f"::warning title=Unreadable JUnit report::{_data(f'{junit}: {error}')}")
    for binary_id, test, attempts in flaky:
        print(
            f"::warning title={_property(f'Flaky test in {group}')}::"
            f"{_data(f'{binary_id} {test} failed {attempts} time(s) before passing on a retry')}"
        )
    lines = [f"### Retries in {group}", ""]
    if not flaky and not failed:
        lines.append("No test needed a retry.")
    if flaky:
        lines += [
            f"{len(flaky)} test(s) passed only on a retry. They did not fail this job; "
            "tests flaky in several runs are serialised by the learned-serial job.",
            "",
            "| Binary | Test | Failed attempts |",
            "| --- | --- | --- |",
        ]
        lines += [f"| `{b}` | `{t}` | {n} |" for b, t, n in flaky]
    if failed:
        lines += ["", f"{len(failed)} test(s) failed every attempt:", ""]
        lines += [f"- `{b}` `{t}`" for b, t in failed]
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with Path(summary).open("a", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
    print("\n".join(lines))
    return 0


def learn(junit_dir: Path, min_runs: int) -> dict:
    """Count, per test, the runs it was flaky in. Each run is a subdirectory."""
    counts: dict[tuple[str, str], set[str]] = {}
    runs = sorted(p for p in junit_dir.iterdir() if p.is_dir()) if junit_dir.is_dir() else []
    for run in runs:
        for junit in sorted(run.rglob("*.xml")):
            try:
                for binary_id, test, _ in flaky_tests(junit):
                    counts.setdefault((binary_id, test), set()).add(run.name)
            except (OSError, ElementTree.ParseError) as error:
                print(f"skipping unreadable {junit}: {error}", file=sys.stderr)
    learned = sorted(
        (
            {"binary_id": b, "test": t, "flaky_runs": len(seen)}
            for (b, t), seen in counts.items()
            if len(seen) >= min_runs and BINARY_ID.fullmatch(b) and TEST_NAME.fullmatch(t)
        ),
        key=lambda entry: (-entry["flaky_runs"], entry["binary_id"], entry["test"]),
    )[:MAX_LEARNED]
    return {
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "runs": len(runs),
        "min_runs": min_runs,
        "tests": learned,
    }


def override(learned: dict) -> str | None:
    """Return the nextest override for a learned list, or None when it is empty."""
    entries = learned.get("tests", []) if isinstance(learned, dict) else []
    terms = []
    for entry in entries[:MAX_LEARNED]:
        if not isinstance(entry, dict):
            continue
        binary_id, test = entry.get("binary_id"), entry.get("test")
        if isinstance(binary_id, str) and isinstance(test, str) and BINARY_ID.fullmatch(binary_id) and TEST_NAME.fullmatch(test):
            terms.append(f"(binary_id(={binary_id}) & test(={test}))")
    if not terms:
        return None
    joined = "\n  | ".join(terms)
    return (
        "\n# Appended by .github/scripts/nextest-flaky.py apply: tests flaky in\n"
        "# recent CI runs, learned by .github/workflows/nextest-learned-serial.yml.\n"
        "[[profile.default.overrides]]\n"
        f"filter = '''\n  {joined}\n'''\n"
        f"test-group = '{GROUP}'\n"
    )


def apply(learned_file: Path, config: Path) -> int:
    try:
        learned = json.loads(learned_file.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        print(f"::notice title=No learned serial list::{_data(f'{learned_file}: {error}')}")
        return 0
    text = override(learned)
    if text is None:
        print("The learned serial list is empty; nothing to serialise.")
        return 0
    with config.open("a", encoding="utf-8") as handle:
        handle.write(text)
    print(f"Running these tests one at a time ({config}):{text}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)

    report_cmd = commands.add_parser("report", help="annotate tests that passed on a retry")
    report_cmd.add_argument("--group", required=True, help="CI group the reports came from")
    report_cmd.add_argument("junit", nargs="*", type=Path)

    learn_cmd = commands.add_parser("learn", help="derive the serial list from many runs")
    learn_cmd.add_argument("--junit-dir", required=True, type=Path, help="one subdirectory per run")
    learn_cmd.add_argument("--min-runs", type=int, default=2, help="flaky in at least this many runs")
    learn_cmd.add_argument("--out", required=True, type=Path)

    apply_cmd = commands.add_parser("apply", help="serialise the learned tests")
    apply_cmd.add_argument("--learned", required=True, type=Path)
    apply_cmd.add_argument("--config", type=Path, default=Path(".config/nextest.toml"))

    args = parser.parse_args(argv)
    if args.command == "report":
        return report(args.junit, args.group)
    if args.command == "learn":
        result = learn(args.junit_dir, max(1, args.min_runs))
        args.out.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(f"{len(result['tests'])} test(s) flaky in >= {result['min_runs']} of {result['runs']} run(s):")
        for entry in result["tests"]:
            print(f"  {entry['flaky_runs']:3}  {entry['binary_id']}  {entry['test']}")
        return 0
    return apply(args.learned, args.config)


if __name__ == "__main__":
    sys.exit(main())
