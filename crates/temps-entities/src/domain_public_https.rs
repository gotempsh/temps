// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Domain ingress observations. Certificate loading uses the narrower
//! certificate data in `domains` and does not need these diagnostics.

use sea_orm::entity::prelude::*;
use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use temps_core::{public_https::PublicHttpsResult, DBDateTime};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult)]
#[serde(transparent)]
pub struct StoredPublicHttpsResult(pub PublicHttpsResult);

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "domains")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub domain: String,
    pub generated_hostname: bool,
    pub public_https_checked_at: Option<DBDateTime>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub public_https_result: Option<StoredPublicHttpsResult>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
