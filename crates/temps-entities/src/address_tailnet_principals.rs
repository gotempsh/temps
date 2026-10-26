// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who a `tailnet_users` address admits (ADR 065 D6). See
//! `m20261026_000002_tailnet_principals`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A Tailscale login name: `alice@example.com`, `alice@github`.
pub const KIND_LOGIN: &str = "login";
/// A tag a tagged device carries: `tag:ci`.
pub const KIND_TAG: &str = "tag";

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "address_tailnet_principals")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// `temps_core::worker_ingress::private_address_id` of the address.
    pub address_id: String,
    /// [`KIND_LOGIN`] or [`KIND_TAG`].
    pub kind: String,
    /// ASCII-lower-cased, as it is compared.
    pub value: String,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// The list `rows` store, normalized, sorted and without duplicates. A row
/// that does not parse is dropped: the list admits fewer people, never more.
pub fn principals<'a>(
    rows: impl IntoIterator<Item = &'a Model>,
) -> Vec<temps_core::tailnet::TailnetPrincipal> {
    let mut list: Vec<_> = rows
        .into_iter()
        .filter_map(|row| {
            temps_core::tailnet::TailnetPrincipal {
                kind: temps_core::tailnet::PrincipalKind::parse(&row.kind)?,
                value: row.value.clone(),
            }
            .normalized()
            .ok()
        })
        .collect();
    list.sort();
    list.dedup();
    list
}
