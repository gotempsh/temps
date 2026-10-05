# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import argparse
import os
import unittest
import urllib.error
from unittest.mock import patch

import io
from contextlib import redirect_stderr, redirect_stdout

from release_test_gate import fetch_runs, classify, guidance, print_state, wait

SHA = "a" * 40


def run(event="push", status="completed", conclusion="success"):
    return {
        "event": event,
        "status": status,
        "conclusion": conclusion,
        "head_branch": "main",
        "html_url": "https://example.invalid/run",
    }


def args(**overrides):
    values = {
        "repository": "owner/repo",
        "sha": SHA,
        "workflow": "rust-tests.yml",
        "ref_name": "v1.2.3",
        "timeout_minutes": 10,
        "missing_grace_minutes": 2,
        "poll_seconds": 60,
    }
    values.update(overrides)
    return argparse.Namespace(**values)


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class FetchTests(unittest.TestCase):
    def test_success_on_second_page_is_not_lost(self):
        import json
        responses = [io.BytesIO(json.dumps({"workflow_runs": [run(conclusion="failure")] * 100}).encode()),
                     io.BytesIO(json.dumps({"workflow_runs": [run()]}).encode())]
        with patch("urllib.request.urlopen", side_effect=responses) as fetch:
            runs = fetch_runs("owner/repo", "rust-tests.yml", SHA, "test-token")
        self.assertEqual(classify(runs), "success")
        self.assertEqual(fetch.call_count, 2)
        self.assertIn("page=2", fetch.call_args.args[0].full_url)


class ClassifyTests(unittest.TestCase):
    def test_successful_push_run_passes(self):
        self.assertEqual(classify([run()]), "success")

    def test_successful_dispatch_run_passes(self):
        self.assertEqual(classify([run(event="workflow_dispatch")]), "success")

    def test_any_success_wins_over_an_earlier_failure(self):
        self.assertEqual(
            classify([run(conclusion="failure"), run(conclusion="success")]), "success"
        )

    def test_pull_request_success_does_not_count(self):
        # A docs-only PR run skips every job and still concludes success.
        self.assertEqual(classify([run(event="pull_request")]), "missing")

    def test_in_progress_run_is_pending(self):
        self.assertEqual(
            classify([run(status="in_progress", conclusion=None)]), "pending"
        )
        self.assertEqual(classify([run(status="queued", conclusion=None)]), "pending")

    def test_failed_and_cancelled_runs_block(self):
        self.assertEqual(classify([run(conclusion="failure")]), "failed")
        self.assertEqual(classify([run(conclusion="cancelled")]), "failed")
        self.assertEqual(classify([run(conclusion="timed_out")]), "failed")

    def test_failure_with_a_rerun_in_progress_is_pending(self):
        self.assertEqual(
            classify(
                [run(conclusion="failure"), run(status="in_progress", conclusion=None)]
            ),
            "pending",
        )

    def test_no_runs_is_missing(self):
        self.assertEqual(classify([]), "missing")


class WaitTests(unittest.TestCase):
    def setUp(self):
        patcher = patch.dict(os.environ, {"GH_TOKEN": "token", "GITHUB_STEP_SUMMARY": ""})
        patcher.start()
        self.addCleanup(patcher.stop)

    def drive(self, responses, **overrides):
        clock = FakeClock()
        calls = []

        def fetch(repository, workflow, sha, token):
            calls.append((repository, workflow, sha, token))
            response = responses[min(len(calls) - 1, len(responses) - 1)]
            if isinstance(response, Exception):
                raise response
            return response

        with patch("builtins.print"):
            code = wait(args(**overrides), fetch=fetch, sleep=clock.sleep, clock=clock)
        return code, calls, clock

    def test_success_returns_zero_immediately(self):
        code, calls, _ = self.drive([[run()]])
        self.assertEqual(code, 0)
        self.assertEqual(calls, [("owner/repo", "rust-tests.yml", SHA, "token")])

    def test_waits_for_a_pending_run_to_succeed(self):
        pending = [run(status="in_progress", conclusion=None)]
        code, calls, clock = self.drive([pending, pending, [run()]])
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 3)
        self.assertEqual(clock.now, 120)

    def test_failed_run_blocks_without_waiting(self):
        code, calls, _ = self.drive([[run(conclusion="failure")]])
        self.assertEqual(code, 1)
        self.assertEqual(len(calls), 1)

    def test_missing_run_is_retried_for_the_grace_period_then_blocks(self):
        code, calls, clock = self.drive([[]])
        self.assertEqual(code, 1)
        self.assertEqual(clock.now, 120)
        self.assertEqual(len(calls), 3)

    def test_run_registered_during_grace_period_is_waited_for(self):
        pending = [run(status="queued", conclusion=None)]
        code, _, _ = self.drive([[], pending, [run()]])
        self.assertEqual(code, 0)

    def test_pending_run_times_out(self):
        code, _, clock = self.drive([[run(status="in_progress", conclusion=None)]])
        self.assertEqual(code, 1)
        self.assertEqual(clock.now, 600)

    def test_api_errors_are_retried_not_treated_as_success(self):
        code, _, _ = self.drive([urllib.error.URLError("boom"), [run()]])
        self.assertEqual(code, 0)
        code, _, _ = self.drive([urllib.error.URLError("boom")])
        self.assertEqual(code, 1)

    def test_missing_token_is_an_error(self):
        with patch.dict(os.environ, {"GH_TOKEN": "", "GITHUB_TOKEN": ""}), patch(
            "builtins.print"
        ):
            self.assertEqual(wait(args(), fetch=lambda *a: [run()]), 2)


class PrintStateTests(unittest.TestCase):
    def query(self, fetch):
        out = io.StringIO()
        with patch.dict(os.environ, {"GH_TOKEN": "token"}), redirect_stdout(
            out
        ), redirect_stderr(io.StringIO()):
            code = print_state(args(), fetch=fetch)
        return code, out.getvalue().strip()

    def test_prints_the_classified_state_once(self):
        self.assertEqual(self.query(lambda *a: [run()]), (0, "tests_state=success"))
        self.assertEqual(
            self.query(lambda *a: [run(conclusion="failure")]),
            (0, "tests_state=failed"),
        )

    def test_api_error_is_unknown_not_failed(self):
        def fetch(*_):
            raise urllib.error.URLError("boom")

        self.assertEqual(self.query(fetch), (0, "tests_state=unknown"))


class GuidanceTests(unittest.TestCase):
    def test_guidance_names_the_rerun_command_and_the_bypass(self):
        for state in ("failed", "missing", "pending"):
            text = guidance(state, "rust-tests.yml", SHA, "v1.2.3")
            self.assertIn("gh workflow run rust-tests.yml --ref v1.2.3", text)
            self.assertIn("skip_test_gate=true", text)
            self.assertIn(SHA, text)


if __name__ == "__main__":
    unittest.main()
