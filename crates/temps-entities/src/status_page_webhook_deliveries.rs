// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One page event queued for one webhook endpoint (ADR 108 D2).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_webhook_deliveries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub webhook_id: i32,
    /// The page event's key (`u…`, `r…`, `m…`); unique per endpoint.
    pub event_key: String,
    /// The JSON body, built from the public snapshot when queued.
    pub payload: String,
    pub attempts: i32,
    pub next_attempt_at: DBDateTime,
    pub delivered_at: Option<DBDateTime>,
    pub failed_at: Option<DBDateTime>,
    pub last_error: Option<String>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_page_webhooks::Entity",
        from = "Column::WebhookId",
        to = "super::status_page_webhooks::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Webhook,
}

impl Related<super::status_page_webhooks::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Webhook.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
