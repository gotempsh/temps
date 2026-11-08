# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Plan which managed-database images `database-images.yml` builds and publishes.

Every image Temps runs for a managed database service (`images/*-walg`,
`images/postgres-ha`) is published to GHCR from here. Two tag kinds:

* floating -- one per supported major (`18-bookworm`, `pg18`, `8.0`). Rebuilt
  on every run so the image picks up upstream security rebuilds of the base.
* pinned -- one per upstream release (`18.3-bookworm`, `8.10.3-trixie`),
  discovered from the upstream registry so a new release is published without
  a code change. The newest release of each major is still refreshed
  upstream, so it is rebuilt every run; older releases are frozen upstream and
  are only built when missing from GHCR or when their build context changed.

For images with an upstream, every tag carries the exact server version its
build must report. A floating tag must report the newest upstream release of
its major in *any* variant: when upstream stops publishing the Debian variant
an image is built from (Redis did, after 8.4.0-bookworm), the floating base
tag silently freezes on an old release, and this check turns that into a
failed build instead of a weekly republish of an unpatched server.

New majors are not published automatically: supporting one is a product
decision (runtime defaults, allowlists, upgrade paths). `--new-majors` lists
every upstream major newer than the newest one an image supports, so the
weekly run fails visibly instead of nobody noticing. Add the major to
`majors`, or record it in `deferred_majors` to acknowledge it for now.

Usage (from the workflow):
    database_image_matrix.py --owner gotempsh [--force postgres-walg,...] [--force-all]
                             [--only redis-walg,...] [--missing-only | --check | --new-majors]

Releases run it with --missing-only (publish what is absent, move nothing
else) and then --check, which fails the release unless every required tag is
anonymously pullable -- the way a fresh install will pull it.

A tag that is already published is never rebuilt by a release: a missing
alias of a published image is re-pointed at that image's digest (`retags`)
instead, and a GHCR lookup that fails -- rather than answering "absent" --
aborts the plan, since "could not look" must never read as "missing".

