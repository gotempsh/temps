# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise installer release lookup without network access.

A fake `curl` serves canned GitHub API responses and refuses every download,
so each run stops right after the installer has decided what to fetch; the
download URL in the failure message tells us which release it picked. A fake
`uname` pins the platform (linux-amd64 unless a test says otherwise).

`CompletedInstallTests` instead hands the fake `curl` a local archive, so the
installer runs to the end inside a temporary directory and its final output
can be checked. `SignatureVerificationTests` adds fake `cosign` and `gh`
binaries to check which verifier runs, with which pinned identity, and that a
failed or missing signature aborts the install.
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time
import unittest


INSTALLER = Path(__file__).resolve().parents[2] / "scripts" / "install.sh"
API = "https://api.github.com/repos/gotempsh/temps/"
DOWNLOAD = "https://github.com/gotempsh/temps/releases/download/"

FAKE_CURL = r'''#!/usr/bin/env python3
import json, os, sys
url = sys.argv[-1]
with open(os.environ["FAKE_CALLS"], "a") as calls:
    calls.write(url + "\n")
if "api.github.com" not in url:
    archive = os.environ.get("FAKE_ARCHIVE")
    if not archive:
        sys.stderr.write("download refused by test\n")
        sys.exit(22)
    output = sys.argv[sys.argv.index("--output") + 1]
    if url.endswith(".sigstore.json"):
        if os.environ.get("FAKE_CURL_ARGS"):
            with open(os.environ["FAKE_CURL_ARGS"], "a") as recorded:
                recorded.write("\t".join(sys.argv[1:]) + "\n")
        bundle = os.environ.get("FAKE_BUNDLE")
        if bundle is None:
            sys.stdout.write("404")
        else:
            open(output, "w").write(bundle)
            sys.stdout.write("200")
        sys.exit(0)
    if url.endswith(".sha256"):
        import hashlib
        digest = hashlib.sha256(open(archive, "rb").read()).hexdigest()
        open(output, "w").write(digest + "  archive.tar.gz\n")
    else:
        open(output, "wb").write(open(archive, "rb").read())
    sys.exit(0)
routes = json.load(open(os.environ["FAKE_ROUTES"]))
status, body, *exit_codes = routes.get(url.split("/repos/gotempsh/temps/", 1)[1], routes["default"])
sys.stdout.write(body + "\n" + status)
sys.exit(exit_codes[0] if exit_codes else 0)
'''


# A verifier stand-in: records its arguments, exits with FAKE_<NAME>_EXIT.
FAKE_VERIFIER = r'''#!/usr/bin/env python3
import os, sys
name = os.path.basename(sys.argv[0])
with open(os.environ["FAKE_TOOL_CALLS"], "a") as calls:
    calls.write(name + "\t" + "\t".join(sys.argv[1:]) + "\n")
if name == "gh" and sys.argv[1:3] == ["auth", "status"]:
    sys.exit(int(os.environ.get("FAKE_GH_AUTH_EXIT", "0")))
code = int(os.environ.get("FAKE_" + name.upper() + "_EXIT", "0"))
if code:
    sys.stderr.write(name + ": verification refused by test\n")
sys.exit(code)
'''


def system_tool_dir(root):
    """Every command in /usr/bin and /bin except the ones a test controls.

    A runner image may ship `gh` (GitHub's does) or `cosign`; the tests decide
    whether a verifier exists, so those are left out, as is any `temps`.
    """
    farm = root / "system-bin"
    farm.mkdir()
    for directory in ("/usr/bin", "/bin"):
        for entry in sorted(Path(directory).iterdir()):
            if entry.name in ("gh", "cosign", "temps") or (farm / entry.name).exists():
                continue
            if os.access(entry, os.X_OK) and not entry.is_dir():
                (farm / entry.name).symlink_to(entry)
    return farm


def release(tag, platforms=("linux-amd64", "darwin-arm64"), body=""):
    """A GitHub release object shaped like the REST API's."""
    return {
        "url": f"{API}releases/1",
        "tag_name": tag,
        "name": tag,
        "draft": False,
        "prerelease": "-" in tag,
        "body": body,
        "assets": [
            {
                "name": f"temps-{platform}.tar.gz",
                "url": f"{API}releases/assets/1",
                "browser_download_url": f"{DOWNLOAD}{tag}/temps-{platform}.tar.gz",
            }
            for platform in platforms
        ],
    }


