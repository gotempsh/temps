// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self,manager:&SchemaManager)->Result<(),DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE workflow_dev_sessions(id UUID PRIMARY KEY,environment_id INTEGER NOT NULL UNIQUE REFERENCES environments(id) ON DELETE CASCADE,manifest JSONB,expires_at TIMESTAMPTZ NOT NULL);
        CREATE TABLE workflow_dev_requests(id UUID PRIMARY KEY,session_id UUID NOT NULL REFERENCES workflow_dev_sessions(id) ON DELETE CASCADE,request_encrypted TEXT NOT NULL CHECK(octet_length(request_encrypted)<=12000000),response_encrypted TEXT CHECK(octet_length(response_encrypted)<=12000000),delivered BOOLEAN NOT NULL DEFAULT false,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),expires_at TIMESTAMPTZ NOT NULL);
        CREATE INDEX workflow_dev_requests_pending ON workflow_dev_requests(session_id,created_at,id) WHERE NOT delivered;
        CREATE INDEX workflow_dev_requests_expiry ON workflow_dev_requests(session_id,expires_at)").await?;Ok(())
    }
    async fn down(&self,manager:&SchemaManager)->Result<(),DbErr> {
        manager.get_connection().execute_unprepared("DROP TABLE workflow_dev_requests; DROP TABLE workflow_dev_sessions").await?;Ok(())
    }
}
