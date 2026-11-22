// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A credential the user provided to the assistant through a sealed form.
//!
//! Lifecycle: `awaiting` (the model asked, the user has not answered) →
//! `filled` (the value is stored encrypted in [`ciphertext`]) → erased when
//! the slot expires or is cancelled. The model never sees the value, only the
//! slot's `public_id`, which becomes the handle `secret_ref:v1:<public_id>`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_secret_slots")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Random 128-bit id, hex encoded. Unguessable: it is the whole handle.
    pub public_id: String,
    pub conversation_id: i64,
    /// The only user who may fill or use the slot.
    pub user_id: i32,
    /// The operation the value may be used for.
    pub operation_id: String,
    /// JSON pointer of the request-body field the value may be used for.
    pub field: String,
    /// What the form asks the user for, e.g. "S3 secret access key".
    pub label: String,
    /// `awaiting`, `filled` or `cancelled`.
    pub status: String,
    /// The value, encrypted with `EncryptionService`. `None` until filled and
    /// again once the slot expires.
    #[serde(skip_serializing)]
    pub ciphertext: Option<String>,
    pub expires_at: DBDateTime,
    pub created_at: DBDateTime,
    pub filled_at: Option<DBDateTime>,
}

// Not derived: the derived impl would print `ciphertext`.
impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ai_secret_slots::Model")
            .field("public_id", &self.public_id)
            .field("conversation_id", &self.conversation_id)
            .field("user_id", &self.user_id)
            .field("operation_id", &self.operation_id)
            .field("field", &self.field)
            .field("status", &self.status)
            .field("ciphertext", &self.ciphertext.as_ref().map(|_| "***"))
            .finish()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
