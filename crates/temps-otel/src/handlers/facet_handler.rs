//! HTTP handlers for OTel span attribute facet management.
//!
//! Facets allow admins to mark any OTel attribute key as "faceted", which pre-populates
//! a dedicated slot column (ClickHouse or TimescaleDB, whichever backend is active) and
//! enables index-accelerated filtering for that key — replacing the default
//! full-JSON/text-parse predicate. See `services::facet_service` for the backfill
//! lifecycle these endpoints expose (`status`: pending -> running -> completed/failed).
//!
//! ## Endpoints
//!
//! - `GET    /otel/facets`             — list all registered facets, with status (OtelRead)
//! - `POST   /otel/facets`             — create a facet for an attribute key (OtelWrite)
//! - `DELETE /otel/facets/{key}`       — remove a facet by attribute key (OtelWrite)
//! - `POST   /otel/facets/{key}/retry` — retry a failed backfill (OtelWrite)
//!
//! ## CLI parity
//!
//! `temps facets list/create/remove/retry` in `apps/temps-cli/src/commands/facets/`,
//! using the regular generated client (these handlers are registered in the main
//! `ApiDoc`, not plugin-only — `apps/temps-cli/openapi.json` includes them like any
//! other endpoint; see CLAUDE.md "Regenerating the OpenAPI clients").

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::{FacetBackfillRetriedAudit, FacetCreatedAudit, FacetDeletedAudit};
use crate::services::facet_service::FacetError;
use crate::services::FacetInfo;
use crate::OtelAppState;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, ProblemDetails, RequestMetadata};

// ── Error conversion ─────────────────────────────────────────────────────────

impl From<FacetError> for Problem {
    fn from(error: FacetError) -> Self {
        match error {
            FacetError::AlreadyFaceted { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Facet Already Registered")
                .with_detail(error.to_string()),

            FacetError::CapacityExceeded { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Facet Capacity Exceeded")
                .with_detail(error.to_string()),

            FacetError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Facet Not Found")
                .with_detail(error.to_string()),

            FacetError::NotFailed { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Facet Backfill Not Failed")
                .with_detail(error.to_string()),

            FacetError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),

            FacetError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),

            FacetError::Storage { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Storage Error")
                .with_detail(error.to_string()),
        }
    }
}

// ── Request / response DTOs ───────────────────────────────────────────────────

/// Request body for registering a new facet.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFacetRequest {
    /// The OTel attribute key to facet (e.g. `enduser.id`, `galachain.contract`).
    /// Must be non-empty, ≤200 characters, and not already registered.
    pub attribute_key: String,
}

/// Response body for facet list.
#[derive(Debug, Serialize, ToSchema)]
pub struct FacetsResponse {
    pub data: Vec<FacetInfo>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// List all registered OTel span attribute facets.
///
/// Returns all facets registered on this platform (newest first). Since the
/// `spans` ClickHouse table is platform-global, facets are also platform-global.
#[utoipa::path(
    tag = "OTel Facets",
    get,
    path = "/otel/facets",
    responses(
        (status = 200, description = "Registered facets", body = FacetsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_facets(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let data = state.facet_service.list_facets().await?;
    Ok(Json(FacetsResponse { data }))
}

/// Register an OTel attribute key as a facet.
///
/// Assigns the key to the lowest available slot column (1..=20) and inserts
/// the mapping into Postgres with `status: pending`. Returns immediately —
/// the historical backfill (populating the slot column for spans already
/// ingested before this call) runs entirely in the background, advanced by a
/// periodic poller; poll `GET /otel/facets` and check the returned `status`
/// (`pending` -> `running` -> `completed`/`failed`) to track progress. New
/// spans start getting the attribute written into the slot column right
/// away, independent of backfill progress.
#[utoipa::path(
    tag = "OTel Facets",
    post,
    path = "/otel/facets",
    request_body = CreateFacetRequest,
    responses(
        (status = 201, description = "Facet registered", body = FacetInfo),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "Already registered or capacity exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_facet(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateFacetRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);

    let info = state
        .facet_service
        .create_facet(request.attribute_key.clone(), Some(auth.user_id()))
        .await?;

    let audit = FacetCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        attribute_key: info.attribute_key.clone(),
        slot: info.slot,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for facet creation: {}", e);
    }

    Ok((StatusCode::CREATED, Json(info)))
}

/// Remove a registered OTel span attribute facet.
///
/// Marks the facet `deleting` and stops new spans from populating its slot
/// immediately, but the Postgres row (and its slot reservation) isn't
/// removed until the background poller confirms the slot column has been
/// cleared for all existing spans — otherwise a future facet reusing the
/// same slot could see stale data. `GET /otel/facets` will keep returning
/// this facet with `status: deleting` until that finishes.
#[utoipa::path(
    tag = "OTel Facets",
    delete,
    path = "/otel/facets/{key}",
    params(
        ("key" = String, Path, description = "The OTel attribute key (URL-encoded)"),
    ),
    responses(
        (status = 204, description = "Facet deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Facet not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_facet(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);

    state.facet_service.delete_facet(&key).await?;

    let audit = FacetDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        attribute_key: key.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for facet deletion: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Retry a failed OTel span attribute facet backfill.
///
/// Only valid when the facet's `status` is `failed`. Resets its progress and
/// lets the background poller re-attempt the backfill from the beginning on
/// its next tick.
#[utoipa::path(
    tag = "OTel Facets",
    post,
    path = "/otel/facets/{key}/retry",
    params(
        ("key" = String, Path, description = "The OTel attribute key (URL-encoded)"),
    ),
    responses(
        (status = 200, description = "Backfill retry scheduled", body = FacetInfo),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Facet not found", body = ProblemDetails),
        (status = 409, description = "Facet is not in a failed state", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn retry_facet_backfill(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);

    let info = state.facet_service.retry_backfill(&key).await?;

    let audit = FacetBackfillRetriedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        attribute_key: key,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for facet backfill retry: {}", e);
    }

    Ok(Json(info))
}
