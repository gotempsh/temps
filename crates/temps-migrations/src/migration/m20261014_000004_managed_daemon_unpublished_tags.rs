// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Point stored managed-daemon image references at a tag that exists.
//!
//! Release `0.1.0-beta.57` and earlier defaulted AI application workspaces to
//! `ghcr.io/gotempsh/temps-sandbox-{nodejs,python,all}:0.3.4`. That bare tag
//! was never published: main-branch builds only push `0.3.4-beta`, and the
//! stable tag exists once a release publishes it. A workspace row that still
//! names the bare tag therefore fails its image pull with a 404 on every
//! start, and the CHECK constraint widened by
//! `m20261012_000003_managed_daemon_beta_images` only made the `-beta` form
//! legal; it did not move any existing row onto it.
//!
//! This migration rewrites those references on both rows that record the
//! image of a workspace: the workspace itself and the sandbox it points at.
//! Rewriting only the workspace would make the two disagree, and the
//! mismatch is exactly what the workspace reconciler treats as drift and
//! answers by rebuilding the compute.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The bare tag that was never published, on the three managed repositories.
const UNPUBLISHED_PATTERN: &str =
    "^ghcr[.]io/gotempsh/temps-sandbox-(nodejs|python|all):0[.]3[.]4$";

fn up_sql() -> String {
    format!(
        "UPDATE ai_application_workspaces
SET image = image || '-beta', updated_at = NOW()
WHERE image ~ '{UNPUBLISHED_PATTERN}';

UPDATE sandboxes
SET image = image || '-beta'
WHERE image ~ '{UNPUBLISHED_PATTERN}'
  AND public_id IN (
      SELECT sandbox_public_id
      FROM ai_application_workspaces
      WHERE sandbox_public_id IS NOT NULL
  );"
    )
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&up_sql())
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The rewritten rows now name an image that exists; moving them back
        // to one that does not would only break them again.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[test]
    fn both_statements_use_the_same_pattern() {
        assert_eq!(up_sql().matches(UNPUBLISHED_PATTERN).count(), 2);
    }

    #[test]
    fn rewrite_only_names_the_beta_tag_that_main_builds_publish() {
        // The result of the rewrite must satisfy the CHECK constraint that
        // `m20261012_000003` installs, which only accepts `-beta` on the
        // three managed repositories.
        let constraint = super::super::m20261012_000003_managed_daemon_beta_images::up_sql()
            .expect("valid constraint");
        assert!(constraint.contains("-beta$"));
        assert!(up_sql().contains("|| '-beta'"));
    }

    async fn value_of(db: &DatabaseConnection, sql: &str) -> Result<Option<String>, DbErr> {
        let row = db
            .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
            .await?;
        row.map(|row| row.try_get::<Option<String>>("", "image"))
            .transpose()
            .map(Option::flatten)
    }

    #[tokio::test]
    async fn postgres_rewrites_unpublished_tags_and_leaves_everything_else(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok_and(|value| !value.trim().is_empty()) {
            eprintln!("Skipping isolated migration test: external database in use");
            return Ok(());
        }
        let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .with_cmd(vec![
                "postgres",
                "-c",
                "timescaledb.max_background_workers=0",
            ])
            .with_startup_timeout(std::time::Duration::from_secs(120))
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                eprintln!("Skipping isolated migration test: Docker unavailable: {error}");
                return Ok(());
            }
        };
        let port = container.get_host_port_ipv4(5432).await?;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let db = Database::connect(format!(
            "postgresql://postgres:postgres@localhost:{port}/postgres"
        ))
        .await?;

        // The constraint the previous migrations leave behind, so the test
        // proves the rewritten value is one the table still accepts.
        let constraint = super::super::m20261012_000003_managed_daemon_beta_images::up_sql()?;
        db.execute_unprepared(
            "CREATE TABLE ai_application_workspaces (
                id BIGSERIAL PRIMARY KEY,
                sandbox_public_id TEXT,
                image TEXT,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW() - INTERVAL '1 day',
                CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL))",
        )
        .await?;
        db.execute_unprepared(&constraint).await?;
        db.execute_unprepared(
            "CREATE TABLE sandboxes (
                id SERIAL PRIMARY KEY,
                public_id TEXT NOT NULL,
                image TEXT)",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO ai_application_workspaces (id, sandbox_public_id, image) VALUES
                (1, 'sbx_node', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4'),
                (2, 'sbx_python', 'ghcr.io/gotempsh/temps-sandbox-python:0.3.4'),
                (3, 'sbx_all', 'ghcr.io/gotempsh/temps-sandbox-all:0.3.4'),
                (4, NULL, 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4'),
                (5, 'sbx_old', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2'),
                (6, 'sbx_beta', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta'),
                (7, 'sbx_none', NULL)",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO sandboxes (public_id, image) VALUES
                ('sbx_node', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4'),
                ('sbx_python', 'ghcr.io/gotempsh/temps-sandbox-python:0.3.4'),
                ('sbx_all', 'ghcr.io/gotempsh/temps-sandbox-all:0.3.4'),
                ('sbx_old', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2'),
                ('sbx_unlinked', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4')",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        for (id, expected) in [
            (1, Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")),
            (2, Some("ghcr.io/gotempsh/temps-sandbox-python:0.3.4-beta")),
            (3, Some("ghcr.io/gotempsh/temps-sandbox-all:0.3.4-beta")),
            (4, Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")),
            (5, Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2")),
            (6, Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")),
            (7, None),
        ] {
            assert_eq!(
                value_of(
                    &db,
                    &format!("SELECT image FROM ai_application_workspaces WHERE id = {id}")
                )
                .await?
                .as_deref(),
                expected,
                "workspace {id}"
            );
        }
        for (public_id, expected) in [
            (
                "sbx_node",
                "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta",
            ),
            (
                "sbx_python",
                "ghcr.io/gotempsh/temps-sandbox-python:0.3.4-beta",
            ),
            ("sbx_all", "ghcr.io/gotempsh/temps-sandbox-all:0.3.4-beta"),
            ("sbx_old", "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2"),
            // Not a workspace's sandbox: not this migration's to change.
            (
                "sbx_unlinked",
                "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4",
            ),
        ] {
            assert_eq!(
                value_of(
                    &db,
                    &format!("SELECT image FROM sandboxes WHERE public_id = '{public_id}'")
                )
                .await?
                .as_deref(),
                Some(expected),
                "sandbox {public_id}"
            );
        }
        let touched = db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT (updated_at > NOW() - INTERVAL '1 hour') AS fresh \
                 FROM ai_application_workspaces WHERE id = 1"
                    .to_string(),
            ))
            .await?
            .expect("row");
        assert!(touched.try_get::<bool>("", "fresh")?);

        // Re-running is a no-op: the migration is idempotent.
        Migration.up(&manager).await?;
        assert_eq!(
            value_of(
                &db,
                "SELECT image FROM ai_application_workspaces WHERE id = 1"
            )
            .await?
            .as_deref(),
            Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")
        );
        Ok(())
    }
}
