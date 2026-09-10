// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared service-level authorization for the backup/restore/upgrade handlers.
//!
//! `RequireAuth` proves a caller is authenticated and `permission_guard!`
//! proves their role holds a permission, but plain `Role::User` holds most
//! backup/restore/upgrade permissions **instance-wide** — there is no project
//! qualifier on the role itself. The thing that actually confines a caller to
//! only the projects they belong to is [`temps_core::ProjectAccessChecker`],
//! an optional extension point that is `None` in plain OSS (a no-op there,
//! since there is no team boundary yet) and gets registered by the Teams
//! plugin in EE. Every handler here that is keyed by an `external_services`
//! id must additionally call [`require_service_access`] before touching the
//! target resource, or it is a cross-tenant IDOR the moment Teams is
//! installed.
//!
//! Originally written for the restore endpoints (a restore reads one
//! service's data and writes it into another — both halves are privileged),
//! but the same gap exists anywhere a handler is keyed by a bare
//! `service_id`/`schedule_id` path or body parameter, so this module is
//! shared by `restore_handler`, `backup_handler` and `pg_upgrade_handler`.

use axum::http::StatusCode;
use std::collections::{BTreeMap, BTreeSet};
use temps_auth::Permission;
use temps_core::problemdetails::{self, Problem};
use tracing::error;

use crate::handlers::types::BackupAppState;
use crate::services::{
    BackupAccessScope, BackupCollectionAccessScope, BackupError, BackupScheduleAccessScope,
    BackupWithAccessScope, ServiceProjectScope,
};

/// A deployment token is minted for exactly one project, so it may only reach
/// services linked to that project. Pure so the rule is testable without a
/// database; a service linked to no project is reachable by no token.
pub(crate) fn deployment_token_may_access(token_project_id: i32, project_ids: &[i32]) -> bool {
    project_ids.contains(&token_project_id)
}

fn caller_is_instance_admin(auth: &temps_auth::AuthContext) -> bool {
    auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin)
}

fn access_denied(what: &str, id: i32, operation: &str) -> Problem {
    problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Insufficient Permissions")
        .with_detail(format!(
            "You do not have access to the {} ({}) involved in this {}",
            what, id, operation
        ))
}

/// Deny unless the caller may act on `service_id`.
///
/// * Instance-wide Admin/PlatformAdmin bypass, matching the documented
///   contract of [`temps_core::ProjectAccessChecker`].
/// * A deployment token is confined to its own project — enforced here even
///   when no checker is registered, since it needs no external policy.
/// * Otherwise, if a checker is registered, the caller must be able to reach
///   at least one project the service is linked to. With no checker (plain
///   OSS) this is a no-op, which is the documented fail-open-when-unconfigured
///   behaviour of the extension point.
///
/// `what` names the resource in the denial message (e.g. `"target service"`,
/// `"external service"`); `operation` names what the caller was attempting
/// (e.g. `"restore"`, `"backup"`, `"PostgreSQL upgrade"`), so the same
/// message shape reads naturally from every call site.
pub(crate) async fn require_service_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    service_id: i32,
    required_permission: Permission,
    what: &str,
    operation: &str,
) -> Result<(), Problem> {
    require_services_access(
        app_state,
        auth,
        &[service_id],
        required_permission,
        what,
        operation,
    )
    .await
    .map(|_| ())
}

