// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "Resolve in the next release" (ADR 074, D1.5): ordered releases per
//! project, and the release a group is resolved after.
//!
//! A release is ordered by `released_at`: the creation time of the first
//! Temps deployment that carried it (a git deployment's release is its commit
//! SHA, ADR 033), or the time it was first seen when no deployment matches.
//! Ingest registers each release once per process; a bounded in-memory set of
//! releases already registered keeps that off the per-event path.

use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

use super::super::types::ErrorTrackingError;

/// Longest release string Temps orders. Sentry caps a release at 200
/// characters; a longer one is stored on the event but never registered, so it
/// cannot reopen a group resolved in the next release.
pub const MAX_RELEASE_CHARS: usize = 250;

/// Releases remembered as registered, across all projects of this process.
const KNOWN_RELEASES_CAPACITY: u64 = 20_000;

/// Recent deployments whose commit SHA is registered as a release when a group
/// is resolved in the next release, so a release that is deployed but has not
/// reported an event yet still counts as the current one.
const DEPLOYMENTS_SYNCED_ON_RESOLVE: i64 = 20;

/// Registers the releases ingest sees.
pub struct ReleaseRegistry {
    db: Arc<DatabaseConnection>,
    known: Cache<(i32, String), ()>,
}

impl ReleaseRegistry {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            known: Cache::builder()
                .max_capacity(KNOWN_RELEASES_CAPACITY)
                .time_to_idle(Duration::from_secs(6 * 3600))
                .build(),
        }
    }

    /// Record `version` for `project_id` if it is new. One statement on the
    /// first sight of a release in this process, nothing afterwards. Called
    /// outside the ingest transaction, so a rolled-back event cannot leave
    /// the set claiming a release that was never written.
    pub async fn observe(
        &self,
        project_id: i32,
        version: Option<&str>,
        deployment_id: Option<i32>,
    ) -> Result<(), ErrorTrackingError> {
        let Some(version) = version.map(str::trim).filter(|v| valid_release(v)) else {
            return Ok(());
        };
        let key = (project_id, version.to_owned());
        if self.known.contains_key(&key) {
            return Ok(());
        }
        register_release(self.db.as_ref(), project_id, version, deployment_id).await?;
        self.known.insert(key, ()).await;
        Ok(())
    }
}

fn valid_release(version: &str) -> bool {
    !version.is_empty() && version.chars().count() <= MAX_RELEASE_CHARS && !version.contains('\0')
}

/// Insert a release unless it is known. `released_at` is the creation time of
/// the first deployment of the project with this commit SHA, else now; the
/// event's own deployment is only a link, never an order, because a DSN can
/// outlive the deployment it was issued for.
pub(crate) async fn register_release<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
    version: &str,
    deployment_id: Option<i32>,
) -> Result<(), ErrorTrackingError> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO error_releases (project_id, version, released_at, first_seen_at, deployment_id) \
         SELECT $1, $2, COALESCE(d.created_at, NOW()), NOW(), \
                COALESCE(d.id, (SELECT id FROM deployments WHERE id = $3 AND project_id = $1)) \
         FROM (SELECT 1) one \
         LEFT JOIN LATERAL ( \
             SELECT id, created_at FROM deployments \
             WHERE project_id = $1 AND commit_sha = $2 \
             ORDER BY created_at, id LIMIT 1) d ON TRUE \
         ON CONFLICT (project_id, version) DO NOTHING",
        [project_id.into(), version.into(), deployment_id.into()],
    ))
    .await?;
    Ok(())
}

/// The newest release of a project, after registering the commit SHAs of its
/// recent deployments that became ready. A group resolved in the next release
/// stays resolved for events from this release and older ones.
///
/// Fails with a validation error when the project has no release at all:
/// there is nothing for "the next release" to come after.
pub(crate) async fn current_release<C: ConnectionTrait>(
    tx: &C,
    project_id: i32,
) -> Result<CurrentRelease, ErrorTrackingError> {
    tx.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO error_releases (project_id, version, released_at, first_seen_at, deployment_id) \
         SELECT DISTINCT ON (d.commit_sha) d.project_id, d.commit_sha, d.created_at, NOW(), d.id \
         FROM (SELECT project_id, commit_sha, created_at, id FROM deployments \
               WHERE project_id = $1 AND commit_sha IS NOT NULL AND commit_sha <> '' \
                 AND ready_at IS NOT NULL \
               ORDER BY created_at DESC, id DESC LIMIT $2) d \
         ORDER BY d.commit_sha, d.created_at, d.id \
         ON CONFLICT (project_id, version) DO NOTHING",
        [project_id.into(), DEPLOYMENTS_SYNCED_ON_RESOLVE.into()],
    ))
    .await?;
    let row = tx
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, version FROM error_releases WHERE project_id = $1 \
             ORDER BY released_at DESC, id DESC LIMIT 1",
            [project_id.into()],
        ))
        .await?
        .ok_or_else(|| {
            ErrorTrackingError::Validation(format!(
                "project {project_id} has no release yet, so an error group cannot be \
                 resolved in the next release; resolve it instead, or send events with a release"
            ))
        })?;
    Ok(CurrentRelease {
        id: row.try_get("", "id")?,
        version: row.try_get("", "version")?,
    })
}

/// The release a group is resolved after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentRelease {
    pub id: i32,
    pub version: String,
}

/// Version strings of releases by id, for display.
pub(crate) async fn release_versions<C: ConnectionTrait>(
    db: &C,
    ids: &[i32],
) -> Result<std::collections::HashMap<i32, String>, ErrorTrackingError> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, version FROM error_releases WHERE id = ANY($1)",
            [ids.to_vec().into()],
        ))
        .await?;
    rows.into_iter()
        .map(|row| Ok((row.try_get("", "id")?, row.try_get("", "version")?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_strings_are_bounded() {
        assert!(valid_release("v1.2.3"));
        assert!(valid_release(&"a".repeat(MAX_RELEASE_CHARS)));
        assert!(!valid_release(""));
        assert!(!valid_release(&"a".repeat(MAX_RELEASE_CHARS + 1)));
        assert!(!valid_release("v1\0"));
    }
}
