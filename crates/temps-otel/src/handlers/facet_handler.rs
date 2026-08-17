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
//! - `POST   /otel/facets`             — create a facet for an attribute key (OtelWrite + instance admin)
//! - `DELETE /otel/facets/{key}`       — remove a facet by attribute key (OtelWrite + instance admin)
//! - `POST   /otel/facets/{key}/retry` — retry a failed backfill (OtelWrite + instance admin)
//!
//! ## Why the writes are admin-only
//!
//! Facets are **platform-global**, not project-scoped: the spans table is
//! shared, there are only 20 slots, and creating or deleting one triggers an
//! `ALTER TABLE ... UPDATE` mutation that rewrites facet columns across every
//! tenant's spans. `OtelWrite` alone is not an instance-administrator
//! permission — `Role::User` holds it, and project/team admins can be granted
//! it for ordinary project telemetry — so gating the mutations on it would let
//! any authenticated tenant exhaust the slots, delete facets other tenants
//! rely on, and repeatedly trigger table-wide mutations. There is no
//! `project_id` on these routes to scope against, so instance administration
//! is the correct boundary.
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

/// Reject anything that is not an instance administrator.
///
/// Deliberately not a `permission_guard!`: no `Permission` variant means
/// "instance administrator". `is_admin()`/`PlatformAdmin` are role checks,
/// which also excludes deployment tokens — they carry `Role::Custom` and no
/// user identity, so a project-scoped machine credential can never rewrite the
/// shared spans table even if `OtelWrite` were later added to the
/// deployment-token permission mapping in `AuthContext::has_permission`.
fn require_instance_admin(
    auth: &temps_auth::context::AuthContext,
    operation: &str,
) -> Result<(), Problem> {
    if auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin) {
        return Ok(());
    }
    Err(
        temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Instance Administrator Required")
            .detail(format!(
                "OTel facets are platform-global: {operation} rewrites facet columns across \
                 every project's spans and consumes one of the shared facet slots. Only an \
                 instance administrator may change them."
            ))
            .value("user_role", auth.effective_role.to_string())
            .build(),
    )
}

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
    require_instance_admin(&auth, "creating a facet")?;

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
    require_instance_admin(&auth, "deleting a facet")?;

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
    require_instance_admin(&auth, "retrying a facet backfill")?;

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

#[cfg(test)]
mod tests {
    use super::require_instance_admin;
    use axum::response::IntoResponse;
    use temps_auth::permissions::Permission;
    use temps_auth::{AuthContext, Role};
    use temps_entities::deployment_tokens::DeploymentTokenPermission;

    fn user(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            temps_entities::users::Model {
                id: 1,
                name: "Test User".to_string(),
                email: "test@example.com".to_string(),
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

    /// Facet writes rewrite the shared spans table and consume one of the 20
    /// platform-global slots, so `OtelWrite` alone must not be enough — it is
    /// held by `Role::User` and grantable to project admins.
    #[test]
    fn otel_write_alone_does_not_authorise_facet_mutations() {
        let tenant = user(Role::User);
        assert!(
            tenant.has_permission(&Permission::OtelWrite),
            "precondition: Role::User holds OtelWrite, which is why the extra gate is needed"
        );

        let problem = require_instance_admin(&tenant, "creating a facet")
            .expect_err("a plain user must not be able to mutate global facets");
        assert_eq!(
            problem.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn instance_admins_may_mutate_facets() {
        assert!(require_instance_admin(&user(Role::Admin), "creating a facet").is_ok());
        assert!(require_instance_admin(&user(Role::PlatformAdmin), "creating a facet").is_ok());
    }

    /// A FullAccess deployment token satisfies every `permission_guard!`, so
    /// the gate must be a role check rather than a permission check.
    #[test]
    fn full_access_deployment_tokens_may_not_mutate_facets() {
        let token = AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        );

        let problem = require_instance_admin(&token, "creating a facet")
            .expect_err("a project-scoped token must not rewrite the shared spans table");
        assert_eq!(
            problem.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        );
    }
}
