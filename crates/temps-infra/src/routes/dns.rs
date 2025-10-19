use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
    http::StatusCode,
};
use tracing::info;
use utoipa::OpenApi;

use crate::types::{DnsLookupRequest, DnsLookupResponse, DnsLookupError};
use crate::services::DnsService;

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
    ),
    tag = "DNS"
)]
pub async fn lookup_dns_a_records<T>(
    State(app_state): State<Arc<T>>,
    Query(request): Query<DnsLookupRequest>,
) -> impl IntoResponse
where
    T: DnsAppState,
{
    info!("Looking up DNS A records for domain: {}", request.domain);

    match app_state.dns_service().lookup_a_records(&request.domain).await {
        Ok(result) => {
            let response = DnsLookupResponse {
                domain: request.domain.clone(),
                count: result.records.len(),
                records: result.records,
                dns_servers: result.dns_servers,
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            let error = DnsLookupError {
                error: e.to_string(),
                domain: request.domain.clone(),
            };
            (StatusCode::BAD_REQUEST, Json(error)).into_response()
        }
    }
}

/// Configure DNS routes
pub fn configure_dns_routes<T>() -> Router<Arc<T>>
where
    T: DnsAppState,
{
    Router::new()
        .route("/dns/lookup", get(lookup_dns_a_records::<T>))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    struct TestAppState {
        dns_service: DnsService,
    }

    impl DnsAppState for TestAppState {
        fn dns_service(&self) -> &DnsService {
            &self.dns_service
        }
    }

    #[tokio::test]
    async fn test_lookup_dns_a_records_success() {
        let state = Arc::new(TestAppState {
            dns_service: DnsService::new(),
        });

        let app = configure_dns_routes::<TestAppState>().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dns/lookup?domain=google.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_lookup_dns_a_records_failure() {
        let state = Arc::new(TestAppState {
            dns_service: DnsService::new(),
        });

        let app = configure_dns_routes::<TestAppState>().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dns/lookup?domain=this-domain-definitely-does-not-exist-12345.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
