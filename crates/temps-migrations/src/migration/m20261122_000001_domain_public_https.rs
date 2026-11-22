// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Store the latest public HTTPS observation on the domain it describes.
//! Older installations have no measurement and retain ownership of their
//! hostnames. A suffix alone cannot establish who created a domain.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE domains
                ADD COLUMN IF NOT EXISTS generated_hostname boolean NOT NULL DEFAULT false,
                ADD COLUMN IF NOT EXISTS public_https_result jsonb,
                ADD COLUMN IF NOT EXISTS public_https_checked_at timestamptz
                CONSTRAINT domain_public_https_observation_pair
                    CHECK ((public_https_checked_at IS NULL) = (public_https_result IS NULL))"
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE domains
                DROP CONSTRAINT IF EXISTS domain_public_https_observation_pair,
                DROP COLUMN IF EXISTS public_https_checked_at,
                DROP COLUMN IF EXISTS public_https_result,
                DROP COLUMN IF EXISTS generated_hostname"
        ).await?;
        Ok(())
    }
}
