// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-scoped sender authorization and idempotent deployment email delivery.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
CREATE TABLE email_domain_projects (
    domain_id INTEGER NOT NULL REFERENCES email_domains(id) ON DELETE CASCADE,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (domain_id, project_id)
);

CREATE INDEX idx_email_domain_projects_project
    ON email_domain_projects (project_id, domain_id);

CREATE TABLE email_idempotency_keys (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    idempotency_key VARCHAR(128) NOT NULL,
    payload_hash VARCHAR(64) NOT NULL,
    email_id UUID NOT NULL REFERENCES emails(id) ON DELETE CASCADE,
    lease_expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, idempotency_key)
);

CREATE UNIQUE INDEX idx_email_idempotency_email
    ON email_idempotency_keys (email_id);

CREATE INDEX idx_email_idempotency_stale_lease
    ON email_idempotency_keys (lease_expires_at);
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS email_idempotency_keys")
            .await?;
        conn.execute_unprepared("DROP TABLE IF EXISTS email_domain_projects")
            .await?;
        Ok(())
    }
}
