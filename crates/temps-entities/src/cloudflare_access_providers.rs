// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A Cloudflare token with Access permissions (ADR 065 D9): separate from
//! the DNS and tunnel token, because both of its permissions are
//! account-wide. The token is sealed with `EncryptionService` and never
//! returned; the team's signing keys are public and kept here so every
//! route snapshot can carry them.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cloudflare_access_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    /// The Cloudflare account the Access apps live in.
    pub account_id: String,
    /// `<team>.cloudflareaccess.com`: the issuer of the team's Access JWTs.
    pub team_domain: String,
    /// Sealed with `EncryptionService`. Never returned.
    #[serde(skip_serializing)]
    pub api_token_encrypted: String,
    /// `{"keys": [{"kid", "n", "e"}, ..]}`, RS256 keys only, as last fetched.
    pub jwks: Option<Json>,
    pub jwks_fetched_at: Option<DBDateTime>,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
