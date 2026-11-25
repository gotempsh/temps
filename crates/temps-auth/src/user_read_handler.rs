// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET /users/{user_id}`: read one user back, in the shape the list returns.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use temps_core::problemdetails::Problem;

use crate::permission_guard;
use crate::state::AuthState;
use crate::types::RouteUserWithRoles;
use crate::user_service::UserServiceError;
use crate::RequireAuth;

/// One user with their roles, as listed (deleted users included, with
/// `deleted_at` set).
#[utoipa::path(
    tag = "Users",
    get,
    path = "/users/{user_id}",
    operation_id = "get_user",
    params(("user_id" = i32, Path, description = "User ID")),
    responses(
        (status = 200, description = "The user and their roles", body = RouteUserWithRoles),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "User not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn get_user(
    State(app_state): State<Arc<AuthState>>,
    RequireAuth(auth): RequireAuth,
    Path(user_id): Path<i32>,
) -> Result<Json<RouteUserWithRoles>, Problem> {
    // The same permission as listing users.
    permission_guard!(auth, UsersWrite);
    // User 0 is the internal system user, never listed.
    if user_id == 0 {
        return Err(UserServiceError::NotFound(format!("User {user_id} not found")).into());
    }
    let user = app_state.user_service.get_user_with_roles(user_id).await?;
    Ok(Json(user.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_test_support::{auth_state, insert_user};
    use crate::{AuthContext, Role};
    use axum::http::StatusCode;
    use temps_test_utils::TestDatabase;
    use tower::ServiceExt;

    fn session_auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            temps_entities::users::Model {
                id: 1,
                name: "Admin".to_string(),
                email: "admin@example.test".to_string(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: now,
                updated_at: now,
            },
            role,
        )
    }

    #[tokio::test]
    async fn an_admin_reads_one_user_and_others_are_refused() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                println!("Docker not available, skipping test: {error}");
                return;
            }
            Err(error) => panic!("test database setup failed: {error}"),
        };
        let connection = db.connection_arc();
        let state = auth_state(connection.clone());
        let target = insert_user(connection.as_ref(), "read-me@example.test").await;

        let Json(found) = get_user(
            State(state.clone()),
            RequireAuth(session_auth(Role::Admin)),
            Path(target.id),
        )
        .await
        .expect("an admin reads a user");
        let body = serde_json::to_value(&found).expect("json");
        assert_eq!(body["user"]["id"], target.id);
        assert_eq!(body["user"]["email"], "read-me@example.test");
        assert!(body["user"]["created_at"].as_str().unwrap().ends_with('Z'));
        assert!(!body.to_string().contains("password_hash"));

        for missing in [0, target.id + 1000] {
            let problem = get_user(
                State(state.clone()),
                RequireAuth(session_auth(Role::Admin)),
                Path(missing),
            )
            .await
            .err()
            .expect("missing user");
            assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
            let detail = problem
                .body
                .get("detail")
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .to_string();
            assert!(detail.contains(&missing.to_string()), "{detail}");
        }

        let problem = get_user(
            State(state.clone()),
            RequireAuth(session_auth(Role::User)),
            Path(target.id),
        )
        .await
        .err()
        .expect("a plain user cannot read other users");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);

        let response = crate::http_test_support::router(state, connection)
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/users/{}", target.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
