// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-user second-factor verification state.
//!
//! One row per user who has ever had a second factor checked. Kept out of
//! `users` so the replay/throttle bookkeeping does not ride along on every
//! `users::Model` read and write.
//!
//! Every reader and writer must hold the owning `users` row lock
//! (`SELECT ... FOR UPDATE`) for the duration of its transaction: that lock
//! is what serializes concurrent verifications for the same user, so the
//! read-check-write on these columns is never racy.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user_mfa_guards")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i32,
    /// Highest TOTP time-step counter ever accepted for this user. A code is
    /// only accepted when its matched step is strictly greater, so a code can
    /// never be used twice (RFC 6238 §5.2).
    pub last_totp_step: Option<i64>,
    /// Consecutive failed second-factor attempts across every verification
    /// path (sign-in challenge, step-up, password change, MFA disable).
    /// Reset to zero on any successful verification.
    pub failed_attempts: i32,
    /// While in the future, every second-factor attempt is refused without
    /// being evaluated, regardless of the client address.
    pub locked_until: Option<DBDateTime>,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    User,
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
