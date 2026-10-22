// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One owner per internal DNS name (ADR-055 §10).
//!
//! A row says which product object owns a `*.temps.local` name. The owner is
//! the stable object (an environment, a managed service, a cluster member),
//! never a revision of it, so a name survives redeploys. `fqdn` is stored
//! normalised (lower case, no trailing dot) and is the primary key, so two
//! owners can never hold one name.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_name_claims")]
pub struct Model {
    /// Normalised name, e.g. `production.shop.temps.local`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub fqdn: String,
    /// Same vocabulary as `service_endpoints.owner_kind`.
    pub owner_kind: String,
    pub owner_id: i64,
    /// The workload the name currently points at, once workloads exist
    /// (ADR-055 phase 2). NULL for names published by the legacy publishers.
    pub target_workload_id: Option<i64>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
