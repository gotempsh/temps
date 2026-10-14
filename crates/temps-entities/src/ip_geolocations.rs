// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ip_geolocations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub ip_address: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub country: String,
    pub country_code: Option<String>,
    pub timezone: Option<String>,
    pub is_eu: bool,
    /// The ASN organization name (e.g. "Hetzner Online GmbH"). Maps to the `asn`
    /// column, which existed in the schema unused/unmapped before this field.
    #[sea_orm(column_name = "asn")]
    pub asn_org: Option<String>,
    pub is_hosting_provider: Option<bool>,
    pub created_at: DBDateTime,
    /// When the geolocation was last verified against the GeoIP database.
    pub updated_at: DBDateTime,
    /// When a writer last resolved this row's id for use as a reference.
    ///
    /// Distinct from `updated_at`, which only advances once per
    /// `geo.stale_lookup_days`. Retention deletes an address only once this is
    /// older than its window, so an id a writer is about to store (or holds in
    /// its cache) is never one the cleanup is entitled to remove. `NULL` on
    /// rows written before the column existed: read as `updated_at`.
    pub last_used_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}