def minified(value):
    return json.dumps(value, separators=(",", ":"))


def pretty(value):
    return json.dumps(value, indent=2)


class InstallerRun:
    def __init__(self, returncode, output, calls):
        self.returncode = returncode
        self.output = output
        self.calls = calls

    @property
    def api_calls(self):
        return [call for call in self.calls if "api.github.com" in call]

    @property
    def downloads(self):
        return [call for call in self.calls if "api.github.com" not in call]


class ReleaseLookupTests(unittest.TestCase):
    def install(self, *arguments, routes=None, status="404", platform="Linux x86_64"):
        """Run the installer against canned API routes (path -> (status, body))."""
        table = {"default": [status, "{}"]}
        table.update(routes or {})
        with tempfile.TemporaryDirectory(prefix="temps-install-lookup-") as temporary:
            root = Path(temporary)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            (bin_dir / "curl").write_text(FAKE_CURL)
            (bin_dir / "uname").write_text(f'#!/bin/sh\necho "{platform}"\n')
            for tool in ("curl", "uname"):
                (bin_dir / tool).chmod(0o755)
            (root / "routes.json").write_text(json.dumps(table))
            calls = root / "calls"
            calls.touch()
            environment = dict(
                os.environ,
                PATH=str(bin_dir) + os.pathsep + os.environ["PATH"],
                HOME=str(root / "home"),
                TEMPS_INSTALL=str(root / "install"),
                FAKE_ROUTES=str(root / "routes.json"),
                FAKE_CALLS=str(calls),
            )
            result = subprocess.run(
                ["bash", str(INSTALLER), *arguments],
                env=environment,
                capture_output=True,
                text=True,
                timeout=30,
            )
            run = InstallerRun(result.returncode, result.stdout + result.stderr,
                               calls.read_text().splitlines())
        # Downloads are always refused, so every run ends in an error.
        self.assertEqual(run.returncode, 1, run.output)
        return run

    def assert_picked(self, run, tag):
        self.assertEqual(run.downloads, [f"{DOWNLOAD}{tag}/temps-linux-amd64.tar.gz"], run.output)

    def assert_picked_nothing(self, run):
        self.assertEqual(run.downloads, [], run.output)

    # -- API failures -------------------------------------------------------

    def test_missing_stable_explains_beta_opt_in(self):
        run = self.install(status="404")
        self.assertIn("has not published a stable release yet", run.output)
        self.assertIn("--channel beta", run.output)
        self.assert_picked_nothing(run)

    def test_rate_limit_is_not_reported_as_no_stable_release(self):
        for status in ["403", "429"]:
            with self.subTest(status=status):
                run = self.install(status=status)
                self.assertIn("GitHub API rate limit", run.output)
                self.assertNotIn("has not published a stable release", run.output)

    def test_network_error_is_explained(self):
        run = self.install(status="000")
        self.assertIn("network or DNS failure", run.output)

    def test_timed_out_beta_listing_discards_partial_http_200(self):
        partial = minified([release("v1.0.0-beta.1")])[:-1]
        run = self.install("--channel", "beta", routes={
            "releases?per_page=100&page=1": ("200", partial, 28),
        })
        self.assertIn("network or DNS failure", run.output)
        self.assert_picked_nothing(run)

    def test_beta_lookup_reports_api_errors(self):
        run = self.install("--channel", "beta", status="403")
        self.assertIn("Could not query GitHub for beta releases", run.output)
        self.assert_picked_nothing(run)

    # -- stable -------------------------------------------------------------

    def test_stable_parses_minified_and_pretty_latest_release(self):
        for encode in (minified, pretty):
            with self.subTest(encoding=encode.__name__):
                latest = release("v0.2.0", body="Upgrade from v0.1.0 first.")
                run = self.install(routes={
                    "releases/latest": ("200", encode(latest)),
                    "releases?per_page=100": ("200", encode([latest])),
                })
                self.assert_picked(run, "v0.2.0")
                self.assertEqual(run.api_calls[0], f"{API}releases/latest")

    def test_stable_without_platform_asset_is_refused(self):
        latest = release("v0.2.0", platforms=("darwin-arm64",))
        run = self.install(routes={"releases/latest": ("200", minified(latest))})
        self.assertIn("has no temps-linux-amd64.tar.gz asset", run.output)
        self.assert_picked_nothing(run)

    def test_stable_warns_when_it_predates_the_beta_line(self):
        latest = release("v0.0.9")
        listing = [release("v0.1.0-rc.10"), release("v0.1.0-nightly.20261005.abc12345"), latest]
        run = self.install(routes={
            "releases/latest": ("200", minified(latest)),
            "releases?per_page=100": ("200", minified(listing)),
        })
        self.assert_picked(run, "v0.0.9")
        self.assertIn("predates the current beta", run.output)
        self.assertIn("newest: v0.1.0-rc.10", run.output)

    def test_nightly_only_optional_warning_does_not_abort_stable_install(self):
        source = INSTALLER.read_text()
        begin = source.index("release_tag_pattern=")
        end = source.index("\n# Explain", begin)
        listing = minified([release("v0.1.0-nightly.20261005.abc12345")])
        script = "set -euo pipefail\ntarget=linux-amd64\n" + source[begin:end] + "\n" + f'''
github_api_get() {{ api_status=200; api_body='{listing}'; }}
warning() {{ echo warning; }}
warn_if_stable_predates_beta v1.0.0
echo stable-install-continues
'''
        result = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("stable-install-continues", result.stdout)
        self.assertNotIn("warning", result.stdout)

    # -- beta and nightly ---------------------------------------------------

    def beta_listing(self):
        # Newest-created first, as the API orders them -- deliberately not
        # version order, and full of releases that must never be picked.
        return [
            release("v0.1.0-rc.2"),
            release("v9.9.9-rc.1", platforms=("darwin-arm64",)),  # no linux tarball
            release("test-v9.9.9"),  # release-workflow smoke test
            release("v0.1.0-nightly.20261006.abc12345"),
            release("v0.1.0-rc.10"),
            release("v8.0.0", platforms=(),
                    body=f"curl -LO {DOWNLOAD}v8.0.0/temps-linux-amd64.tar.gz"),
            release("v0.1.0-rc.9"),
            release("v0.1.0-beta.99"),
        ]

    def test_beta_picks_highest_installable_semver_not_newest_created(self):
        for encode in (minified, pretty):
            with self.subTest(encoding=encode.__name__):
                run = self.install("--channel", "beta", routes={
                    "releases?per_page=100&page=1": ("200", encode(self.beta_listing())),
                })
                self.assert_picked(run, "v0.1.0-rc.10")

    def test_beta_picks_stable_release_when_it_is_highest(self):
        listing = [release("v0.1.0-rc.10"), release("v0.1.0"), release("v0.1.0-nightly.20261007.abc12345")]
        run = self.install("--channel=beta", routes={
            "releases?per_page=100&page=1": ("200", minified(listing)),
        })
        self.assert_picked(run, "v0.1.0")

    def test_short_page_stops_pagination(self):
        run = self.install("--channel", "beta", routes={
            "releases?per_page=100&page=1": ("200", minified(self.beta_listing())),
        })
        self.assertEqual(run.api_calls, [f"{API}releases?per_page=100&page=1"])

    def test_beta_compares_across_pages(self):
        page_one = [release(f"v0.0.1-beta.{number}") for number in range(100)]
        page_two = [release("v0.0.2-rc.1"), release("v0.0.1")]
        run = self.install("--channel", "beta", routes={
            "releases?per_page=100&page=1": ("200", minified(page_one)),
            "releases?per_page=100&page=2": ("200", minified(page_two)),
        })
        self.assert_picked(run, "v0.0.2-rc.1")

    def test_rate_limited_later_page_falls_back_to_first_page(self):
        page_one = [release(f"v0.0.1-beta.{number}") for number in range(100)]
        run = self.install("--channel", "beta", routes={
            "releases?per_page=100&page=1": ("200", minified(page_one)),
            "releases?per_page=100&page=2": ("403", "{}"),
        })
        self.assertIn("Could not read page 2", run.output)
        self.assert_picked(run, "v0.0.1-beta.99")

    def test_nightly_picks_highest_nightly(self):
        listing = [
            release("v0.1.0-nightly.20261004.ffff0000"),
            release("v0.1.0-nightly.20261006.abc12345", platforms=("darwin-arm64",)),
            release("v0.1.0-nightly.20261005.0abc1234"),
            release("v0.1.0-rc.10"),
            release("v0.1.0"),
        ]
        run = self.install("--channel", "nightly", routes={
            "releases?per_page=100&page=1": ("200", minified(listing)),
        })
        self.assert_picked(run, "v0.1.0-nightly.20261005.0abc1234")

    def test_channel_without_installable_release_explains(self):
        listing = [release("test-v1.0.0"), release("v1.0.0-rc.1", platforms=())]
        run = self.install("--channel", "beta", routes={
            "releases?per_page=100&page=1": ("200", minified(listing)),
        })
        self.assertIn("No installable release found on channel 'beta'", run.output)
        self.assert_picked_nothing(run)

    # -- explicit versions --------------------------------------------------

    def test_pinned_version_is_normalized_without_api_lookup(self):
        for argument, tag in [("0.1.0", "v0.1.0"), ("v0.1.0", "v0.1.0"),
                              ("0.1.0-rc.1", "v0.1.0-rc.1"),
                              ("v0.1.0-nightly.20261005.abc12345", "v0.1.0-nightly.20261005.abc12345")]:
            with self.subTest(argument=argument):
                run = self.install(argument)
                self.assert_picked(run, tag)
                self.assertEqual(run.api_calls, [])

    def test_arm64_linux_picks_the_arm64_archive(self):
        for platform in ("Linux aarch64", "Linux arm64"):
            with self.subTest(platform=platform):
                run = self.install("v0.1.0", platform=platform)
                self.assertEqual(run.downloads,
                                 [f"{DOWNLOAD}v0.1.0/temps-linux-arm64.tar.gz"], run.output)

    def test_malformed_pinned_version_is_rejected(self):
        for argument in ["latest", "v1.2", "1.2.3.4", "v1.2.3-", "v1.2.3-rc..1",
                         "1.2.3/../../other", "test-v0.1.0", "vv1.2.3", ""]:
            with self.subTest(argument=argument):
                run = self.install(argument)
                self.assertIn("is not a release version", run.output)
                self.assertEqual(run.calls, [])

    # -- semver ordering ----------------------------------------------------

    def test_highest_semver_tag_ordering(self):
        source = INSTALLER.read_text()
        begin = source.index("highest_semver_tag()")
        end = source.index("\n# Print, one per line", begin)
        cases = [
            (["v0.1.0-rc.2", "v0.1.0-rc.10", "v0.1.0-rc.9"], "v0.1.0-rc.10"),
            (["v0.1.0-rc.10", "v0.1.0", "v0.1.0-beta.99"], "v0.1.0"),
            (["v0.1.0-beta.56", "v0.1.0-rc.1"], "v0.1.0-rc.1"),
            (["v0.9.0", "v0.10.0", "v0.2.0"], "v0.10.0"),
            (["v1.0.0", "v0.99.99"], "v1.0.0"),
            (["v1.0.0-alpha", "v1.0.0-alpha.1"], "v1.0.0-alpha.1"),
            (["v1.0.0-alpha.beta", "v1.0.0-alpha.1"], "v1.0.0-alpha.beta"),
            (["v1.0.0-beta.11", "v1.0.0-beta.2", "v1.0.0-beta"], "v1.0.0-beta.11"),
            (["v0.1.0-nightly.20261004.ffff0000", "v0.1.0-nightly.20261005.0abc1234"],
             "v0.1.0-nightly.20261005.0abc1234"),
            ([], ""),
        ]
        for tags, expected in cases:
            with self.subTest(tags=tags):
                script = source[begin:end] + "\nhighest_semver_tag\n"
                result = subprocess.run(["bash", "-c", script], input="\n".join(tags) + "\n",
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)


