// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project / per-environment client-IP allow/deny list.
//!
//! One row per scope: `environment_id = NULL` is the project-wide policy and a
//! set `environment_id` overrides it for that environment. Validation and
//! canonicalisation of `cidrs` live in `temps_proxy::cidr_set`; the proxy reads
//! the rows into an in-memory snapshot and never queries this table per
//! request.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr, FromJsonQueryResult};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

/// One CIDR in a policy, in canonical form (network address, explicit prefix).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IpAccessCidrEntry {
    /// Canonical CIDR, e.g. `10.0.0.0/8` or `fd7a:115c:a1e0::/48`.
    #[schema(example = "100.64.0.0/10")]
    pub cidr: String,
    /// Optional human label, e.g. `Office`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(example = "Tailscale")]
    pub description: Option<String>,
}

/// JSON column wrapper for the CIDR list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, FromJsonQueryResult)]
pub struct IpAccessCidrs(pub Vec<IpAccessCidrEntry>);

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "project_ip_access_rules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    /// `None` is the project-wide policy.
    pub environment_id: Option<i32>,
    /// `disabled`, `allow` or `deny`.
    pub mode: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub cidrs: IpAccessCidrs,
    pub updated_by: Option<i32>,
    /// Change-feed position, assigned by a database trigger on every write.
    /// Read-only here: the proxy polls it to pick up edits made by any writer.
    pub revision: i64,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::projects::Entity",
        from = "Column::ProjectId",
        to = "crate::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "crate::environments::Entity",
        from = "Column::EnvironmentId",
        to = "crate::environments::Column::Id",
        on_delete = "Cascade"
    )]
    Environment,
}

impl Related<crate::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<crate::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

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
        // Always bump so `updated_at` reflects every save, even a no-op one.
        self.updated_at = Set(now);
        Ok(self)
    }
}
