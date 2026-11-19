// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Merging and splitting error groups (ADR 074, D1.2).
//!
//! A merge repoints the fingerprint aliases ingest already reads, so future
//! events of a merged-away group land in the surviving group with no change
//! to the ingest path and no rewrite of stored events. Counts stay exact
//! because the surviving group adds the merged group's `total_count` and the
//! merged group keeps it, which is precisely what unmerge reverses. Neither
//! operation touches a group's status, so merging can never reopen an issue
//! or fire a regression alert.

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};

use super::super::types::ErrorTrackingError;
use super::GroupWorkflowService;

/// Groups one merge request may absorb at once.
pub const MAX_MERGE_SOURCES: usize = 50;

/// Fingerprints one split request may move at once.
pub const MAX_SPLIT_FINGERPRINTS: usize = 200;

/// Result of merging groups into one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeOutcome {
    /// The group that survived and now receives the events.
    pub target_group_id: i32,
    /// The groups that were merged away, in request order.
    pub merged_group_ids: Vec<i32>,
    /// Fingerprint aliases repointed at the surviving group.
    pub moved_fingerprints: u64,
    /// Events the surviving group now accounts for.
    pub total_count: i64,
}

/// Result of taking one group back out of a merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmergeOutcome {
    pub group_id: i32,
    /// The group it was merged into.
    pub target_group_id: i32,
    /// Fingerprint aliases moved back.
    pub moved_fingerprints: u64,
}

/// Result of splitting fingerprints out of a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitOutcome {
    /// The group the fingerprints were taken out of.
    pub group_id: i32,
    /// The group created for them; future events land here.
    pub new_group_id: i32,
    /// Fingerprint aliases moved to the new group.
    pub moved_fingerprints: u64,
}

/// One fingerprint of a group, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupFingerprint {
    pub fingerprint_hash: String,
    /// The group this alias was moved away from by a merge, if any. Such an
    /// alias belongs to that merge: unmerge moves it back, a split cannot
    /// take it.
    pub merged_from_group_id: Option<i32>,
}

/// A group row as the workflow needs it: identity, counts and merge state.
#[derive(Debug, Clone)]
struct GroupRow {
    id: i32,
    title: String,
    error_type: String,
    message_template: Option<String>,
    total_count: i32,
    environment_id: Option<i32>,
    merged_into_group_id: Option<i32>,
}