/// Authorize many services using one service-layer project-resolution query.
/// The returned scopes are used by schedule attachment to validate that a
/// project-scoped schedule is not expanded into unrelated projects.
pub(crate) async fn require_services_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    service_ids: &[i32],
    required_permission: Permission,
    what: &str,
    operation: &str,
) -> Result<Vec<ServiceProjectScope>, Problem> {
    if caller_is_instance_admin(auth) {
        return Ok(Vec::new());
    }

    let token_project_id = auth.project_id();
    if token_project_id.is_none() && app_state.project_access_checker.is_none() {
        // Plain OSS deliberately has no team boundary yet.
        return Ok(Vec::new());
    }

    let scopes = app_state
        .backup_service
        .project_scopes_for_services(service_ids)
        .await
        .map_err(Problem::from)?;

    if let Some(project_id) = token_project_id {
        for scope in &scopes {
            if !deployment_token_may_access(project_id, &scope.project_ids) {
                return Err(access_denied(what, scope.service_id, operation));
            }
        }
        return Ok(scopes);
    }

    let checker = app_state
        .project_access_checker
        .as_deref()
        .ok_or_else(|| authorization_configuration_error(what, operation))?;
    let project_access = project_permission_decisions(
        checker,
        auth.user_id(),
        scopes
            .iter()
            .flat_map(|scope| scope.project_ids.iter().copied()),
        required_permission,
        what,
        operation,
    )
    .await?;

    for scope in &scopes {
        let granted = scope
            .project_ids
            .iter()
            .any(|project_id| project_access.get(project_id) == Some(&true));
        if !granted {
            return Err(access_denied(what, scope.service_id, operation));
        }
    }

    Ok(scopes)
}

/// Require access to every project represented by a schedule. Global and
/// ownerless schedules are administrators-only when project-aware auth is
/// active. `None` means the caller bypassed scope checks (instance admin or
/// plain OSS); project-scoped callers receive the resolved scope.
pub(crate) async fn require_schedule_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    schedule_id: i32,
    required_permission: Permission,
    operation: &str,
) -> Result<Option<BackupScheduleAccessScope>, Problem> {
    if caller_is_instance_admin(auth) {
        return Ok(None);
    }

    let token_project_id = auth.project_id();
    if token_project_id.is_none() && app_state.project_access_checker.is_none() {
        return Ok(None);
    }

    let scope = app_state
        .backup_service
        .access_scope_for_schedule(schedule_id)
        .await
        .map_err(Problem::from)?;

    require_resolved_schedule_access(app_state, auth, &scope, required_permission, operation)
        .await?;
    Ok(Some(scope))
}

pub(crate) async fn require_resolved_schedule_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    scope: &BackupScheduleAccessScope,
    required_permission: Permission,
    operation: &str,
) -> Result<(), Problem> {
    if caller_is_instance_admin(auth)
        || (auth.project_id().is_none() && app_state.project_access_checker.is_none())
    {
        return Ok(());
    }

    let project_ids = match &scope {
        BackupScheduleAccessScope::Global {
            schedule_id,
            reason,
        } => {
            error!(
                schedule_id,
                reason = %reason,
                operation,
                "non-admin caller denied access to global backup schedule"
            );
            return Err(Problem::from(BackupError::Forbidden {
                resource: format!("backup schedule {schedule_id}"),
                detail: format!("Only an administrator may {operation} this schedule"),
            }));
        }
        BackupScheduleAccessScope::Projects { project_ids, .. } => project_ids,
    };

    if let Some(project_id) = auth.project_id() {
        if project_ids.iter().all(|candidate| *candidate == project_id) {
            return Ok(());
        }
        error!(
            token_project_id = project_id,
            represented_project_ids = ?project_ids,
            operation,
            "deployment token denied access to multi-project backup schedule"
        );
        return Err(Problem::from(BackupError::Forbidden {
            resource: format!("backup schedule {}", scope.schedule_id()),
            detail: format!("This deployment token cannot {operation} the backup schedule"),
        }));
    }

    let checker = app_state
        .project_access_checker
        .as_deref()
        .ok_or_else(|| authorization_configuration_error("backup schedule", operation))?;
    let decisions = project_permission_decisions(
        checker,
        auth.user_id(),
        project_ids.iter().copied(),
        required_permission,
        "backup schedule",
        operation,
    )
    .await?;
    if project_ids
        .iter()
        .all(|project_id| decisions.get(project_id) == Some(&true))
    {
        Ok(())
    } else {
        Err(Problem::from(BackupError::Forbidden {
            resource: format!("backup schedule {}", scope.schedule_id()),
            detail: format!(
                "The caller must have access to every project represented by the schedule to {operation} it"
            ),
        }))
    }
}

