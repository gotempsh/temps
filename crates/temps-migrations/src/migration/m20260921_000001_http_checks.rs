// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE http_checks (
                id SERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                env_var_id INTEGER REFERENCES env_vars(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                encrypted_spec TEXT NOT NULL,
                encrypted_credential TEXT,
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                interval_seconds INTEGER NOT NULL DEFAULT 86400 CHECK (interval_seconds BETWEEN 300 AND 604800),
                next_check_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                lease_until TIMESTAMPTZ,
                lease_token TEXT,
                last_result JSONB,
                last_checked_at TIMESTAMPTZ,
                last_notified_fingerprint TEXT NOT NULL DEFAULT '',
                consecutive_unknowns INTEGER NOT NULL DEFAULT 0,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX http_checks_due_idx ON http_checks(next_check_at) WHERE enabled;
            CREATE INDEX http_checks_project_idx ON http_checks(project_id, id);
            CREATE INDEX http_checks_env_var_idx ON http_checks(env_var_id);
            CREATE FUNCTION reschedule_http_checks_after_env_change() RETURNS TRIGGER AS $$
            BEGIN
                UPDATE http_checks SET next_check_at=NOW(),last_result=NULL,last_checked_at=NULL,
                    lease_until=NULL,lease_token=NULL,consecutive_unknowns=0
                WHERE env_var_id=NEW.id;
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER http_checks_credential_rotated AFTER UPDATE OF value,is_encrypted ON env_vars
            FOR EACH ROW WHEN (OLD.value IS DISTINCT FROM NEW.value OR OLD.is_encrypted IS DISTINCT FROM NEW.is_encrypted)
            EXECUTE FUNCTION reschedule_http_checks_after_env_change();
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TRIGGER http_checks_credential_rotated ON env_vars; DROP FUNCTION reschedule_http_checks_after_env_change(); DROP TABLE http_checks").await?;
        Ok(())
    }
}
