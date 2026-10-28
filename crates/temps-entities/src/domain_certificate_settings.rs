// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "domain_certificate_settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub domain_id: i32,
    pub challenge_type: String,
    pub renewal_days: i32,
    pub alert_days: Option<i32>,
    pub alerts_enabled: Option<bool>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