Writes `matrix=<json>`, `count=<n>`, `retags=<json>` and `retag_count=<n>` to
$GITHUB_OUTPUT when set, otherwise prints the plan as JSON.
"""

import argparse
import base64
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

# How many of the newest upstream releases to publish per major.
PINNED_MINORS_PER_MAJOR = 4

POSTGRES_UPSTREAM = "library/postgres"
# PostgreSQL releases are `major.minor`; Redis releases are `major.minor.patch`.
POSTGRES_RELEASE = r"(\d+)\.(\d+)"
REDIS_UPSTREAM = "library/redis"
REDIS_RELEASE = r"(\d+)\.(\d+)\.(\d+)"
POSTGRES_MINOR_TAG = re.compile(rf"^{POSTGRES_RELEASE}-bookworm$")

# One entry per published repository. `majors` must cover every tag the
# runtime defaults and `ALLOWED_POSTGRES_DOCKER_IMAGES` reference.
#
# New upstream majors are watched in `majors_upstream` (default:
# `pinned_upstream`), whose tags matching `major_tag` (default: a plain
# `release` tag) name a major in their first group. `deferred_majors` are
# newer majors seen upstream and deliberately not supported yet.
CATALOG = [
    {
        "image": "postgres-walg",
        "build_arg": "PG_VERSION",
        "majors": ["15", "16", "17", "18"],
        "floating_tag": "{major}-bookworm",
        "pinned_tag": "{version}-bookworm",
        "pinned_upstream": POSTGRES_UPSTREAM,
        "release": POSTGRES_RELEASE,
        "version_command": "postgres",
    },
    {
        "image": "postgres-ha",
        "build_arg": "PG_VERSION",
        "majors": ["15", "16", "17", "18"],
        "floating_tag": "{major}-bookworm",
        # DEFAULT_CLUSTER_IMAGE (postgres_cluster.rs) names the `-walg` variant
        # from before WAL-G was bundled; every build now has WAL-G, so the
        # alias points at the same image.
        "floating_aliases": ["{major}-bookworm-walg"],
        "pinned_tag": "{version}-bookworm",
        "pinned_upstream": POSTGRES_UPSTREAM,
        "release": POSTGRES_RELEASE,
        "version_command": "postgres",
    },
    {
        "image": "pgvector-walg",
        "build_arg": "PG_VERSION",
        "majors": ["17", "18"],
        "floating_tag": "pg{major}",
        "majors_upstream": "pgvector/pgvector",
        "major_tag": r"^pg(\d+)$",
    },
    {
        "image": "timescaledb-walg",
        "build_arg": "PG_VERSION",
        "majors": ["17", "18"],
        "floating_tag": "pg{major}",
        "majors_upstream": "timescale/timescaledb-ha",
        "major_tag": r"^pg(\d+)$",
    },
    {
        # Trixie, not bookworm: upstream publishes no Redis release after
        # 8.4.0 for bookworm, so `redis:8-bookworm` stopped receiving fixes.
        "image": "redis-walg",
        "build_arg": "REDIS_VERSION",
        "majors": ["8"],
        "floating_tag": "{major}-trixie",
        "pinned_tag": "{version}-trixie",
        "pinned_upstream": REDIS_UPSTREAM,
        "release": REDIS_RELEASE,
        "version_command": "redis-server",
    },
    {
        "image": "mongodb-walg",
        "build_arg": "MONGO_VERSION",
        "majors": ["7.0", "8.0"],
        "floating_tag": "{major}",
        "majors_upstream": "library/mongo",
        # Only `X.0` lines: since 5.0 the x.1-x.3 lines are rapid releases.
        "major_tag": r"^(\d+\.0)\.\d+$",
        "deferred_majors": ["9.0"],
    },
]

HTTP_TIMEOUT_SECS = 30
# GHCR lookups that fail transiently (rate limit, outage, network) are retried
# this many times, LOOKUP_RETRY_DELAY_SECS * attempt apart, before giving up.
LOOKUP_ATTEMPTS = 3
LOOKUP_RETRY_DELAY_SECS = 2


def _get(url, headers=None):
    request = urllib.request.Request(url, headers=headers or {})
    with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT_SECS) as response:
        return response.read(), response.headers


def upstream_tags(repository):
    """All tags of a Docker Hub repository, following registry pagination."""
    token_url = (
        "https://auth.docker.io/token?service=registry.docker.io&scope="
        + urllib.parse.quote(f"repository:{repository}:pull")
    )
    try:
        body, _ = _get(token_url)
        token = json.loads(body)["token"]
        tags = []
        url = f"https://registry-1.docker.io/v2/{repository}/tags/list?n=10000"
        while url:
            body, headers = _get(url, {"Authorization": f"Bearer {token}"})
            tags.extend(json.loads(body).get("tags") or [])
            link = headers.get("Link")
            match = re.search(r"<([^>]+)>", link) if link else None
            url = urllib.parse.urljoin(url, match.group(1)) if match else None
        return tags
    except (urllib.error.URLError, KeyError, ValueError) as error:
        raise RuntimeError(
            f"Failed to list upstream tags for docker.io/{repository}: {error}"
        ) from error


def pinned_minors(tags, majors, keep, pattern=POSTGRES_MINOR_TAG):
    """Newest `keep` upstream releases per major, newest first: {major: [version]}.

    `pattern` matches one release tag; its first group is the major and the
    rest are the numeric components that order releases within it."""
    found = {major: set() for major in majors}
    for tag in tags:
        match = pattern.match(tag)
        if match and match.group(1) in found:
            found[match.group(1)].add(tuple(int(part) for part in match.groups()[1:]))
    return {
        major: [
            ".".join([major, *map(str, release)])
            for release in sorted(releases, reverse=True)[:keep]
        ]
        for major, releases in found.items()
    }


def _variant_pattern(spec):
    """Release tags of the variant `spec` builds from, e.g. `18.3-bookworm`."""
    suffix = spec["pinned_tag"].format(version="")
    return re.compile(rf"^{spec['release']}{re.escape(suffix)}$")


def newest_release(tags, spec, major):
    """Newest upstream release of `major` in any variant, from the plain
    release tags (`8.10.3`, `18.6`). Raises when there is none: a supported
    major upstream no longer publishes cannot be built or verified."""
    newest = pinned_minors(tags, [major], 1, re.compile(rf"^{spec['release']}$"))[major]
    if not newest:
        raise RuntimeError(
            f"No upstream release of {spec['image']} major {major} found in "
            f"docker.io/{spec['pinned_upstream']}; check `majors` in CATALOG"
        )
    return newest[0]


def _major_key(major):
    return tuple(int(part) for part in major.split("."))


def majors_upstream(spec):
    """Docker Hub repository watched for new majors of `spec`, or None."""
    return spec.get("majors_upstream") or spec.get("pinned_upstream")


def new_majors(spec, tags):
    """Upstream majors newer than the newest one `spec` supports and not in
    its `deferred_majors`, oldest first. Older majors upstream still ships
    (PostgreSQL 14) are unsupported on purpose and never reported."""
    pattern = re.compile(spec.get("major_tag") or rf"^{spec['release']}$")
    newest_supported = max(_major_key(major) for major in spec["majors"])
    deferred = set(spec.get("deferred_majors", []))
    found = {match.group(1) for tag in tags for match in [pattern.match(tag)] if match}
    return sorted(
        (
            major
            for major in found
            if _major_key(major) > newest_supported and major not in deferred
        ),
        key=_major_key,
    )


def _expect_version(spec, version):
    """Regex the image's `<version_command> --version` must match for `version`."""
    if spec["version_command"] == "postgres":
        return rf"\(PostgreSQL\) {re.escape(version)}(\s|$)"
    return rf"v={re.escape(version)}(\s|$)"