CAPABILITIES_URL = "https://temps.sh/docs/capabilities"


RELEASE_IDENTITY = "https://github.com/gotempsh/temps/.github/workflows/release.yml@refs/tags/v0.1.0"
OIDC_ISSUER = "https://token.actions.githubusercontent.com"


class InstallResult:
    def __init__(self, returncode, output, installed, tool_calls):
        self.returncode = returncode
        self.output = output
        self.installed = installed
        self.tool_calls = tool_calls

    def calls_to(self, tool):
        return [call[1:] for call in self.tool_calls if call[0] == tool]


# A `timeout` stand-in: records its arguments, then runs the command.
FAKE_TIMEOUT = """#!/bin/sh
printf '%s\\n' "$*" >> "$FAKE_TIMEOUT_CALLS"
shift
exec "$@"
"""


def run_install(arguments=("v0.1.0",), tools=(), bundle="{}", environment_extra=None,
                temps_already_on_path=False, fake_timeout=False):
    """Run the installer to the end against a local archive.

    `tools` names fake verifiers to put on PATH (`cosign`, `gh`); `bundle` is
    the body served for `<tarball>.sigstore.json`, or None for a 404.
    """
    with tempfile.TemporaryDirectory(prefix="temps-install-complete-") as temporary:
        root = Path(temporary)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        (bin_dir / "curl").write_text(FAKE_CURL)
        (bin_dir / "uname").write_text('#!/bin/sh\necho "Linux x86_64"\n')
        names = ["curl", "uname"]
        if temps_already_on_path:
            (bin_dir / "temps").write_text("#!/bin/sh\n")
            names.append("temps")
        for tool in tools:
            (bin_dir / tool).write_text(FAKE_VERIFIER)
            names.append(tool)
        if fake_timeout:
            (bin_dir / "timeout").write_text(FAKE_TIMEOUT)
            names.append("timeout")
        for name in names:
            (bin_dir / name).chmod(0o755)

        payload = root / "payload"
        payload.mkdir()
        (payload / "temps").write_text("#!/bin/sh\necho fake temps\n")
        archive = root / "temps-linux-amd64.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle_archive:
            bundle_archive.add(payload / "temps", arcname="temps")

        home = root / "home"
        home.mkdir()
        calls = root / "calls"
        calls.touch()
        tool_calls = root / "tool-calls"
        tool_calls.touch()
        environment = dict(
            os.environ,
            PATH=os.pathsep.join([str(bin_dir), str(system_tool_dir(root))]),
            HOME=str(home),
            SHELL="/bin/bash",
            TEMPS_INSTALL=str(root / "install"),
            FAKE_ROUTES=str(root / "routes.json"),
            FAKE_CALLS=str(calls),
            FAKE_ARCHIVE=str(archive),
            FAKE_TOOL_CALLS=str(tool_calls),
        )
        for name in ("XDG_CONFIG_HOME", "FAKE_BUNDLE", "GH_TOKEN", "GITHUB_TOKEN"):
            environment.pop(name, None)
        if bundle is not None:
            environment["FAKE_BUNDLE"] = bundle
        environment.update(environment_extra or {})
        result = subprocess.run(
            ["bash", str(INSTALLER), *arguments],
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
        )
        return InstallResult(
            result.returncode,
            result.stdout + result.stderr,
            (root / "install" / "bin" / "temps").is_file(),
            [line.split("\t") for line in tool_calls.read_text().splitlines()],
        )


