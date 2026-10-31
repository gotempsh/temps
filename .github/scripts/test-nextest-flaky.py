#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for nextest-flaky.py: reading JUnit, learning and applying the list."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import tomllib
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent.parent

spec = importlib.util.spec_from_file_location("nextest_flaky", SCRIPTS / "nextest-flaky.py")
assert spec is not None and spec.loader is not None
flaky = importlib.util.module_from_spec(spec)
spec.loader.exec_module(flaky)

# The shape nextest writes: one testsuite per binary, retries as
# flakyFailure (passed in the end) or rerunFailure (failed every attempt).
JUNIT = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="1" errors="0">
  <testsuite name="temps-proxy" tests="3" disabled="0" errors="0" failures="1">
    <testcase name="handler::tests::steady" classname="temps-proxy" time="0.1"/>
    <testcase name="handler::tests::racy" classname="temps-proxy" time="2.0">
      <flakyFailure type="test failure" message="port in use">thread panicked</flakyFailure>
    </testcase>
    <testcase name="handler::tests::broken" classname="temps-proxy" time="1.0">
      <failure type="test failure">assertion failed</failure>
      <rerunFailure type="test failure">assertion failed</rerunFailure>
    </testcase>
  </testsuite>
  <testsuite name="temps-otel::e2e_http_test" tests="1" disabled="0" errors="0" failures="0">
    <testcase name="ingest_roundtrip" classname="temps-otel::e2e_http_test" time="3.0">
      <flakyFailure type="test failure">timeout</flakyFailure>
      <flakyFailure type="test failure">timeout</flakyFailure>
    </testcase>
  </testsuite>
</testsuites>
"""


class NextestFlakyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.dir = Path(self.temp.name)

    def junit(self, path: str, text: str = JUNIT) -> Path:
        target = self.dir / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
        return target

    def test_flaky_tests_are_those_that_passed_on_a_retry(self) -> None:
        self.assertEqual(
            flaky.flaky_tests(self.junit("junit.xml")),
            [
                ("temps-proxy", "handler::tests::racy", 1),
                ("temps-otel::e2e_http_test", "ingest_roundtrip", 2),
            ],
        )
        self.assertEqual(
            flaky.failed_tests(self.junit("junit.xml")), [("temps-proxy", "handler::tests::broken")]
        )

    def test_report_annotates_and_never_fails(self) -> None:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = flaky.report([self.junit("junit.xml"), self.dir / "missing.xml"], "unit-b")
        self.assertEqual(code, 0)
        text = output.getvalue()
        self.assertIn("::warning title=Flaky test in unit-b::temps-proxy handler::tests::racy", text)
        self.assertIn("No JUnit report at", text)

    def test_learn_counts_runs_not_reports(self) -> None:
        self.junit("runs/101/nextest-junit-unit-a/junit.xml")
        self.junit("runs/101/nextest-junit-unit-b/junit.xml")  # same run twice
        self.junit("runs/102/nextest-junit-unit-b/junit.xml")
        self.junit("runs/103/nextest-junit-unit-b/junit.xml", "not xml")
        learned = flaky.learn(self.dir / "runs", min_runs=2)
        self.assertEqual(learned["runs"], 3)
        self.assertEqual(
            [(e["binary_id"], e["test"], e["flaky_runs"]) for e in learned["tests"]],
            [
                ("temps-otel::e2e_http_test", "ingest_roundtrip", 2),
                ("temps-proxy", "handler::tests::racy", 2),
            ],
        )
        self.assertEqual(flaky.learn(self.dir / "runs", min_runs=3)["tests"], [])

    def test_apply_appends_an_override_nextest_can_parse(self) -> None:
        config = self.dir / "nextest.toml"
        config.write_text((REPO_ROOT / ".config/nextest.toml").read_text(encoding="utf-8"), encoding="utf-8")
        learned = self.dir / "learned.json"
        self.junit("runs/1/x/junit.xml")
        learned.write_text(json.dumps(flaky.learn(self.dir / "runs", 1)), encoding="utf-8")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(flaky.apply(learned, config), 0)
        parsed = tomllib.loads(config.read_text(encoding="utf-8"))
        self.assertIn(flaky.GROUP, parsed["test-groups"])
        last = parsed["profile"]["default"]["overrides"][-1]
        self.assertEqual(last["test-group"], flaky.GROUP)
        self.assertIn("(binary_id(=temps-proxy) & test(=handler::tests::racy))", last["filter"])

    def test_apply_drops_entries_that_are_not_rust_names(self) -> None:
        learned = {
            "tests": [
                {"binary_id": "temps-proxy", "test": "ok::name"},
                {"binary_id": "temps-proxy') | all() | test(=x", "test": "evil"},
                {"binary_id": "temps-proxy", "test": "x'''\n[profile.default]"},
                "not a dict",
            ]
        }
        text = flaky.override(learned)
        self.assertIsNotNone(text)
        self.assertIn("test(=ok::name)", text)
        self.assertNotIn("evil", text)
        self.assertNotIn("[profile.default]\n", text.split("filter")[1])

    def test_apply_without_a_list_leaves_the_config_alone(self) -> None:
        config = self.dir / "nextest.toml"
        config.write_text("[profile.default]\n", encoding="utf-8")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(flaky.apply(self.dir / "absent.json", config), 0)
            empty = self.dir / "empty.json"
            empty.write_text('{"tests": []}', encoding="utf-8")
            self.assertEqual(flaky.apply(empty, config), 0)
        self.assertEqual(config.read_text(encoding="utf-8"), "[profile.default]\n")

    def test_the_ci_profile_retries_and_writes_junit(self) -> None:
        config = tomllib.loads((REPO_ROOT / ".config/nextest.toml").read_text(encoding="utf-8"))
        ci = config["profile"]["ci"]
        self.assertEqual(ci["retries"]["count"], 2)
        self.assertEqual(ci["junit"]["path"], "junit.xml")
        self.assertEqual(config["test-groups"][flaky.GROUP], {"max-threads": 1})


if __name__ == "__main__":
    unittest.main()
