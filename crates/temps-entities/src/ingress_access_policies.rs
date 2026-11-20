// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Access policies stored for an ingress endpoint (ADR-055 §11), evaluated
//! in `position` order after the policies compiled from the owner's existing
//! access settings. Every policy must pass. The column is the typed
//! [`AccessPolicy`], never free-form annotations.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr, FromJsonQueryResult};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use temps_workload_ingress::AccessPolicy;

/// JSON column wrapper for the typed policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult)]
pub struct StoredPolicy(pub AccessPolicy);

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ingress_access_policies")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub endpoint_id: i64,
    pub position: i32,
    #[sea_orm(column_type = "JsonBinary")]
    pub policy: StoredPolicy,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::ingress_endpoints::Entity",
        from = "Column::EndpointId",
        to = "crate::ingress_endpoints::Column::Id",
        on_delete = "Cascade"
    )]
    Endpoint,
}

impl Related<crate::ingress_endpoints::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Endpoint.def()
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
