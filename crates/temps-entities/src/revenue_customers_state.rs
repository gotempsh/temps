// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Current state of each customer, upserted from customer/subscription
/// events. `churned_at` is populated when every subscription belonging
/// to the customer is canceled.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "revenue_customers_state")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub integration_id: i32,
    pub provider: String,
    pub provider_customer_ref: String,
    pub first_seen_at: DBDateTime,
    pub churned_at: Option<DBDateTime>,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::revenue_integrations::Entity",
        from = "Column::IntegrationId",
        to = "super::revenue_integrations::Column::Id",
        on_delete = "Cascade"
    )]
    Integration,
}

impl Related<super::revenue_integrations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Integration.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, _insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        self.updated_at = Set(chrono::Utc::now());
        Ok(self)
    }
}
