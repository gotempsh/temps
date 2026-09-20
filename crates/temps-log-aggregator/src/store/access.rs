// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resolves a caller into a [`LogAccessScope`] allow-list (ADR-045 §7).
//!
//! # Why an allow-list
//!
//! The retired scan engine inlined authorization into its candidate SQL as a
//! **deny**-list: `NOT (p.id = ANY($hidden::int[]))`, plus a bound-project
//! narrowing and an `EXISTS` over `project_services`. That shape fails *open*.
//! If `hidden_projects` came back empty for the wrong reason — a checker that
//! was not registered, a query that errored and was `unwrap_or_default()`ed, a
//! future refactor that forgot a branch — the predicate degenerates to "show
//! everything", and nothing downstream can tell the difference.
//!
//! It also cannot survive a second storage backend: a store that cannot join
//! `projects`/`project_services` has no way to *verify* a deny-list. So the
//! decision is made once, here, in Postgres, and handed to the store as an
//! explicit set of ids.
//!
//! # Invariants this module upholds
//!
//! 1. **Allow-list, never deny-list.** The store is told what the caller *may*
//!    read. An empty allow-list matches nothing.
//! 2. **Failure is an error.** Any infrastructure failure while resolving —
//!    the access checker erroring, the project query erroring, a session with
//!    no user id — returns `Err`. It is never downgraded to "no restrictions"
//!    and never to a silent empty page attributed to "no logs found".
//! 3. **Admin is its own variant.** Instance administrators resolve to
//!    [`LogAccessScope::All`], so "administrator" and "resolution produced
//!    nothing" can never be confused for one another.

use std::sync::Arc;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};
use temps_auth::AuthContext;
use temps_core::ProjectAccessChecker;
use thiserror::Error;

use super::LogAccessScope;

/// Why a caller could not be resolved to a log-access allow-list.
///
/// Every variant is a refusal. None of them has a "carry on unrestricted"
/// interpretation — that is the entire point of the type.
#[derive(Debug, Error)]
pub enum LogAccessError {
    #[error("Could not resolve log access: the project access checker failed for user {user_id}: {reason}")]
    CheckerFailed { user_id: i32, reason: String },

    #[error("Could not resolve log access: enumerating authorized projects failed: {reason}")]
    ProjectLookupFailed { reason: String },

    #[error("Could not resolve log access: enumerating authorized services failed: {reason}")]
    ServiceLookupFailed { reason: String },

    #[error("Could not resolve log access: the caller has no user identity and no bound project")]
    NoIdentity,

    #[error("Could not resolve log access: deployment token {token_project:?} is not bound to a project")]
    UnboundDeploymentToken { token_project: Option<i32> },
}

/// Resolve the caller's log-access allow-list.
///
/// Resolution order — each branch is exhaustive and terminal:
///
/// 1. **Deployment token** → exactly its bound project, plus the external
///    services linked to that project. A token with no bound project is an
///    error, not "everything".
/// 2. **Instance administrator** → [`LogAccessScope::All`].
/// 3. **Everyone else** → every project the caller is not denied, plus the
///    external services reachable from those projects (and any standalone
///    service they created themselves, matching the database-access policy
///    the per-resource guards already apply).
pub async fn resolve_log_access_scope(
    db: &DatabaseConnection,
    auth: &AuthContext,
    checker: Option<&Arc<dyn ProjectAccessChecker>>,
) -> Result<LogAccessScope, LogAccessError> {
    // 1. Deployment tokens are bound to exactly one project.
    if auth.is_deployment_token() {
        let Some(project_id) = auth.project_id() else {
            return Err(LogAccessError::UnboundDeploymentToken {
                token_project: None,
            });
        };
        let external_service_ids = services_linked_to_projects(db, &[project_id]).await?;
        return Ok(LogAccessScope::Allowed {
            project_ids: vec![project_id],
            external_service_ids,
        });
    }

    // 2. Instance administrators are never restricted by team membership.
    //    Expressed as its own variant, not as "an allow-list of everything",
    //    so the two can never be confused downstream.
    if auth.is_instance_admin() {
        return Ok(LogAccessScope::All);
    }

    // 3. Everyone else: enumerate, then subtract what the checker denies.
    let all_project_ids = all_project_ids(db).await?;

    let project_ids = match checker {
        // No checker registered: OSS instances have no team-scoped access
        // model, so every project is readable. This is still returned as an
        // explicit allow-list rather than `All`, so the set is auditable and
        // an admin is still distinguishable from an ordinary user.
        None => all_project_ids,
        Some(checker) => {
            let Some(user_id) = auth.user_id_opt() else {
                return Err(LogAccessError::NoIdentity);
            };
            match checker.hidden_project_ids(user_id).await {
                // The checker has no opinion — nothing is hidden.
                Ok(None) => all_project_ids,
                Ok(Some(hidden)) => {
                    let hidden: std::collections::HashSet<i32> = hidden.into_iter().collect();
                    all_project_ids
                        .into_iter()
                        .filter(|id| !hidden.contains(id))
                        .collect()
                }
                // Fail closed, loudly. Never `unwrap_or_default()`.
                Err(error) => {
                    return Err(LogAccessError::CheckerFailed {
                        user_id,
                        reason: error.to_string(),
                    })
                }
            }
        }
    };

    let mut external_service_ids = services_linked_to_projects(db, &project_ids).await?;

    // Standalone services (linked to no project) stay readable by their
    // creator — the same carve-out `guard_external_service_access` applies to
    // the per-service endpoints, kept in step here so global search and the
    // per-service view agree on what a creator can see.
    if let Some(user_id) = auth.user_id_opt() {
        external_service_ids.extend(unlinked_services_created_by(db, user_id).await?);
    }
    external_service_ids.sort_unstable();
    external_service_ids.dedup();

    Ok(LogAccessScope::Allowed {
        project_ids,
        external_service_ids,
    })
}

