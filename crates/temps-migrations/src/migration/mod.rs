// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Every migration in this directory, registered by `build.rs`.
//!
//! Adding a migration means adding a `mYYYYMMDD_NNNNNN_description.rs` file
//! here with a stamp later than every existing one. `build.rs` declares one
//! module per file and generates the list `Migrator::migrations()` returns:
//! the file names sorted, with the shipped exceptions in
//! `order_exceptions.txt` (rules in `src/migration_order.rs`). The order is
//! upgrade history, checked against `tests/fixtures/migration_order.txt` by
//! `registry_tests` below.
//!
//! `m20260421_000001_squash_apr_post_v006` squashes the 34 migrations from
//! m20260403 through m20260420 into one. Databases created before it still
//! have the original names in `seaql_migrations`; on a local database already
//! past those, insert the squash migration's name into `seaql_migrations` by
//! hand to mark it done.

pub use sea_orm_migration::prelude::*;

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        generated_migrations()
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn migration_names_are_unique_and_upgrade_history_stays_stable() {
        let names = Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>();
        let unique = names.iter().collect::<HashSet<_>>();
        assert_eq!(
            unique.len(),
            names.len(),
            "every registry entry must have a unique persisted migration name"
        );

        for (shipped, added) in [
            (
                "m20260912_000003_managed_daemon_workspace_images_v032",
                "m20260713_000002_add_dns_ownership",
            ),
            (
                "m20260713_000002_add_dns_ownership",
                "m20260714_000001_create_domain_delivery",
            ),
            (
                "m20260810_000001_create_sandbox_snapshots",
                "m20260810_000001_add_cli_session_id_to_ai_conversations",
            ),
            (
                "m20260811_000001_create_renewal_attempts",
                "m20260811_000001_add_cli_session_fingerprint",
            ),
            (
                "m20260815_000001_add_facet_attr_columns_to_otel_spans",
                "m20260815_000001_default_preview_inclusion_off",
            ),
            (
                "m20260830_000001_add_external_service_creator",
                "m20260831_000002_add_managed_status_monitors",
            ),
            (
                "m20260831_000002_add_managed_status_monitors",
                "m20260903_000001_add_service_project_identity",
            ),
            (
                "m20260903_000001_add_service_project_identity",
                "m20260904_000001_reset_ambiguous_managed_status_monitors",
            ),
            (
                "m20260903_000001_add_service_project_identity",
                "m20261013_000003_allow_source_built_service_release",
            ),
            (
                "m20261013_000003_allow_source_built_service_release",
                "m20261014_000001_add_instantiation_claim_token",
            ),
            (
                "m20260929_000001_create_template_instantiations",
                "m20261014_000001_add_instantiation_claim_token",
            ),
            (
                "m20260904_000001_reset_ambiguous_managed_status_monitors",
                "m20260831_000001_ai_first_applications",
            ),
            (
                "m20260903_000004_repair_application_primary_projects",
                "m20260908_000001_reconcile_legacy_status_monitors",
            ),
            (
                "m20260928_000001_host_git_imports",
                "m20260928_000001_add_node_id_to_sandboxes",
            ),
            (
                "m20261002_000001_add_bunny_hostname_owned",
                "m20261002_000001_secret_checks_and_history",
            ),
        ] {
            let shipped_position = names
                .iter()
                .position(|name| name == shipped)
                .unwrap_or_else(|| panic!("shipped migration '{shipped}' is missing"));
            let added_position = names
                .iter()
                .position(|name| name == added)
                .unwrap_or_else(|| panic!("added migration '{added}' is missing"));
            assert!(
                shipped_position < added_position,
                "shipped migration '{shipped}' must precede '{added}'"
            );
        }
    }

    fn registered_names() -> Vec<String> {
        Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_string())
            .collect()
    }

    /// The shipped history fixture, one migration name per line.
    fn shipped_history() -> Vec<&'static str> {
        include_str!("../../tests/fixtures/migration_order.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect()
    }

    /// `Migrator::migrations()` order is upgrade history: an existing database
    /// recorded these names in this order, and a fresh one must apply them in
    /// the same order. The registered list must start with the fixture exactly;
    /// only migrations newer than all of it may follow.
    #[test]
    fn registered_order_starts_with_shipped_history_fixture() {
        let names = registered_names();
        let shipped = shipped_history();
        assert!(
            names.len() >= shipped.len(),
            "{} migrations are registered but the shipped history fixture lists {}: \
             a shipped migration was removed",
            names.len(),
            shipped.len()
        );
        for (position, (registered, expected)) in names.iter().zip(&shipped).enumerate() {
            assert_eq!(
                registered,
                expected,
                "migration order diverges from shipped history at position {position}: \
                 the fixture has '{expected}' but '{registered}' is registered there. A new \
                 migration must sort after '{}'; give it a later timestamp",
                shipped.last().copied().unwrap_or_default()
            );
        }
    }

    /// Every `mYYYYMMDD_NNNNNN_*.rs` file in the migration directory is a
    /// registered migration, and every registered migration has a file.
    #[test]
    fn every_migration_file_is_registered() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migration");
        let mut files = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("reading {}: {error}", directory.display()))
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| {
                !name.starts_with('.') && name != "mod.rs" && name != "order_exceptions.txt"
            })
            .map(|name| {
                name.strip_suffix(".rs")
                    .unwrap_or_else(|| panic!("{name} in {} is not a .rs file", directory.display()))
                    .to_string()
            })
            .collect::<Vec<_>>();
        files.sort();
        let mut names = registered_names();
        names.sort();
        assert_eq!(
            files.len(),
            names.len(),
            "{} migration files but {} registered migrations",
            files.len(),
            names.len()
        );
        assert_eq!(files, names, "migration files and registered names differ");
    }

    /// The analytics ingest-key migration also drops `NOT NULL` on
    /// `performance_metrics` / `session_replay_sessions` / `events` scope
    /// columns, which the entity models already assume. An unregistered
    /// migration means those entities decode `Option<i32>` out of a `NOT
    /// NULL` column and every no-deployment ingest keeps failing — so
    /// registration is asserted, not left to review.
    ///
    /// Checks presence only, not position. An earlier version of this test
    /// asserted the migration was registered *last*, which would fail the
    /// moment any unrelated PR registered a newer migration — a maintenance
    /// burden unconnected to whatever that PR actually changed.
    #[test]
    fn analytics_ingest_keys_migration_is_registered() {
        let names = Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>();

        assert!(
            names
                .iter()
                .any(|name| name == "m20260831_000001_create_analytics_ingest_keys"),
            "the analytics ingest-key migration must be registered in Migrator::migrations()"
        );
    }
}
