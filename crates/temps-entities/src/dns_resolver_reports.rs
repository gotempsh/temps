// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The last report of each node's internal DNS resolver (ADR-095).
//!
//! One row per node; node 0 is the control plane's own resolver, which has
//! no `nodes` row, hence no foreign key.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_resolver_reports")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: i32,
    /// `ok`, `agent_too_old` (the agent predates the endpoint),
    /// `unreachable`, `rejected` (authentication refused) or `invalid` (the
    /// report could not be read).
    pub report_state: String,
    /// When the last pull (or local report) was recorded, whatever its state.
    pub reported_at: DBDateTime,
    /// When the last `ok` report was recorded.
    pub last_ok_at: Option<DBDateTime>,
    /// The reporting observer's per-process id; a new one means the
    /// resolver's counters restarted.
    pub instance_id: Option<String>,
    pub build_version: Option<String>,
    pub running: bool,
    pub tasks_alive: bool,
    pub listen_address: Option<String>,
    pub zone_generation: Option<i64>,
    pub record_count: Option<i64>,
    /// Seconds since the resolver last synced with the control plane, as of
    /// `last_ok_at`; `None` before its first sync or for a local feed.
    pub last_sync_age_secs: Option<i64>,
    pub consecutive_sync_failures: i32,
    pub last_error: Option<String>,
    /// The last query-ring sequence ingested for `instance_id`.
    pub query_cursor: Option<i64>,
    /// Log entries the resolver dropped (its channel was full), cumulative
    /// for `instance_id`.
    pub dropped_log_entries: i64,
    /// How the control plane serves `*.temps.local` HTTP calls to its own
    /// containers (ADR-104 D4): the document a worker sends as the
    /// heartbeat's `capacity.internal_proxy`. Only node 0's row sets it;
    /// `None` means not reported.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub internal_proxy: Option<Json>,
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
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        self.updated_at = Set(now);
        Ok(self)
    }
}