/// Every project id on the instance.
///
/// The allow-list direction requires enumeration; the set is one `int` per
/// project, which is negligible next to the log volume it guards.
async fn all_project_ids(db: &DatabaseConnection) -> Result<Vec<i32>, LogAccessError> {
    temps_entities::projects::Entity::find()
        .filter(temps_entities::projects::Column::DeletedAt.is_null())
        .select_only()
        .column(temps_entities::projects::Column::Id)
        .into_tuple::<i32>()
        .all(db)
        .await
        .map_err(|e| LogAccessError::ProjectLookupFailed {
            reason: e.to_string(),
        })
}

/// External services reachable from any of `project_ids` via `project_services`.
async fn services_linked_to_projects(
    db: &DatabaseConnection,
    project_ids: &[i32],
) -> Result<Vec<i32>, LogAccessError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = temps_entities::project_services::Entity::find()
        .filter(temps_entities::project_services::Column::ProjectId.is_in(project_ids.to_vec()))
        .select_only()
        .column(temps_entities::project_services::Column::ServiceId)
        .into_tuple::<i32>()
        .all(db)
        .await
        .map_err(|e| LogAccessError::ServiceLookupFailed {
            reason: e.to_string(),
        })?;
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Standalone services (no `project_services` row) created by `user_id`.
async fn unlinked_services_created_by(
    db: &DatabaseConnection,
    user_id: i32,
) -> Result<Vec<i32>, LogAccessError> {
    let created: Vec<i32> = temps_entities::external_services::Entity::find()
        .filter(temps_entities::external_services::Column::CreatedByUserId.eq(user_id))
        .select_only()
        .column(temps_entities::external_services::Column::Id)
        .into_tuple::<i32>()
        .all(db)
        .await
        .map_err(|e| LogAccessError::ServiceLookupFailed {
            reason: e.to_string(),
        })?;
    if created.is_empty() {
        return Ok(Vec::new());
    }
    let linked: Vec<i32> = temps_entities::project_services::Entity::find()
        .filter(temps_entities::project_services::Column::ServiceId.is_in(created.clone()))
        .select_only()
        .column(temps_entities::project_services::Column::ServiceId)
        .into_tuple::<i32>()
        .all(db)
        .await
        .map_err(|e| LogAccessError::ServiceLookupFailed {
            reason: e.to_string(),
        })?;
    let linked: std::collections::HashSet<i32> = linked.into_iter().collect();
    Ok(created
        .into_iter()
        .filter(|id| !linked.contains(id))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_resolution_failure_is_terminal() {
        // A compile-time-ish guard on the design: the error type carries no
        // variant that a caller could reasonably interpret as "carry on".
        for error in [
            LogAccessError::CheckerFailed {
                user_id: 7,
                reason: "connection reset".into(),
            },
            LogAccessError::ProjectLookupFailed {
                reason: "timeout".into(),
            },
            LogAccessError::ServiceLookupFailed {
                reason: "timeout".into(),
            },
            LogAccessError::NoIdentity,
            LogAccessError::UnboundDeploymentToken {
                token_project: None,
            },
        ] {
            let message = error.to_string();
            assert!(
                message.starts_with("Could not resolve log access"),
                "every variant must read as a refusal: {message}"
            );
        }
    }

    #[test]
    fn checker_failure_error_carries_the_user_and_reason() {
        let error = LogAccessError::CheckerFailed {
            user_id: 42,
            reason: "pool exhausted".into(),
        };
        let message = error.to_string();
        assert!(message.contains("42"), "{message}");
        assert!(message.contains("pool exhausted"), "{message}");
    }
}
