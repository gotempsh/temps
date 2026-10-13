# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise installer release lookup without network access or installation.

A fake `curl` serves canned GitHub API responses and refuses every download,
so each run stops right after the installer has decided what to fetch; the
download URL in the failure message tells us which release it picked. A fake
`uname` pins the platform to linux-amd64.
"""

import json
import os
from pathlib import Path
import subprocess
import tempfile
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
    sys.stderr.write("download refused by test\n")
    sys.exit(22)
routes = json.load(open(os.environ["FAKE_ROUTES"]))
status, body = routes.get(url.split("/repos/gotempsh/temps/", 1)[1], routes["default"])
sys.stdout.write(body + "\n" + status)
'''


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
    def install(self, *arguments, routes=None, status="404"):
        """Run the installer against canned API routes (path -> (status, body))."""
        table = {"default": [status, "{}"]}
        for path, (route_status, body) in (routes or {}).items():
            table[path] = [route_status, body]
        with tempfile.TemporaryDirectory(prefix="temps-install-lookup-") as temporary:
            root = Path(temporary)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            (bin_dir / "curl").write_text(FAKE_CURL)
            (bin_dir / "uname").write_text('#!/bin/sh\necho "Linux x86_64"\n')
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


if __name__ == "__main__":
    unittest.main()