/// Filter a schedule collection without issuing one authorization query per
/// row. Infrastructure errors remain fatal; inaccessible schedules are
/// omitted so list endpoints do not disclose their existence.
pub(crate) async fn filter_accessible_schedules(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    schedules: Vec<temps_entities::backup_schedules::Model>,
    required_permission: Permission,
    operation: &str,
) -> Result<Vec<temps_entities::backup_schedules::Model>, Problem> {
    if caller_is_instance_admin(auth)
        || (auth.project_id().is_none() && app_state.project_access_checker.is_none())
    {
        return Ok(schedules);
    }

    let schedule_ids: Vec<i32> = schedules.iter().map(|schedule| schedule.id).collect();
    let scopes = app_state
        .backup_service
        .access_scopes_for_schedules(&schedule_ids)
        .await
        .map_err(Problem::from)?;
    let scopes_by_schedule: BTreeMap<i32, BackupScheduleAccessScope> = scopes
        .into_iter()
        .map(|scope| (scope.schedule_id(), scope))
        .collect();

    if let Some(token_project_id) = auth.project_id() {
        return Ok(schedules
            .into_iter()
            .filter(|schedule| {
                matches!(
                    scopes_by_schedule.get(&schedule.id),
                    Some(BackupScheduleAccessScope::Projects { project_ids, .. })
                        if project_ids.iter().all(|project_id| *project_id == token_project_id)
                )
            })
            .collect());
    }

    let checker = app_state
        .project_access_checker
        .as_deref()
        .ok_or_else(|| authorization_configuration_error("backup schedules", operation))?;
    let decisions = project_permission_decisions(
        checker,
        auth.user_id(),
        scopes_by_schedule.values().flat_map(|scope| match scope {
            BackupScheduleAccessScope::Global { .. } => Vec::new(),
            BackupScheduleAccessScope::Projects { project_ids, .. } => project_ids.clone(),
        }),
        required_permission,
        "backup schedules",
        operation,
    )
    .await?;

    Ok(schedules
        .into_iter()
        .filter(|schedule| match scopes_by_schedule.get(&schedule.id) {
            Some(BackupScheduleAccessScope::Projects { project_ids, .. }) => project_ids
                .iter()
                .all(|project_id| decisions.get(project_id) == Some(&true)),
            Some(BackupScheduleAccessScope::Global { .. }) | None => false,
        })
        .collect())
}

/// Guard operations that have no project-confined target, such as global
/// cleanup or creating a schedule that targets all services/control-plane.
pub(crate) fn require_global_backup_admin(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    operation: &str,
) -> Result<(), Problem> {
    if caller_is_instance_admin(auth)
        || (auth.project_id().is_none() && app_state.project_access_checker.is_none())
    {
        return Ok(());
    }
    Err(Problem::from(BackupError::Forbidden {
        resource: "global backup resources".to_string(),
        detail: format!("Only an administrator may {operation}"),
    }))
}

pub(crate) async fn require_backup_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    scope: &BackupAccessScope,
    required_permission: Permission,
    operation: &str,
) -> Result<(), Problem> {
    match scope {
        BackupAccessScope::Services { service_ids, .. } => require_services_access(
            app_state,
            auth,
            service_ids,
            required_permission,
            "backup producer services",
            operation,
        )
        .await
        .map(|_| ()),
        BackupAccessScope::Global { .. } => require_global_backup_admin(app_state, auth, operation),
    }
}