class CompletedInstallTests(unittest.TestCase):
    """Run a pinned install to the end against a local archive."""

    def install(self, temps_already_on_path):
        result = run_install(temps_already_on_path=temps_already_on_path)
        self.assertEqual(result.returncode, 0, result.output)
        self.assertTrue(result.installed, result.output)
        return result.output

    def test_fresh_install_links_to_capabilities_page(self):
        output = self.install(temps_already_on_path=False)
        self.assertIn("temps --help", output)
        self.assertIn(CAPABILITIES_URL, output)

    def test_reinstall_with_temps_on_path_links_to_capabilities_page(self):
        output = self.install(temps_already_on_path=True)
        self.assertIn("Run 'temps --help' to get started", output)
        self.assertIn(CAPABILITIES_URL, output)


def with_deadline_source():
    """The installer's `with_deadline` function, to run on its own."""
    text = INSTALLER.read_text()
    start = text.index("with_deadline() {")
    end = text.index("\n}\n", start) + 3
    return text[start:end]


def run_with_deadline(command, deadline, with_timeout_tool):
    """Run `with_deadline <command>` under bash with the given deadline, with
    or without a `timeout` tool on PATH; returns (exit code, output, seconds)."""
    with tempfile.TemporaryDirectory(prefix="temps-install-deadline-") as temporary:
        root = Path(temporary)
        farm = system_tool_dir(root)
        if not with_timeout_tool and (farm / "timeout").exists():
            (farm / "timeout").unlink()
        script = (f"set -euo pipefail\nverifier_deadline={deadline}\n"
                  f"{with_deadline_source()}\n"
                  f'output=$(with_deadline {command}) && code=0 || code=$?\n'
                  'printf "%s|%s" "$code" "$output"\n')
        started = time.monotonic()
        result = subprocess.run(["bash", "-c", script], env=dict(os.environ, PATH=str(farm)),
                                capture_output=True, text=True, timeout=60)
        elapsed = time.monotonic() - started
    code, _, output = result.stdout.partition("|")
    return int(code), output, elapsed


