// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use tracing::info;
use utoipa::OpenApi;

use crate::services::DnsService;
use crate::types::{DnsLookupError, DnsLookupRequest, DnsLookupResponse};

/// Application state trait for DNS routes
pub trait DnsAppState: Send + Sync + 'static {
    fn dns_service(&self) -> &DnsService;
}

/// OpenAPI documentation for DNS endpoints
#[derive(OpenApi)]
#[openapi(
    paths(lookup_dns_a_records),
    components(
        schemas(DnsLookupRequest, DnsLookupResponse, DnsLookupError)
    ),
    tags(
        (name = "DNS", description = "DNS lookup operations")
    )
)]
pub struct DnsApiDoc;

/// Lookup DNS A records for a domain
#[utoipa::path(
    get,
    path = "/dns/lookup",
    params(
        ("domain" = String, Query, description = "Domain name to lookup")
    ),
    responses(
        (status = 200, description = "Successfully retrieved DNS A records", body = DnsLookupResponse),
        (status = 400, description = "Invalid domain name or lookup failed", body = DnsLookupError),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    tag = "DNS",
    security(("bearer_auth" = []))
)]
pub async fn lookup_dns_a_records<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Query(request): Query<DnsLookupRequest>,
) -> Result<impl IntoResponse, Problem>
where
    T: DnsAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    info!("Looking up DNS A records for domain: {}", request.domain);

    match app_state
        .dns_service()
        .lookup_a_records(&request.domain)
        .await
    {
        Ok(result) => {
            let response = DnsLookupResponse {
                domain: request.domain.clone(),
                count: result.records.len(),
                records: result.records,
                dns_servers: result.dns_servers,
            };
            Ok((StatusCode::OK, Json(response)).into_response())
        }
        Err(e) => {
            let error = DnsLookupError {
                error: e.to_string(),
                domain: request.domain.clone(),
            };
            Ok((StatusCode::BAD_REQUEST, Json(error)).into_response())
        }
    }
}

/// Configure DNS routes
pub fn configure_dns_routes<T>() -> Router<Arc<T>>
where
    T: DnsAppState,
{
    Router::new().route("/dns/lookup", get(lookup_dns_a_records::<T>))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware;
    use chrono::Utc;
    use temps_auth::{AuthContext, Role};
    use temps_entities::users;
    use tower::ServiceExt;

    struct TestAppState {
        dns_service: DnsService,
    }

    impl DnsAppState for TestAppState {
        fn dns_service(&self) -> &DnsService {
            &self.dns_service
        }
    }

    fn test_user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "user@example.com".to_string(),
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
        }
    }

    /// Build the DNS router with an auth context of the given role injected,
    /// mirroring how the real auth middleware would populate it for an
    /// authenticated request.
    fn app_with_role(role: Role) -> Router {
        let state = Arc::new(TestAppState {
            dns_service: DnsService::new(),
        });

        let auth_middleware = middleware::from_fn(
            move |mut req: Request<Body>, next: axum::middleware::Next| {
                let role = role.clone();
                async move {
                    req.extensions_mut()
                        .insert(AuthContext::new_session(test_user(), role));
                    next.run(req).await
                }
            },
        );

        configure_dns_routes::<TestAppState>()
            .layer(auth_middleware)
            .with_state(state)
    }

    /// Router with no auth middleware at all, for proving unauthenticated
    /// requests are rejected before ever reaching the DNS resolver.
    fn app_without_auth() -> Router {
        let state = Arc::new(TestAppState {
            dns_service: DnsService::new(),
        });

        configure_dns_routes::<TestAppState>().with_state(state)
    }

    #[tokio::test]
    async fn test_lookup_dns_a_records_success() {
        let app = app_with_role(Role::Admin);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dns/lookup?domain=google.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_lookup_dns_a_records_failure() {
        let app = app_with_role(Role::Admin);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dns/lookup?domain=this-domain-definitely-does-not-exist-12345.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// Regression test: this endpoint used to have no `RequireAuth` extractor
    /// at all, making the server an open unauthenticated DNS-lookup oracle.
    /// An anonymous caller must now be turned away before any lookup runs.
    #[tokio::test]
    async fn test_lookup_dns_a_records_requires_authentication() {
        let app = app_without_auth();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dns/lookup?domain=google.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// A role without `PlatformInfoRead` (the same permission siblings in
    /// `platform.rs` require) must be denied even once authenticated.
    #[tokio::test]
    async fn test_lookup_dns_a_records_requires_platform_info_read() {
        let app = app_with_role(Role::ApiReader);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dns/lookup?domain=google.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
