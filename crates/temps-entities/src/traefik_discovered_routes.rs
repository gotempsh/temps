// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Routes discovered from Traefik-style Docker labels on containers Temps did
//! **not** deploy.
//!
//! These rows are written exclusively by the Traefik discovery reconciler
//! (`temps_deployer::traefik_discovery`) and read by
//! `CachedPeerTable::load_routes`. Persisting them (rather than injecting
//! directly into the in-memory table) is deliberate: the existing DB trigger →
//! `pg_notify('route_table_changes')` → `load_routes()` machinery then
//! propagates every discovery change in-process *and* to every other control
//! plane node for free.
//!
//! `host` is unique: a hostname resolves to exactly one discovered backend.
//! Hosts already owned by a Temps deployment, custom route, or custom domain
//! are never written here — see the conflict handling in the discovery
//! service, and the belt-and-braces precedence check in `load_routes`.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "traefik_discovered_routes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Normalized (lowercased) hostname from the container's `Host()` rule.
    #[sea_orm(unique)]
    pub host: String,
    /// Traefik router name the host came from, kept for operator diagnostics.
    pub router_name: String,
    /// Full Docker container ID of the discovered backend.
    pub target_container_id: String,
    /// Docker container name — this is what the proxy resolves over the
    /// Docker network's internal DNS.
    pub target_container_name: String,
    /// Container-internal port the backend listens on.
    pub target_port: i32,
    /// Host-published port for `target_port`, when the container publishes
    /// one. Required for baremetal installs where Temps runs outside Docker
    /// and cannot resolve container names.
    pub target_host_port: Option<i32>,
    /// Docker network the container was discovered on. Scopes reconciliation
    /// deletes so one watcher never removes another network's rows.
    pub network: String,
    /// Whether the router requested TLS (`traefik.http.routers.<n>.tls`).
    pub tls: bool,
    /// Operator kill-switch for a single discovered route. Disabled rows stay
    /// in the table (so the operator can see what was found) but are skipped
    /// by `load_routes`.
    pub enabled: bool,
    /// Last time the reconciler saw this container/host pair alive.
    pub last_seen_at: DBDateTime,
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
            if self.last_seen_at.is_not_set() {
                self.last_seen_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}