class DeadlineTests(unittest.TestCase):
    def test_a_hung_verifier_is_stopped_without_a_timeout_tool(self):
        code, _, elapsed = run_with_deadline("sleep 30", 1, with_timeout_tool=False)
        self.assertNotEqual(code, 0)
        self.assertLess(elapsed, 15)

    def test_a_verifier_keeps_its_output_and_status_without_a_timeout_tool(self):
        code, output, elapsed = run_with_deadline("sh -c 'echo checked; exit 3'", 30,
                                                  with_timeout_tool=False)
        self.assertEqual((code, output), (3, "checked"))
        # The watcher must not hold the captured pipe open until the deadline.
        self.assertLess(elapsed, 15)

    def test_a_hung_verifier_is_stopped_with_a_timeout_tool(self):
        if not shutil.which("timeout"):
            self.skipTest("no coreutils timeout on this host")
        code, _, elapsed = run_with_deadline("sleep 30", 1, with_timeout_tool=True)
        self.assertNotEqual(code, 0)
        self.assertLess(elapsed, 15)


class SignatureVerificationTests(unittest.TestCase):
    def assert_installed(self, result):
        self.assertEqual(result.returncode, 0, result.output)
        self.assertTrue(result.installed, result.output)

    def assert_aborted(self, result):
        self.assertEqual(result.returncode, 1, result.output)
        self.assertFalse(result.installed, result.output)

    def test_without_a_verifier_installs_and_says_origin_was_not_verified(self):
        result = run_install()
        self.assert_installed(result)
        self.assertIn("origin of", result.output)
        self.assertIn("NOT verified", result.output)
        # The manual command carries the pinned identity for this tag.
        self.assertIn(f"--certificate-identity {RELEASE_IDENTITY}", result.output)
        self.assertEqual(result.tool_calls, [])

    def test_signature_download_is_bounded_in_time_and_retried(self):
        with tempfile.TemporaryDirectory(prefix="temps-install-curl-args-") as temporary:
            recorded = Path(temporary) / "curl-args"
            result = run_install(environment_extra={"FAKE_CURL_ARGS": str(recorded)})
            (line,) = recorded.read_text().splitlines()
        self.assert_installed(result)
        arguments = line.split("\t")
        self.assertEqual(arguments[arguments.index("--connect-timeout") + 1], "15")
        self.assertEqual(arguments[arguments.index("--max-time") + 1], "60")
        self.assertEqual(arguments[arguments.index("--retry") + 1], "3")

    def test_verifiers_run_under_a_deadline_where_timeout_exists(self):
        with tempfile.TemporaryDirectory(prefix="temps-install-timeout-") as temporary:
            recorded = Path(temporary) / "timeout-calls"
            recorded.touch()
            result = run_install(tools=("gh",), fake_timeout=True,
                                 environment_extra={"FAKE_TIMEOUT_CALLS": str(recorded)})
            calls = recorded.read_text().splitlines()
        self.assert_installed(result)
        self.assertEqual([call.split()[:4] for call in calls],
                         [["300", "gh", "auth", "status"],
                          ["300", "gh", "attestation", "verify"]])

    def test_cosign_verifies_with_the_pinned_identity_and_issuer(self):
        result = run_install(tools=("cosign", "gh"))
        self.assert_installed(result)
        self.assertIn("Signature verified (cosign)", result.output)
        (arguments,) = result.calls_to("cosign")
        self.assertEqual(arguments[0], "verify-blob")
        self.assertEqual(arguments[arguments.index("--certificate-identity") + 1],
                         RELEASE_IDENTITY)
        self.assertEqual(arguments[arguments.index("--certificate-oidc-issuer") + 1],
                         OIDC_ISSUER)
        self.assertTrue(arguments[arguments.index("--bundle") + 1]
                        .endswith("temps-linux-amd64.tar.gz.sigstore.json"))
        self.assertTrue(arguments[-1].endswith("temps-linux-amd64.tar.gz"))
        # cosign wins; gh is not consulted.
        self.assertEqual(result.calls_to("gh"), [])

    def test_cosign_failure_aborts_the_install(self):
        result = run_install(tools=("cosign",), environment_extra={"FAKE_COSIGN_EXIT": "1"})
        self.assert_aborted(result)
        self.assertIn("Signature verification FAILED", result.output)
        self.assertIn(RELEASE_IDENTITY, result.output)
        self.assertIn("verification refused by test", result.output)

    def test_authenticated_gh_verifies_the_release_workflow_at_the_tag(self):
        result = run_install(tools=("gh",))
        self.assert_installed(result)
        self.assertIn("Build provenance verified (gh)", result.output)
        auth, verify = result.calls_to("gh")
        self.assertEqual(auth[:2], ["auth", "status"])
        self.assertEqual(verify[:2], ["attestation", "verify"])
        self.assertEqual(verify[verify.index("--repo") + 1], "gotempsh/temps")
        # Exact identity, never --signer-workflow (a prefix match in some gh
        # releases, which `release.yml-x.yml` would also satisfy).
        self.assertNotIn("--signer-workflow", verify)
        self.assertEqual(verify[verify.index("--cert-identity") + 1], RELEASE_IDENTITY)
        self.assertEqual(verify[verify.index("--source-ref") + 1], "refs/tags/v0.1.0")

    def test_gh_failure_aborts_the_install(self):
        result = run_install(tools=("gh",), environment_extra={"FAKE_GH_EXIT": "1"})
        self.assert_aborted(result)
        self.assertIn("Build provenance verification FAILED", result.output)

    def test_unauthenticated_gh_counts_as_no_verifier(self):
        # gh refuses to run without a login, which must not abort the install.
        result = run_install(tools=("gh",), environment_extra={"FAKE_GH_AUTH_EXIT": "1"})
        self.assert_installed(result)
        self.assertIn("NOT verified", result.output)
        self.assertEqual([call[0] for call in result.calls_to("gh")], ["auth"])

    def test_release_without_a_signature_is_refused(self):
        for tools in ((), ("cosign",)):
            with self.subTest(tools=tools):
                result = run_install(tools=tools, bundle=None)
                self.assert_aborted(result)
                self.assertIn("publishes no Sigstore signature", result.output)
                self.assertIn("temps-linux-amd64.tar.gz.sigstore.json", result.output)
                self.assertIn("--allow-unsigned v0.1.0", result.output)

    def test_allow_unsigned_installs_a_release_without_a_signature(self):
        result = run_install(arguments=("--allow-unsigned", "v0.1.0"), tools=("cosign",),
                             bundle=None)
        self.assert_installed(result)
        self.assertIn("--allow-unsigned was passed", result.output)
        self.assertEqual(result.calls_to("cosign"), [])

    def test_allow_unsigned_never_excuses_a_failed_signature(self):
        result = run_install(arguments=("--allow-unsigned", "v0.1.0"), tools=("cosign",),
                             environment_extra={"FAKE_COSIGN_EXIT": "1"})
        self.assert_aborted(result)
        self.assertIn("Signature verification FAILED", result.output)


if __name__ == "__main__":
    unittest.main()
