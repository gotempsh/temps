// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE stateless_control_plane (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    instance_id TEXT NOT NULL,
    management_url TEXT NOT NULL,
    storage_identity TEXT NOT NULL,
    secret_verifier TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE stateless_cloud_link_state (
    id SMALLINT PRIMARY KEY CHECK (id = 1),
    ciphertext TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE stateless_cloud_backfill_checkpoints (
    project_id INTEGER PRIMARY KEY,
    window_from TIMESTAMPTZ NOT NULL,
    window_to TIMESTAMPTZ NOT NULL,
    cursor JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE durable_jobs (
    id UUID PRIMARY KEY,
    job_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE durable_job_deliveries (
    job_id UUID NOT NULL REFERENCES durable_jobs(id) ON DELETE CASCADE,
    consumer TEXT NOT NULL,
    claimed_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    PRIMARY KEY (job_id, consumer)
);

CREATE INDEX idx_durable_job_deliveries_pending
    ON durable_job_deliveries (consumer, claimed_at, job_id)
    WHERE completed_at IS NULL;
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE IF EXISTS durable_job_deliveries;\
                 DROP TABLE IF EXISTS durable_jobs;\
                 DROP TABLE IF EXISTS stateless_cloud_backfill_checkpoints;\
                 DROP TABLE IF EXISTS stateless_cloud_link_state;\
                 DROP TABLE IF EXISTS stateless_control_plane;",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn stateless_schema_reverses_and_reapplies() {
        let container = match GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "migration-test")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                let message = error.to_string().to_lowercase();
                if message.contains("socket")
                    || message.contains("connection refused")
                    || message.contains("permission denied")
                {
                    eprintln!("Skipping stateless migration test: Docker unavailable: {error}");
                    return;
                }
                panic!("stateless migration database startup failed: {error}");
            }
        };
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Postgres port");
        let db = Database::connect(format!(
            "postgres://postgres:migration-test@127.0.0.1:{port}/postgres"
        ))
        .await
        .expect("connect");
        let manager = SchemaManager::new(&db);
        Migration
            .up(&manager)
            .await
            .expect("apply stateless migration");
        db.execute_unprepared("INSERT INTO stateless_control_plane (id, instance_id, management_url, storage_identity, secret_verifier) VALUES (1, 'test', 'https://example.test', 'bucket/prefix', 'encrypted');").await.expect("persist singleton");
        assert!(db.execute_unprepared("INSERT INTO stateless_control_plane (id, instance_id, management_url, storage_identity, secret_verifier) VALUES (2, 'other', 'https://example.test', 'bucket/prefix', 'encrypted');").await.is_err());
        Migration
            .down(&manager)
            .await
            .expect("reverse stateless migration");
        assert!(!manager
            .has_table("stateless_control_plane")
            .await
            .expect("table absence"));
        assert!(!manager
            .has_table("durable_jobs")
            .await
            .expect("queue absence"));
        Migration
            .up(&manager)
            .await
            .expect("reapply stateless migration");
        assert!(manager
            .has_table("stateless_cloud_link_state")
            .await
            .expect("Cloud state table"));
        assert!(manager
            .has_table("stateless_cloud_backfill_checkpoints")
            .await
            .expect("Cloud backfill checkpoint table"));
        assert!(manager
            .has_table("durable_job_deliveries")
            .await
            .expect("delivery table"));
    }
}
