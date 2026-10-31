#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for classify-ci-changes.py, which decides what rust-tests.yml skips.

A classifier that calls code "documentation", or a Rust input "frontend",
lets an untested change merge with every required check green, so most of
these tests pin the fail-closed cases. WorkflowWiringTests then evaluate
rust-tests.yml's job conditions for each kind of change and check that the
right jobs run and every required check still reports. The last group is a
contract over the repository itself: a script under `scripts/` or `.github/`
that reads a page under `docs/` makes that page test input, and it must then
be listed in TEST_INPUT_DOCS so editing it still runs the jobs that read it.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent.parent
CLASSIFIER = SCRIPTS / "classify-ci-changes.py"

spec = importlib.util.spec_from_file_location("classify_ci_changes", CLASSIFIER)
assert spec is not None and spec.loader is not None
classify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(classify)

affected_spec = importlib.util.spec_from_file_location(
    "ci_affected_crates", SCRIPTS / "ci-affected-crates.py"
)
assert affected_spec is not None and affected_spec.loader is not None
affected = importlib.util.module_from_spec(affected_spec)
affected_spec.loader.exec_module(affected)

# Everything the `changes` job exports: both scripts' outputs, plus `release`,
# which the classify step sets itself ("false" on every pull request).
ALL_OUTPUTS = classify.OUTPUTS + affected.OUTPUTS + ("release",)


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


class ClassificationTests(unittest.TestCase):
    def test_docs_and_root_markdown_are_documentation(self) -> None:
        self.assertIsNone(
            classify.first_code_path(
                ["docs/concepts/page.mdx", "docs/images/diagram.png", "README.md", "CHANGELOG.md"]
            )
        )

    def test_any_non_documentation_file_requires_full_ci(self) -> None:
        self.assertEqual(
            classify.first_code_path(["docs/concepts/page.mdx", "crates/temps-core/src/lib.rs"]),
            "crates/temps-core/src/lib.rs",
        )

    def test_nested_markdown_is_not_documentation(self) -> None:
        # Embedded into the binary via include_dir!, and fixtures carry READMEs.
        for path in (
            "crates/temps-core/templates/README.md",
            "crates/temps-deployments/tests/fixtures/simple-nodejs/README.md",
            "skills/temps/SKILL.md",
        ):
            with self.subTest(path=path):
                self.assertEqual(classify.first_code_path([path]), path)

    def test_pages_read_by_ci_require_full_ci(self) -> None:
        for path in sorted(classify.TEST_INPUT_DOCS):
            with self.subTest(path=path):
                self.assertEqual(classify.first_code_path([path]), path)

    def test_paths_that_only_resemble_docs_require_full_ci(self) -> None:
        for path in ("docs", "docs-site/index.md", "web/docs/design/README.md", "README.md/x"):
            with self.subTest(path=path):
                self.assertEqual(classify.first_code_path([path]), path)

    def test_empty_change_set_requires_full_ci(self) -> None:
        self.assertEqual(classify.first_code_path([]), "<empty diff>")
        self.assertEqual(classify.first_code_path([""]), "<empty diff>")
        self.assertEqual(classify.first_rust_path([]), "<empty diff>")
        self.assertEqual(
            classify.classify([]),
            {
                "code": "true",
                "rust": "true",
                "web": "true",
                "cli": "true",
                "code_reason": "<empty diff>",
                "rust_reason": "<empty diff>",
            },
        )


