// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable signing authority for each authenticated worker boot. Existing
//! saved grants are not enrolled or activated. Keys are encrypted by Temps;
//! worker sealing keys and interception certificates are public material.
//! Downgrade refuses to discard established authorities.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE IF NOT EXISTS sandbox_access_authorities (
    id UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    node_id INTEGER REFERENCES nodes(id) ON DELETE RESTRICT,
    sdk_node_id UUID NOT NULL UNIQUE CHECK (sdk_node_id <> '00000000-0000-0000-0000-000000000000'),
    seal_public_key TEXT NOT NULL CHECK (length(seal_public_key) = 44),
    snapshot_key_encrypted TEXT NOT NULL CHECK (length(snapshot_key_encrypted) > 0),
    session_key_encrypted TEXT NOT NULL CHECK (length(session_key_encrypted) > 0),
    ca_certificate TEXT NOT NULL CHECK (length(ca_certificate) BETWEEN 1 AND 16384),
    ca_key_encrypted TEXT NOT NULL CHECK (length(ca_key_encrypted) > 0),
    snapshot_version BIGINT NOT NULL DEFAULT 0 CHECK (snapshot_version >= 0),
    closed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS sandbox_access_authorities_node ON sandbox_access_authorities (node_id,created_at DESC,id DESC);
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
    IF to_regclass('sandbox_access_authorities') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM sandbox_access_authorities) THEN
            RAISE EXCEPTION 'Cannot downgrade sandbox access authorities while worker registrations exist';
        END IF;
    END IF;
END $$;
DROP TABLE IF EXISTS sandbox_access_authorities;
"#).await?;
        Ok(())
    }
}