class RegistryLookupError(RuntimeError):
    """GHCR could not say whether a tag exists (rate limit, outage, network)."""


def _ghcr(request, what):
    """Send a GHCR request and return the response body, or None when GHCR
    answers that `what` is absent or not visible to these credentials
    (401/403/404). Any other failure is retried and then raised."""
    failure = ""
    for attempt in range(1, LOOKUP_ATTEMPTS + 1):
        try:
            with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT_SECS) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            if error.code in (401, 403, 404):
                return None
            failure = f"HTTP {error.code}"
        except OSError as error:
            failure = str(error)
        if attempt < LOOKUP_ATTEMPTS:
            time.sleep(LOOKUP_RETRY_DELAY_SECS * attempt)
    raise RegistryLookupError(
        f"GHCR lookup of {what} failed after {LOOKUP_ATTEMPTS} attempts: {failure}"
    )


class GhcrRegistry:
    """Existence checks against GHCR. `has_tag` is false only when GHCR says
    the tag is absent (or not visible to these credentials); a lookup that
    cannot be completed raises RegistryLookupError instead."""

    def __init__(self, owner, username=None, password=None):
        self.owner = owner
        self.username = username
        self.password = password
        self._tokens = {}

    def _token(self, image):
        if image not in self._tokens:
            url = "https://ghcr.io/token?scope=" + urllib.parse.quote(
                f"repository:{self.owner}/{image}:pull"
            )
            headers = {}
            if self.username and self.password:
                basic = base64.b64encode(
                    f"{self.username}:{self.password}".encode()
                ).decode()
                headers["Authorization"] = f"Basic {basic}"
            what = f"the pull token for {self.owner}/{image}"
            # GHCR refuses a token (403) for a package that does not exist yet
            # or is private to these credentials: none of its tags are pullable.
            body = _ghcr(urllib.request.Request(url, headers=headers), what)
            token = None
            if body is not None:
                try:
                    token = json.loads(body)["token"]
                except (ValueError, KeyError, TypeError) as error:
                    raise RegistryLookupError(
                        f"GHCR lookup of {what} returned an unusable response: {error!r}"
                    ) from error
            self._tokens[image] = token
        return self._tokens[image]

    def has_tag(self, image, tag):
        token = self._token(image)
        if not token:
            return False
        request = urllib.request.Request(
            f"https://ghcr.io/v2/{self.owner}/{image}/manifests/{tag}",
            method="HEAD",
            headers={
                "Authorization": f"Bearer {token}",
                "Accept": ", ".join(
                    [
                        "application/vnd.oci.image.index.v1+json",
                        "application/vnd.docker.distribution.manifest.list.v2+json",
                        "application/vnd.oci.image.manifest.v1+json",
                        "application/vnd.docker.distribution.manifest.v2+json",
                    ]
                ),
            },
        )
        return _ghcr(request, f"{self.owner}/{image}:{tag}") is not None


