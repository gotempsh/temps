// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "project_custom_domains")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub domain: String,
    pub redirect_to: Option<String>,
    pub status_code: Option<i32>,
    pub branch: Option<String>,
    pub status: String,
    pub message: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub certificate_id: Option<i32>,
    /// Docker Compose service name this domain routes to (null = all services)
    pub service_name: Option<String>,
    /// Who this address is for (ADR 064): `public`, or the reserved
    /// `private` / `signed_in`. Defaults to `public`.
    #[serde(default = "crate::audience::default_audience")]
    pub audience: String,
    /// For a `private` address (ADR 064 D5): the network range a visitor
    /// must come from, e.g. `100.64.0.0/10`.
    #[serde(default)]
    pub private_range: Option<String>,
    /// For a `private` address: the worker whose private listener serves it.
    #[serde(default)]
    pub private_node_id: Option<i32>,
    /// For a `private` address: the listener address DNS points at.
    #[serde(default)]
    pub private_listener: Option<String>,
    /// For a `signed_in` address (ADR 065 D7): `temps` or
    /// `cloudflare_access`. Null on every other address.
    #[serde(default)]
    pub gate: Option<String>,
    /// For a `signed_in` address with the Temps gate: the teams and users
    /// that may sign in, `{"teams": [..], "users": [..]}`. Null means every
    /// user with access to the project.
    #[serde(default)]
    pub gate_allow: Option<Json>,
    /// Gate tokens issued before this time are refused (ADR 065 D8).
    #[serde(default)]
    pub gate_not_before: Option<DBDateTime>,
    /// For a `cloudflare_access` gate (ADR 065 D9): the Access token
    /// provider whose app stands in front of the address.
    #[serde(default)]
    pub gate_access_provider_id: Option<i32>,
    /// That Access app's id at Cloudflare.
    #[serde(default)]
    pub gate_access_app_id: Option<String>,
    /// That Access app's AUD tag: what a listener checks a JWT's `aud`
    /// against.
    #[serde(default)]
    pub gate_access_aud: Option<String>,
    /// How a `private` address is served (ADR 065 D1): `listener`,
    /// `tailscale_serve` or `tailscale_service` ([`crate::audience::via`]).
    /// Set exactly when the audience is private; a check enforces it.
    #[serde(default)]
    pub private_via: Option<String>,
    /// For `tailscale_service`: the Service's `svc:` name.
    #[serde(default)]
    pub private_service: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Projects,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id"
    )]
    Environments,
    #[sea_orm(
        belongs_to = "super::domains::Entity",
        from = "Column::CertificateId",
        to = "super::domains::Column::Id"
    )]
    Domains,
}

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

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environments.def()
    }
}

impl Related<super::domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Domains.def()
    }
}
