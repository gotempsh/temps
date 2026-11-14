// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-user password-login throttling state.
//!
//! One row per account that has ever had a password attempt rejected. Kept
//! out of `users` so the throttle bookkeeping does not ride along on every
//! `users::Model` read and write, exactly like
//! [`super::user_mfa_guards`].
//!
//! Unlike the second-factor guard, these columns are never read-modify-
//! written from Rust: `temps_auth::login_guard` counts a failure with a
//! single `INSERT ... ON CONFLICT DO UPDATE` that derives the new count and
//! deadline from the stored row. That keeps the count exact under concurrent
//! attempts without holding the `users` row lock across the Argon2
//! verification a login has to perform.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user_login_guards")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i32,
    /// Consecutive rejected password attempts for this account, from any
    /// client address. Reset to zero by a successful password verification.
    pub failed_attempts: i32,
    /// While in the future, a password attempt for this account is refused
    /// without being evaluated, and answered exactly like a wrong password
    /// so it cannot be used to tell that the account exists.
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
