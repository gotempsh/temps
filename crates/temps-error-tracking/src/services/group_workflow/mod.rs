// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Issue workflow on error groups (ADR 074, D1): triage of many groups at
//! once. Every operation here is scoped to one project and runs as a bounded
//! number of statements; nothing iterates over groups one query at a time.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};

use super::error_crud_service::validate_group_status;
use super::types::ErrorTrackingError;

pub mod count_event;
pub mod ignore;
pub mod merge;

#[cfg(test)]
mod ignore_tests;
#[cfg(test)]
mod merge_tests;
#[cfg(test)]
mod tests;

pub use ignore::{IgnoreConditions, MAX_IGNORE_EVENT_COUNT, MAX_IGNORE_USER_COUNT};
pub use merge::{
    GroupFingerprint, MergeOutcome, SplitOutcome, UnmergeOutcome, MAX_MERGE_SOURCES,
    MAX_SPLIT_FINGERPRINTS,
};

/// Most groups one bulk request may touch. Larger selections report the rest
/// in `remaining`, so the caller pages through them.
pub const BULK_GROUP_LIMIT: usize = 1000;

/// Longest free-text filter accepted by a bulk selection.
pub const BULK_SEARCH_MAX_CHARS: usize = 256;

/// Status transition applied by a bulk request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkGroupAction {
    /// Mark the groups `resolved`.
    Resolve,
    /// Reopen the groups: `unresolved`.
    Unresolve,
    /// Mark the groups `ignored`.
    Ignore,
}

impl BulkGroupAction {
    /// The `error_groups.status` value this action writes.
    pub fn target_status(self) -> &'static str {
        match self {
            Self::Resolve => "resolved",
            Self::Unresolve => "unresolved",
            Self::Ignore => "ignored",
        }
    }
}

/// "Every group matching" selection; the same semantics as the list endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BulkGroupFilter {
    /// Only groups currently in this status.
    pub status: Option<String>,
    /// Only groups first seen in this environment.
    pub environment_id: Option<i32>,
    /// With `end_date`: only groups with at least one event in the window.
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    /// Case-insensitive substring of the title, error type or message.
    pub search: Option<String>,
}

/// Which groups a bulk request addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkGroupSelector {
    /// Explicit groups; every one must belong to the project.
    Ids(Vec<i32>),
    /// Every group matching the filter, oldest activity last, capped at
    /// [`BULK_GROUP_LIMIT`].
    Filter(BulkGroupFilter),
}

/// What a bulk request changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BulkGroupOutcome {
    /// Groups whose status changed, in no particular order.
    pub updated_ids: Vec<i32>,
    /// Selected groups already in the target status.
    pub skipped: u64,
    /// Matching groups left for a follow-up request because this one hit
    /// [`BULK_GROUP_LIMIT`]. Always 0 for an explicit id list.
    pub remaining: u64,
}

/// Issue workflow operations on error groups.
pub struct GroupWorkflowService {
    db: Arc<DatabaseConnection>,
}

/// SQL fragment and bound values selecting the groups a filter matches.
struct FilterSql {
    conditions: String,
    values: Vec<sea_orm::Value>,
}

