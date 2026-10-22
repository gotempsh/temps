// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Owners that publish an internal DNS name another owner holds
//! (ADR-055 §10).
//!
//! A row is recorded by the claims backfill for every owner that lost a
//! name it shared, and by the registry whenever a publish is refused because
//! the name is held. Nothing is removed for a recorded owner: its existing
//! records stay in the zone until one side goes. The rows are what `temps
//! doctor` and the cluster DNS status report.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_name_conflicts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Normalised name both owners publish.
    pub fqdn: String,
    /// The owner whose publish is refused.
    pub owner_kind: String,
    pub owner_id: i64,
    /// The owner that holds the claim.
    pub holder_kind: String,
    pub holder_id: i64,
    pub first_seen_at: DBDateTime,
    pub last_seen_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