def _entry(spec, owner, version, tag, expect_version, aliases=()):
    repository = f"ghcr.io/{owner}/{spec['image']}"
    tags = [tag, *aliases]
    return {
        "image": spec["image"],
        "repository": repository,
        "context": f"images/{spec['image']}",
        "build_arg": f"{spec['build_arg']}={version}",
        "tag": tag,
        "tags": tags,
        # Newline-separated full references, the format build-push-action takes.
        "refs": "\n".join(f"{repository}:{t}" for t in tags),
        "version": version,
        # Binary whose `--version` the built image is checked with, or "" to skip.
        "version_command": spec.get("version_command", ""),
        # Regex that output must match, or "" to skip.
        "expect_version": expect_version,
    }


def candidates(owner, minors_by_upstream, catalog=CATALOG):
    """Every tag the catalog publishes, as (entry, refresh) pairs. `refresh` is
    true for tags whose upstream base still moves (floating majors and the
    newest minor of each major), which a regular run rebuilds unconditionally."""
    for spec in catalog:
        upstream = spec.get("pinned_upstream")
        released = minors_by_upstream[upstream] if upstream else []
        for major in spec["majors"]:
            # A floating tag must carry the newest release of its major, not
            # merely some release of it: see the module docstring.
            expect = (
                _expect_version(spec, newest_release(released, spec, major))
                if upstream
                else ""
            )
            tag = spec["floating_tag"].format(major=major)
            aliases = [alias.format(major=major) for alias in spec.get("floating_aliases", [])]
            yield _entry(spec, owner, major, tag, expect, aliases), True

        if not upstream:
            continue
        minors = pinned_minors(
            released, spec["majors"], PINNED_MINORS_PER_MAJOR, _variant_pattern(spec)
        )
        for versions in minors.values():
            for index, version in enumerate(versions):
                tag = spec["pinned_tag"].format(version=version)
                yield _entry(spec, owner, version, tag, _expect_version(spec, version)), index == 0


def _published(registry, entry):
    return all(registry.has_tag(entry["image"], tag) for tag in entry["tags"])


def plan(owner, minors_by_upstream, registry, forced, catalog=CATALOG, missing_only=False):
    """Decide what to publish. Returns `(builds, retags)`.

    `forced` is a set of image names (or {"*"}) to rebuild in full regardless
    of what GHCR already holds. `missing_only` (release runs) builds nothing
    that is already published, so a release never moves a tag under a
    running database.

    `retags` covers an image that is published under some of its tags but not
    all (a new alias): each missing tag is pointed at the existing image
    rather than rebuilt, which would replace the tags already there."""
    builds, retags = [], []
    for entry, refresh in candidates(owner, minors_by_upstream, catalog):
        rebuild = "*" in forced or entry["image"] in forced or refresh
        if rebuild and not missing_only:
            builds.append(entry)
            continue
        present = [tag for tag in entry["tags"] if registry.has_tag(entry["image"], tag)]
        if not present:
            builds.append(entry)
            continue
        source = f"{entry['repository']}:{present[0]}"
        for tag in entry["tags"]:
            if tag not in present:
                retags.append(
                    {
                        "image": entry["image"],
                        "source": source,
                        "target": f"{entry['repository']}:{tag}",
                    }
                )
    return builds, retags


