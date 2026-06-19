use sea_orm_migration::prelude::*;

/// Adds opt-in metrics-collection columns to `external_services` and
/// `deployments`.
///
/// - `external_services.metrics_enabled` — whether to scrape DB-level metrics
///   (pg_stat_*, Redis INFO, etc.) for this service.
/// - `deployments.metrics_enabled` — whether to scrape OTLP / Prometheus
///   metrics exposed by this deployment.
/// - `deployments.metrics_port` — port the app exposes metrics on
///   (NULL = use the deployment's primary port).
/// - `deployments.metrics_path` — HTTP path to scrape
///   (default `/metrics`, per Prometheus convention).
///
/// **Safely re-runnable:** uses `IF NOT EXISTS` column guard inside a
/// PL/pgSQL block.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    -- external_services: opt-in metrics scraping
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'external_services'
          AND column_name = 'metrics_enabled'
    ) THEN
        ALTER TABLE external_services
            ADD COLUMN metrics_enabled BOOL NOT NULL DEFAULT false;
    END IF;

    -- deployments: opt-in OTLP / Prometheus scraping
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'deployments'
          AND column_name = 'metrics_enabled'
    ) THEN
        ALTER TABLE deployments
            ADD COLUMN metrics_enabled BOOL NOT NULL DEFAULT false;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'deployments'
          AND column_name = 'metrics_port'
    ) THEN
        ALTER TABLE deployments
            ADD COLUMN metrics_port INT;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'deployments'
          AND column_name = 'metrics_path'
    ) THEN
        ALTER TABLE deployments
            ADD COLUMN metrics_path TEXT NOT NULL DEFAULT '/metrics';
    END IF;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'deployments' AND column_name = 'metrics_path'
    ) THEN
        ALTER TABLE deployments DROP COLUMN metrics_path;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'deployments' AND column_name = 'metrics_port'
    ) THEN
        ALTER TABLE deployments DROP COLUMN metrics_port;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'deployments' AND column_name = 'metrics_enabled'
    ) THEN
        ALTER TABLE deployments DROP COLUMN metrics_enabled;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'external_services' AND column_name = 'metrics_enabled'
    ) THEN
        ALTER TABLE external_services DROP COLUMN metrics_enabled;
    END IF;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }
}
