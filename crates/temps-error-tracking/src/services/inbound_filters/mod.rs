// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-side inbound filters (ADR 111, D1.4): per-project rules that drop
//! noisy Sentry error events before they are stored.
//!
//! - [`matcher`]: the rule kinds and how one event is matched.
//! - [`registry`]: the lock-free in-memory rules ingest reads, and the
//!   per-rule drop counters.
//! - [`InboundFilterService`]: reading and writing the rules; every write
//!   swaps the registry's snapshot before it returns.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_entities::error_inbound_filters;

use super::types::ErrorTrackingError;

pub mod matcher;
pub mod registry;

#[cfg(test)]
mod tests;

pub use matcher::{InboundFilterKind, MAX_PATTERN_CHARS};
pub use registry::InboundFilterRegistry;

/// Most rules one project may hold. Bounds the per-event work and the
/// registry's memory.
pub const MAX_FILTERS_PER_PROJECT: u64 = 50;

/// A rule as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundFilter {
    pub id: i32,
    pub project_id: i32,
    pub kind: InboundFilterKind,
    pub pattern: Option<String>,
    pub enabled: bool,
    /// Events dropped, including those counted in memory and not yet
    /// flushed.
    pub dropped_count: i64,
    pub last_dropped_at: Option<chrono::DateTime<Utc>>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

/// Reads and writes inbound filter rules and keeps the registry current.
pub struct InboundFilterService {
    db: Arc<DatabaseConnection>,
    registry: Arc<InboundFilterRegistry>,
}

impl InboundFilterService {
    pub fn new(db: Arc<DatabaseConnection>, registry: Arc<InboundFilterRegistry>) -> Self {
        Self { db, registry }
    }

