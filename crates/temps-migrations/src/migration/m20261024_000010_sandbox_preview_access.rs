// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox preview access without a `legacy` mode (ADR-055 §11).
//!
//! `m20261022_000008_ingress_endpoints` added `sandboxes.preview_access` as
//! `legacy | temps_login | public`, where `legacy` meant "password wall when
//! a password is set, otherwise open to anyone who has the URL". That second
//! half is exactly what a sandbox must never do by default, so `legacy` is
//! gone and the column becomes `temps_login | password | public`:
//!
//! - every row with a preview password becomes `password`, whatever its old
//!   mode -- the password was the protection the owner chose, and keeping a
//!   `public` row public while it holds a hash would silently drop it;
//! - every other `legacy` row becomes `temps_login`: an anonymous visitor
//!   is no longer admitted, a signed-in Temps user who may read the sandbox
//!   is;
//! - `temps_login` and `public` rows without a password stay as they are.
//!
//! A second CHECK makes `password` without a hash unrepresentable, so a
//! password-mode sandbox can never fall open.
//!
//! It also adds `sandboxes.preview_allowed_cidrs`: the client networks
//! allowed to open the preview URLs, checked before the access mode. Empty
//! means any address. Capped at 50 entries, the same bound the API enforces.
//!
//! `down` maps `password` back to `legacy` (the old mode that honoured the
//! password) and restores the old CHECK. A `temps_login` row that came from
//! `legacy` stays `temps_login`: the more restrictive reading survives a
//! rollback.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP_SQL: &str = r#"
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_preview_access_valid;
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_preview_password_has_hash;
UPDATE sandboxes SET preview_access = 'password'
    WHERE preview_password_hash IS NOT NULL;
UPDATE sandboxes SET preview_access = 'temps_login'
    WHERE preview_access = 'legacy' OR preview_access IS NULL;
ALTER TABLE sandboxes ALTER COLUMN preview_access SET DEFAULT 'temps_login';
ALTER TABLE sandboxes
    ADD CONSTRAINT sandboxes_preview_access_valid
        CHECK (preview_access IN ('temps_login', 'password', 'public'));
ALTER TABLE sandboxes
    ADD CONSTRAINT sandboxes_preview_password_has_hash
        CHECK (preview_access <> 'password' OR preview_password_hash IS NOT NULL);
ALTER TABLE sandboxes
    ADD COLUMN IF NOT EXISTS preview_allowed_cidrs TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_preview_allowed_cidrs_bounded;
ALTER TABLE sandboxes
    ADD CONSTRAINT sandboxes_preview_allowed_cidrs_bounded
        CHECK (cardinality(preview_allowed_cidrs) <= 50);
"#;

