// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who decides what a tunnel publishes (ADR 058 D9).
//!
//! - `ingress_connectors.auto_publish`: whether the tunnel publishes every
//!   worker-served hostname under its domain, or only the ones chosen for it.
//! - `ingress_hostname_choices`: an operator's choice for one hostname. A
//!   connector id sends it through that tunnel; NULL keeps it off every
//!   tunnel. No row means automatic.

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
ALTER TABLE ingress_connectors
    ADD COLUMN IF NOT EXISTS auto_publish BOOLEAN NOT NULL DEFAULT TRUE;

CREATE TABLE IF NOT EXISTS ingress_hostname_choices (
    id SERIAL PRIMARY KEY,
    hostname VARCHAR(253) NOT NULL UNIQUE,
    connector_id INTEGER REFERENCES ingress_connectors(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP TABLE IF EXISTS ingress_hostname_choices;
ALTER TABLE ingress_connectors DROP COLUMN IF EXISTS auto_publish;
"#,
            )
            .await?;
        Ok(())
    }
}