    /// Every rule of a project, oldest first. At most
    /// [`MAX_FILTERS_PER_PROJECT`] rows.
    pub async fn list(&self, project_id: i32) -> Result<Vec<InboundFilter>, ErrorTrackingError> {
        self.require_project(project_id).await?;
        let pending = self.registry.pending_drops();
        error_inbound_filters::Entity::find()
            .filter(error_inbound_filters::Column::ProjectId.eq(project_id))
            .order_by_asc(error_inbound_filters::Column::Id)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|row| to_filter(row, &pending))
            .collect()
    }

    /// One rule of a project, with how many events it dropped.
    pub async fn get(
        &self,
        project_id: i32,
        filter_id: i32,
    ) -> Result<InboundFilter, ErrorTrackingError> {
        let row = self.find(project_id, filter_id).await?;
        to_filter(row, &self.registry.pending_drops())
    }

    pub async fn create(
        &self,
        project_id: i32,
        kind: InboundFilterKind,
        pattern: Option<String>,
        enabled: bool,
    ) -> Result<InboundFilter, ErrorTrackingError> {
        let pattern = validate_pattern(kind, pattern)?;
        // The limit and duplicate checks run in the transaction that inserts,
        // under a lock on the project row, so concurrent creates for one
        // project serialize and cannot overshoot the limit (#2112). Dropping
        // the transaction on an early return rolls it back.
        let txn = self.db.begin().await?;
        lock_project(&txn, project_id).await?;
        let existing = error_inbound_filters::Entity::find()
            .filter(error_inbound_filters::Column::ProjectId.eq(project_id))
            .count(&txn)
            .await?;
        if existing >= MAX_FILTERS_PER_PROJECT {
            return Err(ErrorTrackingError::InboundFilterLimit {
                project_id,
                limit: MAX_FILTERS_PER_PROJECT,
            });
        }
        if !kind.takes_pattern() {
            let duplicate = error_inbound_filters::Entity::find()
                .filter(error_inbound_filters::Column::ProjectId.eq(project_id))
                .filter(error_inbound_filters::Column::Kind.eq(kind.as_str()))
                .count(&txn)
                .await?;
            if duplicate > 0 {
                return Err(ErrorTrackingError::InboundFilterExists {
                    project_id,
                    kind: kind.as_str().to_string(),
                });
            }
        }
        if let Some(pattern) = pattern.as_deref() {
            ensure_pattern_free(&txn, project_id, kind, pattern, None).await?;
        }
        let now = Utc::now();
        let row = error_inbound_filters::ActiveModel {
            project_id: Set(project_id),
            kind: Set(kind.as_str().to_string()),
            pattern: Set(pattern),
            enabled: Set(enabled),
            dropped_count: Set(0),
            last_dropped_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(|error| match error.sql_err() {
            // A concurrent create of the same built-in kind.
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => {
                ErrorTrackingError::InboundFilterExists {
                    project_id,
                    kind: kind.as_str().to_string(),
                }
            }
            _ => error.into(),
        })?;
        txn.commit().await?;
        self.reload().await;
        to_filter(row, &Default::default())
    }

    /// Change a rule's pattern and/or whether it is enabled.
    pub async fn update(
        &self,
        project_id: i32,
        filter_id: i32,
        pattern: Option<String>,
        enabled: Option<bool>,
    ) -> Result<InboundFilter, ErrorTrackingError> {
        let txn = self.db.begin().await?;
        let row = Self::find_in(&txn, project_id, filter_id).await?;
        let kind = parse_kind(&row)?;
        let mut active: error_inbound_filters::ActiveModel = row.into();
        if pattern.is_some() {
            let pattern = validate_pattern(kind, pattern)?;
            if let Some(pattern) = pattern.as_deref() {
                // Same lock as create, so two edits cannot race to one pattern.
                lock_project(&txn, project_id).await?;
                ensure_pattern_free(&txn, project_id, kind, pattern, Some(filter_id)).await?;
            }
            active.pattern = Set(pattern);
        }
        if let Some(enabled) = enabled {
            active.enabled = Set(enabled);
        }
        active.updated_at = Set(Utc::now());
        let row = active.update(&txn).await?;
        txn.commit().await?;
        self.reload().await;
        to_filter(row, &self.registry.pending_drops())
    }

    pub async fn delete(
        &self,
        project_id: i32,
        filter_id: i32,
    ) -> Result<InboundFilter, ErrorTrackingError> {
        let row = self.find(project_id, filter_id).await?;
        error_inbound_filters::Entity::delete_by_id(row.id)
            .exec(self.db.as_ref())
            .await?;
        self.reload().await;
        to_filter(row, &Default::default())
    }

    /// A missing (or deleted) project is a 404 naming it: not an empty
    /// listing, and not a foreign-key violation surfaced as a 500 on create.
    async fn require_project(&self, project_id: i32) -> Result<(), ErrorTrackingError> {
        if temps_entities::projects::live_project_exists(self.db.as_ref(), project_id).await? {
            Ok(())
        } else {
            Err(ErrorTrackingError::ProjectNotFound { project_id })
        }
    }

    async fn find(
        &self,
        project_id: i32,
        filter_id: i32,
    ) -> Result<error_inbound_filters::Model, ErrorTrackingError> {
        Self::find_in(self.db.as_ref(), project_id, filter_id).await
    }

    async fn find_in<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
        filter_id: i32,
    ) -> Result<error_inbound_filters::Model, ErrorTrackingError> {
        error_inbound_filters::Entity::find_by_id(filter_id)
            .filter(error_inbound_filters::Column::ProjectId.eq(project_id))
            .one(conn)
            .await?
            .ok_or(ErrorTrackingError::InboundFilterNotFound {
                project_id,
                filter_id,
            })
    }

    /// Swap in the rules as stored now. A failed reload keeps the previous
    /// rules in force and is retried by the periodic refresh, so the write
    /// that triggered it still succeeds.
    async fn reload(&self) {
        if let Err(error) = self.registry.reload(self.db.as_ref()).await {
            tracing::error!(
                error = %error,
                "Failed to reload inbound filters after a change; the periodic refresh will retry"
            );
        }
    }
}

/// Locks a live project's row for the rest of the transaction, or reports it
/// missing (a 404 naming it, not a foreign-key violation on insert).
async fn lock_project<C: ConnectionTrait>(
    conn: &C,
    project_id: i32,
) -> Result<(), ErrorTrackingError> {
    temps_entities::projects::Entity::find_by_id(project_id)
        .filter(temps_entities::projects::Column::DeletedAt.is_null())
        .lock_exclusive()
        .one(conn)
        .await?
        .map(|_| ())
        .ok_or(ErrorTrackingError::ProjectNotFound { project_id })
}

