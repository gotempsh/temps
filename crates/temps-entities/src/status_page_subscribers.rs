// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email subscribers of a public status page (ADR 108 D2).
//!
//! An address is personal data. It is stored per page, cascades with the
//! page, and is never shown to anyone but the page's administrators. No token
//! is stored in plaintext; see `m20261120_108001_status_page_subscriptions`.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// The address asked to subscribe and has not followed the confirmation link.
pub const STATE_PENDING: &str = "pending";
/// The address confirmed and receives notices.
pub const STATE_CONFIRMED: &str = "confirmed";

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_subscribers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub status_page_id: i32,
    /// Lowercased address. Unique per page.
    pub email: String,
    /// [`STATE_PENDING`] or [`STATE_CONFIRMED`].
    pub state: String,
    /// Hex SHA-256 of the one-time confirmation token, set when the
    /// confirmation mail is sent and cleared on confirmation.
    pub confirm_token_hash: Option<String>,
    pub confirm_expires_at: Option<DBDateTime>,
    /// `None` until the confirmation mail is sent (or after a repeated
    /// request asks for it again).
    pub confirmation_sent_at: Option<DBDateTime>,
    /// Latest subscribe request; an unconfirmed row is deleted once it is
    /// older than the confirmation window.
    pub requested_at: DBDateTime,
    /// Random per-row input to the unsubscribe token derivation.
    pub unsubscribe_nonce: String,
    /// Hex SHA-256 of the unsubscribe token. Unique.
    pub unsubscribe_token_hash: String,
    pub confirmed_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
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
            if self.requested_at.is_not_set() {
                self.requested_at = Set(now);
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