impl GroupWorkflowService {
    /// Merge `source_group_ids` into `target_group_id`.
    ///
    /// Every id must be a group of `project_id`. A group that is already
    /// merged away cannot be merged or be a target, and a group that has
    /// absorbed others cannot itself be merged away until those are taken
    /// back out, which keeps every alias one step from its original group and
    /// makes unmerge unambiguous.
    pub async fn merge_groups(
        &self,
        project_id: i32,
        target_group_id: i32,
        source_group_ids: Vec<i32>,
    ) -> Result<MergeOutcome, ErrorTrackingError> {
        let mut sources = source_group_ids.clone();
        sources.sort_unstable();
        sources.dedup();
        if sources.is_empty() {
            return Err(ErrorTrackingError::Validation(
                "source_group_ids must name at least one error group to merge".into(),
            ));
        }
        if sources.len() > MAX_MERGE_SOURCES {
            return Err(ErrorTrackingError::Validation(format!(
                "source_group_ids names {} error groups; at most {MAX_MERGE_SOURCES} per request",
                sources.len()
            )));
        }
        if sources.contains(&target_group_id) {
            return Err(ErrorTrackingError::Validation(format!(
                "error group {target_group_id} cannot be merged into itself"
            )));
        }

        let tx = self.db.begin().await?;
        // One ordered lock over every group involved: two concurrent merges
        // touching the same groups serialize instead of interleaving their
        // count arithmetic.
        let mut locked_ids = sources.clone();
        locked_ids.push(target_group_id);
        locked_ids.sort_unstable();
        let rows = load_groups_for_update(&tx, project_id, &locked_ids).await?;
        let missing: Vec<i32> = locked_ids
            .iter()
            .copied()
            .filter(|id| !rows.iter().any(|row| row.id == *id))
            .collect();
        if !missing.is_empty() {
            return Err(ErrorTrackingError::GroupsNotFound {
                project_id,
                group_ids: missing,
            });
        }

        let target = rows
            .iter()
            .find(|row| row.id == target_group_id)
            .ok_or(ErrorTrackingError::GroupNotFound)?;
        if let Some(into) = target.merged_into_group_id {
            return Err(ErrorTrackingError::GroupAlreadyMerged {
                group_id: target.id,
                merged_into_group_id: into,
            });
        }
        for id in &sources {
            let source = rows
                .iter()
                .find(|row| row.id == *id)
                .ok_or(ErrorTrackingError::GroupNotFound)?;
            if let Some(into) = source.merged_into_group_id {
                return Err(ErrorTrackingError::GroupAlreadyMerged {
                    group_id: source.id,
                    merged_into_group_id: into,
                });
            }
            if is_merge_target(&tx, project_id, source.id).await? {
                return Err(ErrorTrackingError::GroupIsMergeTarget {
                    group_id: source.id,
                });
            }
        }

        let moved_fingerprints = tx
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_group_fingerprints SET merged_from_group_id = error_group_id, \
                 error_group_id = $3 WHERE project_id = $1 AND error_group_id = ANY($2)",
                [
                    project_id.into(),
                    sources.clone().into(),
                    target_group_id.into(),
                ],
            ))
            .await?
            .rows_affected();

        let moved_count: i64 = rows
            .iter()
            .filter(|row| sources.contains(&row.id))
            .map(|row| i64::from(row.total_count))
            .sum();
        // The merged-away rows keep their own counts and events, so the sum
        // the surviving group shows is exact and unmerge is a subtraction.
        // `error_groups.total_count` is an INT4 column; the sum is computed
        // in bigint so a large merge cannot overflow mid-expression.
        let total_count: i32 = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_groups target SET \
                 total_count = target.total_count + $2::bigint, \
                 first_seen = LEAST(target.first_seen, merged.first_seen), \
                 last_seen = GREATEST(target.last_seen, merged.last_seen), \
                 updated_at = $4 \
                 FROM (SELECT MIN(first_seen) AS first_seen, MAX(last_seen) AS last_seen \
                       FROM error_groups WHERE id = ANY($3)) merged \
                 WHERE target.id = $1 RETURNING target.total_count",
                [
                    target_group_id.into(),
                    moved_count.into(),
                    sources.clone().into(),
                    Utc::now().into(),
                ],
            ))
            .await?
            .ok_or(ErrorTrackingError::GroupNotFound)?
            .try_get("", "total_count")?;

        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_groups SET merged_into_group_id = $2, updated_at = $3 \
             WHERE project_id = $4 AND id = ANY($1)",
            [
                sources.clone().into(),
                target_group_id.into(),
                Utc::now().into(),
                project_id.into(),
            ],
        ))
        .await?;
        tx.commit().await?;

        Ok(MergeOutcome {
            target_group_id,
            merged_group_ids: source_group_ids
                .into_iter()
                .filter(|id| sources.contains(id))
                .collect(),
            moved_fingerprints,
            total_count: i64::from(total_count),
        })
    }

    /// Take `group_id` back out of the merge it is part of.
    ///
    /// Its fingerprints go back to it, so its future events do too, and the
    /// count it contributed is subtracted from the group it was merged into.
    /// Events ingested while it was merged stay where they were stored, in
    /// the surviving group, together with the count that covers them.
    pub async fn unmerge_group(
        &self,
        project_id: i32,
        group_id: i32,
    ) -> Result<UnmergeOutcome, ErrorTrackingError> {
        let tx = self.db.begin().await?;
        let rows = load_groups_for_update(&tx, project_id, &[group_id]).await?;
        let group = rows
            .first()
            .ok_or(ErrorTrackingError::GroupNotFound)?
            .clone();
        let Some(target_group_id) = group.merged_into_group_id else {
            return Err(ErrorTrackingError::GroupNotMerged { group_id });
        };

        let moved_fingerprints = tx
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_group_fingerprints SET error_group_id = merged_from_group_id, \
                 merged_from_group_id = NULL \
                 WHERE project_id = $1 AND merged_from_group_id = $2",
                [project_id.into(), group_id.into()],
            ))
            .await?
            .rows_affected();
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_groups SET total_count = GREATEST(total_count - $2, 0), \
             updated_at = $3 WHERE id = $1",
            [
                target_group_id.into(),
                group.total_count.into(),
                Utc::now().into(),
            ],
        ))
        .await?;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_groups SET merged_into_group_id = NULL, updated_at = $2 WHERE id = $1",
            [group_id.into(), Utc::now().into()],
        ))
        .await?;
        tx.commit().await?;

        Ok(UnmergeOutcome {
            group_id,
            target_group_id,
            moved_fingerprints,
        })
    }

    /// Move `fingerprints` out of `group_id` into a new group.
    ///
    /// The new group receives the events those fingerprints produce from now
    /// on. Stored events keep the group they were ingested into, so nothing
    /// moves between hypertable rows and no count is invented: the new group
    /// starts at zero events and the original keeps its history.
    pub async fn split_group(
        &self,
        project_id: i32,
        group_id: i32,
        fingerprints: Vec<String>,
        title: Option<String>,
    ) -> Result<SplitOutcome, ErrorTrackingError> {
        let mut wanted = fingerprints;
        wanted.sort();
        wanted.dedup();
        if wanted.is_empty() {
            return Err(ErrorTrackingError::Validation(
                "fingerprints must name at least one fingerprint to split out".into(),
            ));
        }
        if wanted.len() > MAX_SPLIT_FINGERPRINTS {
            return Err(ErrorTrackingError::Validation(format!(
                "fingerprints names {} entries; at most {MAX_SPLIT_FINGERPRINTS} per request",
                wanted.len()
            )));
        }
        if let Some(title) = title.as_deref() {
            if title.trim().is_empty() || title.chars().count() > 500 {
                return Err(ErrorTrackingError::Validation(
                    "title must be between 1 and 500 characters".into(),
                ));
            }
        }

        let tx = self.db.begin().await?;
        let rows = load_groups_for_update(&tx, project_id, &[group_id]).await?;
        let group = rows
            .first()
            .ok_or(ErrorTrackingError::GroupNotFound)?
            .clone();
        if let Some(into) = group.merged_into_group_id {
            return Err(ErrorTrackingError::GroupAlreadyMerged {
                group_id,
                merged_into_group_id: into,
            });
        }

        // Only aliases that belong to this group and are not part of a merge
        // can be split; a merged-in alias is the merge's to move back.
        let owned: Vec<String> = tx
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT fingerprint_hash FROM error_group_fingerprints \
                 WHERE project_id = $1 AND error_group_id = $2 AND merged_from_group_id IS NULL \
                 FOR UPDATE",
                [project_id.into(), group_id.into()],
            ))
            .await?
            .into_iter()
            .map(|row| row.try_get::<String>("", "fingerprint_hash"))
            .collect::<Result<Vec<String>, _>>()?;
        let unknown: Vec<String> = wanted
            .iter()
            .filter(|hash| !owned.contains(hash))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(ErrorTrackingError::FingerprintsNotInGroup {
                group_id,
                fingerprints: unknown,
            });
        }
        if wanted.len() == owned.len() {
            return Err(ErrorTrackingError::Validation(format!(
                "splitting every fingerprint out of error group {group_id} would leave it empty; \
                 keep at least one"
            )));
        }

        let now = Utc::now();
        let new_group_id: i32 = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO error_groups (title, error_type, message_template, first_seen, \
                 last_seen, total_count, status, project_id, environment_id, created_at, \
                 updated_at) VALUES ($1, $2, $3, $4, $4, 0, 'unresolved', $5, $6, $4, $4) \
                 RETURNING id",
                [
                    title
                        .unwrap_or_else(|| format!("{} (split)", group.title))
                        .into(),
                    group.error_type.clone().into(),
                    group.message_template.clone().into(),
                    now.into(),
                    project_id.into(),
                    group.environment_id.into(),
                ],
            ))
            .await?
            .ok_or_else(|| {
                ErrorTrackingError::Validation(
                    "creating the split error group returned no id".into(),
                )
            })?
            .try_get("", "id")?;

        let moved_fingerprints = tx
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_group_fingerprints SET error_group_id = $3 \
                 WHERE project_id = $1 AND error_group_id = $2 AND fingerprint_hash = ANY($4) \
                 AND merged_from_group_id IS NULL",
                [
                    project_id.into(),
                    group_id.into(),
                    new_group_id.into(),
                    wanted.into(),
                ],
            ))
            .await?
            .rows_affected();
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_groups SET updated_at = $2 WHERE id = $1",
            [group_id.into(), now.into()],
        ))
        .await?;
        tx.commit().await?;

        Ok(SplitOutcome {
            group_id,
            new_group_id,
            moved_fingerprints,
        })
    }

    /// The fingerprints routing to a group, so a caller can choose which to
    /// split out. Bounded by the group's own alias count.
    pub async fn list_group_fingerprints(
        &self,
        project_id: i32,
        group_id: i32,
    ) -> Result<Vec<GroupFingerprint>, ErrorTrackingError> {
        let exists = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT 1 AS present FROM error_groups WHERE id = $1 AND project_id = $2",
                [group_id.into(), project_id.into()],
            ))
            .await?;
        if exists.is_none() {
            return Err(ErrorTrackingError::GroupNotFound);
        }
        self.db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT fingerprint_hash, merged_from_group_id FROM error_group_fingerprints \
                 WHERE project_id = $1 AND error_group_id = $2 \
                 ORDER BY merged_from_group_id NULLS FIRST, fingerprint_hash",
                [project_id.into(), group_id.into()],
            ))
            .await?
            .into_iter()
            .map(|row| {
                Ok(GroupFingerprint {
                    fingerprint_hash: row.try_get("", "fingerprint_hash")?,
                    merged_from_group_id: row.try_get("", "merged_from_group_id")?,
                })
            })
            .collect()
    }

    /// Groups merged into `group_id`, so its detail view can link to them.
    pub async fn merged_group_ids(
        &self,
        project_id: i32,
        group_id: i32,
    ) -> Result<Vec<i32>, ErrorTrackingError> {
        self.db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id FROM error_groups WHERE project_id = $1 AND merged_into_group_id = $2 \
                 ORDER BY id",
                [project_id.into(), group_id.into()],
            ))
            .await?
            .into_iter()
            .map(|row| row.try_get::<i32>("", "id").map_err(Into::into))
            .collect()
    }
}

