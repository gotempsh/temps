// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The exported guard macros expand to `$crate::...` paths, so a crate that
//! depends only on `temps-authz` can call them without importing
//! `temps-core`, `axum` or `tracing`. This file is such a caller: it names
//! nothing but `temps_authz`.

use std::sync::Arc;

use temps_authz::{AuthContext, Permission};

type Checker = Option<Arc<dyn temps_authz::__private::temps_core::ProjectAccessChecker>>;
type Problem = temps_authz::__private::temps_core::problemdetails::Problem;

fn token() -> AuthContext {
    AuthContext::new_deployment_token(7, None, None, 1, "deploy".to_string(), vec![])
}

fn guarded(auth: &AuthContext) -> Result<(), Problem> {
    temps_authz::permission_guard!(auth, ProjectsRead);
    Ok(())
}

fn checked(auth: &AuthContext) -> Result<(), Problem> {
    temps_authz::permission_check!(auth, Permission::ProjectsRead);
    Ok(())
}

fn scoped(auth: &AuthContext, project_id: i32) -> Result<(), Problem> {
    temps_authz::project_scope_guard!(auth, project_id);
    Ok(())
}

fn no_tokens(auth: &AuthContext) -> Result<(), Problem> {
    temps_authz::deny_deployment_token!(auth);
    Ok(())
}

async fn project_access(
    auth: &AuthContext,
    project_id: i32,
    checker: Checker,
) -> Result<(), Problem> {
    temps_authz::project_access_guard!(auth, project_id, checker);
    Ok(())
}

async fn project_permission(
    auth: &AuthContext,
    project_id: i32,
    checker: Checker,
) -> Result<(), Problem> {
    temps_authz::project_permission_guard!(auth, AnalyticsRead, project_id, checker);
    Ok(())
}

#[tokio::test]
async fn guard_macros_resolve_from_a_caller_that_names_only_temps_authz() {
    let auth = token();
    let forbidden = temps_authz::__private::axum::http::StatusCode::FORBIDDEN;

    assert_eq!(guarded(&auth).map_err(|p| p.status_code), Err(forbidden));
    assert_eq!(checked(&auth).map_err(|p| p.status_code), Err(forbidden));
    assert!(scoped(&auth, 7).is_ok());
    assert_eq!(scoped(&auth, 8).map_err(|p| p.status_code), Err(forbidden));
    assert_eq!(no_tokens(&auth).map_err(|p| p.status_code), Err(forbidden));
    assert!(project_access(&auth, 7, None).await.is_ok());
    assert_eq!(
        project_permission(&auth, 7, None)
            .await
            .map_err(|p| p.status_code),
        Err(forbidden)
    );
}
