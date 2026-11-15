// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The client's JA4 TLS fingerprint on the request log (ADR 066, D4 and I8):
//! `proxy_logs.ja4` and `ja4_source`. Nullable, so existing rows and
//! requests without one store nothing.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS ja4 TEXT;
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS ja4_source TEXT;
"#;

const DOWN: &str = r#"
ALTER TABLE proxy_logs DROP COLUMN IF EXISTS ja4_source;
ALTER TABLE proxy_logs DROP COLUMN IF EXISTS ja4;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    /// Existing rows gain no JA4, a new one stores it, and `down` then `up`
    /// again is clean.
    #[tokio::test]
    async fn ja4_columns_are_optional() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping ja4 migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping ja4 migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "ja4-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:ja4-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        db.execute_unprepared(
            "CREATE TABLE proxy_logs(id SERIAL PRIMARY KEY, path TEXT); \
             INSERT INTO proxy_logs(path) VALUES ('/');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;
        db.execute_unprepared(
            "INSERT INTO proxy_logs(path, ja4, ja4_source) \
             VALUES ('/x', 't13d1516h2_8daaf6152771_e5627efa2ab1', 'direct')",
        )
        .await?;
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) FILTER (WHERE ja4 IS NULL AND ja4_source IS NULL) AS empty, \
                 count(*) FILTER (WHERE ja4_source = 'direct') AS direct FROM proxy_logs"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("proxy_logs".into()))?;
        assert_eq!(row.try_get::<i64>("", "empty")?, 1);
        assert_eq!(row.try_get::<i64>("", "direct")?, 1);

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