class AreaTests(unittest.TestCase):
    def outputs(self, *paths: str) -> dict[str, str]:
        result = classify.classify(paths)
        return {key: result[key] for key in classify.OUTPUTS}

    def test_frontend_areas(self) -> None:
        for path, expected in (
            ("web/src/pages/Projects.tsx", "web"),
            ("web/bun.lock", "web"),
            ("web/e2e/authenticated/projects.spec.ts", "web"),
            ("apps/temps-cli/src/commands/docs.ts", "cli"),
            ("apps/temps-cli/openapi.json", "cli"),
            ("skills/temps/SKILL.md", "skills"),
        ):
            with self.subTest(path=path):
                self.assertEqual(classify.area(path), expected)

    def test_everything_unrecognised_is_rust(self) -> None:
        for path in (
            "crates/temps-cli/build.rs",
            "Cargo.lock",
            "Dockerfile",
            ".github/workflows/rust-tests.yml",
            "scripts/test-compose-security.sh",
            # Built by the scenario E2E suite, not part of any frontend area.
            "packages/api/src/index.ts",
            "sdks/node/packages/node-sdk/src/index.ts",
            "apps/temps-e2e/src/index.ts",
            # Prefixes are whole directories.
            "web-legacy/index.ts",
            "apps/temps-cli-ee/index.ts",
            "skills.json",
            "webapp/x.ts",
            # A docs page CI reads is still test input.
            "docs/upgrade/page.mdx",
        ):
            with self.subTest(path=path):
                self.assertEqual(classify.area(path), "rust")

    def test_web_only_change_skips_rust(self) -> None:
        self.assertEqual(
            self.outputs("web/src/App.tsx", "docs/concepts/page.mdx"),
            {"code": "true", "rust": "false", "web": "true", "cli": "false"},
        )

    def test_cli_only_change_skips_rust(self) -> None:
        self.assertEqual(
            self.outputs("apps/temps-cli/src/index.ts"),
            {"code": "true", "rust": "false", "web": "false", "cli": "true"},
        )

    def test_skills_only_change_skips_everything_but_code(self) -> None:
        self.assertEqual(
            self.outputs("skills/temps/SKILL.md"),
            {"code": "true", "rust": "false", "web": "false", "cli": "false"},
        )

    def test_any_rust_input_sets_every_area(self) -> None:
        # Consumers gate on `web` or `cli` alone; a Rust change must reach them.
        self.assertEqual(
            self.outputs("web/src/App.tsx", "crates/temps-core/src/lib.rs"),
            {"code": "true", "rust": "true", "web": "true", "cli": "true"},
        )



class GitDiffTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        git(self.repo, "init", "--quiet", "--initial-branch=main")
        git(self.repo, "config", "user.email", "ci@example.test")
        git(self.repo, "config", "user.name", "CI")
        (self.repo / "crates").mkdir()
        (self.repo / "docs").mkdir()
        (self.repo / "crates" / "lib.rs").write_text("pub fn f() {}\n" * 20)
        (self.repo / "docs" / "page.mdx").write_text("# Page\n")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "--quiet", "-m", "base")
        self.base = git(self.repo, "rev-parse", "HEAD").strip()
        self.cwd = Path.cwd()
        os.chdir(self.repo)

    def tearDown(self) -> None:
        os.chdir(self.cwd)
        self.tmp.cleanup()

    def commit(self, message: str) -> str:
        git(self.repo, "add", "--all")
        git(self.repo, "commit", "--quiet", "-m", message)
        return git(self.repo, "rev-parse", "HEAD").strip()

    def run_main(self, base: str, head: str) -> tuple[int, str]:
        output = self.repo / "github_output"
        output.write_text("")
        previous = os.environ.get("GITHUB_OUTPUT")
        os.environ["GITHUB_OUTPUT"] = str(output)
        try:
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                status = classify.main(["--base", base, "--head", head])
        finally:
            if previous is None:
                del os.environ["GITHUB_OUTPUT"]
            else:
                os.environ["GITHUB_OUTPUT"] = previous
        return status, output.read_text()

    def test_moving_code_into_docs_still_reports_the_source_file(self) -> None:
        git(self.repo, "mv", "crates/lib.rs", "docs/lib.md")
        head = self.commit("move")
        paths = classify.changed_paths(self.base, head)
        self.assertIn("crates/lib.rs", paths)
        self.assertEqual(classify.first_code_path(paths), "crates/lib.rs")

    def test_docs_only_commit_writes_everything_false(self) -> None:
        (self.repo / "docs" / "page.mdx").write_text("# Page\n\nMore.\n")
        head = self.commit("docs")
        self.assertEqual(
            self.run_main(self.base, head), (0, "code=false\nrust=false\nweb=false\ncli=false\n")
        )

    def test_web_commit_writes_rust_false(self) -> None:
        (self.repo / "web").mkdir()
        (self.repo / "web" / "App.tsx").write_text("export {}\n")
        head = self.commit("web")
        self.assertEqual(
            self.run_main(self.base, head), (0, "code=true\nrust=false\nweb=true\ncli=false\n")
        )

    def test_code_commit_writes_everything_true(self) -> None:
        (self.repo / "crates" / "lib.rs").write_text("pub fn g() {}\n")
        head = self.commit("code")
        self.assertEqual(
            self.run_main(self.base, head), (0, "code=true\nrust=true\nweb=true\ncli=true\n")
        )

    def test_moving_rust_into_web_still_counts_as_rust(self) -> None:
        (self.repo / "web").mkdir()
        git(self.repo, "mv", "crates/lib.rs", "web/lib.rs")
        head = self.commit("move")
        self.assertEqual(
            self.run_main(self.base, head), (0, "code=true\nrust=true\nweb=true\ncli=true\n")
        )

    def test_unreadable_diff_fails_and_leaves_output_unset(self) -> None:
        # rust-tests.yml runs every job unless an output is exactly "false".
        self.assertEqual(self.run_main(self.base, "no-such-revision"), (1, ""))


