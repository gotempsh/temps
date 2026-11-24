// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's gate key (ADR 065 D8): one Ed25519 key, generated on
//! first use. Only the public half leaves the control plane, in the route
//! snapshot; the private half is sealed with `EncryptionService`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "gate_keys")]
pub struct Model {
    /// Always 1: there is one gate key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,
    /// Base64url public key.
    pub public_key: String,
    /// The 32-byte secret, sealed with `EncryptionService`. Never returned.
    #[serde(skip_serializing)]
    pub private_key_encrypted: String,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
