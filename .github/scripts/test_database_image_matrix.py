# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import re
import unittest

from database_image_matrix import (
    CATALOG,
    PINNED_MINORS_PER_MAJOR,
    main,
    pinned_minors,
    plan,
    unpublished,
)

UPSTREAM_TAGS = [
    "18", "18-bookworm", "18-alpine", "18.6", "18.6-trixie", "18.6-alpine",
    "18.0-bookworm", "18.1-bookworm", "18.2-bookworm", "18.3-bookworm",
    "18.4-bookworm", "18.6-bookworm", "17.9-bookworm", "17.10-bookworm",
    "17.11-bookworm", "16.15-bookworm", "15.19-bookworm", "14.20-bookworm",
    "18.7rc1-bookworm", "latest",
]


class FakeRegistry:
    def __init__(self, existing=()):
        self.existing = set(existing)
        self.lookups = []

    def has_tag(self, image, tag):
        self.lookups.append((image, tag))
        return (image, tag) in self.existing


def tags(include, image):
    return [entry["tag"] for entry in include if entry["image"] == image]


def run(registry, forced=frozenset(), missing_only=False):
    return plan(
        "gotempsh",
        {"library/postgres": UPSTREAM_TAGS},
        registry,
        set(forced),
        missing_only=missing_only,
    )


def every_tag():
    return {(e["image"], tag) for e in run(FakeRegistry(), {"*"}) for tag in e["tags"]}


class PinnedMinorsTests(unittest.TestCase):
    def test_minors_sort_numerically_not_lexically(self):
        minors = pinned_minors(UPSTREAM_TAGS, ["17"], 2)
        self.assertEqual(minors["17"], ["17.11", "17.10"])

    def test_keeps_newest_n_and_ignores_other_variants(self):
        minors = pinned_minors(UPSTREAM_TAGS, ["18"], 4)
        # No 18.5-bookworm upstream: the gap is skipped, not invented.
        self.assertEqual(minors["18"], ["18.6", "18.4", "18.3", "18.2"])

    def test_unsupported_majors_are_not_published(self):
        minors = pinned_minors(UPSTREAM_TAGS, ["18"], 4)
        self.assertNotIn("14", minors)


class PlanTests(unittest.TestCase):
    def test_first_run_publishes_every_floating_and_pinned_tag(self):
        include = run(FakeRegistry())
        walg = tags(include, "postgres-walg")
        for tag in ["15-bookworm", "16-bookworm", "17-bookworm", "18-bookworm",
                    "18.6-bookworm", "18.4-bookworm", "18.3-bookworm", "18.2-bookworm"]:
            self.assertIn(tag, walg)
        self.assertNotIn("18.1-bookworm", walg)  # beyond PINNED_MINORS_PER_MAJOR
        self.assertEqual(tags(include, "mongodb-walg"), ["7.0", "8.0"])
        self.assertEqual(tags(include, "pgvector-walg"), ["pg17", "pg18"])

    def test_scheduled_run_skips_frozen_minors_already_published(self):
        existing = {("postgres-walg", f"18.{m}-bookworm") for m in (2, 3, 4, 6)}
        include = run(FakeRegistry(existing))
        walg = tags(include, "postgres-walg")
        # Newest minor still receives upstream rebuilds, so it is always refreshed.
        self.assertIn("18.6-bookworm", walg)
        for frozen in ["18.4-bookworm", "18.3-bookworm", "18.2-bookworm"]:
            self.assertNotIn(frozen, walg)
        # Floating tags are rebuilt every run.
        self.assertIn("18-bookworm", walg)

    def test_forced_image_rebuilds_existing_minors_without_lookups(self):
        existing = {("postgres-ha", "18.3-bookworm")}
        registry = FakeRegistry(existing)
        include = run(registry, {"postgres-ha"})
        self.assertIn("18.3-bookworm", tags(include, "postgres-ha"))
        self.assertFalse(any(image == "postgres-ha" for image, _ in registry.lookups))

    def test_force_all_covers_every_image(self):
        existing = {("postgres-walg", "18.3-bookworm"), ("postgres-ha", "18.3-bookworm")}
        include = run(FakeRegistry(existing), {"*"})
        self.assertIn("18.3-bookworm", tags(include, "postgres-walg"))
        self.assertIn("18.3-bookworm", tags(include, "postgres-ha"))

    def test_entries_carry_build_inputs_and_ghcr_repository(self):
        entry = next(e for e in run(FakeRegistry()) if e["tag"] == "18.3-bookworm")
        self.assertEqual(entry["repository"], "ghcr.io/gotempsh/postgres-walg")
        self.assertEqual(entry["context"], "images/postgres-walg")
        self.assertEqual(entry["build_arg"], "PG_VERSION=18.3")

    def test_postgres_version_expectation_is_exact(self):
        include = run(FakeRegistry())
        pinned = next(e for e in include if e["tag"] == "18.3-bookworm")
        floating = next(e for e in include if e["tag"] == "18-bookworm")
        ok = "postgres (PostgreSQL) 18.3 (Debian 18.3-1.pgdg12+1)"
        self.assertRegex(ok, pinned["expect_postgres"])
        self.assertNotRegex("postgres (PostgreSQL) 18.30", pinned["expect_postgres"])
        self.assertNotRegex("postgres (PostgreSQL) 18.6 (Debian)", pinned["expect_postgres"])
        self.assertRegex("postgres (PostgreSQL) 18.6 (Debian)", floating["expect_postgres"])
        self.assertNotRegex("postgres (PostgreSQL) 17.11", floating["expect_postgres"])

    def test_non_postgres_images_skip_the_version_check(self):
        for entry in run(FakeRegistry()):
            if entry["image"] in {"redis-walg", "mongodb-walg", "timescaledb-walg", "pgvector-walg"}:
                self.assertEqual(entry["expect_postgres"], "")

    def test_matrix_stays_within_github_limit(self):
        self.assertLessEqual(len(run(FakeRegistry(), {"*"})), 256)

    def test_runtime_referenced_tags_are_in_the_catalog(self):
        # Tags the runtime defaults and the Postgres allowlist reference
        # (crates/temps-providers/src/externalsvc/*.rs) must be published.
        include = run(FakeRegistry())
        published = {f"{e['image']}:{tag}" for e in include for tag in e["tags"]}
        for reference in [
            "postgres-walg:15-bookworm", "postgres-walg:16-bookworm",
            "postgres-walg:17-bookworm", "postgres-walg:18-bookworm",
            "pgvector-walg:pg17", "pgvector-walg:pg18", "timescaledb-walg:pg18",
            "postgres-ha:17-bookworm", "postgres-ha:18-bookworm",
            "postgres-ha:18-bookworm-walg",
            "redis-walg:8-bookworm", "mongodb-walg:7.0", "mongodb-walg:8.0",
        ]:
            self.assertIn(reference, published)

    def test_catalog_contexts_exist(self):
        import os

        root = os.path.join(os.path.dirname(__file__), "..", "..")
        for spec in CATALOG:
            dockerfile = os.path.join(root, "images", spec["image"], "Dockerfile")
            self.assertTrue(os.path.isfile(dockerfile), dockerfile)
            with open(dockerfile, encoding="utf-8") as handle:
                self.assertRegex(handle.read(), rf"ARG {re.escape(spec['build_arg'])}=")

    def test_pinned_minor_count_matches_constant(self):
        include = run(FakeRegistry())
        pinned = [t for t in tags(include, "postgres-walg") if t.startswith("18.")]
        self.assertEqual(len(pinned), PINNED_MINORS_PER_MAJOR)