class WorkflowWiringTests(unittest.TestCase):
    """rust-tests.yml runs the right jobs, and reports every required check.

    These evaluate each job's `if:` for the outputs `changes` produces on each
    kind of pull request, and for a `changes` job that failed outright.

    A job skipped through `if:` reports success, so skipping is safe for an
    ordinary required job. A matrix job skipped that way is never expanded:
    `Unit Tests (unit-a)` would never report and branch protection would block
    the pull request forever. A gated job that a failed classifier skips would
    let a pull request merge with nothing tested.
    """

    WORKFLOW = REPO_ROOT / ".github" / "workflows" / "rust-tests.yml"
    RUST_GATE = "needs.changes.outputs.rust != 'false'"
    NO_RUST = "needs.changes.outputs.rust == 'false'"
    # The required status checks on `main` that this workflow produces. Keep in
    # step with branch protection; adding a name here only makes the test stricter.
    REQUIRED_CHECKS = {
        "Cargo Check",
        "Formatting",
        "Web TypeScript Check",
        "OpenAPI Spec Format",
        "Unit Tests (unit-a)",
        "Unit Tests (unit-b)",
        "Unit Tests (unit-integration)",
    }
    # Matrix jobs that may be skipped outright when no Rust input changed. None
    # of their checks is required. A new matrix job has to be added here or
    # made to run with its steps skipped, the way unit-tests does.
    SKIPPABLE_MATRIX_JOBS = {"integration-tests"}
    RUST_JOBS = {
        "otel-protobuf-compat",
        "check",
        "clippy",
        "build-test-binaries",
        "integration-tests",
        "mariadb-pitr-e2e",
        "upgrade-test",
        "compose-security",
    }
    ALWAYS = {"changes", "workflow-contracts", "fmt", "openapi-canonical", "unit-tests"}
    # What `changes` outputs for each kind of pull request, from the classifier
    # itself so the scenarios cannot drift from what it really reports.
    SCENARIOS = {
        "docs": ["docs/concepts/page.mdx", "README.md"],
        "web": ["web/src/App.tsx"],
        "cli": ["apps/temps-cli/src/index.ts"],
        "skills": ["skills/temps/SKILL.md"],
        "web+cli": ["web/src/App.tsx", "apps/temps-cli/src/index.ts"],
        "rust": ["crates/temps-core/src/lib.rs"],
        # Reaches neither temps-providers nor temps-backup.
        "rust-leaf": ["crates/temps-proxy/src/lib.rs"],
    }
    # Jobs that need the musl release build, which only the merge queue and
    # `main` compile.
    RELEASE_JOBS = {"build-binary", "compose-security"}
    EXPECTED_RUNNING = {
        "docs": ALWAYS,
        "skills": ALWAYS,
        "web": ALWAYS | {"build-binary", "web-typecheck", "e2e", "e2e-ui"},
        "cli": ALWAYS | {"build-binary", "scenario-e2e"},
        "web+cli": ALWAYS | {"build-binary", "web-typecheck", "e2e", "e2e-ui", "scenario-e2e"},
    }
    MATRIX_REFERENCE = re.compile(r"\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}")
    TOKEN = re.compile(
        r"\s+|!cancelled\(\)|&&|\|\||==|!=|\(|\)|'[^']*'"
        r"|needs\.[A-Za-z0-9_-]+\.(?:outputs\.[A-Za-z0-9_-]+|result)"
    )

    @classmethod
    def setUpClass(cls) -> None:
        # Ruby's YAML ships on every runner and already parses workflows in
        # test-nightly-release-workflows.sh; Python has no YAML in the stdlib.
        dumped = subprocess.run(
            [
                "ruby",
                "-ryaml",
                "-rjson",
                "-e",
                "puts JSON.generate(YAML.safe_load(File.read(ARGV[0]), aliases: true)['jobs'])",
                str(cls.WORKFLOW),
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        cls.jobs = json.loads(dumped)

    @staticmethod
    def needs(job: dict) -> list[str]:
        value = job.get("needs", [])
        return [value] if isinstance(value, str) else list(value)

    @staticmethod
    def is_matrix(job: dict) -> bool:
        return "matrix" in job.get("strategy", {})

    @classmethod
    def condition(cls, job: dict) -> str:
        text = str(job.get("if", "")).strip()
        if text.startswith("${{") and text.endswith("}}"):
            text = text[3:-2]
        return " ".join(text.split())

    def evaluate(self, job_id: str, condition: str, outputs: dict[str, str], results: dict[str, str]) -> bool:
        """Evaluate a job condition; fail the test on anything unrecognised."""
        python: list[str] = []
        position = 0
        for match in self.TOKEN.finditer(condition):
            if match.start() != position:
                self.fail(f"{job_id}: cannot evaluate {condition[position:match.start()]!r} in {condition!r}")
            position = match.end()
            token = match.group(0)
            if token.isspace():
                continue
            if token == "!cancelled()":
                python.append("True")
            elif token in ("&&", "||"):
                python.append(" and " if token == "&&" else " or ")
            elif token.startswith("needs."):
                _, dep, rest = token.split(".", 2)
                if rest == "result":
                    python.append(repr(results[dep]))
                else:
                    self.assertEqual(dep, "changes", f"{job_id}: unexpected output {token}")
                    python.append(repr(outputs[rest.removeprefix("outputs.")]))
            else:
                python.append(token)
        if position != len(condition):
            self.fail(f"{job_id}: cannot evaluate {condition[position:]!r} in {condition!r}")
        return bool(eval("".join(python), {"__builtins__": {}}))  # noqa: S307 - tokens are whitelisted

    def outcome(self, outputs: dict[str, str], changes_result: str = "success") -> dict[str, str]:
        """Return "success" or "skipped" for every job, given `changes`' outputs."""
        results: dict[str, str] = {}

        def resolve(job_id: str) -> str:
            if job_id in results:
                return results[job_id]
            if job_id == "changes":
                results[job_id] = changes_result
                return changes_result
            job = self.jobs[job_id]
            deps = {dep: resolve(dep) for dep in self.needs(job)}
            condition = self.condition(job)
            if "cancelled()" not in condition and "always()" not in condition:
                # No status function means an implicit success() in front.
                if any(result != "success" for result in deps.values()):
                    results[job_id] = "skipped"
                    return "skipped"
            runs = self.evaluate(job_id, condition, outputs, results) if condition else True
            results[job_id] = "success" if runs else "skipped"
            return results[job_id]

        for job_id in self.jobs:
            resolve(job_id)
        return results

    @classmethod
    def workspace(cls):
        if not hasattr(cls, "_workspace"):
            cls._workspace = affected.load_workspace(REPO_ROOT)
        return cls._workspace

    def scenario(self, name: str) -> dict[str, str]:
        paths = self.SCENARIOS[name]
        result = classify.classify(paths)
        outputs = {key: result[key] for key in classify.OUTPUTS}
        outputs.update(affected.outputs(self.workspace(), paths))
        outputs["release"] = "false"
        return outputs

    def running(self, outcome: dict[str, str]) -> set[str]:
        return {job_id for job_id, result in outcome.items() if result == "success"}

    def check_names(self, job_id: str) -> list[str]:
        job = self.jobs[job_id]
        name = str(job.get("name", job_id))
        include = job.get("strategy", {}).get("matrix", {}).get("include")
        if not include:
            return [name]
        return [
            self.MATRIX_REFERENCE.sub(lambda m, entry=entry: str(entry[m.group(1)]), name)
            for entry in include
        ]

    def test_changes_job_exports_every_output(self) -> None:
        changes = self.jobs["changes"]
        for key in classify.OUTPUTS:
            with self.subTest(output=key):
                self.assertEqual(changes["outputs"][key], f"${{{{ steps.classify.outputs.{key} }}}}")
        for key in affected.OUTPUTS:
            with self.subTest(output=key):
                self.assertEqual(changes["outputs"][key], f"${{{{ steps.affected.outputs.{key} }}}}")
        self.assertNotIn("needs", changes)

    def test_jobs_reading_changes_survive_a_failed_classifier(self) -> None:
        # A failed `changes` job leaves every output empty. Only `!= 'false'`
        # under `!cancelled()` turns that into "run"; `== 'true'` or the
        # implicit success() would skip the job and report it green.
        for job_id, job in self.jobs.items():
            condition = self.condition(job)
            if "needs.changes" not in condition:
                continue
            with self.subTest(job=job_id):
                self.assertIn("changes", self.needs(job))
                self.assertIn("!cancelled()", condition)
                self.assertNotIn("== 'true'", condition)

    def test_a_failed_classifier_runs_every_job(self) -> None:
        outcome = self.outcome(dict.fromkeys(ALL_OUTPUTS, ""), changes_result="failure")
        skipped = sorted(job for job, result in outcome.items() if result == "skipped")
        self.assertEqual(skipped, [])

    def test_a_rust_change_runs_every_job_but_the_release_suites(self) -> None:
        outcome = self.outcome(self.scenario("rust"))
        self.assertEqual(self.running(outcome), set(self.jobs) - self.RELEASE_JOBS)

    def test_a_dockerfile_change_still_compiles_once(self) -> None:
        outputs = self.scenario("rust")
        for path in ("Dockerfile", "Cargo.lock", ".github/workflows/rust-tests.yml"):
            with self.subTest(path=path):
                result = classify.classify([path])
                outputs.update({key: result[key] for key in classify.OUTPUTS})
                running = self.running(self.outcome(outputs))
                self.assertNotIn("build-binary", running)
                self.assertIn("build-test-binaries", running)

    def test_the_merge_queue_and_main_run_every_job(self) -> None:
        outputs = dict.fromkeys(classify.OUTPUTS, "true")
        outputs.update({"tests": "all()", "docker_tests": "true", "release": "true"})
        self.assertEqual(self.running(self.outcome(outputs)), set(self.jobs))

    def test_release_is_false_on_pull_requests_and_true_otherwise(self) -> None:
        self.assertEqual(self.jobs["changes"]["outputs"]["release"], "${{ steps.classify.outputs.release }}")
        step = next(step for step in self.jobs["changes"]["steps"] if step.get("id") == "classify")
        self.assertIn("release=true", step["run"])
        self.assertIn('echo "release=false" >> "$GITHUB_OUTPUT"', step["run"])

    def test_a_change_out_of_reach_of_the_docker_tests_skips_their_e2e(self) -> None:
        outputs = self.scenario("rust-leaf")
        self.assertEqual(outputs["docker_tests"], "false")
        self.assertNotIn("package(=temps-providers)", outputs["tests"])
        outcome = self.outcome(outputs)
        self.assertEqual(
            self.running(outcome), set(self.jobs) - self.RELEASE_JOBS - {"mariadb-pitr-e2e"}
        )

    def test_docker_tests_archive_steps_follow_docker_tests(self) -> None:
        steps = {step.get("name"): step for step in self.jobs["build-test-binaries"]["steps"]}
        for name in ("Archive docker-tests test binaries", "Upload docker-tests archive"):
            with self.subTest(step=name):
                self.assertEqual(steps[name].get("if"), "needs.changes.outputs.docker_tests != 'false'")

    def test_integration_groups_skip_every_step_without_their_archive(self) -> None:
        job = self.jobs["integration-tests"]
        self.assertIn("changes", self.needs(job))
        first, *rest = job["steps"]
        self.assertEqual(first["name"], "Decide whether this group runs")
        self.assertEqual(first["env"]["DOCKER_TESTS"], "${{ needs.changes.outputs.docker_tests }}")
        for step in rest:
            with self.subTest(step=step.get("name")):
                self.assertIn("env.GROUP_RUNS == 'true'", str(step.get("if", "")))

    def test_every_test_run_is_narrowed_retried_and_reported(self) -> None:
        for job_id in ("unit-tests", "integration-tests"):
            steps = self.jobs[job_id]["steps"]
            runs = [step for step in steps if "cargo nextest run" in str(step.get("run", ""))]
            names = [step.get("name") for step in steps]
            with self.subTest(job=job_id):
                self.assertEqual(len(runs), 1)
                command = " ".join(str(runs[0]["run"]).split())
                self.assertIn("--profile ci", command)
                self.assertIn("--no-tests=pass", command)
                self.assertIn(
                    "-E '(${{ matrix.filter }}) and (${{ needs.changes.outputs.tests || 'all()' }})'",
                    command,
                )
                run_at = names.index(runs[0]["name"])
                self.assertLess(names.index("Serialise the tests CI has seen flake"), run_at)
                self.assertGreater(names.index("Report flaky tests"), run_at)
                self.assertGreater(names.index("Upload JUnit report"), run_at)
                self.assertEqual(
                    self.jobs[job_id]["permissions"], {"contents": "read", "actions": "read"}
                )

    def test_merge_queue_runs_every_test(self) -> None:
        workflow = self.WORKFLOW.read_text(encoding="utf-8")
        self.assertRegex(workflow, r"(?m)^  merge_group:$")
        affected_step = next(
            step for step in self.jobs["changes"]["steps"] if step.get("id") == "affected"
        )
        # Only pull requests are narrowed; merge_group and main print all().
        self.assertIn('"$GITHUB_EVENT_NAME" != "pull_request"', affected_step["run"])
        self.assertIn("'tests=all()' docker_tests=true", affected_step["run"])

    def test_consumers_never_wait_on_a_skipped_release_build(self) -> None:
        # With build-binary skipped, every binary consumer must still run on
        # the debug binary, and pick that artifact.
        for job_id in ("e2e", "e2e-ui", "scenario-e2e", "upgrade-test"):
            job = self.jobs[job_id]
            with self.subTest(job=job_id):
                self.assertIn("build-test-binaries", self.needs(job))
                self.assertEqual(
                    job["with"]["binary-artifact"],
                    "${{ needs.build-binary.result == 'success' && 'temps-binary-musl-fast' || 'temps-binary-debug' }}",
                )

    def test_changes_without_rust_run_only_the_jobs_that_can_see_them(self) -> None:
        for name, expected in self.EXPECTED_RUNNING.items():
            with self.subTest(scenario=name):
                running = self.running(self.outcome(self.scenario(name)))
                self.assertEqual(running, expected)
                self.assertEqual(running & self.RUST_JOBS, set())

    def test_required_checks_report_in_every_scenario(self) -> None:
        produced = {check: job_id for job_id in self.jobs for check in self.check_names(job_id)}
        for check in sorted(self.REQUIRED_CHECKS):
            self.assertIn(check, produced, f"no job in rust-tests.yml produces {check!r}")
        for name in self.SCENARIOS:
            outcome = self.outcome(self.scenario(name))
            for check in sorted(self.REQUIRED_CHECKS):
                job_id = produced[check]
                if not self.is_matrix(self.jobs[job_id]):
                    continue  # skipped through `if:` reports success
                with self.subTest(scenario=name, check=check):
                    self.assertEqual(
                        outcome[job_id],
                        "success",
                        f"{job_id} is a matrix job; skipping it means {check!r} never reports",
                    )

    def test_matrix_jobs_skipped_without_rust_changes_are_not_required(self) -> None:
        for name in self.EXPECTED_RUNNING:
            outcome = self.outcome(self.scenario(name))
            skipped = {
                job_id
                for job_id, job in self.jobs.items()
                if self.is_matrix(job) and outcome[job_id] == "skipped"
            }
            with self.subTest(scenario=name):
                self.assertEqual(skipped, self.SKIPPABLE_MATRIX_JOBS)

    def test_matrix_jobs_kept_without_rust_changes_skip_every_step(self) -> None:
        # Such a job runs although the builds it consumes were skipped, so any
        # ungated step would try to download an artifact that does not exist.
        for job_id, job in self.jobs.items():
            if not self.is_matrix(job) or self.NO_RUST not in self.condition(job):
                continue
            with self.subTest(job=job_id):
                conditions = [str(step.get("if", "")) for step in job["steps"]]
                self.assertIn(self.NO_RUST, conditions)
                # A step may add to the gate (`!cancelled() && <gate>`), never
                # offer an alternative to it.
                ungated = [
                    step.get("name", "<unnamed>")
                    for step, condition in zip(job["steps"], conditions)
                    if condition not in (self.RUST_GATE, self.NO_RUST)
                    and not (self.RUST_GATE in condition and "||" not in condition)
                ]
                self.assertEqual(ungated, [])

    def test_e2e_serves_the_checkouts_console_when_the_binary_is_reused(self) -> None:
        e2e = self.jobs["e2e"]
        self.assertEqual(
            e2e["with"]["console-from-source"],
            "${{ needs.build-binary.result != 'success' || needs.build-binary.outputs.reused-from != '' }}",
        )
        build = self.jobs["build-binary"]
        self.assertEqual(
            build["outputs"]["reused-from"], "${{ steps.binary-source.outputs.reused-from }}"
        )

    def test_build_binary_compiles_unless_a_binary_was_reused(self) -> None:
        steps = self.jobs["build-binary"]["steps"]
        names = [step.get("name") for step in steps]
        first_build = names.index("Free up disk space")
        upload = names.index("Upload temps binary")
        self.assertLess(names.index("Record binary source"), first_build)
        gate = "steps.binary-source.outputs.reused-from == ''"
        for step in steps[first_build:upload]:
            with self.subTest(step=step.get("name")):
                self.assertTrue(str(step.get("if", "")).startswith(gate))
        # The lookup runs only where reuse is sound: a pull request, no Rust.
        lookup = steps[names.index("Find a reusable main binary")]
        self.assertEqual(
            lookup["if"], "github.event_name == 'pull_request' && " + self.NO_RUST
        )
        self.assertNotIn("if", steps[upload])


class RepositoryContractTests(unittest.TestCase):
    """Pages under docs/ that CI reads must be listed in TEST_INPUT_DOCS."""

    EXCLUDED = {
        ".github/scripts/classify-ci-changes.py",
        ".github/scripts/test-classify-ci-changes.py",
    }
    # Scripts that read docs/ but run only in the `workflow-contracts` job, which
    # runs on every pull request, docs-only ones included (see
    # WorkflowWiringTests.ALWAYS). Editing the pages they read cannot skip them,
    # so those pages stay documentation.
    # test_ungated_readers_run_in_workflow_contracts keeps that true.
    UNGATED_JOB = "workflow-contracts"
    UNGATED_READERS = {
        "scripts/adr_index.py",
        "scripts/test_adr_index.py",
        "scripts/crate_map.py",
        "scripts/test_crate_map.py",
    }
    # A docs/ path not preceded by anything that would make it part of a longer
    # name. URLs are removed first, so temps.sh/docs/... links do not count.
    DOCS_REFERENCE = re.compile(r"(?<![A-Za-z0-9_.-])docs/[A-Za-z0-9_./-]*[A-Za-z0-9_]")
    URL = re.compile(r"https?://\S+")

    def tracked(self) -> list[str]:
        return [p for p in git(REPO_ROOT, "ls-files", "-z").split("\0") if p]

    def docs_read_by_ci(self) -> dict[str, set[str]]:
        tracked = self.tracked()
        docs_files = {p for p in tracked if p.startswith("docs/")}
        readers: dict[str, set[str]] = {}
        for path in tracked:
            if not path.startswith(("scripts/", ".github/")) or path in self.EXCLUDED | self.UNGATED_READERS:
                continue
            try:
                text = (REPO_ROOT / path).read_text(encoding="utf-8")
            except (UnicodeDecodeError, FileNotFoundError):
                continue
            for match in self.DOCS_REFERENCE.finditer(self.URL.sub(" ", text)):
                reference = match.group(0)
                prefix = reference.rstrip("/") + "/"
                for doc in docs_files:
                    if doc == reference or doc.startswith(prefix):
                        readers.setdefault(doc, set()).add(path)
        return readers

    def test_docs_read_by_ci_scripts_are_test_inputs(self) -> None:
        unlisted = {
            doc: sorted(readers)
            for doc, readers in self.docs_read_by_ci().items()
            if classify.is_documentation(doc)
        }
        self.assertEqual(
            unlisted,
            {},
            "these docs pages are read by CI scripts but would let a pull request "
            "skip CI; add them to TEST_INPUT_DOCS in classify-ci-changes.py",
        )

    def test_ungated_readers_run_in_workflow_contracts(self) -> None:
        dumped = subprocess.run(
            [
                "ruby",
                "-ryaml",
                "-rjson",
                "-e",
                "puts JSON.generate(YAML.safe_load(File.read(ARGV[0]), aliases: true)['jobs'])",
                str(WorkflowWiringTests.WORKFLOW),
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        job = json.loads(dumped)[self.UNGATED_JOB]
        self.assertNotIn("if", job, f"{self.UNGATED_JOB} must run on every pull request")
        self.assertIn(self.UNGATED_JOB, WorkflowWiringTests.ALWAYS)
        commands = "\n".join(str(step.get("run", "")) for step in job.get("steps", []))
        for script in sorted(self.UNGATED_READERS):
            self.assertIn(f"python3 {script}", commands, f"{script} must run in {self.UNGATED_JOB}")

    def test_test_input_docs_exist(self) -> None:
        tracked = set(self.tracked())
        self.assertEqual(sorted(classify.TEST_INPUT_DOCS - tracked), [])


if __name__ == "__main__":
    unittest.main()