def unpublished(owner, minors_by_upstream, registry, catalog=CATALOG):
    """Tags the catalog requires that `registry` cannot pull."""
    return [
        entry
        for entry, _ in candidates(owner, minors_by_upstream, catalog)
        if not _published(registry, entry)
    ]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--owner", required=True, help="GHCR namespace, e.g. gotempsh")
    parser.add_argument(
        "--force",
        default="",
        help="Comma-separated images to rebuild in full (build context changed)",
    )
    parser.add_argument("--force-all", action="store_true")
    parser.add_argument(
        "--only",
        default="",
        help="Comma-separated images to restrict the plan to (pull request checks)",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--missing-only",
        action="store_true",
        help="Plan only tags not yet on GHCR (release runs)",
    )
    mode.add_argument(
        "--check",
        action="store_true",
        help="Exit 1 unless every required tag is anonymously pullable from GHCR",
    )
    mode.add_argument(
        "--new-majors",
        action="store_true",
        help="Exit 1 if upstream released a major newer than an image supports",
    )
    args = parser.parse_args(argv)

    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", args.owner):
        parser.error(f"invalid GHCR owner '{args.owner}': must be lowercase")

    known = {spec["image"] for spec in CATALOG}
    forced = {name for name in args.force.split(",") if name}
    only = {name for name in args.only.split(",") if name}
    for flag, names in (("--force", forced), ("--only", only)):
        unknown = names - known
        if unknown:
            parser.error(f"unknown image(s) in {flag}: {', '.join(sorted(unknown))}")
    if args.force_all:
        forced = {"*"}
    catalog = [spec for spec in CATALOG if not only or spec["image"] in only]

    if args.new_majors:
        return report_new_majors(catalog)

    upstreams = {spec["pinned_upstream"] for spec in catalog if spec.get("pinned_upstream")}
    minors_by_upstream = {upstream: upstream_tags(upstream) for upstream in upstreams}

    try:
        return _run(args, catalog, forced, minors_by_upstream)
    except RegistryLookupError as error:
        print(
            f"error: {error}. Not treating the tag as missing: that would rebuild over "
            "an image that may already be published. Re-run once GHCR is reachable.",
            file=sys.stderr,
        )
        return 1


def report_new_majors(catalog):
    """Print every unsupported newer upstream major; 1 when there is any."""
    watched = {majors_upstream(spec) for spec in catalog} - {None}
    tags = {repository: upstream_tags(repository) for repository in sorted(watched)}
    lines = []
    for spec in catalog:
        repository = majors_upstream(spec)
        if not repository:
            continue
        for major in new_majors(spec, tags[repository]):
            lines.append(
                f"- `{spec['image']}`: docker.io/{repository} released {major}; "
                f"supported majors are {', '.join(spec['majors'])}"
            )
    if not lines:
        print("No upstream major newer than the supported ones")
        return 0
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write("### New upstream majors not supported yet\n\n")
            handle.write("\n".join(lines) + "\n")
    print(
        "error: upstream released major version(s) no managed-database image "
        "supports yet. Add each to `majors` in CATALOG (and the runtime "
        "defaults, allowlists and console upgrade options), or to "
        "`deferred_majors` to acknowledge it:\n" + "\n".join(lines),
        file=sys.stderr,
    )
    return 1


def _run(args, catalog, forced, minors_by_upstream):
    if args.check:
        # Anonymous on purpose: a fresh Temps install pulls without GHCR
        # credentials, so a private package is as broken as a missing one.
        missing = unpublished(args.owner, minors_by_upstream, GhcrRegistry(args.owner), catalog)
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        lines = [f"- `{ref}`" for entry in missing for ref in entry["refs"].splitlines()]
        if summary and missing:
            with open(summary, "a", encoding="utf-8") as handle:
                handle.write("### Database images not publicly pullable from GHCR\n\n")
                handle.write("\n".join(lines) + "\n")
        if missing:
            print(
                f"error: {len(missing)} required database image tag(s) are missing from "
                "GHCR or not public. Run the 'Database images' workflow, and set any "
                "new package's visibility to Public in the organization package "
                "settings:\n" + "\n".join(lines),
                file=sys.stderr,
            )
            return 1
        print("All required database image tags are publicly pullable from GHCR")
        return 0

    registry = GhcrRegistry(
        args.owner, os.environ.get("GHCR_USERNAME"), os.environ.get("GHCR_TOKEN")
    )
    include, retags = plan(
        args.owner, minors_by_upstream, registry, forced, catalog, args.missing_only
    )

    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as handle:
            handle.write(f"matrix={json.dumps({'include': include})}\n")
            handle.write(f"count={len(include)}\n")
            handle.write(f"retags={json.dumps(retags)}\n")
            handle.write(f"retag_count={len(retags)}\n")
    else:
        print(json.dumps({"builds": include, "retags": retags}, indent=2))
    for entry in include:
        print(f"plan: build {entry['refs'].replace(chr(10), ' ')}", file=sys.stderr)
    for retag in retags:
        print(f"plan: retag {retag['target']} -> {retag['source']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