impl GroupWorkflowService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Change the status of many groups of one project in one statement.
    ///
    /// An explicit id that is not a group of `project_id` fails the whole
    /// request with [`ErrorTrackingError::GroupsNotFound`] before anything is
    /// written. A filter never fails on an empty match: it reports zero
    /// updates. The caller evaluates status-change alert rules for
    /// `updated_ids` and audit-logs the request.
    pub async fn bulk_update_status(
        &self,
        project_id: i32,
        selector: BulkGroupSelector,
        action: BulkGroupAction,
        ignore: Option<IgnoreConditions>,
    ) -> Result<BulkGroupOutcome, ErrorTrackingError> {
        let target = action.target_status();
        IgnoreConditions::check_for_status(ignore.as_ref(), target, Utc::now())?;
        let write = StatusWrite::new(target, ignore.unwrap_or_default());
        let tx = self.db.begin().await?;
        let outcome = match selector {
            BulkGroupSelector::Ids(ids) => {
                self.bulk_update_by_ids(&tx, project_id, ids, &write)
                    .await?
            }
            BulkGroupSelector::Filter(filter) => {
                self.bulk_update_by_filter(&tx, project_id, filter, &write)
                    .await?
            }
        };
        // Users counted for an old ignore never carry over to a new state.
        count_event::clear_ignore_users_many(&tx, &outcome.updated_ids).await?;
        tx.commit().await?;
        Ok(outcome)
    }

    async fn bulk_update_by_ids<C: ConnectionTrait>(
        &self,
        tx: &C,
        project_id: i32,
        ids: Vec<i32>,
        write: &StatusWrite,
    ) -> Result<BulkGroupOutcome, ErrorTrackingError> {
        let mut ids = ids;
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Err(ErrorTrackingError::Validation(
                "group_ids must name at least one error group".into(),
            ));
        }
        if ids.len() > BULK_GROUP_LIMIT {
            return Err(ErrorTrackingError::Validation(format!(
                "group_ids names {} error groups; at most {BULK_GROUP_LIMIT} per request",
                ids.len()
            )));
        }
        let known = tx
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id FROM error_groups WHERE project_id = $1 AND id = ANY($2)",
                [project_id.into(), ids.clone().into()],
            ))
            .await?
            .into_iter()
            .map(|row| row.try_get::<i32>("", "id"))
            .collect::<Result<Vec<i32>, _>>()?;
        let missing: Vec<i32> = ids
            .iter()
            .copied()
            .filter(|id| !known.contains(id))
            .collect();
        if !missing.is_empty() {
            return Err(ErrorTrackingError::GroupsNotFound {
                project_id,
                group_ids: missing,
            });
        }
        let mut values: Vec<sea_orm::Value> = vec![project_id.into(), ids.clone().into()];
        let set = write.set_clause(&mut values);
        let selects = write.selects_clause("error_groups", &mut values);
        let updated_ids = tx
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE error_groups SET {set} \
                     WHERE project_id = $1 AND id = ANY($2) AND {selects} RETURNING id"
                ),
                values,
            ))
            .await?
            .into_iter()
            .map(|row| row.try_get::<i32>("", "id"))
            .collect::<Result<Vec<i32>, _>>()?;
        Ok(BulkGroupOutcome {
            skipped: (ids.len() - updated_ids.len()) as u64,
            updated_ids,
            remaining: 0,
        })
    }

    async fn bulk_update_by_filter<C: ConnectionTrait>(
        &self,
        tx: &C,
        project_id: i32,
        filter: BulkGroupFilter,
        write: &StatusWrite,
    ) -> Result<BulkGroupOutcome, ErrorTrackingError> {
        let FilterSql { conditions, values } = filter_sql(project_id, &filter, write)?;
        let mut update_values = values.clone();
        let set = write.set_clause(&mut update_values);
        update_values.push((BULK_GROUP_LIMIT as i64).into());
        let limit = update_values.len();
        let updated_ids = tx
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE error_groups SET {set} \
                     WHERE id IN (SELECT g.id FROM error_groups g WHERE {conditions} \
                                  ORDER BY g.last_seen DESC, g.id DESC LIMIT ${limit}) \
                     RETURNING id"
                ),
                update_values,
            ))
            .await?
            .into_iter()
            .map(|row| row.try_get::<i32>("", "id"))
            .collect::<Result<Vec<i32>, _>>()?;
        // Updated rows now carry the target status, so the same filter counts
        // exactly the groups a follow-up request would still change. A
        // re-ignore with conditions selects ignored groups too, so it reports
        // nothing remaining rather than counting the groups it just wrote.
        let remaining: i64 = if write.reapplies() {
            0
        } else {
            tx.query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("SELECT COUNT(*) AS remaining FROM error_groups g WHERE {conditions}"),
                values,
            ))
            .await?
            .ok_or_else(|| {
                ErrorTrackingError::Validation(
                    "bulk error group count returned no result".to_string(),
                )
            })?
            .try_get("", "remaining")?
        };
        Ok(BulkGroupOutcome {
            updated_ids,
            skipped: 0,
            remaining: remaining.max(0) as u64,
        })
    }
}

/// A status written by a triage request, with the ignore conditions that go
/// with it. Every status write sets the conditions: an `ignored` write
/// records them, any other write clears them, so a group never carries
/// conditions from an earlier ignore.
pub(crate) struct StatusWrite {
    status: &'static str,
    ignore: IgnoreConditions,
}

impl StatusWrite {
    pub(crate) fn new(status: &'static str, ignore: IgnoreConditions) -> Self {
        Self { status, ignore }
    }

