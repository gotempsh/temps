//! Add ASN organization + hosting-provider classification to `ip_geolocations`.
//!
//! The live-visitors view was showing datacenter/scraper traffic as real human
//! visitors: the existing bot detector only inspects the user-agent string, so
//! traffic that spoofs a normal browser UA from a hosting/VPS IP sails through
//! undetected. `asn_org` and `is_hosting_provider` are computed once per IP at
//! geolocation time (via the optional GeoLite2-ASN database) and cached on this
//! row like the rest of the geo data, so filtering on them at query time is free.
//!
//! Both columns are nullable: `is_hosting_provider = NULL` means "the ASN
//! database wasn't available when this IP was resolved", not "confirmed human".

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
                ADD COLUMN IF NOT EXISTS asn_org varchar,
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
                DROP COLUMN IF EXISTS is_hosting_provider,
                DROP COLUMN IF EXISTS asn_org;
            "#,
        )
        .await?;

        Ok(())
    }
}
