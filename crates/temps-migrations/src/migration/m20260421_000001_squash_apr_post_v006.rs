// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Squashed migration for all changes after prod commit b8d6519 (Apr 1, 2026).
//!
//! Collapses 34 individual migrations (m20260403_000001 .. m20260420_000003)
//! into one. Written against the final schema state — intermediate alter-then-
//! modify chains are skipped (e.g. `sandbox_enabled` is created nullable from
//! the start rather than NOT NULL then nullable). Prod deployments still have
//! the original migrations registered and applied; this file replaces them
//! only on fresh local setups.
//!
//! See mod.rs for the exact list of migrations this supersedes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // ==========================================================
        // project_agents additions (sandbox + skills/mcp + ai + webhook)
        // ==========================================================
        // Originally added across m20260403, m20260404, m20260408,
        // m20260409_000001, m20260409_000002, m20260413_000002.
        // Final shape: sandbox_enabled nullable from the start.
        db.execute_unprepared(
            "ALTER TABLE project_agents \
             ADD COLUMN IF NOT EXISTS sandbox_enabled BOOLEAN DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS config_repo_url VARCHAR DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS config_repo_branch VARCHAR DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS mcp_servers_config JSONB DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS skills_config JSONB DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS tools_config JSONB DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS webhook_id VARCHAR DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS webhook_token VARCHAR DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS ai_model VARCHAR DEFAULT NULL",
        )
        .await?;

        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_project_agents_webhook_id \
             ON project_agents (webhook_id) WHERE webhook_id IS NOT NULL",
        )
        .await?;

        // ==========================================================
        // agent_secrets (was project_secrets, renamed & made global)
        // ==========================================================
        // Originally m20260408_000001 + m20260408_000002. Final shape:
        // global (no project_id) with unique name.
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS agent_secrets (
                id SERIAL PRIMARY KEY,
                name VARCHAR NOT NULL UNIQUE,
                secret_type VARCHAR NOT NULL DEFAULT 'env',
                encrypted_value TEXT NOT NULL,
                mount_path VARCHAR DEFAULT NULL,
                description VARCHAR DEFAULT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
        )
        .await?;

        // ==========================================================
        // workspace_sessions (final consolidated shape)
        // ==========================================================
        // Originally m20260406_000001 + m20260406_000004 + m20260406_000005
        // + m20260407_000001/2/3 + m20260412_000001 + m20260415_000002.
        // public_id is NOT NULL with a default so no backfill step is needed
        // on a fresh DB.
        //
        // DORMANT: the temps-workspace feature was removed; these tables are
        // kept so historical data is preserved for users who had sessions
        // before the rip. No active code reads or writes them.
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS workspace_sessions (
                id SERIAL PRIMARY KEY,
                public_id VARCHAR(32) NOT NULL DEFAULT ('wss_' || substr(md5(random()::text), 1, 16)),
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                status VARCHAR(20) NOT NULL DEFAULT 'active',
                title TEXT,
                sandbox_container_id VARCHAR(255),
                sandbox_volume_name VARCHAR(255),
                work_dir TEXT,
                branch_name VARCHAR(255),
                base_branch_name TEXT,
                ai_provider VARCHAR(50) NOT NULL DEFAULT 'claude_cli',
                ai_model VARCHAR(100),
                skills_config JSONB,
                mcp_servers_config JSONB,
                preview_password_hash TEXT,
                preview_password_hint VARCHAR(8),
                idle_timeout_minutes INTEGER,
                cpu_milli INTEGER,
                memory_limit_mb INTEGER,
                pids_limit INTEGER,
                tokens_input INTEGER NOT NULL DEFAULT 0,
                tokens_output INTEGER NOT NULL DEFAULT 0,
                estimated_cost_cents INTEGER NOT NULL DEFAULT 0,
                files_changed INTEGER NOT NULL DEFAULT 0,
                metadata JSONB,
                last_activity_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                closed_at TIMESTAMPTZ,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .await?;

        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workspace_sessions_project_id ON workspace_sessions(project_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workspace_sessions_user_id ON workspace_sessions(user_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workspace_sessions_status ON workspace_sessions(status)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_sessions_public_id \
             ON workspace_sessions(public_id)",
        )
        .await?;

        // workspace_messages
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS workspace_messages (
                id BIGSERIAL PRIMARY KEY,
                session_id INTEGER NOT NULL REFERENCES workspace_sessions(id) ON DELETE CASCADE,
                role VARCHAR(20) NOT NULL,
                content TEXT NOT NULL,
                metadata JSONB,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workspace_messages_session_id ON workspace_messages(session_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workspace_messages_session_created ON workspace_messages(session_id, created_at)",
        )
        .await?;

        // ==========================================================
        // workflow_memory (embeddings + expiry in final shape)
        // ==========================================================
        // Originally m20260406_000002 + m20260415_000001.
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS workflow_memory (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent_id INTEGER NOT NULL REFERENCES project_agents(id) ON DELETE CASCADE,
                fact TEXT NOT NULL,
                tags JSONB NOT NULL DEFAULT '[]'::jsonb,
                confidence REAL NOT NULL DEFAULT 0.5,
                times_used INTEGER NOT NULL DEFAULT 0,
                source_run_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
                superseded_by BIGINT REFERENCES workflow_memory(id) ON DELETE SET NULL,
                embedding BYTEA,
                expires_at TIMESTAMPTZ,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                last_used_at TIMESTAMPTZ
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workflow_memory_agent_active \
             ON workflow_memory(agent_id) WHERE superseded_by IS NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workflow_memory_project ON workflow_memory(project_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workflow_memory_tags ON workflow_memory USING gin(tags)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workflow_memory_fact_fts \
             ON workflow_memory USING gin(to_tsvector('english', fact))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workflow_memory_lookup \
             ON workflow_memory(agent_id, confidence DESC, times_used DESC) \
             WHERE superseded_by IS NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_workflow_memory_expires_at \
             ON workflow_memory(expires_at) \
             WHERE expires_at IS NOT NULL AND superseded_by IS NULL",
        )
        .await?;

        // ==========================================================
        // project_skill_definitions + project_mcp_definitions
        // ==========================================================
        // Originally m20260410_000001 + m20260411_000001 (nullable project_id)
        // + m20260411_000002 (archive).
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS project_skill_definitions (
                id SERIAL PRIMARY KEY,
                project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
                slug VARCHAR NOT NULL,
                name VARCHAR NOT NULL,
                description TEXT DEFAULT NULL,
                content TEXT NOT NULL,
                archive BYTEA DEFAULT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                UNIQUE(project_id, slug)
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_global_skill_definitions_slug \
             ON project_skill_definitions (slug) WHERE project_id IS NULL",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS project_mcp_definitions (
                id SERIAL PRIMARY KEY,
                project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
                slug VARCHAR NOT NULL,
                name VARCHAR NOT NULL,
                description TEXT DEFAULT NULL,
                config JSONB NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                UNIQUE(project_id, slug)
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_global_mcp_definitions_slug \
             ON project_mcp_definitions (slug) WHERE project_id IS NULL",
        )
        .await?;

        // ==========================================================
        // agent_runs additions
        // ==========================================================
        // Originally m20260409_000003 (ai_session_id), m20260413_000001 (ai_provider),
        // m20260416_000004 (source/ephemeral_yaml + drop NOT NULL on config_id),
        // m20260417_000002 (prompt_text), m20260417_000003 (workspace_volume).
        db.execute_unprepared(
            "ALTER TABLE agent_runs \
             ADD COLUMN IF NOT EXISTS ai_session_id VARCHAR DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS ai_provider VARCHAR DEFAULT NULL, \
             ADD COLUMN IF NOT EXISTS source VARCHAR NOT NULL DEFAULT 'committed', \
             ADD COLUMN IF NOT EXISTS ephemeral_yaml TEXT, \
             ADD COLUMN IF NOT EXISTS prompt_text TEXT, \
             ADD COLUMN IF NOT EXISTS workspace_volume TEXT",
        )
        .await?;

        db.execute_unprepared("ALTER TABLE agent_runs ALTER COLUMN config_id DROP NOT NULL")
            .await?;

        // ==========================================================
        // sandboxes (final shape: includes preview password)
        // ==========================================================
        // Originally m20260414_000001 + m20260416_000001.
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS sandboxes (
                id SERIAL PRIMARY KEY,
                public_id VARCHAR(64) NOT NULL UNIQUE,
                user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                name VARCHAR(255) NOT NULL,
                status VARCHAR(20) NOT NULL DEFAULT 'running',
                image VARCHAR(255),
                work_dir TEXT NOT NULL DEFAULT '/workspace',
                timeout_secs INTEGER NOT NULL DEFAULT 3600,
                metadata JSONB,
                preview_password_hash TEXT,
                preview_password_hint VARCHAR(8),
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                last_activity_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                expires_at TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '1 hour')
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandboxes_user_id ON sandboxes(user_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandboxes_status ON sandboxes(status)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandboxes_expires_at ON sandboxes(expires_at) \
             WHERE status = 'running'",
        )
        .await?;
        // Keep the sequence offset so standalone sandbox IDs don't collide
        // with agent-run IDs in the shared container-name namespace.
        db.execute_unprepared("ALTER SEQUENCE sandboxes_id_seq RESTART WITH 1000000")
            .await?;

        // ==========================================================
        // s3_sources.is_default
        // ==========================================================
        // Originally m20260416_000002.
        db.execute_unprepared(
            "ALTER TABLE s3_sources \
             ADD COLUMN IF NOT EXISTS is_default BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_s3_sources_single_default \
             ON s3_sources (is_default) WHERE is_default = TRUE",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE s3_sources SET is_default = TRUE \
             WHERE id = (SELECT id FROM s3_sources ORDER BY id ASC LIMIT 1) \
             AND (SELECT COUNT(*) FROM s3_sources) = 1",
        )
        .await?;

        // ==========================================================
        // postgres_major_upgrades
        // ==========================================================
        // Originally m20260416_000003.
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS postgres_major_upgrades (
                id SERIAL PRIMARY KEY,
                service_id INTEGER NOT NULL
                    REFERENCES external_services(id) ON DELETE CASCADE,
                from_version VARCHAR(16) NOT NULL,
                to_version VARCHAR(16) NOT NULL,
                from_image VARCHAR(512) NOT NULL,
                to_image VARCHAR(512) NOT NULL,
                status VARCHAR(20) NOT NULL DEFAULT 'pending',
                phase VARCHAR(32) NOT NULL DEFAULT 'pre_backup',
                pre_upgrade_backup_id INTEGER
                    REFERENCES backups(id) ON DELETE SET NULL,
                log_id VARCHAR(64) NOT NULL,
                rollback_volume_name VARCHAR(255),
                rollback_volume_expires_at TIMESTAMPTZ,
                error_message TEXT,
                attempt INTEGER NOT NULL DEFAULT 1,
                started_at TIMESTAMPTZ,
                finished_at TIMESTAMPTZ,
                created_by INTEGER NOT NULL REFERENCES users(id),
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_pg_major_upgrades_service_id \
             ON postgres_major_upgrades(service_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_pg_major_upgrades_status \
             ON postgres_major_upgrades(status)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_pg_major_upgrades_service_active \
             ON postgres_major_upgrades(service_id) \
             WHERE status IN ('pending', 'running')",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_pg_major_upgrades_rollback_expiry \
             ON postgres_major_upgrades(rollback_volume_expires_at) \
             WHERE rollback_volume_name IS NOT NULL AND status = 'completed'",
        )
        .await?;

        // ==========================================================
        // restore_runs
        // ==========================================================
        // Originally m20260417_000001.
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS restore_runs (
                id SERIAL PRIMARY KEY,
                source_backup_id INTEGER NOT NULL
                    REFERENCES backups(id) ON DELETE RESTRICT,
                source_service_id INTEGER NOT NULL
                    REFERENCES external_services(id) ON DELETE CASCADE,
                target_service_id INTEGER
                    REFERENCES external_services(id) ON DELETE SET NULL,
                target_service_name VARCHAR(255),
                mode VARCHAR(32) NOT NULL,
                status VARCHAR(20) NOT NULL DEFAULT 'pending',
                phase VARCHAR(32) NOT NULL DEFAULT 'prepare',
                recovery_target JSONB,
                parameter_overrides JSONB NOT NULL DEFAULT '{}'::jsonb,
                resume_token JSONB,
                log_id VARCHAR(64) NOT NULL,
                error_message TEXT,
                attempt INTEGER NOT NULL DEFAULT 1,
                started_at TIMESTAMPTZ,
                finished_at TIMESTAMPTZ,
                created_by INTEGER NOT NULL REFERENCES users(id),
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                CONSTRAINT restore_runs_mode_check
                    CHECK (mode IN ('in_place', 'new_service', 'pitr')),
                CONSTRAINT restore_runs_status_check
                    CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
                CONSTRAINT restore_runs_phase_check
                    CHECK (phase IN ('prepare', 'provision', 'restore', 'recover', 'verify', 'completed', 'failed'))
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_restore_runs_source_service \
             ON restore_runs(source_service_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_restore_runs_target_service \
             ON restore_runs(target_service_id) WHERE target_service_id IS NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_restore_runs_source_backup \
             ON restore_runs(source_backup_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_restore_runs_status \
             ON restore_runs(status)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_restore_runs_source_service_active \
             ON restore_runs(source_service_id) \
             WHERE status IN ('pending', 'running') AND mode = 'in_place'",
        )
        .await?;

        // ==========================================================
        // revenue_* tables (final shape with config + price/product_id)
        // ==========================================================
        // Originally m20260420_000001 + m20260420_000002 + m20260420_000003.

        // revenue_integrations (config JSONB included from the start)
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS revenue_integrations (
                id SERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                provider VARCHAR(32) NOT NULL,
                webhook_path_token VARCHAR(64) NOT NULL UNIQUE,
                webhook_signing_secret_encrypted TEXT NOT NULL,
                status VARCHAR(16) NOT NULL DEFAULT 'pending',
                config JSONB NULL,
                last_event_at TIMESTAMPTZ NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_revenue_integrations_project \
             ON revenue_integrations (project_id, provider)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_revenue_integrations_active \
             ON revenue_integrations (project_id, provider) \
             WHERE status <> 'disabled'",
        )
        .await?;

        // revenue_events (price_id + product_id included from the start)
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS revenue_events (
                id BIGSERIAL NOT NULL,
                project_id INTEGER NOT NULL,
                integration_id INTEGER NOT NULL,
                provider VARCHAR(32) NOT NULL,
                provider_event_id VARCHAR(255) NOT NULL,
                event_type VARCHAR(64) NOT NULL,
                customer_ref VARCHAR(255) NULL,
                subscription_ref VARCHAR(255) NULL,
                subscription_status VARCHAR(32) NULL,
                mrr_minor BIGINT NULL,
                amount_minor BIGINT NULL,
                currency CHAR(3) NULL,
                price_id TEXT NULL,
                product_id TEXT NULL,
                occurred_at TIMESTAMPTZ NOT NULL,
                payload JSONB NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (id, occurred_at)
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_revenue_events_project_time \
             ON revenue_events (project_id, occurred_at DESC)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_revenue_events_project_type_time \
             ON revenue_events (project_id, event_type, occurred_at DESC)",
        )
        .await?;
        // Best-effort hypertable conversion. Swallowed errors when TimescaleDB
        // is absent; the dedup unique index below still works on vanilla PG.
        let _ = db
            .execute_unprepared(
                "DO $$ BEGIN \
                    PERFORM create_hypertable('revenue_events', 'occurred_at', \
                        if_not_exists => TRUE, migrate_data => TRUE); \
                 EXCEPTION WHEN OTHERS THEN NULL; END $$",
            )
            .await;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_revenue_events_dedup \
             ON revenue_events (integration_id, provider_event_id, occurred_at)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_revenue_events_price \
             ON revenue_events (project_id, price_id) \
             WHERE price_id IS NOT NULL",
        )
        .await?;

        // revenue_subscriptions_state
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS revenue_subscriptions_state (
                id SERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL,
                integration_id INTEGER NOT NULL REFERENCES revenue_integrations(id) ON DELETE CASCADE,
                provider VARCHAR(32) NOT NULL,
                provider_subscription_id VARCHAR(255) NOT NULL,
                customer_ref VARCHAR(255) NULL,
                status VARCHAR(32) NOT NULL,
                mrr_minor BIGINT NOT NULL DEFAULT 0,
                currency CHAR(3) NULL,
                started_at TIMESTAMPTZ NULL,
                canceled_at TIMESTAMPTZ NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                UNIQUE (integration_id, provider_subscription_id)
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_revenue_subs_project_status \
             ON revenue_subscriptions_state (project_id, status)",
        )
        .await?;

        // revenue_customers_state
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS revenue_customers_state (
                id SERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL,
                integration_id INTEGER NOT NULL REFERENCES revenue_integrations(id) ON DELETE CASCADE,
                provider VARCHAR(32) NOT NULL,
                provider_customer_ref VARCHAR(255) NOT NULL,
                first_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                churned_at TIMESTAMPTZ NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                UNIQUE (integration_id, provider_customer_ref)
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_revenue_customers_project_seen \
             ON revenue_customers_state (project_id, first_seen_at)",
        )
        .await?;

        // revenue_mrr_daily continuous aggregate (TimescaleDB only; no-ops elsewhere)
        let _ = db
            .execute_unprepared(
                r#"
                DO $$ BEGIN
                    EXECUTE $q$
                        CREATE MATERIALIZED VIEW IF NOT EXISTS revenue_mrr_daily
                        WITH (timescaledb.continuous) AS
                        SELECT
                            time_bucket('1 day', occurred_at) AS bucket,
                            project_id,
                            currency,
                            SUM(mrr_minor) FILTER (
                                WHERE event_type IN ('subscription.created', 'subscription.updated', 'subscription.canceled')
                                  AND mrr_minor IS NOT NULL
                            ) AS mrr_delta_minor,
                            COUNT(*) FILTER (WHERE event_type = 'charge.succeeded') AS charge_count,
                            SUM(amount_minor) FILTER (WHERE event_type = 'charge.succeeded') AS charge_total_minor,
                            SUM(amount_minor) FILTER (WHERE event_type = 'charge.refunded') AS refund_total_minor
                        FROM revenue_events
                        WHERE currency IS NOT NULL
                        GROUP BY bucket, project_id, currency
                        WITH NO DATA
                    $q$;

                    PERFORM add_continuous_aggregate_policy('revenue_mrr_daily',
                        start_offset => INTERVAL '30 days',
                        end_offset => INTERVAL '1 hour',
                        schedule_interval => INTERVAL '30 minutes');
                EXCEPTION WHEN OTHERS THEN NULL;
                END $$
                "#,
            )
            .await;
        let _ = db
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_revenue_mrr_daily_project_bucket \
                 ON revenue_mrr_daily (project_id, bucket DESC)",
            )
            .await;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop in reverse dependency order.
        let _ = db
            .execute_unprepared("DROP MATERIALIZED VIEW IF EXISTS revenue_mrr_daily CASCADE")
            .await;
        db.execute_unprepared("DROP TABLE IF EXISTS revenue_customers_state CASCADE")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS revenue_subscriptions_state CASCADE")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS revenue_events CASCADE")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS revenue_integrations CASCADE")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS restore_runs")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS postgres_major_upgrades")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_s3_sources_single_default")
            .await?;
        db.execute_unprepared("ALTER TABLE s3_sources DROP COLUMN IF EXISTS is_default")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS sandboxes")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE agent_runs \
             DROP COLUMN IF EXISTS workspace_volume, \
             DROP COLUMN IF EXISTS prompt_text, \
             DROP COLUMN IF EXISTS ephemeral_yaml, \
             DROP COLUMN IF EXISTS source, \
             DROP COLUMN IF EXISTS ai_provider, \
             DROP COLUMN IF EXISTS ai_session_id",
        )
        .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS project_mcp_definitions")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS project_skill_definitions")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS workflow_memory")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS workspace_messages")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS workspace_sessions")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS agent_secrets")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_project_agents_webhook_id")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE project_agents \
             DROP COLUMN IF EXISTS ai_model, \
             DROP COLUMN IF EXISTS webhook_token, \
             DROP COLUMN IF EXISTS webhook_id, \
             DROP COLUMN IF EXISTS tools_config, \
             DROP COLUMN IF EXISTS skills_config, \
             DROP COLUMN IF EXISTS mcp_servers_config, \
             DROP COLUMN IF EXISTS config_repo_branch, \
             DROP COLUMN IF EXISTS config_repo_url, \
             DROP COLUMN IF EXISTS sandbox_enabled",
        )
        .await?;

        Ok(())
    }
}