class AliasTests(unittest.TestCase):
    def test_cluster_default_alias_is_built_with_its_major(self):
        entry = next(e for e in run(FakeRegistry()) if e["image"] == "postgres-ha" and e["tag"] == "18-bookworm")
        self.assertEqual(entry["tags"], ["18-bookworm", "18-bookworm-walg"])
        self.assertEqual(
            entry["refs"].splitlines(),
            ["ghcr.io/gotempsh/postgres-ha:18-bookworm", "ghcr.io/gotempsh/postgres-ha:18-bookworm-walg"],
        )

    def test_missing_alias_alone_triggers_a_build_and_fails_the_check(self):
        published = every_tag() - {("postgres-ha", "18-bookworm-walg")}
        include = run(FakeRegistry(published), missing_only=True)
        self.assertEqual([(e["image"], e["tag"]) for e in include], [("postgres-ha", "18-bookworm")])
        missing = unpublished("gotempsh", {"library/postgres": UPSTREAM_TAGS}, FakeRegistry(published))
        self.assertEqual([e["tag"] for e in missing], ["18-bookworm"])

    def test_pinned_minors_have_no_aliases(self):
        for entry in run(FakeRegistry()):
            if "." in entry["version"] and entry["image"].startswith("postgres"):
                self.assertEqual(entry["tags"], [entry["tag"]])


class ReleaseModeTests(unittest.TestCase):
    def test_missing_only_never_moves_published_tags(self):
        # A release must not rebuild 18-bookworm under running databases just
        # because Temps shipped; only absent tags are built.
        published = every_tag() - {("postgres-walg", "16-bookworm"), ("redis-walg", "8-bookworm")}
        include = run(FakeRegistry(published), missing_only=True)
        self.assertEqual(
            {(e["image"], e["tag"]) for e in include},
            {("postgres-walg", "16-bookworm"), ("redis-walg", "8-bookworm")},
        )

    def test_missing_only_with_everything_published_builds_nothing(self):
        self.assertEqual(run(FakeRegistry(every_tag()), missing_only=True), [])

    def test_unpublished_lists_exactly_the_gaps(self):
        published = every_tag() - {("postgres-ha", "18.3-bookworm")}
        missing = unpublished("gotempsh", {"library/postgres": UPSTREAM_TAGS}, FakeRegistry(published))
        self.assertEqual([(e["image"], e["tag"]) for e in missing], [("postgres-ha", "18.3-bookworm")])

    def test_unpublished_is_empty_when_everything_is_public(self):
        missing = unpublished("gotempsh", {"library/postgres": UPSTREAM_TAGS}, FakeRegistry(every_tag()))
        self.assertEqual(missing, [])

    def test_check_and_missing_only_are_mutually_exclusive(self):
        with self.assertRaises(SystemExit):
            main(["--owner", "gotempsh", "--check", "--missing-only"])

    def test_rejects_unknown_images_and_uppercase_owner(self):
        for argv in (
            ["--owner", "gotempsh", "--force", "postgres-wal"],
            ["--owner", "gotempsh", "--only", "mariadb-walg"],
            ["--owner", "GoTempsh"],
        ):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                main(argv)


if __name__ == "__main__":
    unittest.main()
