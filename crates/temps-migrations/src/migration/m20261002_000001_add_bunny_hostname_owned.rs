// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Remember whether Temps added a Bunny delivery binding's hostname to the
//! Pull Zone, so removing the binding detaches only hostnames Temps added.
//!
//! Existing bindings default to `false`: a hostname whose origin is unknown is
//! never detached, which keeps any hostname the user had attached themselves.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE domain_delivery_bindings \
                 ADD COLUMN IF NOT EXISTS bunny_hostname_owned boolean NOT NULL DEFAULT false",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE domain_delivery_bindings DROP COLUMN IF EXISTS bunny_hostname_owned",
            )
            .await?;
        Ok(())
    }
}
