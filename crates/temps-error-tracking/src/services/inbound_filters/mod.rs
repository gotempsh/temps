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
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
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
        let existing = error_inbound_filters::Entity::find()
            .filter(error_inbound_filters::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
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
                .count(self.db.as_ref())
                .await?;
            if duplicate > 0 {
                return Err(ErrorTrackingError::InboundFilterExists {
                    project_id,
                    kind: kind.as_str().to_string(),
                });
            }
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
        .insert(self.db.as_ref())
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
        let row = self.find(project_id, filter_id).await?;
        let kind = parse_kind(&row)?;
        let mut active: error_inbound_filters::ActiveModel = row.into();
        if pattern.is_some() {
            active.pattern = Set(validate_pattern(kind, pattern)?);
        }
        if let Some(enabled) = enabled {
            active.enabled = Set(enabled);
        }
        active.updated_at = Set(Utc::now());
        let row = active.update(self.db.as_ref()).await?;
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

    async fn find(
        &self,
        project_id: i32,
        filter_id: i32,
    ) -> Result<error_inbound_filters::Model, ErrorTrackingError> {
        error_inbound_filters::Entity::find_by_id(filter_id)
            .filter(error_inbound_filters::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
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
        (true, Some(pattern)) => Ok(Some(pattern)),
        (false, Some(_)) => Err(ErrorTrackingError::Validation(format!(
            "an inbound filter of kind '{}' takes no pattern",
            kind.as_str()
        ))),
        (false, None) => Ok(None),
    }
}
