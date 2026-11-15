// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::TransactionTrait;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let tx = manager.get_connection().begin().await?;
        // Wait for in-flight group writes before taking the count snapshot.
        // Subsequent transactional ingests increment from the reconciled value.
        tx.execute_unprepared("LOCK TABLE error_groups IN SHARE ROW EXCLUSIVE MODE")
            .await?;
        tx.execute_unprepared("UPDATE error_groups g SET total_count = counts.n FROM (SELECT g.id, count(e.id)::integer AS n FROM error_groups g LEFT JOIN error_events e ON e.error_group_id=g.id GROUP BY g.id) counts WHERE g.id=counts.id AND g.total_count IS DISTINCT FROM counts.n").await?;
        tx.execute_unprepared("CREATE TABLE IF NOT EXISTS error_group_fingerprints (project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, fingerprint_hash VARCHAR(64) NOT NULL, error_group_id INTEGER NOT NULL REFERENCES error_groups(id) ON DELETE CASCADE, PRIMARY KEY(project_id,fingerprint_hash)); CREATE INDEX IF NOT EXISTS error_group_fingerprints_group ON error_group_fingerprints(error_group_id)").await?;
        // error_events is a compressed TimescaleDB hypertable, and its FKs to
        // projects/error_groups are not a guarantee there: real installs hold
        // events whose project or group no longer exists. Index only
        // fingerprints whose group still exists in the event's own project;
        // orphaned events are left untouched and never become a lookup target.
        tx.execute_unprepared(
            "INSERT INTO error_group_fingerprints(project_id,fingerprint_hash,error_group_id) \
             SELECT e.project_id,e.fingerprint_hash,min(e.error_group_id) FROM error_events e \
             JOIN projects p ON p.id=e.project_id \
             JOIN error_groups g ON g.id=e.error_group_id AND g.project_id=e.project_id \
             GROUP BY e.project_id,e.fingerprint_hash ON CONFLICT DO NOTHING",
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE error_group_fingerprints")
            .await?;
        // Accurate counts remain accurate on rollback; no records are merged.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn fingerprint_backfill_skips_events_with_missing_project_or_group(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping error-group reconcile migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping error-group reconcile migration integration");
            return Ok(());
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let requested_port = listener.local_addr()?.port();
        drop(listener);
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "error-group-reconcile-test")
            .with_mapped_port(
                requested_port,
                testcontainers::core::ContainerPort::Tcp(5432),
            )
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:error-group-reconcile-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Production FKs are kept; the orphans are written with FK triggers
        // disabled, which is how they look on an affected install.
        db.execute_unprepared(
            "CREATE TABLE projects(id INTEGER PRIMARY KEY);
             CREATE TABLE error_groups(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, total_count INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE error_events(id BIGINT PRIMARY KEY, error_group_id INTEGER NOT NULL REFERENCES error_groups(id) ON DELETE CASCADE, project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, fingerprint_hash TEXT NOT NULL);
             INSERT INTO projects VALUES (1), (2);
             INSERT INTO error_groups(id, project_id) VALUES (10, 1), (11, 1), (20, 2);
             INSERT INTO error_events VALUES (1, 11, 1, 'live'), (2, 10, 1, 'live'), (3, 20, 2, 'other');
             SET session_replication_role = replica;
             INSERT INTO error_events VALUES
                 (4, 10, 99, 'deleted-project'),
                 (5, 98, 99, 'deleted-project-and-group'),
                 (6, 12, 1, 'deleted-group'),
                 (7, 10, 2, 'group-from-other-project');
             SET session_replication_role = DEFAULT;",
        )
        .await?;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT project_id, fingerprint_hash, error_group_id FROM error_group_fingerprints ORDER BY project_id, fingerprint_hash".to_string(),
            ))
            .await?
            .into_iter()
            .map(|row| {
                Ok((
                    row.try_get::<i32>("", "project_id")?,
                    row.try_get::<String>("", "fingerprint_hash")?,
                    row.try_get::<i32>("", "error_group_id")?,
                ))
            })
            .collect::<Result<Vec<_>, DbErr>>()?;
        assert_eq!(
            rows,
            vec![(1, "live".to_string(), 10), (2, "other".to_string(), 20)]
        );
        let orphans = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::bigint AS count FROM error_events WHERE id >= 4".to_string(),
            ))
            .await?
            .ok_or("orphan count query returned no row")?;
        assert_eq!(
            orphans.try_get::<i64>("", "count")?,
            4,
            "the migration must not delete orphaned events"
        );
        let counts = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT id, total_count FROM error_groups ORDER BY id".to_string(),
            ))
            .await?
            .into_iter()
            .map(|row| {
                Ok((
                    row.try_get::<i32>("", "id")?,
                    row.try_get::<i32>("", "total_count")?,
                ))
            })
            .collect::<Result<Vec<_>, DbErr>>()?;
        assert_eq!(counts, vec![(10, 3), (11, 1), (20, 1)]);

        // Re-running over an already-migrated database is a no-op.
        Migration.up(&manager).await?;
        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        db.close().await?;
        Ok(())
    }
}