/// Authorize a historical backup collection from immutable per-backup scope.
///
/// Producer service ids are checked in one batched call. Any ownerless row is
/// global and makes the collection administrator-only, preventing a mutable
/// schedule from retroactively granting access to control-plane or legacy
/// backups created before its current project scope existed.
pub(crate) async fn require_backup_collection_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    backups: &[BackupWithAccessScope],
    required_permission: Permission,
    operation: &str,
) -> Result<(), Problem> {
    let scope = BackupCollectionAccessScope {
        contains_global: backups
            .iter()
            .any(|backup| matches!(backup.access_scope, BackupAccessScope::Global { .. })),
        service_ids: backups
            .iter()
            .flat_map(|backup| match &backup.access_scope {
                BackupAccessScope::Services { service_ids, .. } => service_ids.iter().copied(),
                BackupAccessScope::Global { .. } => [].iter().copied(),
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    };
    require_backup_collection_scope_access(app_state, auth, &scope, required_permission, operation)
        .await
}

/// Authorize a bounded collection summary produced directly by the service's
/// existence/distinct-service queries.
pub(crate) async fn require_backup_collection_scope_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    scope: &BackupCollectionAccessScope,
    required_permission: Permission,
    operation: &str,
) -> Result<(), Problem> {
    if scope.contains_global {
        return require_global_backup_admin(app_state, auth, operation);
    }
    if scope.service_ids.is_empty() {
        return Ok(());
    }

    require_services_access(
        app_state,
        auth,
        &scope.service_ids,
        required_permission,
        "historical backup producer services",
        operation,
    )
    .await
    .map(|_| ())
}

/// Validate that an attachment does not expand an authorized project-scoped
/// schedule into unrelated projects. Authorization happens before mutation.
pub(crate) async fn require_schedule_attach_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    schedule_id: i32,
    service_ids: &[i32],
    required_permission: Permission,
) -> Result<(), Problem> {
    let schedule_scope = require_schedule_access(
        app_state,
        auth,
        schedule_id,
        required_permission,
        "attach services to",
    )
    .await?;
    let service_scopes = require_services_access(
        app_state,
        auth,
        service_ids,
        required_permission,
        "external service",
        "backup schedule attachment",
    )
    .await?;

    let Some(BackupScheduleAccessScope::Projects { project_ids, .. }) = schedule_scope else {
        return Ok(());
    };
    let allowed_projects: BTreeSet<i32> = project_ids.into_iter().collect();
    for service_scope in service_scopes {
        if service_scope.project_ids.is_empty()
            || !service_scope
                .project_ids
                .iter()
                .all(|project_id| allowed_projects.contains(project_id))
        {
            return Err(Problem::from(BackupError::Forbidden {
                resource: format!("backup schedule {schedule_id}"),
                detail: format!(
                    "External service {} belongs to projects outside the schedule scope",
                    service_scope.service_id
                ),
            }));
        }
    }
    Ok(())
}

async fn project_permission_decisions(
    checker: &dyn temps_core::ProjectAccessChecker,
    user_id: i32,
    project_ids: impl IntoIterator<Item = i32>,
    required_permission: Permission,
    resource: &str,
    operation: &str,
) -> Result<BTreeMap<i32, bool>, Problem> {
    let unique_project_ids: BTreeSet<i32> = project_ids.into_iter().collect();
    let project_ids: Vec<i32> = unique_project_ids.iter().copied().collect();
    let required_permission_string = required_permission.to_string();
    let permissions = checker
        .effective_project_permissions_batch(user_id, &project_ids)
        .await
        .map_err(|checker_error| {
            project_permission_batch_check_failed(
                checker_error.as_ref(),
                user_id,
                &project_ids,
                resource,
                operation,
                required_permission,
                "effective project permission resolution failed",
            )
        })?;

    let fallback_project_ids: Vec<i32> = project_ids
        .iter()
        .copied()
        .filter(|project_id| matches!(permissions.get(project_id), Some(None)))
        .collect();
    let coarse_access = checker
        .user_can_access_projects(user_id, &fallback_project_ids)
        .await
        .map_err(|checker_error| {
            project_permission_batch_check_failed(
                checker_error.as_ref(),
                user_id,
                &fallback_project_ids,
                resource,
                operation,
                required_permission,
                "coarse project membership fallback failed",
            )
        })?;

    let mut decisions = BTreeMap::new();
    for project_id in project_ids {
        let allowed = match permissions.get(&project_id) {
            Some(Some(project_permissions)) => project_permissions
                .iter()
                .any(|permission| permission == &required_permission_string),
            Some(None) => coarse_access.get(&project_id).copied().ok_or_else(|| {
                project_permission_batch_result_incomplete(
                    user_id,
                    &[project_id],
                    resource,
                    operation,
                    required_permission,
                    "coarse project membership fallback omitted a requested project",
                )
            })?,
            None => {
                return Err(project_permission_batch_result_incomplete(
                    user_id,
                    &[project_id],
                    resource,
                    operation,
                    required_permission,
                    "effective project permission resolution omitted a requested project",
                ));
            }
        };
        decisions.insert(project_id, allowed);
    }
    Ok(decisions)
}