/// Refuses a pattern the project already has for `kind`, compared ignoring
/// case as matching does. Reads at most [`MAX_FILTERS_PER_PROJECT`] rows.
async fn ensure_pattern_free<C: ConnectionTrait>(
    conn: &C,
    project_id: i32,
    kind: InboundFilterKind,
    pattern: &str,
    except_filter_id: Option<i32>,
) -> Result<(), ErrorTrackingError> {
    let existing: Vec<(i32, Option<String>)> = error_inbound_filters::Entity::find()
        .select_only()
        .column(error_inbound_filters::Column::Id)
        .column(error_inbound_filters::Column::Pattern)
        .filter(error_inbound_filters::Column::ProjectId.eq(project_id))
        .filter(error_inbound_filters::Column::Kind.eq(kind.as_str()))
        .into_tuple()
        .all(conn)
        .await?;
    let wanted = pattern.to_lowercase();
    if let Some((filter_id, Some(stored))) = existing.into_iter().find(|(id, stored)| {
        Some(*id) != except_filter_id
            && stored.as_deref().map(str::to_lowercase).as_deref() == Some(wanted.as_str())
    }) {
        return Err(ErrorTrackingError::InboundPatternFilterExists {
            project_id,
            kind: kind.as_str().to_string(),
            pattern: stored,
            filter_id,
        });
    }
    Ok(())
}

fn parse_kind(row: &error_inbound_filters::Model) -> Result<InboundFilterKind, ErrorTrackingError> {
    InboundFilterKind::parse(&row.kind).ok_or_else(|| {
        ErrorTrackingError::Validation(format!(
            "inbound filter {} has an unknown kind '{}'",
            row.id, row.kind
        ))
    })
}

fn to_filter(
    row: error_inbound_filters::Model,
    pending: &std::collections::HashMap<i32, u64>,
) -> Result<InboundFilter, ErrorTrackingError> {
    let kind = parse_kind(&row)?;
    let unflushed = pending
        .get(&row.id)
        .map(|count| i64::try_from(*count).unwrap_or(i64::MAX))
        .unwrap_or(0);
    Ok(InboundFilter {
        id: row.id,
        project_id: row.project_id,
        kind,
        pattern: row.pattern,
        enabled: row.enabled,
        dropped_count: row.dropped_count.saturating_add(unflushed),
        last_dropped_at: row.last_dropped_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// A pattern kind needs a non-empty pattern of bounded length; the other
/// kinds take none.
fn validate_pattern(
    kind: InboundFilterKind,
    pattern: Option<String>,
) -> Result<Option<String>, ErrorTrackingError> {
    let pattern = pattern
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    match (kind.takes_pattern(), pattern) {
        (true, None) => Err(ErrorTrackingError::Validation(format!(
            "an inbound filter of kind '{}' needs a pattern, such as \"*ResizeObserver*\"",
            kind.as_str()
        ))),
        (true, Some(pattern)) if pattern.chars().count() > MAX_PATTERN_CHARS => {
            Err(ErrorTrackingError::Validation(format!(
                "inbound filter pattern is {} characters; at most {MAX_PATTERN_CHARS}",
                pattern.chars().count()
            )))
        }
        (true, Some(pattern)) if pattern.contains('\0') => Err(ErrorTrackingError::Validation(
            "inbound filter pattern contains a NUL character".into(),
        )),
        // Usually a pattern built from an empty variable; it would drop every
        // event of the project (#2112).
        (true, Some(pattern)) if pattern.chars().all(|c| c == '*') => {
            Err(ErrorTrackingError::Validation(format!(
                "inbound filter pattern '{pattern}' matches every event of kind '{}' and would \
                 drop them all; disable error tracking for the project instead, or use a more \
                 specific pattern",
                kind.as_str()
            )))
        }
        (true, Some(pattern)) => Ok(Some(pattern)),
        (false, Some(_)) => Err(ErrorTrackingError::Validation(format!(
            "an inbound filter of kind '{}' takes no pattern",
            kind.as_str()
        ))),
        (false, None) => Ok(None),
    }
}
