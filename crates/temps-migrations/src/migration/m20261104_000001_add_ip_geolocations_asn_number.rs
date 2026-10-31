// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Store the ASN number and the hosting-classifier version on `ip_geolocations`.
//!
//! Hosting/datacenter classification used to match substrings of the ASN
//! organization name only, which misses large clouds whose organization name
//! carries no keyword (Tencent's is a street address) and wrongly flags
//! Cloudflare, which carries iCloud Private Relay users. It now classifies by
//! ASN number first, so the number is stored next to the organization name.
//!
//! `hosting_classifier_version` records which version of the classification
//! rules produced `is_hosting_provider`. Every row written before this
//! migration has it `NULL`, so each one is re-resolved once, on its next
//! lookup, against the current rules and the bundled ASN database -- instead
//! of keeping a verdict computed without an ASN database (or with the old
//! name-only rules) until the generic staleness window expires.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            ALTER TABLE ip_geolocations
                ADD COLUMN IF NOT EXISTS asn_number integer,
                ADD COLUMN IF NOT EXISTS hosting_classifier_version smallint;
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            ALTER TABLE ip_geolocations
                DROP COLUMN IF EXISTS hosting_classifier_version,
                DROP COLUMN IF EXISTS asn_number;
            "#,
        )
        .await?;

        Ok(())
    }
}
