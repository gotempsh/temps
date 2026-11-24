// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the runtime AI model catalog comes from and how it is refreshed
//! (ADR 113).
//!
//! A single row (`id = 1`). Without a row the defaults apply: the built-in
//! source, automatic refresh every 24 hours and periodic rediscovery of each
//! provider key's models. The `last_*` columns are written by the refresh and
//! report its outcome to the console.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// The only `id` the table allows.
pub const SINGLETON_ID: i16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_model_catalog_settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i16,
    /// `https://` URL of a models.dev-compatible catalog. `NULL` uses the
    /// catalog published in the Temps repository.
    pub source_url: Option<String>,
    /// Refresh the catalog in the background. Defaults to `true`.
    pub auto_refresh: bool,
    /// Hours between background refreshes, 1 to 168. Defaults to 24.
    pub refresh_interval_hours: i32,
    /// Rediscover every active provider key's models on the same interval.
    /// Defaults to `true`.
    pub discover_provider_models: bool,
    /// When a refresh last started, whether or not it succeeded.
    pub last_attempt_at: Option<DBDateTime>,
    /// When a refresh last succeeded.
    pub last_refreshed_at: Option<DBDateTime>,
    /// The URL the stored catalog was fetched from.
    pub last_source_url: Option<String>,
    /// Why the last attempt failed; `NULL` after a success.
    pub last_error: Option<String>,
    /// Entries stored by the last successful refresh.
    pub entry_count: i32,
    /// Entries the last successful refresh rejected as invalid.
    pub skipped_count: i32,
    /// When provider keys' models were last rediscovered in the background.
    pub last_discovery_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
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
