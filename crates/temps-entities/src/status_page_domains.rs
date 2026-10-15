// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A custom hostname attached to a status page. See
//! `m20261012_000002_create_status_page_domains` for the schema rationale.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_domains")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The page the hostname serves. `None` for a tombstone: a hostname that
    /// was removed, or whose page or project was deleted, and that keeps
    /// answering 404 until it is attached again.
    pub status_page_id: Option<i32>,
    /// Normalized hostname: lowercase ASCII (IDNA punycode), no trailing dot.
    pub hostname: String,
    /// `http-01` or `dns-01`: how ownership is proven through ACME.
    pub verification_method: String,
    /// The `domains` row holding the ACME order and, once ownership is
    /// proven, the certificate. Owned exclusively by this binding.
    pub domain_id: Option<i32>,
    /// When the binding became a tombstone.
    pub detached_at: Option<DBDateTime>,
    /// Lease held by a verify or renew in progress, so two certificate
    /// operations on one hostname never overlap. Expired leases are free.
    pub certificate_operation_until: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_pages::Entity",
        from = "Column::StatusPageId",
        to = "super::status_pages::Column::Id"
    )]
    StatusPage,
    #[sea_orm(
        belongs_to = "super::domains::Entity",
        from = "Column::DomainId",
        to = "super::domains::Column::Id"
    )]
    Domain,
}

impl Related<super::status_pages::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusPage.def()
    }
}

impl Related<super::domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Domain.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        self.updated_at = Set(now);
        Ok(self)
    }
}

/// Lowercase, trim and drop a trailing dot: the stored form of `hostname`.
fn stored_form(hostname: &str) -> String {
    hostname.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Serialize every claim of `hostname` (status page, project or environment
/// domain, custom route) for the rest of the current transaction, so two
/// claims of the same name cannot both pass their availability checks.
/// Wildcards lock their parent, so `*.example.com` and `a.example.com`
/// serialize with each other. MUST run inside a transaction.
pub async fn lock_hostname_claim<C: ConnectionTrait>(
    conn: &C,
    hostname: &str,
) -> Result<(), DbErr> {
    let host = stored_form(hostname);
    let key = host
        .strip_prefix("*.")
        .map(str::to_string)
        .or_else(|| host.split_once('.').map(|(_, parent)| parent.to_string()))
        .unwrap_or(host);
    conn.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtext('hostname-claim:' || $1))",
        [key.into()],
    ))
    .await?;
    Ok(())
}

/// Whether `hostname` (or, for `*.parent`, any single-label name under
/// `parent`) is attached to a status page. Tombstones do not count.
pub async fn attached_hostname_conflict<C: ConnectionTrait>(
    conn: &C,
    hostname: &str,
) -> Result<bool, DbErr> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let host = stored_form(hostname);
    match host.strip_prefix("*.") {
        None => Ok(Entity::find()
            .filter(Column::Hostname.eq(host.as_str()))
            .filter(Column::StatusPageId.is_not_null())
            .one(conn)
            .await?
            .is_some()),
        Some(parent) => {
            let suffix = format!(".{parent}");
            let candidates = Entity::find()
                .filter(Column::Hostname.ends_with(suffix.as_str()))
                .filter(Column::StatusPageId.is_not_null())
                .all(conn)
                .await?;
            Ok(candidates.iter().any(|binding| {
                binding
                    .hostname
                    .strip_suffix(suffix.as_str())
                    .is_some_and(|label| !label.is_empty() && !label.contains('.'))
            }))
        }
    }
}