    /// Whether a group already in the target status is written anyway: an
    /// ignore with conditions replaces the conditions of an ignored group.
    fn reapplies(&self) -> bool {
        self.status == "ignored" && !self.ignore.is_empty()
    }

    /// `SET` assignments, binding their values after those already in
    /// `values`. `ignore.event_count` is relative ("100 more times"), so it
    /// is stored as the `total_count` at which the group reopens.
    pub(crate) fn set_clause(&self, values: &mut Vec<sea_orm::Value>) -> String {
        values.push(self.status.to_owned().into());
        let status = values.len();
        values.push(self.ignore.until.into());
        let until = values.len();
        values.push(self.ignore.event_count.into());
        let events = values.len();
        values.push(self.ignore.user_count.into());
        let users = values.len();
        values.push(self.ignore.forever.into());
        let forever = values.len();
        format!(
            "status = ${status}, updated_at = NOW(), ignore_until = ${until}, \
             ignore_until_event_count = CASE WHEN ${events}::int IS NULL THEN NULL \
               ELSE LEAST(total_count::bigint + ${events}::int, 2147483647)::int END, \
             ignore_until_user_count = ${users}, ignore_user_count = 0, \
             ignore_forever = ${forever}"
        )
    }

    /// Condition selecting the rows this write changes, for `alias`.
    fn selects_clause(&self, alias: &str, values: &mut Vec<sea_orm::Value>) -> String {
        if self.reapplies() {
            return "TRUE".to_string();
        }
        values.push(self.status.to_owned().into());
        format!("{alias}.status <> ${}", values.len())
    }
}

/// Escape `%`, `_` and `\` so a user's text is matched literally by ILIKE.
pub(crate) fn like_pattern(search: &str) -> String {
    format!(
        "%{}%",
        search
            .trim()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

/// `$1` is the project; the filter's own values follow. Groups already in
/// the target status are not selected, unless the write re-applies ignore
/// conditions; merged-away groups never are.
fn filter_sql(
    project_id: i32,
    filter: &BulkGroupFilter,
    write: &StatusWrite,
) -> Result<FilterSql, ErrorTrackingError> {
    let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
    let mut conditions = vec![
        "g.project_id = $1".to_string(),
        "g.merged_into_group_id IS NULL".to_string(),
    ];
    conditions.push(write.selects_clause("g", &mut values));
    if let Some(status) = filter.status.as_deref() {
        validate_group_status(status)?;
        values.push(status.to_owned().into());
        conditions.push(format!("g.status = ${}", values.len()));
    }
    if let Some(environment_id) = filter.environment_id {
        values.push(environment_id.into());
        conditions.push(format!("g.environment_id = ${}", values.len()));
    }
    match (filter.start_date, filter.end_date) {
        (Some(start), Some(end)) => {
            if end < start {
                return Err(ErrorTrackingError::Validation(
                    "end_date must not be before start_date".into(),
                ));
            }
            values.push(start.into());
            let start_index = values.len();
            values.push(end.into());
            let end_index = values.len();
            let environment = filter
                .environment_id
                .map(|_| " AND e.environment_id = g.environment_id".to_string())
                .unwrap_or_default();
            // Always time-bounded: error_events is a hypertable.
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM error_events e WHERE e.error_group_id = g.id \
                 AND e.project_id = g.project_id AND e.timestamp >= ${start_index} \
                 AND e.timestamp <= ${end_index}{environment})"
            ));
        }
        (None, None) => {}
        _ => {
            return Err(ErrorTrackingError::Validation(
                "start_date and end_date must be supplied together".into(),
            ));
        }
    }
    if let Some(search) = filter.search.as_deref().filter(|s| !s.trim().is_empty()) {
        if search.chars().count() > BULK_SEARCH_MAX_CHARS {
            return Err(ErrorTrackingError::Validation(format!(
                "search is longer than {BULK_SEARCH_MAX_CHARS} characters"
            )));
        }
        values.push(like_pattern(search).into());
        let index = values.len();
        conditions.push(format!(
            "(g.title ILIKE ${index} OR g.error_type ILIKE ${index} \
             OR g.message_template ILIKE ${index})"
        ));
    }
    Ok(FilterSql {
        conditions: conditions.join(" AND "),
        values,
    })
}