fn project_permission_batch_check_failed(
    checker_error: &(dyn std::error::Error + Send + Sync),
    user_id: i32,
    project_ids: &[i32],
    resource: &str,
    operation: &str,
    required_permission: Permission,
    failure_kind: &str,
) -> Problem {
    error!(
        user_id,
        ?project_ids,
        required_permission = %required_permission,
        error = %checker_error,
        resource,
        operation,
        failure_kind,
        "batched project permission check failed closed"
    );
    Problem::from(BackupError::Authorization {
        resource: resource.to_string(),
        detail: format!("Project access could not be verified while attempting to {operation}"),
    })
}

fn project_permission_batch_result_incomplete(
    user_id: i32,
    project_ids: &[i32],
    resource: &str,
    operation: &str,
    required_permission: Permission,
    failure_kind: &str,
) -> Problem {
    error!(
        user_id,
        ?project_ids,
        required_permission = %required_permission,
        resource,
        operation,
        failure_kind,
        "batched project permission result was incomplete; failing closed"
    );
    Problem::from(BackupError::Authorization {
        resource: resource.to_string(),
        detail: format!("Project access could not be verified while attempting to {operation}"),
    })
}

#[cfg(test)]
fn project_permission_check_failed(
    checker_error: &(dyn std::error::Error + Send + Sync),
    user_id: i32,
    project_id: i32,
    resource: &str,
    operation: &str,
    required_permission: Permission,
    failure_kind: &str,
) -> Problem {
    error!(
        user_id,
        project_id,
        permission = %required_permission,
        error = %checker_error,
        resource,
        operation,
        failure_kind,
        "project permission check failed closed"
    );
    Problem::from(BackupError::Authorization {
        resource: resource.to_string(),
        detail: format!("Project access could not be verified while attempting to {operation}"),
    })
}

/// Resolve the subset of service ids reachable with a specific permission in
/// a fixed number of batched queries. Used by confidential list filtering:
/// inaccessible rows are omitted, while checker/database failures abort the
/// entire request.
pub(crate) async fn accessible_service_ids(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    service_ids: &[i32],
    required_permission: Permission,
    resource: &str,
    operation: &str,
) -> Result<BTreeSet<i32>, Problem> {
    let requested_service_ids: BTreeSet<i32> = service_ids.iter().copied().collect();
    if caller_is_instance_admin(auth)
        || (auth.project_id().is_none() && app_state.project_access_checker.is_none())
    {
        return Ok(requested_service_ids);
    }

    let scopes = app_state
        .backup_service
        .project_scopes_for_services(service_ids)
        .await
        .map_err(Problem::from)?;

    if let Some(token_project_id) = auth.project_id() {
        return Ok(scopes
            .into_iter()
            .filter(|scope| deployment_token_may_access(token_project_id, &scope.project_ids))
            .map(|scope| scope.service_id)
            .collect());
    }

    let checker = app_state
        .project_access_checker
        .as_deref()
        .ok_or_else(|| authorization_configuration_error(resource, operation))?;
    let decisions = project_permission_decisions(
        checker,
        auth.user_id(),
        scopes
            .iter()
            .flat_map(|scope| scope.project_ids.iter().copied()),
        required_permission,
        resource,
        operation,
    )
    .await?;

    Ok(scopes
        .into_iter()
        .filter(|scope| {
            scope
                .project_ids
                .iter()
                .any(|project_id| decisions.get(project_id) == Some(&true))
        })
        .map(|scope| scope.service_id)
        .collect())
}

fn authorization_configuration_error(resource: &str, operation: &str) -> Problem {
    Problem::from(BackupError::Authorization {
        resource: resource.to_string(),
        detail: format!("No project access checker was available while attempting to {operation}"),
    })
}

