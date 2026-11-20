// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The daily salt of cookieless visitor ids (ADR 109 D3).
//!
//! One row per UTC day, shared by every node that ingests so they all derive
//! the same id for the same visitor. Only the current day's row is kept: an
//! older salt is deleted, after which ids derived from it cannot be recomputed
//! from stored data.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "analytics_identity_salts")]
pub struct Model {
    /// UTC day the salt is used for.
    #[sea_orm(primary_key, auto_increment = false)]
    pub day: Date,
    /// 32 random bytes, encrypted at rest with `EncryptionService`.
    #[serde(skip_serializing)]
    pub salt: String,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Hand-written so a logged row never prints the (encrypted) salt.
impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("day", &self.day)
            .field("salt", &"<redacted>")
            .field("created_at", &self.created_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_salt() {
        let model = Model {
            day: chrono::NaiveDate::from_ymd_opt(2026, 11, 20).expect("valid date"),
            salt: "sealed-salt-value".to_string(),
            created_at: chrono::Utc::now(),
        };
        let printed = format!("{model:?}");
        assert!(!printed.contains("sealed-salt-value"), "{printed}");
        assert!(printed.contains("<redacted>"));
    }
}
