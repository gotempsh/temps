#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for check_quiet_logs.py. Run: python3 scripts/first-run/test_check_quiet_logs.py"""

from __future__ import annotations

import io
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_quiet_logs as cql  # noqa: E402

ALLOWLIST = Path(__file__).resolve().parent / "quiet-logs-allowlist.toml"


def line(ts: str, level: str, rest: str) -> str:
    return f"2026-10-04T{ts}.123456Z {level:>5} {rest}"


class TeardownTests(unittest.TestCase):
    def test_teardown_failure_fails_successful_soak_and_preserves_original_failure(self):
        source = (Path(__file__).parent / "quiet-logs-soak.sh").read_text()
        cleanup = source[source.index("teardown() {"):source.index("trap teardown EXIT")]
        with tempfile.NamedTemporaryFile() as state:
            for original, teardown, expected in [(0, 0, 0), (0, 1, 1), (7, 1, 7)]:
                script = f'STATE="{state.name}"\nfixture() {{ return {teardown}; }}\n' + cleanup
                script += f"trap teardown EXIT\nexit {original}\n"
                result = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
                self.assertEqual(result.returncode, expected, result.stderr)


class ParseTests(unittest.TestCase):
    def test_unstructured_error_and_unrecognized_logs_fail_closed(self):
        [entry] = cql.parse_log(["dependency: ERROR database unavailable"])
        self.assertEqual(entry.level, "ERROR")
        with self.assertRaises(cql.InputError):
            cql.parse_log(["unrecognized server output"])

    def test_extracts_target_after_spans_and_strips_ansi(self) -> None:
        raw = "\x1b[2m2026-10-04T10:00:00.123456Z\x1b[0m \x1b[33m WARN\x1b[0m request{id=7 path=/a}: temps_proxy::proxy: upstream slow: retrying"
        [parsed] = cql.parse_log([raw])
        self.assertEqual(parsed.level, "WARN")
        self.assertEqual(parsed.target, "temps_proxy::proxy")
        self.assertNotIn("\x1b", parsed.text)

    def test_target_without_spans(self) -> None:
        [parsed] = cql.parse_log([line("10:00:00", "ERROR", "temps_backup::scheduler: backup 4 failed: boom")])
        self.assertEqual(parsed.target, "temps_backup::scheduler")

    def test_panic_counts_as_error(self) -> None:
        parsed = cql.parse_log(["thread 'tokio-runtime-worker' panicked at crates/x/src/lib.rs:1:1:"])
        self.assertEqual([(p.level, p.target) for p in parsed], [("ERROR", "panic")])

    def test_unstructured_lines_are_ignored(self) -> None:
        with self.assertRaises(cql.InputError):
            cql.parse_log(["   continuation of a multi-line message", ""])

    def test_nanosecond_timestamps_parse(self) -> None:
        [parsed] = cql.parse_log(["2026-10-04T10:00:00.123456789Z  INFO temps_cli: ready"])
        self.assertEqual(parsed.timestamp.isoformat(), "2026-10-04T10:00:00.123456+00:00")