/// Load the named groups of one project, locking the rows for the rest of the
/// transaction. Ordered by id so concurrent requests cannot deadlock.
async fn load_groups_for_update<C: ConnectionTrait>(
    tx: &C,
    project_id: i32,
    group_ids: &[i32],
) -> Result<Vec<GroupRow>, ErrorTrackingError> {
    tx.query_all(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id, title, error_type, message_template, total_count, environment_id, \
         merged_into_group_id FROM error_groups WHERE project_id = $1 AND id = ANY($2) \
         ORDER BY id FOR UPDATE",
        [project_id.into(), group_ids.to_vec().into()],
    ))
    .await?
    .into_iter()
    .map(|row| {
        Ok(GroupRow {
            id: row.try_get("", "id")?,
            title: row.try_get("", "title")?,
            error_type: row.try_get("", "error_type")?,
            message_template: row.try_get("", "message_template")?,
            total_count: row.try_get("", "total_count")?,
            environment_id: row.try_get("", "environment_id")?,
            merged_into_group_id: row.try_get("", "merged_into_group_id")?,
        })
    })
    .collect()
}

/// Whether any alias was merged into this group, i.e. it absorbed another.
async fn is_merge_target<C: ConnectionTrait>(
    tx: &C,
    project_id: i32,
    group_id: i32,
) -> Result<bool, ErrorTrackingError> {
    Ok(tx
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT 1 AS present FROM error_groups \
             WHERE project_id = $1 AND merged_into_group_id = $2 LIMIT 1",
            [project_id.into(), group_id.into()],
        ))
        .await?
        .is_some())
}