// Retained as a focused coarse-membership test seam. Production paths use
// `project_permission_decisions`, which first consults the permission-specific
// checker API and falls back to this legacy membership semantics on `None`.
#[cfg(test)]
async fn project_access_decisions(
    checker: &dyn temps_core::ProjectAccessChecker,
    user_id: i32,
    project_ids: impl IntoIterator<Item = i32>,
    resource: &str,
    operation: &str,
) -> Result<BTreeMap<i32, bool>, Problem> {
    let unique_project_ids: BTreeSet<i32> = project_ids.into_iter().collect();
    let mut decisions = BTreeMap::new();
    for project_id in unique_project_ids {
        let allowed = checker
            .user_can_access_project(user_id, project_id)
            .await
            .map_err(|checker_error| {
                project_permission_check_failed(
                    checker_error.as_ref(),
                    user_id,
                    project_id,
                    resource,
                    operation,
                    Permission::BackupsRead,
                    "coarse project access test failed",
                )
            })?;
        decisions.insert(project_id, allowed);
    }
    Ok(decisions)
}

/// Whether `user_id` may reach at least one of `project_ids`, per the
/// registered checker. Pulled out of [`require_service_access`] so the
/// tri-state result (granted / denied / infrastructure failure) is testable
/// against a mock [`temps_core::ProjectAccessChecker`] without needing a full
/// `BackupAppState` (which pulls in Docker-backed backup/restore/upgrade
/// services that don't matter for this decision).
///
/// Fail-closed: an infrastructure error on *any* project short-circuits to
/// `Err` even if a later project would have granted access, and an empty
/// `project_ids` (service linked to no project) returns `Ok(false)` — never
/// reads as "unrestricted".
#[cfg(test)]
async fn checker_grants_access(
    checker: &dyn temps_core::ProjectAccessChecker,
    user_id: i32,
    project_ids: &[i32],
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut granted = false;
    for project_id in project_ids {
        match checker.user_can_access_project(user_id, *project_id).await {
            Ok(true) => granted = true,
            Ok(false) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(granted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A deployment token is minted for one project; access to a service
    /// outside it must be refused even before any plugin-provided
    /// project-access checker is consulted.
    #[test]
    fn deployment_token_confined_to_its_own_project() {
        assert!(deployment_token_may_access(7, &[3, 7]));
        assert!(!deployment_token_may_access(7, &[3, 9]));
    }

    /// A service linked to no project is reachable by no deployment token —
    /// an empty link set must never read as "unrestricted".
    #[test]
    fn deployment_token_denied_for_unlinked_service() {
        assert!(!deployment_token_may_access(7, &[]));
    }

    #[test]
    fn access_denied_names_resource_and_operation() {
        let problem = access_denied("external service", 42, "backup");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        let body = serde_json::to_value(&problem.body).unwrap();
        let detail = body["detail"].as_str().unwrap_or("");
        assert!(detail.contains("external service"));
        assert!(detail.contains("42"));
        assert!(detail.contains("backup"));
    }

    // -----------------------------------------------------------------
    // checker_grants_access — the EE-style Teams checker path
    // -----------------------------------------------------------------
    //
    // This is the regression coverage for the actual vulnerability: with
    // `TeamProjectAccessChecker` registered (EE Teams installed), a user who
    // is not a member of the project a service belongs to must be denied,
    // even though their `Role::User` holds the instance-wide
    // Backups*/ExternalServices* permission the handler's `permission_guard!`
    // checks.

    /// Grants access to an explicit allow-list of project ids, or always
    /// errors if `infra_failure` is set — stands in for
    /// `temps-ee-teams::TeamProjectAccessChecker` without pulling in EE.
    struct StubChecker {
        allowed_project_ids: Vec<i32>,
        infra_failure: bool,
    }

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for StubChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            if self.infra_failure {
                return Err("stub checker: simulated infrastructure failure".into());
            }
            Ok(self.allowed_project_ids.contains(&project_id))
        }
    }

    /// A user who belongs to none of the projects a service is linked to —
    /// the cross-tenant IDOR this whole module exists to close — must be
    /// denied even though they hold the instance-wide permission that got
    /// them past `permission_guard!`.
    #[tokio::test]
    async fn checker_denies_user_outside_the_linked_projects() {
        let checker = StubChecker {
            allowed_project_ids: vec![3],
            infra_failure: false,
        };
        let granted = checker_grants_access(&checker, 99, &[7, 8]).await.unwrap();
        assert!(
            !granted,
            "user with no membership in projects 7 or 8 must be denied"
        );
    }

    /// A user who belongs to at least one linked project is granted, even if
    /// the service is (unusually) linked to several.
    #[tokio::test]
    async fn checker_grants_user_in_any_linked_project() {
        let checker = StubChecker {
            allowed_project_ids: vec![8],
            infra_failure: false,
        };
        let granted = checker_grants_access(&checker, 42, &[7, 8]).await.unwrap();
        assert!(granted);
    }

    /// A service linked to zero projects must never read as "unrestricted" —
    /// fail closed, matching the deployment-token rule.
    #[tokio::test]
    async fn checker_denies_service_linked_to_no_project() {
        let checker = StubChecker {
            allowed_project_ids: vec![1, 2, 3],
            infra_failure: false,
        };
        let granted = checker_grants_access(&checker, 1, &[]).await.unwrap();
        assert!(!granted);
    }

    /// An infrastructure failure while checking project access must fail
    /// closed (`Err`), never silently fall through to "allow".
    #[tokio::test]
    async fn checker_infrastructure_failure_is_not_silently_allowed() {
        let checker = StubChecker {
            allowed_project_ids: vec![7],
            infra_failure: true,
        };
        let result = checker_grants_access(&checker, 1, &[7]).await;
        assert!(result.is_err());
    }

    struct RecordingChecker {
        allowed_project_ids: Vec<i32>,
        error_project_id: Option<i32>,
        calls: Mutex<Vec<i32>>,
    }

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for RecordingChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            self.calls
                .lock()
                .expect("record checker call")
                .push(project_id);
            if self.error_project_id == Some(project_id) {
                return Err(format!("simulated checker failure for project {project_id}").into());
            }
            Ok(self.allowed_project_ids.contains(&project_id))
        }
    }

    #[tokio::test]
    async fn test_checker_grants_access_earlier_error_later_allow_fails_immediately() {
        // Arrange: project 8 would be allowed, but project 7 fails first.
        let checker = RecordingChecker {
            allowed_project_ids: vec![8],
            error_project_id: Some(7),
            calls: Mutex::new(Vec::new()),
        };

        // Act.
        let result = checker_grants_access(&checker, 42, &[7, 8]).await;

        // Assert: fail closed and do not consult the later allow result.
        assert!(result.is_err());
        assert_eq!(*checker.calls.lock().expect("read checker calls"), vec![7]);
    }

    #[tokio::test]
    async fn test_project_access_decisions_duplicate_projects_checks_each_project_once() {
        // Arrange.
        let checker = RecordingChecker {
            allowed_project_ids: vec![3],
            error_project_id: None,
            calls: Mutex::new(Vec::new()),
        };

        // Act.
        let decisions =
            project_access_decisions(&checker, 42, [7, 3, 7, 3], "backup schedule", "read")
                .await
                .expect("checker decisions should resolve");

        // Assert: deterministic, deduplicated checks retain both allow/deny decisions.
        assert_eq!(decisions.get(&3), Some(&true));
        assert_eq!(decisions.get(&7), Some(&false));
        assert_eq!(
            *checker.calls.lock().expect("read checker calls"),
            vec![3, 7]
        );
    }

    #[tokio::test]
    async fn test_project_access_decisions_checker_error_returns_internal_error_and_stops() {
        // Arrange: the sorted project order reaches failing project 7 before
        // project 8, which would otherwise be allowed.
        let checker = RecordingChecker {
            allowed_project_ids: vec![8],
            error_project_id: Some(7),
            calls: Mutex::new(Vec::new()),
        };

        // Act.
        let problem = project_access_decisions(&checker, 42, [7, 8], "backup schedule", "read")
            .await
            .expect_err("checker infrastructure failure must fail closed");

        // Assert.
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(*checker.calls.lock().expect("read checker calls"), vec![7]);
    }

    struct PermissionBatchChecker {
        permissions: BTreeMap<i32, Option<Vec<String>>>,
        coarse_access: BTreeMap<i32, bool>,
        permissions_error: bool,
        coarse_error: bool,
        coarse_calls: Mutex<Vec<Vec<i32>>>,
    }

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for PermissionBatchChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self
                .coarse_access
                .get(&project_id)
                .copied()
                .unwrap_or(false))
        }

        async fn user_can_access_projects(
            &self,
            _user_id: i32,
            project_ids: &[i32],
        ) -> Result<BTreeMap<i32, bool>, Box<dyn std::error::Error + Send + Sync>> {
            self.coarse_calls
                .lock()
                .expect("record coarse batch")
                .push(project_ids.to_vec());
            if self.coarse_error {
                return Err("coarse checker unavailable".into());
            }
            Ok(project_ids
                .iter()
                .filter_map(|project_id| {
                    self.coarse_access
                        .get(project_id)
                        .copied()
                        .map(|allowed| (*project_id, allowed))
                })
                .collect())
        }

        async fn effective_project_permissions_batch(
            &self,
            _user_id: i32,
            project_ids: &[i32],
        ) -> Result<BTreeMap<i32, Option<Vec<String>>>, Box<dyn std::error::Error + Send + Sync>>
        {
            if self.permissions_error {
                return Err("permission checker unavailable".into());
            }
            Ok(project_ids
                .iter()
                .filter_map(|project_id| {
                    self.permissions
                        .get(project_id)
                        .cloned()
                        .map(|permissions| (*project_id, permissions))
                })
                .collect())
        }
    }

    fn permission_batch_checker(
        permissions: BTreeMap<i32, Option<Vec<String>>>,
        coarse_access: BTreeMap<i32, bool>,
    ) -> PermissionBatchChecker {
        PermissionBatchChecker {
            permissions,
            coarse_access,
            permissions_error: false,
            coarse_error: false,
            coarse_calls: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn test_project_permission_decisions_viewer_allows_read_denies_mutations() {
        // Arrange: an explicit viewer-like permission answer has read only.
        let checker = permission_batch_checker(
            BTreeMap::from([(7, Some(vec![Permission::BackupsRead.to_string()]))]),
            BTreeMap::new(),
        );

        // Act + Assert.
        for (permission, expected) in [
            (Permission::BackupsRead, true),
            (Permission::BackupsCreate, false),
            (Permission::BackupsWrite, false),
            (Permission::BackupsDelete, false),
        ] {
            let decisions = project_permission_decisions(
                &checker,
                42,
                [7],
                permission,
                "backup schedule",
                "exercise action",
            )
            .await
            .expect("explicit permissions should resolve");
            assert_eq!(decisions.get(&7), Some(&expected), "{permission}");
        }
        assert!(checker
            .coarse_calls
            .lock()
            .expect("read coarse calls")
            .iter()
            .all(Vec::is_empty));
    }

    #[tokio::test]
    async fn test_project_permission_decisions_none_uses_coarse_fallback() {
        // Arrange: None preserves the legacy coarse membership result.
        let checker =
            permission_batch_checker(BTreeMap::from([(7, None)]), BTreeMap::from([(7, true)]));

        // Act.
        let decisions = project_permission_decisions(
            &checker,
            42,
            [7],
            Permission::BackupsDelete,
            "backup schedule",
            "delete",
        )
        .await
        .expect("coarse fallback should resolve");

        // Assert.
        assert_eq!(decisions.get(&7), Some(&true));
        assert_eq!(
            *checker.coarse_calls.lock().expect("read coarse calls"),
            vec![vec![7]]
        );
    }

    #[tokio::test]
    async fn test_project_permission_decisions_permission_checker_error_fails_closed() {
        let mut checker = permission_batch_checker(BTreeMap::new(), BTreeMap::new());
        checker.permissions_error = true;

        let problem = project_permission_decisions(
            &checker,
            42,
            [7],
            Permission::BackupsRead,
            "backup schedule",
            "read",
        )
        .await
        .expect_err("permission checker error must fail closed");

        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(checker
            .coarse_calls
            .lock()
            .expect("read coarse calls")
            .is_empty());
    }

    #[tokio::test]
    async fn test_project_permission_decisions_coarse_fallback_error_fails_closed() {
        let mut checker =
            permission_batch_checker(BTreeMap::from([(7, None)]), BTreeMap::from([(7, true)]));
        checker.coarse_error = true;

        let problem = project_permission_decisions(
            &checker,
            42,
            [7],
            Permission::BackupsRead,
            "backup schedule",
            "read",
        )
        .await
        .expect_err("coarse fallback error must fail closed");

        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
