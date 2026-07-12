//! Add hosting-provider classification to `ip_geolocations`.
//!
//! The live-visitors view was showing datacenter/scraper traffic as real human
//! visitors: the existing bot detector only inspects the user-agent string, so
//! traffic that spoofs a normal browser UA from a hosting/VPS IP sails through
//! undetected. `is_hosting_provider` is computed once per IP at geolocation
//! time (via the optional GeoLite2-ASN database) and cached on this row like
//! the rest of the geo data, so filtering on it at query time is free.
//!
//! `is_hosting_provider` is nullable: `NULL` means "the ASN database wasn't
//! available when this IP was resolved", not "confirmed human".
//!
//! No new column for the ASN organization name: the initial schema
//! (`m20250101_000001_initial_schema`) already created an `asn` text column
//! on this table that was never wired up to the entity model or any code
//! path. We reuse it here instead of adding a second, duplicate column.

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
                ADD COLUMN IF NOT EXISTS is_hosting_provider boolean;
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
                DROP COLUMN IF EXISTS is_hosting_provider;
            "#,
        )
        .await?;

        Ok(())
    }
}
