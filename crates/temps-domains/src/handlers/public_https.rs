// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::DomainAppState;
use crate::domain_service::public_https::ObservationError;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::ErrorBuilder,
    problemdetails::{Problem, ProblemDetails},
    public_https::PublicHttpsResult,
};

/// Latest outside-in check for a domain. An unmeasured row provides the
/// setup location even when the operator has never configured ingress.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PublicHttpsStatusResponse {
    pub configured: bool,
    pub reason: Option<String>,
    pub setup_path: Option<String>,
    pub generated_hostname: bool,
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<PublicHttpsResult>,
}

impl From<ObservationError> for Problem {
    fn from(error: ObservationError) -> Self {
        let (status, title) = match &error {
            ObservationError::NotFound { .. } => (StatusCode::NOT_FOUND, "Domain not found"),
            ObservationError::Read { .. } | ObservationError::Write { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Domain observation failed",
            ),
        };
        ErrorBuilder::new(status)
            .title(title)
            .detail(error.to_string())
            .build()
    }
}

#[utoipa::path(get, path="/domains/{domain_id}/public-https", tag="Domains",
    params(("domain_id"=i32, Path, description="Domain ID")),
    responses((status=200,body=PublicHttpsStatusResponse), (status=401,description="Unauthorized",body=ProblemDetails),
              (status=403,description="Forbidden",body=ProblemDetails), (status=404,description="Domain not found",body=ProblemDetails),
              (status=500,description="Failed to read observation",body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub(super) async fn public_https_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DomainAppState>>,
    Path(id): Path<i32>,
) -> Result<Json<PublicHttpsStatusResponse>, Problem> {
    permission_guard!(auth, DomainsRead);
    let row = state.domain_service.public_https_observation(id).await?;
    let result = row.public_https_result.map(|stored| stored.0);
    let configured = result
        .as_ref()
        .is_some_and(|r| r.status == temps_core::public_https::PublicHttpsStatus::Ready);
    let reason = match &result {
        None => Some("Public HTTPS has not been checked for this domain.".into()),
        Some(value) => value.blockers.first().map(|b| b.detail.clone()),
    };
    Ok(Json(PublicHttpsStatusResponse {
        configured,
        reason,
        setup_path: (!configured).then(|| "/settings/nodes/ingress".into()),
        generated_hostname: row.generated_hostname,
        checked_at: row.public_https_checked_at,
        result,
    }))
}
