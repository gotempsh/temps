// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Outbox of status page announcements to subscribers (ADR 108 D2).
//!
//! One row per public incident update or resolution, keyed per page so the
//! same update is never queued twice. The text is rendered from the public
//! snapshot when the row is queued, so a notice can only carry what the page
//! already shows. Delivery walks confirmed subscribers by id from
//! `cursor_subscriber_id`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_notices")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub status_page_id: i32,
    /// `u{update id}` for a public update, `r{incident id}` for a resolution
    /// that has no public update of its own.
    pub notice_key: String,
    pub subject: String,
    pub body_text: String,
    /// HTML fragment; the unsubscribe footer is added per recipient.
    pub body_html: String,
    /// Highest subscriber id this notice has been sent to.
    pub cursor_subscriber_id: i32,
    pub sent_count: i32,
    pub created_at: DBDateTime,
    pub completed_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_pages::Entity",
        from = "Column::StatusPageId",
        to = "super::status_pages::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    StatusPage,
}

impl Related<super::status_pages::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusPage.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
