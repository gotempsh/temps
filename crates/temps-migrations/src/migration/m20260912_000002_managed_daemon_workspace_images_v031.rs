// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = "ALTER TABLE ai_application_workspaces
    DROP CONSTRAINT ai_application_workspaces_image_check,
    ADD CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL OR image IN (
        'ghcr.io/gotempsh/temps-sandbox-node:0.1.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.1'))";

const DOWN_SQL: &str = "ALTER TABLE ai_application_workspaces
    DROP CONSTRAINT ai_application_workspaces_image_check,
    ADD CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL OR image IN (
        'ghcr.io/gotempsh/temps-sandbox-node:0.1.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.0'))";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Adding the previous constraint checks existing rows atomically.
        // An operator must move all :0.3.1 workspaces to an older approved
        // image before downgrading.
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
    use sea_orm::{ConnectionTrait, Database};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[test]
    fn managed_v031_constraint_matches_the_resolver_and_preserves_history() {
        for runtime in ["node", "bun", "python", "rust", "go", "full"] {
            let image = temps_sandbox_image_for_test(runtime);
            assert!(
                UP_SQL.contains(image),
                "missing current image for {runtime}: {image}"
            );
        }
        for image in [
            "ghcr.io/gotempsh/temps-sandbox-node:0.1.0",
            "ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0",
            "ghcr.io/gotempsh/temps-sandbox-python:0.2.0",
            "ghcr.io/gotempsh/temps-sandbox-all:0.2.0",
            "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.0",
            "ghcr.io/gotempsh/temps-sandbox-python:0.3.0",
            "ghcr.io/gotempsh/temps-sandbox-all:0.3.0",
        ] {
            assert!(UP_SQL.contains(image));
            assert!(DOWN_SQL.contains(image));
        }
        assert!(!DOWN_SQL.contains("temps-sandbox-nodejs:0.3.1"));
        assert!(!UP_SQL.contains("temps-sandbox-nodejs:0.1.0"));
    }

    fn temps_sandbox_image_for_test(runtime: &str) -> &'static str {
        match runtime {
            "node" | "bun" => "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.1",
            "python" => "ghcr.io/gotempsh/temps-sandbox-python:0.3.1",
            _ => "ghcr.io/gotempsh/temps-sandbox-all:0.3.1",
        }
    }

    #[tokio::test]
    async fn postgres_v031_constraint_applies_and_reverses(
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
        db.execute_unprepared("CREATE TABLE ai_application_workspaces (image TEXT, CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL OR image IN ('ghcr.io/gotempsh/temps-sandbox-node:0.1.0', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0', 'ghcr.io/gotempsh/temps-sandbox-python:0.2.0', 'ghcr.io/gotempsh/temps-sandbox-all:0.2.0', 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.0', 'ghcr.io/gotempsh/temps-sandbox-python:0.3.0', 'ghcr.io/gotempsh/temps-sandbox-all:0.3.0')))" ).await?;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;
        db.execute_unprepared("INSERT INTO ai_application_workspaces (image) VALUES ('ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.1'), ('ghcr.io/gotempsh/temps-sandbox-python:0.3.0')").await?;
        assert!(db.execute_unprepared("INSERT INTO ai_application_workspaces (image) VALUES ('evil.example/runtime:0.3.1')").await.is_err());
        assert!(
            Migration.down(&manager).await.is_err(),
            "downgrade must reject selected :0.3.1 images"
        );
        db.execute_unprepared("DELETE FROM ai_application_workspaces WHERE image = 'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.1'").await?;
        Migration.down(&manager).await?;
        assert!(db.execute_unprepared("INSERT INTO ai_application_workspaces (image) VALUES ('ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.1')").await.is_err());
        Ok(())
    }
}
