# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Plan which managed-database images `database-images.yml` builds and publishes.

Every image Temps runs for a managed database service (`images/*-walg`,
`images/postgres-ha`) is published to GHCR from here. Two tag kinds:

* floating -- one per supported major (`18-bookworm`, `pg18`, `8.0`). Rebuilt
  on every run so the image picks up upstream security rebuilds of the base.
* pinned -- one per upstream PostgreSQL minor (`18.3-bookworm`), discovered
  from the upstream registry so a new minor is published without a code
  change. The newest minor of each major is still refreshed upstream, so it is
  rebuilt every run; older minors are frozen upstream and are only built when
  missing from GHCR or when their build context changed.

Usage (from the workflow):
    database_image_matrix.py --owner gotempsh [--force postgres-walg,...] [--force-all]
                             [--only redis-walg,...] [--missing-only | --check]

Releases run it with --missing-only (publish what is absent, move nothing
else) and then --check, which fails the release unless every required tag is
anonymously pullable -- the way a fresh install will pull it.

Writes `matrix=<json>` and `count=<n>` to $GITHUB_OUTPUT when set, otherwise
prints the plan as JSON.
"""

import argparse
import base64
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

# How many of the newest upstream minors to publish per PostgreSQL major.
PINNED_MINORS_PER_MAJOR = 4

POSTGRES_UPSTREAM = "library/postgres"
POSTGRES_MINOR_TAG = re.compile(r"^(\d+)\.(\d+)-bookworm$")

# One entry per published repository. `majors` must cover every tag the
# runtime defaults and `ALLOWED_POSTGRES_DOCKER_IMAGES` reference.
CATALOG = [
    {
        "image": "postgres-walg",
        "build_arg": "PG_VERSION",
        "majors": ["15", "16", "17", "18"],
        "floating_tag": "{major}-bookworm",
        "pinned_tag": "{version}-bookworm",
        "pinned_upstream": POSTGRES_UPSTREAM,
        "postgres_binary": True,
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
        "postgres_binary": True,
    },
    {
        "image": "pgvector-walg",
        "build_arg": "PG_VERSION",
        "majors": ["17", "18"],
        "floating_tag": "pg{major}",
    },
    {
        "image": "timescaledb-walg",
        "build_arg": "PG_VERSION",
        "majors": ["17", "18"],
        "floating_tag": "pg{major}",
    },
    {
        "image": "redis-walg",
        "build_arg": "REDIS_VERSION",
        "majors": ["8"],
        "floating_tag": "{major}-bookworm",
    },
    {
        "image": "mongodb-walg",
        "build_arg": "MONGO_VERSION",
        "majors": ["7.0", "8.0"],
        "floating_tag": "{major}",
    },
]

HTTP_TIMEOUT_SECS = 30


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


def pinned_minors(tags, majors, keep):
    """Newest `keep` upstream minors per major, newest first: {major: [version]}."""
    found = {major: set() for major in majors}
    for tag in tags:
        match = POSTGRES_MINOR_TAG.match(tag)
        if match and match.group(1) in found:
            found[match.group(1)].add(int(match.group(2)))
    return {
        major: [f"{major}.{minor}" for minor in sorted(minors, reverse=True)[:keep]]
        for major, minors in found.items()
    }


class GhcrRegistry:
    """Existence checks against GHCR; any failure reads as "missing" (rebuild)."""

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
            try:
                body, _ = _get(url, headers)
                self._tokens[image] = json.loads(body).get("token")
            except (urllib.error.URLError, ValueError) as error:
                print(
                    f"warning: GHCR token for {self.owner}/{image} unavailable "
                    f"({error}); treating all its tags as missing",
                    file=sys.stderr,
                )
                self._tokens[image] = None
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
        try:
            with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT_SECS):
                return True
        except urllib.error.HTTPError as error:
            if error.code != 404:
                print(
                    f"warning: GHCR lookup of {self.owner}/{image}:{tag} returned "
                    f"HTTP {error.code}; treating it as missing",
                    file=sys.stderr,
                )
            return False
        except urllib.error.URLError as error:
            print(
                f"warning: GHCR lookup of {self.owner}/{image}:{tag} failed "
                f"({error}); treating it as missing",
                file=sys.stderr,
            )
            return False


def _entry(spec, owner, version, tag, expect_postgres, aliases=()):
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
        # Regex the built image's `postgres --version` must match, or "" to skip.
        "expect_postgres": expect_postgres,
    }


def candidates(owner, minors_by_upstream, catalog=CATALOG):
    """Every tag the catalog publishes, as (entry, refresh) pairs. `refresh` is
    true for tags whose upstream base still moves (floating majors and the
    newest minor of each major), which a regular run rebuilds unconditionally."""
    for spec in catalog:
        for major in spec["majors"]:
            expect = (
                rf"\(PostgreSQL\) {re.escape(major)}\." if spec.get("postgres_binary") else ""
            )
            tag = spec["floating_tag"].format(major=major)
            aliases = [alias.format(major=major) for alias in spec.get("floating_aliases", [])]
            yield _entry(spec, owner, major, tag, expect, aliases), True

        upstream = spec.get("pinned_upstream")
        if not upstream:
            continue
        minors = pinned_minors(
            minors_by_upstream[upstream], spec["majors"], PINNED_MINORS_PER_MAJOR
        )
        for versions in minors.values():
            for index, version in enumerate(versions):
                tag = spec["pinned_tag"].format(version=version)
                expect = rf"\(PostgreSQL\) {re.escape(version)}(\s|$)"
                yield _entry(spec, owner, version, tag, expect), index == 0


def _published(registry, entry):
    return all(registry.has_tag(entry["image"], tag) for tag in entry["tags"])


def plan(owner, minors_by_upstream, registry, forced, catalog=CATALOG, missing_only=False):
    """Build the matrix.

    `forced` is a set of image names (or {"*"}) to rebuild in full regardless
    of what GHCR already holds. `missing_only` (release runs) builds nothing
    that is already published, so a release never moves a tag under a
    running database."""
    include = []
    for entry, refresh in candidates(owner, minors_by_upstream, catalog):
        if missing_only:
            build = not _published(registry, entry)
        else:
            force = "*" in forced or entry["image"] in forced
            build = force or refresh or not _published(registry, entry)
        if build:
            include.append(entry)
    return include


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

    upstreams = {spec["pinned_upstream"] for spec in catalog if spec.get("pinned_upstream")}
    minors_by_upstream = {upstream: upstream_tags(upstream) for upstream in upstreams}

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
    include = plan(
        args.owner, minors_by_upstream, registry, forced, catalog, args.missing_only
    )

    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as handle:
            handle.write(f"matrix={json.dumps({'include': include})}\n")
            handle.write(f"count={len(include)}\n")
    else:
        print(json.dumps(include, indent=2))
    for entry in include:
        print(f"plan: {entry['refs'].replace(chr(10), ' ')}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