class AnalyzeTests(unittest.TestCase):
    def analyze(self, raw: list[str], allow: list[cql.AllowEntry] | None = None, threshold: float = 12.0, **window):
        start = cql.parse_timestamp(window["start"]) if "start" in window else None
        end = cql.parse_timestamp(window["end"]) if "end" in window else None
        return cql.analyze(cql.parse_log(raw), allow or [], start, end, threshold)

    def test_quiet_log_passes(self) -> None:
        report = self.analyze([line("10:00:00", "INFO", "temps_cli: started"), line("10:20:00", "INFO", "temps_cli: idle")])
        self.assertTrue(report.ok)

    def test_any_error_fails_even_outside_window(self) -> None:
        report = self.analyze(
            [line("09:00:00", "ERROR", "temps_core::x: startup failed"), line("10:00:00", "INFO", "temps_cli: ok")],
            start="2026-10-04T09:30:00Z",
            end="2026-10-04T10:30:00Z",
        )
        self.assertFalse(report.ok)
        self.assertEqual(len(report.errors), 1)

    def test_warn_rate_is_per_module_and_per_hour(self) -> None:
        # 20-minute window: 5 WARNs = 15/hour (over 12), 3 WARNs = 9/hour (under).
        raw = [line(f"10:0{i}:00", "WARN", "temps_proxy::health: slow") for i in range(5)]
        raw += [line(f"10:1{i}:00", "WARN", "temps_monitoring::checker: late") for i in range(3)]
        report = self.analyze(raw, start="2026-10-04T10:00:00Z", end="2026-10-04T10:20:00Z")
        self.assertEqual(set(report.noisy_modules), {"temps_proxy::health"})
        self.assertAlmostEqual(report.noisy_modules["temps_proxy::health"], 15.0)
        self.assertFalse(report.ok)

    def test_warns_outside_window_do_not_count(self) -> None:
        raw = [line("09:00:00", "WARN", "temps_proxy::health: boot noise")] * 50
        raw.append(line("10:10:00", "INFO", "temps_cli: idle"))
        report = self.analyze(raw, start="2026-10-04T10:00:00Z", end="2026-10-04T10:20:00Z")
        self.assertTrue(report.ok)
        self.assertEqual(report.warn_counts, {})

    def test_allowlisted_error_and_warn_are_excluded(self) -> None:
        allow = [
            cql.AllowEntry(level="ERROR", pattern=cql.re.compile("known flake"), issue="https://github.com/o/r/issues/1", reason="tracked"),
            cql.AllowEntry(level="WARN", pattern=cql.re.compile("slow"), issue="https://github.com/o/r/issues/2", reason="tracked", target="temps_proxy"),
        ]
        raw = [line("10:00:00", "ERROR", "temps_x::y: known flake 42")]
        raw += [line(f"10:0{i}:00", "WARN", "temps_proxy::health: slow") for i in range(9)]
        report = self.analyze(raw, allow, start="2026-10-04T10:00:00Z", end="2026-10-04T10:20:00Z")
        self.assertTrue(report.ok)
        self.assertEqual(len(report.allowed_errors), 1)
        self.assertEqual(report.allowed_warn_counts["temps_proxy::health"], 9)
        self.assertEqual(report.stale_allow_entries, [])

    def test_target_scoped_allow_entry_does_not_match_other_modules(self) -> None:
        allow = [cql.AllowEntry(level="ERROR", pattern=cql.re.compile("boom"), issue="https://github.com/o/r/issues/3", reason="r", target="temps_a")]
        report = self.analyze([line("10:00:00", "ERROR", "temps_ab::x: boom")], allow)
        self.assertFalse(report.ok)
        self.assertEqual(len(report.stale_allow_entries), 1)

    def test_inverted_window_is_rejected(self) -> None:
        with self.assertRaises(cql.InputError):
            self.analyze([], start="2026-10-04T11:00:00Z", end="2026-10-04T10:00:00Z")


class AllowlistTests(unittest.TestCase):
    def write(self, text: str) -> Path:
        handle = tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False)
        handle.write(text)
        handle.close()
        self.addCleanup(Path(handle.name).unlink)
        return Path(handle.name)

    def test_repository_allowlist_is_valid(self) -> None:
        cql.load_allowlist(ALLOWLIST)

    def test_entry_requires_issue_link(self) -> None:
        path = self.write('[[allow]]\nlevel = "ERROR"\npattern = "x"\nreason = "r"\nissue = "TODO"\n')
        with self.assertRaisesRegex(cql.InputError, "issue must be a GitHub issue URL"):
            cql.load_allowlist(path)

    def test_unknown_key_rejected(self) -> None:
        path = self.write('[[allow]]\nlevel = "WARN"\npattern = "x"\nreason = "r"\nissue = "https://github.com/o/r/issues/9"\nexpires = 1\n')
        with self.assertRaisesRegex(cql.InputError, "unknown keys"):
            cql.load_allowlist(path)

    def test_invalid_regex_rejected(self) -> None:
        path = self.write('[[allow]]\nlevel = "WARN"\npattern = "("\nreason = "r"\nissue = "https://github.com/o/r/issues/9"\n')
        with self.assertRaisesRegex(cql.InputError, "invalid pattern"):
            cql.load_allowlist(path)


class CliTests(unittest.TestCase):
    def run_cli(self, log_lines: list[str], *extra: str) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as scratch:
            log = Path(scratch, "temps.log")
            log.write_text("\n".join(log_lines) + "\n", encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out), redirect_stderr(io.StringIO()):
                code = cql.main([str(log), "--allowlist", str(ALLOWLIST), *extra])
            return code, out.getvalue()

    def test_exit_codes(self) -> None:
        code, text = self.run_cli([line("10:00:00", "INFO", "temps_cli: ok")])
        self.assertEqual(code, 0)
        self.assertIn("quiet", text)
        code, text = self.run_cli([line("10:00:00", "ERROR", "temps_cli::serve: kaboom")])
        self.assertEqual(code, 1)
        self.assertIn("kaboom", text)

    def test_bad_input_exits_2(self) -> None:
        with redirect_stderr(io.StringIO()):
            self.assertEqual(cql.main(["/nonexistent/temps.log"]), 2)


if __name__ == "__main__":
    unittest.main()
