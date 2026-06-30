//! Integration tests that PROVE router replay enforces auth / permissions.
//!
//! Each test builds a small `axum::Router` whose handlers read `AuthContext`
//! from request extensions (exactly as `RequireAuth` does), wraps it in an
//! `InternalApiCaller`, then calls through the caller with different scopes and
//! asserts the expected HTTP status codes and response bodies.
//!
//! These tests never touch a database or an external service — they are purely
//! in-process and fast.

#[cfg(test)]
mod tests {
    use axum::{extract::Request, response::IntoResponse, routing::get, Json, Router};
    use chrono::Utc;
    use http::StatusCode;
    use serde_json::{json, Value};
    use temps_auth::{
        context::AuthContext,
        permissions::{Permission, Role},
    };
    use temps_entities::users;
    use utoipa::openapi::{
        path::{OperationBuilder, ParameterBuilder, ParameterIn, PathItem, PathsBuilder},
        schema::{ObjectBuilder, SchemaType, Type},
        OpenApiBuilder, RefOr, Required, Schema,
    };

    use crate::{ApiCallScope, InternalApiCaller};

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build a minimal `users::Model` for test auth contexts.
    fn test_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: format!("Test User {id}"),
            email: format!("user{id}@example.com"),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Build an `AuthContext` for a user with the given role.
    fn auth_with_role(user_id: i32, role: Role) -> AuthContext {
        AuthContext::new_session(test_user(user_id), role)
    }

    // -----------------------------------------------------------------------
    // Test router
    //
    // `/whoami` — returns the caller's user_id (proves AuthContext was injected).
    // `/guarded` — applies a permission check; returns 200 or 403.
    // -----------------------------------------------------------------------

