// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress endpoints (ADR-055 §11).
//!
//! An endpoint belongs to a product object (`owner_kind`/`owner_id`), never
//! to a workload revision. Every environment has one, created by the
//! migration's backfill and by a trigger on `environments` inserts. Its
//! access is the policies compiled from the environment's existing access
//! settings (IP rules, rate limits, attack mode, password wall), followed by
//! the policies stored for it in `ingress_access_policies`. The settings are
//! not copied into rows: they keep a single source of truth, the one the
//! legacy evaluator reads, which is what lets shadow mode compare the two.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr, FromJsonQueryResult};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use temps_workload_ingress::EndpointTarget;

/// JSON column wrapper for the typed target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult)]
pub struct StoredTarget(pub EndpointTarget);

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ingress_endpoints")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// `environment`, `sandbox`, `database` or `function`.
    pub owner_kind: String,
    pub owner_id: i64,
    /// Set when the owner is an environment (cascade on delete).
    pub environment_id: Option<i32>,
    /// Set when the owner is a sandbox (cascade on delete).
    pub sandbox_id: Option<i32>,
    /// ADR-055 Phase 2 workload, once the product runs on the workload
    /// plane. No foreign key yet: the table lands in another phase.
    pub workload_id: Option<i64>,
    #[sea_orm(column_type = "JsonBinary")]
    pub target: StoredTarget,
    /// ADR-050 path route prefix; `None` = the whole host.
    pub path_prefix: Option<String>,
    /// `http`, `tls_passthrough` or `tcp`.
    pub protocol: String,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::environments::Entity",
        from = "Column::EnvironmentId",
        to = "crate::environments::Column::Id",
        on_delete = "Cascade"
    )]
    Environment,
    #[sea_orm(has_many = "crate::ingress_access_policies::Entity")]
    AccessPolicies,
}

impl Related<crate::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<crate::ingress_access_policies::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AccessPolicies.def()
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
        self.updated_at = Set(now);
        Ok(self)
    }
}