pub const DOWN_SQL: &str = r#"
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_preview_allowed_cidrs_bounded;
ALTER TABLE sandboxes DROP COLUMN IF EXISTS preview_allowed_cidrs;
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_preview_password_has_hash;
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_preview_access_valid;
UPDATE sandboxes SET preview_access = 'legacy' WHERE preview_access = 'password';
ALTER TABLE sandboxes
    ADD CONSTRAINT sandboxes_preview_access_valid
        CHECK (preview_access IN ('legacy', 'temps_login', 'public'));
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[test]
    fn up_removes_legacy_and_forbids_password_without_a_hash() {
        assert!(UP_SQL.contains("CHECK (preview_access IN ('temps_login', 'password', 'public'))"));
        assert!(!UP_SQL.contains("'legacy', 'temps_login'"));
        assert!(UP_SQL
            .contains("CHECK (preview_access <> 'password' OR preview_password_hash IS NOT NULL)"));
        assert!(UP_SQL.contains("SET DEFAULT 'temps_login'"));
        assert!(UP_SQL.contains("preview_allowed_cidrs TEXT[] NOT NULL DEFAULT '{}'"));
        assert!(UP_SQL.contains("cardinality(preview_allowed_cidrs) <= 50"));
        // The password backfill runs before the legacy one, so a legacy row
        // with a hash lands on `password`, not `temps_login`.
        let password = UP_SQL
            .find("SET preview_access = 'password'")
            .unwrap_or(usize::MAX);
        let login = UP_SQL
            .find("SET preview_access = 'temps_login'")
            .unwrap_or(0);
        assert!(password < login);
    }

    #[test]
    fn down_restores_the_legacy_check() {
        assert!(DOWN_SQL.contains("CHECK (preview_access IN ('legacy', 'temps_login', 'public'))"));
        assert!(DOWN_SQL.contains("DROP COLUMN IF EXISTS preview_allowed_cidrs"));
        assert!(
            DOWN_SQL.contains("SET preview_access = 'legacy' WHERE preview_access = 'password'")
        );
    }

    #[test]
    fn is_registered_after_the_migration_that_added_the_column() {
        let names: Vec<String> = crate::migration::Migrator::migrations()
            .into_iter()
            .map(|m| m.name().to_string())
            .collect();
        let added = names
            .iter()
            .position(|n| n == "m20261022_000008_ingress_endpoints");
        let reworked = names
            .iter()
            .position(|n| n == "m20261024_000010_sandbox_preview_access");
        match (added, reworked) {
            (Some(added), Some(reworked)) => assert!(added < reworked),
            other => panic!("both migrations must be registered, got {other:?}"),
        }
    }

    async fn access_of(
        db: &sea_orm::DatabaseConnection,
        id: i32,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!("SELECT preview_access FROM sandboxes WHERE id = {id}"),
            ))
            .await?
            .ok_or_else(|| format!("sandbox {id} missing after migration"))?;
        Ok(row.try_get::<String>("", "preview_access")?)
    }

    #[tokio::test]
    async fn backfill_maps_every_legacy_row_and_the_checks_hold(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping sandbox preview access migration test");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping sandbox preview access migration test");
            return Ok(());
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let requested_port = listener.local_addr()?.port();
        drop(listener);
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "preview-access-test")
            .with_mapped_port(
                requested_port,
                testcontainers::core::ContainerPort::Tcp(5432),
            )
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:preview-access-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // The table as m20261022_000008 left it, with one row per case.
        db.execute_unprepared(
            "CREATE TABLE sandboxes(
                id INTEGER PRIMARY KEY,
                preview_password_hash TEXT,
                preview_access TEXT NOT NULL DEFAULT 'temps_login',
                CONSTRAINT sandboxes_preview_access_valid
                    CHECK (preview_access IN ('legacy', 'temps_login', 'public')));
             INSERT INTO sandboxes VALUES
                (1, NULL, 'legacy'),
                (2, '$argon2id$hash', 'legacy'),
                (3, NULL, 'temps_login'),
                (4, NULL, 'public'),
                (5, '$argon2id$hash', 'public'),
                (6, '$argon2id$hash', 'temps_login');",
        )
        .await?;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        for (id, expected) in [
            (1, "temps_login"),
            (2, "password"),
            (3, "temps_login"),
            (4, "public"),
            (5, "password"),
            (6, "password"),
        ] {
            assert_eq!(access_of(&db, id).await?, expected, "sandbox {id}");
        }

        // New rows default to temps_login with an empty allow-list.
        db.execute_unprepared("INSERT INTO sandboxes(id) VALUES (7)")
            .await?;
        assert_eq!(access_of(&db, 7).await?, "temps_login");
        let cidrs = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT cardinality(preview_allowed_cidrs) AS n FROM sandboxes WHERE id = 7"
                    .to_string(),
            ))
            .await?
            .ok_or("sandbox 7 missing")?;
        assert_eq!(cidrs.try_get::<i32>("", "n")?, 0);

        // `legacy` is gone, and `password` without a hash is refused.
        assert!(db
            .execute_unprepared("UPDATE sandboxes SET preview_access = 'legacy' WHERE id = 3")
            .await
            .is_err());
        assert!(db
            .execute_unprepared("UPDATE sandboxes SET preview_access = 'password' WHERE id = 3")
            .await
            .is_err());
        assert!(db
            .execute_unprepared("UPDATE sandboxes SET preview_password_hash = NULL WHERE id = 2")
            .await
            .is_err());
        // More than 50 networks is refused.
        assert!(db
            .execute_unprepared(
                "UPDATE sandboxes SET preview_allowed_cidrs =
                    array_fill('192.0.2.1/32'::text, ARRAY[51]) WHERE id = 3"
            )
            .await
            .is_err());

        Migration.down(&manager).await?;
        assert_eq!(access_of(&db, 2).await?, "legacy");
        assert_eq!(access_of(&db, 1).await?, "temps_login");
        Migration.up(&manager).await?;
        assert_eq!(access_of(&db, 2).await?, "password");
        db.close().await?;
        Ok(())
    }
}