    /// Handler: return the caller's `user_id` as `{"user_id": N}`.
    async fn whoami(req: Request) -> impl IntoResponse {
        let auth = req.extensions().get::<AuthContext>().cloned();

        match auth {
            Some(ctx) => {
                (StatusCode::OK, Json(json!({ "user_id": ctx.user_id() }))).into_response()
            }
            None => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "no auth context" })),
            )
                .into_response(),
        }
    }

    /// Handler: requires `ProjectsRead`; returns 200 or 403.
    async fn guarded(req: Request) -> impl IntoResponse {
        let auth = req.extensions().get::<AuthContext>().cloned();

        match auth {
            None => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "no auth context" })),
            )
                .into_response(),
            Some(ctx) => {
                if ctx.has_permission(&Permission::ProjectsRead) {
                    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
                } else {
                    (
                        StatusCode::FORBIDDEN,
                        Json(json!({ "error": "insufficient permissions" })),
                    )
                        .into_response()
                }
            }
        }
    }

    /// Build the test router.
    fn test_router() -> Router {
        Router::new()
            .route("/whoami", get(whoami))
            .route("/guarded", get(guarded))
    }

    /// Build a minimal `utoipa::openapi::OpenApi` describing the two test routes.
    fn test_openapi() -> utoipa::openapi::OpenApi {
        let whoami_op = OperationBuilder::new()
            .operation_id(Some("whoami"))
            .summary(Some("Returns the caller user_id"))
            .build();

        let guarded_op = OperationBuilder::new()
            .operation_id(Some("guarded"))
            .summary(Some("Permission-gated endpoint"))
            .parameter(
                ParameterBuilder::new()
                    .name("project_id")
                    .parameter_in(ParameterIn::Query)
                    .required(Required::False)
                    .schema(Some(RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::Integer))
                            .build(),
                    ))))
                    .build(),
            )
            .build();

        let paths = PathsBuilder::new()
            .path(
                "/whoami",
                PathItem::new(utoipa::openapi::path::HttpMethod::Get, whoami_op),
            )
            .path(
                "/guarded",
                PathItem::new(utoipa::openapi::path::HttpMethod::Get, guarded_op),
            )
            .build();

        OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Test API", "1.0.0"))
            .paths(paths)
            .build()
    }

    // -----------------------------------------------------------------------
    // Integration tests
    // -----------------------------------------------------------------------

    /// `whoami` with a real user_id in the scope → 200 + body contains the id.
    ///
    /// This proves that:
    /// 1. The `AuthContext` is correctly injected into request extensions.
    /// 2. The handler can read it back (same as `RequireAuth` does in production).
    /// 3. The response body flows back through the size cap unchanged.
    #[tokio::test]
    async fn test_whoami_returns_caller_user_id() {
        let router = test_router();
        let openapi = test_openapi();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        let auth = auth_with_role(42, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        let response = caller
            .call("whoami", Value::Object(Default::default()), &scope)
            .await
            .expect("call should succeed");

        assert_eq!(response.status, 200, "expected 200 OK");
        assert_eq!(
            response.body.get("user_id").and_then(|v| v.as_i64()),
            Some(42),
            "user_id must be 42 in the response body"
        );
    }

    /// `guarded` with Admin role → 200 (has ProjectsRead).
    ///
    /// Proves that the permission check inside the handler evaluates the
    /// injected `AuthContext` — a caller with the right role passes.
    #[tokio::test]
    async fn test_guarded_allows_admin() {
        let router = test_router();
        let openapi = test_openapi();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        let auth = auth_with_role(1, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        // Admin has ProjectsRead → the handler returns 200.
        // The caller surfaces non-2xx as `Upstream` errors, so 200 arrives as Ok.
        let response = caller
            .call("guarded", Value::Object(Default::default()), &scope)
            .await
            .expect("admin should be allowed");

        assert_eq!(response.status, 200, "admin must get 200");
    }

    /// `guarded` with a Custom role that has NO permissions → the handler
    /// returns 403, which `InternalApiCaller` maps to `ApiToolError::Upstream`.
    ///
    /// This is the core security proof: the router's permission check (not any
    /// code in this crate) enforces authz; a caller lacking the permission
    /// cannot reach the protected response.
    #[tokio::test]
    async fn test_guarded_rejects_caller_without_permission() {
        let router = test_router();
        let openapi = test_openapi();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        // Custom role with no permissions — has_permission returns false for
        // ProjectsRead.
        let auth = AuthContext::new_api_key(
            test_user(99),
            Some(Role::Custom),
            Some(vec![]), // empty custom permission list
            "no-perm-key".to_string(),
            1,
        );
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        let err = caller
            .call("guarded", Value::Object(Default::default()), &scope)
            .await
            .expect_err("caller without permission must be rejected");

        // The handler returned 403; InternalApiCaller wraps it as Upstream.
        assert!(
            matches!(err, crate::ApiToolError::Upstream { status: 403, .. }),
            "expected Upstream 403, got: {:?}",
            err
        );
    }

    /// An unknown operation_id → `ApiToolError::UnknownOperation`.
    #[tokio::test]
    async fn test_unknown_operation_returns_error() {
        let router = test_router();
        let openapi = test_openapi();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        let auth = auth_with_role(1, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        let err = caller
            .call("nonexistent_op", Value::Object(Default::default()), &scope)
            .await
            .expect_err("unknown op must return error");

        assert!(
            matches!(err, crate::ApiToolError::UnknownOperation { .. }),
            "expected UnknownOperation, got: {:?}",
            err
        );
    }

    /// Verify that a denylisted operation is not callable (excluded from index).
    #[tokio::test]
    async fn test_denylisted_operation_is_not_callable() {
        let router = test_router();
        let openapi = test_openapi();
        // Denylist "whoami" so it never appears in the index.
        let caller = InternalApiCaller::new(router, &openapi, vec!["whoami".to_string()]);

        let auth = auth_with_role(1, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        let err = caller
            .call("whoami", Value::Object(Default::default()), &scope)
            .await
            .expect_err("denylisted op must not be callable");

        assert!(
            matches!(err, crate::ApiToolError::UnknownOperation { .. }),
            "expected UnknownOperation for denylisted op, got: {:?}",
            err
        );
        // "guarded" is not denylisted; it should still work for admin.
        let response = caller
            .call("guarded", Value::Object(Default::default()), &scope)
            .await
            .expect("non-denylisted op must still work");
        assert_eq!(response.status, 200);
    }

    /// `search` returns matching operations from the index.
    #[tokio::test]
    async fn test_search_returns_relevant_operations() {
        let router = test_router();
        let openapi = test_openapi();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        let auth = auth_with_role(1, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        let results = caller.search("guarded", &scope);
        assert!(
            results.iter().any(|op| op.operation_id == "guarded"),
            "search for 'guarded' should surface the guarded op"
        );
    }

    /// `describe` returns the full schema for a known operation.
    #[tokio::test]
    async fn test_describe_returns_schema_for_known_operation() {
        let router = test_router();
        let openapi = test_openapi();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        let schema = caller.describe("whoami");
        assert!(schema.is_some(), "describe must return Some for a known op");
        let schema = schema.unwrap();
        assert_eq!(schema.operation_id, "whoami");
    }

    /// `describe` returns `None` for an unknown operation.
    #[tokio::test]
    async fn test_describe_returns_none_for_unknown_operation() {
        let router = test_router();
        let openapi = test_openapi();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        assert!(
            caller.describe("does_not_exist").is_none(),
            "describe must return None for unknown op"
        );
    }

    // -----------------------------------------------------------------------
    // ApiToolsHandle tests
    // -----------------------------------------------------------------------

    /// Verify that `ApiToolsHandle::get()` returns `None` before `set()` is
    /// called and `Some` after.
    #[test]
    fn test_api_tools_handle_initially_empty() {
        let handle = crate::ApiToolsHandle::new();
        assert!(handle.get().is_none(), "handle must be empty before set()");
    }

    /// Verify that clones of `ApiToolsHandle` share the same `OnceLock` — a
    /// `set()` on one clone is visible on the other.
    #[test]
    fn test_api_tools_handle_clones_share_state() {
        use utoipa::openapi::{OpenApiBuilder, PathsBuilder};

        let handle = crate::ApiToolsHandle::new();
        let handle_clone = handle.clone();

        assert!(handle.get().is_none());
        assert!(handle_clone.get().is_none());

        // Build a minimal caller to set.
        let router = Router::new();
        let openapi = OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Test", "1.0.0"))
            .paths(PathsBuilder::new().build())
            .build();
        let caller = InternalApiCaller::new(router, &openapi, vec![]);

        handle.set(caller);

        // Both handle and clone should now see the caller via Arc.
        assert!(
            handle.get().is_some(),
            "original handle must have caller after set()"
        );
        assert!(
            handle_clone.get().is_some(),
            "clone must see caller set on original"
        );
    }
}
