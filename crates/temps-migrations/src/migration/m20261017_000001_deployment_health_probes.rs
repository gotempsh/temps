// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = "
-- Preserve the implicit port of existing web environments. New environments
-- still default to process health when their image declares no port. Operators
-- can explicitly clear this override when converting an existing app to a worker.
UPDATE environments e
SET deployment_config = jsonb_set(COALESCE(e.deployment_config, '{}'::jsonb), '{exposedPort}', to_jsonb(legacy.port))
FROM (
    SELECT e.id, MIN(c.container_port) AS port
    FROM environments e
    JOIN projects p ON p.id = e.project_id
    JOIN deployments d ON d.id = e.current_deployment_id
    JOIN deployment_containers c ON c.deployment_id = d.id AND c.deleted_at IS NULL
    WHERE d.static_dir_location IS NULL AND p.preset != 'docker-compose'
      AND c.container_port > 0
      AND e.deployment_config->>'exposedPort' IS NULL
      AND p.deployment_config->>'exposedPort' IS NULL
      AND COALESCE(e.deployment_config->'health'->>'type', p.deployment_config->'health'->>'type', 'http') = 'http'
    GROUP BY e.id
) legacy
WHERE e.id = legacy.id;
ALTER TABLE deployment_containers ALTER COLUMN container_port DROP NOT NULL;
ALTER TABLE deployment_containers ADD COLUMN health_state TEXT NOT NULL DEFAULT 'unknown'
    CHECK (health_state IN ('healthy', 'unhealthy', 'unknown'));
ALTER TABLE deployment_containers ADD COLUMN health_checked_at TIMESTAMPTZ;
ALTER TABLE deployment_containers ADD COLUMN health_detail VARCHAR(4096);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Refuse a lossy rollback while workers exist; redeploy them with a port
        // before restoring the old nonnullable schema.
        manager
            .get_connection()
            .execute_unprepared(
                "
ALTER TABLE deployment_containers ALTER COLUMN container_port SET NOT NULL;
ALTER TABLE deployment_containers DROP COLUMN health_detail;
ALTER TABLE deployment_containers DROP COLUMN health_checked_at;
ALTER TABLE deployment_containers DROP COLUMN health_state;
",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn health_probe_migration_preserves_web_rows_and_supports_portless_health(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(error) => {
                eprintln!("Skipping migration: Docker unavailable: {error}");
                return Ok(());
            }
        };
        if docker.ping().await.is_err() {
            eprintln!("Skipping migration: Docker unavailable");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "migration-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:migration-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        db.execute_unprepared(r#"
CREATE TABLE projects (id INTEGER PRIMARY KEY, preset TEXT, deployment_config JSONB);
CREATE TABLE environments (id INTEGER PRIMARY KEY, project_id INTEGER, current_deployment_id INTEGER, deployment_config JSONB);
CREATE TABLE deployments (id INTEGER PRIMARY KEY, static_dir_location TEXT);
CREATE TABLE deployment_containers (id INTEGER PRIMARY KEY, deployment_id INTEGER, container_port INTEGER NOT NULL, deleted_at TIMESTAMPTZ);
INSERT INTO projects VALUES (1, 'dockerfile', NULL), (2, 'docker-compose', NULL), (3, 'dockerfile', '{"exposedPort":9000}');
INSERT INTO deployments VALUES (1, NULL), (2, NULL), (3, NULL), (4, NULL), (5, NULL);
INSERT INTO environments VALUES (1,1,1,NULL), (2,2,2,NULL), (3,3,3,NULL), (4,1,4,'{"exposedPort":7000}'), (5,1,5,'{"health":{"type":"process"}}');
INSERT INTO deployment_containers (id, deployment_id, container_port) VALUES (1,1,8080),(3,2,8081),(4,3,8082),(5,4,8083),(6,5,8084);
"#).await?;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;
        let ports = db.query_all(Statement::from_string(DatabaseBackend::Postgres,
            "SELECT id, deployment_config->>'exposedPort' AS port FROM environments ORDER BY id".to_string())).await?;
        assert_eq!(
            ports[0].try_get::<Option<String>>("", "port")?,
            Some("8080".into())
        );
        for index in [1, 2, 4] {
            assert_eq!(ports[index].try_get::<Option<String>>("", "port")?, None);
        }
        assert_eq!(
            ports[3].try_get::<Option<String>>("", "port")?,
            Some("7000".into())
        );
        let row = db.query_one(Statement::from_string(DatabaseBackend::Postgres, "SELECT container_port, health_state, health_checked_at, health_detail FROM deployment_containers WHERE id = 1".to_string())).await?.unwrap();
        assert_eq!(row.try_get::<i32>("", "container_port")?, 8080);
        assert_eq!(row.try_get::<String>("", "health_state")?, "unknown");
        assert_eq!(row.try_get::<Option<String>>("", "health_detail")?, None);
        db.execute_unprepared("INSERT INTO deployment_containers (id, container_port, health_state, health_checked_at, health_detail) VALUES (2, NULL, 'healthy', now(), 'process stable')").await?;
        assert!(db
            .execute_unprepared(
                "UPDATE deployment_containers SET health_state = 'skipped' WHERE id = 2"
            )
            .await
            .is_err());
        assert!(db
            .execute_unprepared(&format!(
                "UPDATE deployment_containers SET health_detail = '{}' WHERE id = 2",
                "x".repeat(4097)
            ))
            .await
            .is_err());
        assert!(
            Migration.down(&manager).await.is_err(),
            "lossy downgrade must refuse existing workers"
        );
        db.execute_unprepared("DELETE FROM deployment_containers WHERE id = 2")
            .await?;
        Migration.down(&manager).await?;
        assert!(db
            .execute_unprepared(
                "INSERT INTO deployment_containers (id, container_port) VALUES (7, NULL)"
            )
            .await
            .is_err());
        Migration.up(&manager).await?;
        db.close().await?;
        Ok(())
    }

    #[test]
    fn health_probe_migration_is_registered() {
        assert!(crate::Migrator::migrations()
            .iter()
            .any(|migration| migration.name() == "m20261017_000001_deployment_health_probes"));
    }
}
