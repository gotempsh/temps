//! Authenticated HTTP handlers for managing revenue integrations and
//! reading dashboard metrics. Mounted under `/api/v1/...`.
//!
//! OpenAPI operation IDs are prefixed `revenue_*` to prevent cross-crate
//! collisions (see feedback_openapi_collisions memory note).

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Extension, Multipart, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditLogger, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::error::RevenueError;
use crate::handlers::audit::{
    RevenueCsvImportedAudit, RevenueIntegrationConfigUpdatedAudit, RevenueIntegrationCreatedAudit,
    RevenueIntegrationDeletedAudit, RevenueIntegrationSecretRotatedAudit,
    RevenueIntegrationTokenRotatedAudit,
};
use crate::providers::{LemonSqueezyConfig, MeteredMode, ProviderConfig, StripeConfig};
use crate::service::analytics::{AnalyticsError, Bucket, GlobalEventsFilter};
use crate::service::{
    CreateIntegrationInput, ImportOutcome, ImportRowError, IntegrationView,
    RevenueAnalyticsService, RevenueImportService, RevenueIntegrationService,
};

pub struct ManagementState {
    pub integrations: Arc<RevenueIntegrationService>,
    pub analytics: Arc<RevenueAnalyticsService>,
    pub import: Arc<RevenueImportService>,
    pub audit: Arc<dyn AuditLogger>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl ManagementState {
    pub fn new(
        integrations: Arc<RevenueIntegrationService>,
        analytics: Arc<RevenueAnalyticsService>,
        import: Arc<RevenueImportService>,
        audit: Arc<dyn AuditLogger>,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    ) -> Self {
        Self {
            integrations,
            analytics,
            import,
            audit,
            project_access_checker,
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        revenue_list_integrations,
        revenue_create_integration,
        revenue_delete_integration,
        revenue_rotate_token,
        revenue_update_secret,
        revenue_update_config,
        revenue_list_providers,
        revenue_metrics_summary,
        revenue_metrics_global_mrr,
        revenue_metrics_global_summary,
        revenue_metrics_mrr,
        revenue_metrics_customers,
        revenue_recent_events,
        revenue_global_events,
        revenue_import_subscriptions_csv,
        revenue_import_invoices_csv,
    ),
    components(schemas(
        IntegrationResponse,
        CreateIntegrationBody,
        UpdateSecretBody,
        UpdateConfigBody,
        ProviderDescriptor,
        MetricsSummaryResponse,
        GlobalMrrResponse,
        GlobalRevenueSummaryResponse,
        MrrBucketResponse,
        CustomerMovementResponse,
        RecentEventResponse,
        GlobalRecentEventResponse,
        ImportOutcomeResponse,
        ImportRowErrorResponse,
        ProviderConfig,
        StripeConfig,
        LemonSqueezyConfig,
        MeteredMode,
    )),
    tags(
        (name = "Revenue", description = "Per-project revenue tracking integrations and analytics")
    )
)]
pub struct RevenueApiDoc;

// ---------------------------------------------------------------- DTOs

#[derive(Debug, Serialize, ToSchema)]
pub struct IntegrationResponse {
    pub id: i32,
    pub project_id: i32,
    pub provider: String,
    /// Unguessable token embedded in the public webhook URL. The full
    /// URL is `{api_origin}/webhooks/revenue/{provider}/{webhook_path_token}`.
    pub webhook_path_token: String,
    /// Relative path the UI can display and copy. The frontend builds
    /// the full URL by prefixing its own origin.
    pub webhook_path: String,
    pub status: String,
    pub has_secret: bool,
    pub last_event_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// Typed provider config — allowlist and metered-billing mode. Null
    /// when the operator hasn't configured one yet (accept everything).
    pub config: Option<ProviderConfig>,
}

impl From<IntegrationView> for IntegrationResponse {
    fn from(v: IntegrationView) -> Self {
        let webhook_path = format!("/webhooks/revenue/{}/{}", v.provider, v.webhook_path_token);
        Self {
            id: v.id,
            project_id: v.project_id,
            provider: v.provider,
            webhook_path_token: v.webhook_path_token,
            webhook_path,
            status: v.status,
            has_secret: v.has_secret,
            last_event_at: v.last_event_at,
            created_at: v.created_at,
            config: v.config,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateIntegrationBody {
    /// Registered provider name, e.g. "stripe".
    pub provider: String,
    /// Signing secret from the provider's dashboard.
    pub signing_secret: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSecretBody {
    /// New signing secret from the provider's dashboard. Encrypted at
    /// rest; never returned in any API response.
    pub signing_secret: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateConfigBody {
    /// Typed provider configuration. Setting `config` to `null` clears
    /// the stored config back to the accept-everything default. The
    /// config's `provider` tag must match the integration's provider.
    #[serde(default)]
    pub config: Option<ProviderConfig>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderDescriptor {
    pub name: String,
    pub display_name: String,
    pub recommended_events: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MetricsSummaryResponse {
    pub currency: String,
    pub current_mrr_minor: i64,
    pub current_arr_minor: i64,
    pub active_subscriptions: i64,
    pub active_customers: i64,
    pub churned_last_30d: i64,
    pub arpu_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalMrrResponse {
    pub currency: String,
    pub current_mrr_minor: i64,
    /// MRR 24h before now, reconstructed from the event log.
    pub previous_mrr_minor: i64,
    /// Percentage change vs 24h ago. Null when previous MRR is zero
    /// (no baseline to compare against).
    pub change_percentage: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalRevenueSummaryResponse {
    pub currency: String,
    pub current_mrr_minor: i64,
    pub paid_last_30d_minor: i64,
    pub refunded_last_30d_minor: i64,
    pub paid_all_time_minor: i64,
    pub refunded_all_time_minor: i64,
    pub active_subscriptions: i64,
    pub active_customers: i64,
    pub transactions_last_30d: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MrrBucketResponse {
    pub bucket: DateTime<Utc>,
    pub mrr_minor: i64,
    pub charge_total_minor: i64,
    pub refund_total_minor: i64,
    pub charge_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CustomerMovementResponse {
    pub bucket: DateTime<Utc>,
    pub new_customers: i64,
    pub churned_customers: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecentEventResponse {
    pub occurred_at: DateTime<Utc>,
    pub event_type: String,
    pub customer_ref: Option<String>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub mrr_minor: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalRecentEventResponse {
    pub id: i64,
    pub project_id: i32,
    pub project_name: String,
    pub occurred_at: DateTime<Utc>,
    pub event_type: String,
    pub customer_ref: Option<String>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub mrr_minor: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ImportRowErrorResponse {
    pub row: usize,
    pub reason: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ImportOutcomeResponse {
    pub rows_read: usize,
    pub inserted: usize,
    pub updated: usize,
    pub skipped_stale: usize,
    pub skipped_invalid: usize,
    pub errors: Vec<ImportRowErrorResponse>,
}

impl From<ImportOutcome> for ImportOutcomeResponse {
    fn from(o: ImportOutcome) -> Self {
        Self {
            rows_read: o.rows_read,
            inserted: o.inserted,
            updated: o.updated,
            skipped_stale: o.skipped_stale,
            skipped_invalid: o.skipped_invalid,
            errors: o
                .errors
                .into_iter()
                .map(ImportRowErrorResponse::from)
                .collect(),
        }
    }
}

impl From<ImportRowError> for ImportRowErrorResponse {
    fn from(e: ImportRowError) -> Self {
        Self {
            row: e.row,
            reason: e.reason,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct MrrQuery {
    pub currency: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub bucket: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MovementQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub bucket: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SummaryQuery {
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecentEventsQuery {
    pub limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct GlobalEventsQuery {
    pub project_id: Option<i32>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Comma-separated list of event types (e.g. `invoice.paid,mrr.realized`).
    pub event_types: Option<String>,
    pub limit: Option<u64>,
}

// ---------------------------------------------------------------- Errors

fn revenue_error_to_problem(err: RevenueError) -> Problem {
    match err {
        RevenueError::IntegrationNotFound { .. } | RevenueError::IntegrationNotFoundByToken => {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Integration Not Found")
                .detail(err.to_string())
                .build()
        }
        RevenueError::UnknownProvider { .. } => ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Unknown Provider")
            .detail(err.to_string())
            .build(),
        RevenueError::DuplicateIntegration { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
            .title("Integration Already Exists")
            .detail(err.to_string())
            .build(),
        RevenueError::Validation { .. } => ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail(err.to_string())
            .build(),
        RevenueError::IntegrationDisabled { .. } => ErrorBuilder::new(StatusCode::GONE)
            .title("Integration Disabled")
            .detail(err.to_string())
            .build(),
        RevenueError::ProviderMismatch { .. } | RevenueError::Provider { .. } => {
            ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Provider Error")
                .detail(err.to_string())
                .build()
        }
        RevenueError::EncryptionFailed { .. }
        | RevenueError::DecryptionFailed { .. }
        | RevenueError::Database(_) => {
            error!("revenue internal error: {}", err);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal Server Error")
                .detail("An internal error occurred while processing this request")
                .build()
        }
    }
}

/// Map AnalyticsError -> Problem. Internal DB errors are logged
/// server-side; the response body carries a generic message so internals
/// never leak to the client.
fn analytics_error_to_problem(err: AnalyticsError) -> Problem {
    match err {
        AnalyticsError::InvalidBucket(_) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid bucket")
            .detail(err.to_string())
            .build(),
        AnalyticsError::Database(ref db_err) => {
            error!(
                target: "temps_revenue::analytics",
                error = %db_err,
                debug = ?db_err,
                "analytics internal error",
            );
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal Server Error")
                .detail("Failed to compute analytics result")
                .build()
        }
    }
}

// ---------------------------------------------------------------- Handlers

/// List registered providers (what the UI needs to render the "Connect"
/// dropdown + its wizard instructions).
#[utoipa::path(
    get,
    path = "/revenue/providers",
    operation_id = "revenue_list_providers",
    responses((status = 200, body = Vec<ProviderDescriptor>)),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_list_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    let registry = state.integrations.providers();
    let descriptors: Vec<ProviderDescriptor> = registry
        .names()
        .into_iter()
        .filter_map(|n| registry.get(n))
        .map(|p| ProviderDescriptor {
            name: p.name().to_string(),
            display_name: p.display_name().to_string(),
            recommended_events: p
                .recommended_event_filter()
                .iter()
                .map(|e| e.to_string())
                .collect(),
        })
        .collect();
    Ok(Json(descriptors))
}

/// List revenue integrations for a project.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/revenue/integrations",
    operation_id = "revenue_list_integrations",
    responses((status = 200, body = Vec<IntegrationResponse>)),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_list_integrations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let rows = state
        .integrations
        .list_for_project(project_id)
        .await
        .map_err(revenue_error_to_problem)?;
    let responses: Vec<IntegrationResponse> = rows
        .iter()
        .map(|m| IntegrationResponse::from(IntegrationView::from(m)))
        .collect();
    Ok(Json(responses))
}

/// Create a new revenue integration. Response contains the generated
/// webhook path that the user must paste into their provider's dashboard.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/revenue/integrations",
    operation_id = "revenue_create_integration",
    request_body = CreateIntegrationBody,
    responses(
        (status = 201, body = IntegrationResponse),
        (status = 400, description = "Validation error"),
        (status = 409, description = "Already connected")
    ),
    params(("project_id" = i32, Path)),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_create_integration(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(body): Json<CreateIntegrationBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let input = CreateIntegrationInput {
        project_id,
        provider: body.provider.clone(),
        signing_secret: body.signing_secret,
    };
    let integration = state
        .integrations
        .create(input)
        .await
        .map_err(revenue_error_to_problem)?;

    let audit = RevenueIntegrationCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        integration_id: integration.id,
        project_id,
        provider: integration.provider.clone(),
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        error!("Failed to create revenue integration audit log: {}", e);
    }

    let view = IntegrationView::from(&integration);
    Ok((StatusCode::CREATED, Json(IntegrationResponse::from(view))))
}

/// Delete a revenue integration (permanent — use rotate_token to refresh
/// credentials without breaking history).
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/revenue/integrations/{integration_id}",
    operation_id = "revenue_delete_integration",
    responses((status = 204)),
    params(
        ("project_id" = i32, Path),
        ("integration_id" = i32, Path),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_delete_integration(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, integration_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    // Fetch first so the audit log captures what was deleted.
    let existing = state
        .integrations
        .get(project_id, integration_id)
        .await
        .map_err(revenue_error_to_problem)?;
    state
        .integrations
        .delete(project_id, integration_id)
        .await
        .map_err(revenue_error_to_problem)?;

    let audit = RevenueIntegrationDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        integration_id,
        project_id,
        provider: existing.provider,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        error!("Failed to create revenue integration audit log: {}", e);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Rotate the webhook path token. Returns the new integration state —
/// the user must paste the new URL into their provider's dashboard.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/revenue/integrations/{integration_id}/rotate-token",
    operation_id = "revenue_rotate_token",
    responses((status = 200, body = IntegrationResponse)),
    params(
        ("project_id" = i32, Path),
        ("integration_id" = i32, Path),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_rotate_token(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, integration_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let updated = state
        .integrations
        .rotate_path_token(project_id, integration_id)
        .await
        .map_err(revenue_error_to_problem)?;

    let audit = RevenueIntegrationTokenRotatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        integration_id,
        project_id,
        provider: updated.provider.clone(),
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        error!("Failed to create revenue integration audit log: {}", e);
    }

    Ok(Json(IntegrationResponse::from(IntegrationView::from(
        &updated,
    ))))
}

/// Replace the stored signing secret without rotating the webhook URL.
/// Use this after rotating the secret in the provider's dashboard.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/revenue/integrations/{integration_id}/update-secret",
    operation_id = "revenue_update_secret",
    request_body = UpdateSecretBody,
    responses(
        (status = 200, body = IntegrationResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Integration not found"),
    ),
    params(
        ("project_id" = i32, Path),
        ("integration_id" = i32, Path),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_update_secret(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, integration_id)): Path<(i32, i32)>,
    Json(body): Json<UpdateSecretBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let updated = state
        .integrations
        .update_signing_secret(project_id, integration_id, &body.signing_secret)
        .await
        .map_err(revenue_error_to_problem)?;

    let audit = RevenueIntegrationSecretRotatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        integration_id,
        project_id,
        provider: updated.provider.clone(),
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        error!("Failed to create revenue integration audit log: {}", e);
    }

    Ok(Json(IntegrationResponse::from(IntegrationView::from(
        &updated,
    ))))
}

/// Replace the typed provider config on an integration. Passing `null`
/// clears the config back to the accept-everything default. The config's
/// provider tag must match the integration's provider.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/revenue/integrations/{integration_id}/config",
    operation_id = "revenue_update_config",
    request_body = UpdateConfigBody,
    responses(
        (status = 200, body = IntegrationResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Integration not found"),
    ),
    params(
        ("project_id" = i32, Path),
        ("integration_id" = i32, Path),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_update_config(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, integration_id)): Path<(i32, i32)>,
    Json(body): Json<UpdateConfigBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let cleared = body.config.is_none();
    let updated = state
        .integrations
        .update_config(project_id, integration_id, body.config)
        .await
        .map_err(revenue_error_to_problem)?;

    let audit = RevenueIntegrationConfigUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        integration_id,
        project_id,
        provider: updated.provider.clone(),
        cleared,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        error!(
            "Failed to create revenue integration config audit log: {}",
            e
        );
    }

    Ok(Json(IntegrationResponse::from(IntegrationView::from(
        &updated,
    ))))
}

/// Current MRR / ARR / churn / ARPU for a project, in one currency.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/revenue/metrics/summary",
    operation_id = "revenue_metrics_summary",
    responses((status = 200, body = MetricsSummaryResponse)),
    params(("project_id" = i32, Path)),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_metrics_summary(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Path(project_id): Path<i32>,
    Query(q): Query<SummaryQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let currency = q.currency.as_deref().unwrap_or("usd");
    let summary = state
        .analytics
        .summary(project_id, currency)
        .await
        .map_err(analytics_error_to_problem)?;
    Ok(Json(MetricsSummaryResponse {
        currency: summary.currency,
        current_mrr_minor: summary.current_mrr_minor,
        current_arr_minor: summary.current_arr_minor,
        active_subscriptions: summary.active_subscriptions,
        active_customers: summary.active_customers,
        churned_last_30d: summary.churned_last_30d,
        arpu_minor: summary.arpu_minor,
    }))
}

/// Org-wide MRR total, summed across every project in the install.
/// Powers the single-number MRR card on the main dashboard.
#[utoipa::path(
    get,
    path = "/revenue/metrics/global-mrr",
    operation_id = "revenue_metrics_global_mrr",
    responses((status = 200, body = GlobalMrrResponse)),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_metrics_global_mrr(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Query(q): Query<SummaryQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    let currency = q.currency.as_deref().unwrap_or("usd").to_lowercase();
    let current_mrr_minor = state
        .analytics
        .global_mrr(&currency)
        .await
        .map_err(analytics_error_to_problem)?;
    let yesterday = Utc::now() - chrono::Duration::days(1);
    let previous_mrr_minor = state
        .analytics
        .global_mrr_at(&currency, yesterday)
        .await
        .map_err(analytics_error_to_problem)?;

    let change_percentage = if previous_mrr_minor > 0 {
        let delta = current_mrr_minor - previous_mrr_minor;
        Some((delta as f64 / previous_mrr_minor as f64) * 100.0)
    } else {
        None
    };

    Ok(Json(GlobalMrrResponse {
        currency,
        current_mrr_minor,
        previous_mrr_minor,
        change_percentage,
    }))
}

/// Org-wide revenue summary: MRR, paid cash (30d + all-time), refunds,
/// active subscriptions/customers, and transaction count. Powers the
/// header on the Revenue transactions page.
#[utoipa::path(
    get,
    path = "/revenue/metrics/global-summary",
    operation_id = "revenue_metrics_global_summary",
    responses((status = 200, body = GlobalRevenueSummaryResponse)),
    params(
        ("currency" = Option<String>, Query, description = "ISO-4217 currency code, default USD"),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_metrics_global_summary(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Query(q): Query<SummaryQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    let currency = q.currency.as_deref().unwrap_or("usd").to_lowercase();
    let summary = state
        .analytics
        .global_summary(&currency)
        .await
        .map_err(analytics_error_to_problem)?;
    Ok(Json(GlobalRevenueSummaryResponse {
        currency: summary.currency,
        current_mrr_minor: summary.current_mrr_minor,
        paid_last_30d_minor: summary.paid_last_30d_minor,
        refunded_last_30d_minor: summary.refunded_last_30d_minor,
        paid_all_time_minor: summary.paid_all_time_minor,
        refunded_all_time_minor: summary.refunded_all_time_minor,
        active_subscriptions: summary.active_subscriptions,
        active_customers: summary.active_customers,
        transactions_last_30d: summary.transactions_last_30d,
    }))
}

/// Bucketed MRR timeseries for the revenue chart.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/revenue/metrics/mrr",
    operation_id = "revenue_metrics_mrr",
    responses((status = 200, body = Vec<MrrBucketResponse>)),
    params(("project_id" = i32, Path)),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_metrics_mrr(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Path(project_id): Path<i32>,
    Query(q): Query<MrrQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (from, to) = default_range(q.from, q.to);
    let bucket =
        Bucket::parse(q.bucket.as_deref().unwrap_or("day")).map_err(analytics_error_to_problem)?;
    let currency = q.currency.as_deref().unwrap_or("usd");
    let rows = state
        .analytics
        .mrr_timeseries(project_id, currency, from, to, bucket)
        .await
        .map_err(analytics_error_to_problem)?;
    let out: Vec<MrrBucketResponse> = rows
        .into_iter()
        .map(|r| MrrBucketResponse {
            bucket: r.bucket,
            mrr_minor: r.mrr_minor,
            charge_total_minor: r.charge_total_minor,
            refund_total_minor: r.refund_total_minor,
            charge_count: r.charge_count,
        })
        .collect();
    Ok(Json(out))
}

/// New + churned customers per bucket.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/revenue/metrics/customers",
    operation_id = "revenue_metrics_customers",
    responses((status = 200, body = Vec<CustomerMovementResponse>)),
    params(("project_id" = i32, Path)),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_metrics_customers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Path(project_id): Path<i32>,
    Query(q): Query<MovementQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (from, to) = default_range(q.from, q.to);
    let bucket =
        Bucket::parse(q.bucket.as_deref().unwrap_or("day")).map_err(analytics_error_to_problem)?;
    let rows = state
        .analytics
        .customer_movement(project_id, from, to, bucket)
        .await
        .map_err(analytics_error_to_problem)?;
    let out: Vec<CustomerMovementResponse> = rows
        .into_iter()
        .map(|r| CustomerMovementResponse {
            bucket: r.bucket,
            new_customers: r.new_customers,
            churned_customers: r.churned_customers,
        })
        .collect();
    Ok(Json(out))
}

/// Recent ingested events for the activity feed.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/revenue/events",
    operation_id = "revenue_recent_events",
    responses((status = 200, body = Vec<RecentEventResponse>)),
    params(("project_id" = i32, Path)),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_recent_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Path(project_id): Path<i32>,
    Query(q): Query<RecentEventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let limit = q.limit.unwrap_or(50);
    let rows = state
        .analytics
        .recent_events(project_id, limit)
        .await
        .map_err(analytics_error_to_problem)?;
    let out: Vec<RecentEventResponse> = rows
        .into_iter()
        .map(|r| RecentEventResponse {
            occurred_at: r.occurred_at,
            event_type: r.event_type,
            customer_ref: r.customer_ref,
            amount_minor: r.amount_minor,
            currency: r.currency,
            mrr_minor: r.mrr_minor,
        })
        .collect();
    Ok(Json(out))
}

/// Org-wide revenue events across every project. Powers the revenue
/// transactions page. Supports filtering by project, date range, and
/// event type.
#[utoipa::path(
    get,
    path = "/revenue/events",
    operation_id = "revenue_global_events",
    responses((status = 200, body = Vec<GlobalRecentEventResponse>)),
    params(
        ("project_id" = Option<i32>, Query, description = "Filter to a single project"),
        ("from" = Option<String>, Query, description = "Lower bound (inclusive), ISO-8601"),
        ("to" = Option<String>, Query, description = "Upper bound (inclusive), ISO-8601"),
        ("event_types" = Option<String>, Query, description = "Comma-separated event types (e.g. `invoice.paid,charge.succeeded`)"),
        ("limit" = Option<i64>, Query, description = "Max rows, default 100, max 500"),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_global_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Query(q): Query<GlobalEventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    let event_types = q.event_types.as_deref().map(|s| {
        s.split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
    });
    let filter = GlobalEventsFilter {
        project_id: q.project_id,
        from: q.from,
        to: q.to,
        event_types,
        limit: q.limit.unwrap_or(100),
    };
    let rows = state
        .analytics
        .global_recent_events(filter)
        .await
        .map_err(analytics_error_to_problem)?;
    let out: Vec<GlobalRecentEventResponse> = rows
        .into_iter()
        .map(|r| GlobalRecentEventResponse {
            id: r.id,
            project_id: r.project_id,
            project_name: r.project_name,
            occurred_at: r.occurred_at,
            event_type: r.event_type,
            customer_ref: r.customer_ref,
            amount_minor: r.amount_minor,
            currency: r.currency,
            mrr_minor: r.mrr_minor,
        })
        .collect();
    Ok(Json(out))
}

/// Max uploaded CSV size. Stripe's largest practical exports (tens of
/// thousands of rows) are well under this.
const MAX_CSV_BYTES: usize = 50 * 1024 * 1024;

async fn read_csv_field(mut multipart: Multipart) -> Result<Vec<u8>, Problem> {
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Multipart Error")
            .detail(e.to_string())
            .build()
    })? {
        if field.name().unwrap_or_default() == "file" {
            let data = field.bytes().await.map_err(|e| {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .title("File Read Error")
                    .detail(e.to_string())
                    .build()
            })?;
            if data.len() > MAX_CSV_BYTES {
                return Err(ErrorBuilder::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .title("CSV Too Large")
                    .detail(format!(
                        "CSV size {} bytes exceeds maximum of {} bytes",
                        data.len(),
                        MAX_CSV_BYTES
                    ))
                    .build());
            }
            return Ok(data.to_vec());
        }
    }
    Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
        .title("Missing File")
        .detail("Multipart body must include a 'file' field containing the CSV")
        .build())
}

/// Import a Stripe subscriptions CSV export. Use this to backfill MRR /
/// active subscriptions when migrating from Stripe without providing
/// API keys. Webhooks remain the source of truth for live updates —
/// CSV rows never overwrite newer webhook state.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/revenue/integrations/{integration_id}/import/subscriptions",
    operation_id = "revenue_import_subscriptions_csv",
    responses(
        (status = 200, body = ImportOutcomeResponse),
        (status = 400, description = "Malformed CSV or wrong provider"),
        (status = 404, description = "Integration not found"),
        (status = 413, description = "CSV too large"),
    ),
    params(
        ("project_id" = i32, Path),
        ("integration_id" = i32, Path),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_import_subscriptions_csv(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, integration_id)): Path<(i32, i32)>,
    multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let bytes = read_csv_field(multipart).await?;
    let outcome = state
        .import
        .import_subscriptions_csv(project_id, integration_id, &bytes)
        .await
        .map_err(revenue_error_to_problem)?;

    // Fetch provider name for audit — cheap since the import already
    // validated the integration exists.
    let provider = state
        .integrations
        .get(project_id, integration_id)
        .await
        .map(|m| m.provider)
        .unwrap_or_else(|_| "stripe".to_string());

    let audit = RevenueCsvImportedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        integration_id,
        project_id,
        provider,
        kind: "subscriptions".into(),
        rows_read: outcome.rows_read,
        inserted: outcome.inserted,
        updated: outcome.updated,
        skipped: outcome.skipped_stale + outcome.skipped_invalid,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        error!("Failed to create revenue CSV import audit log: {}", e);
    }

    Ok(Json(ImportOutcomeResponse::from(outcome)))
}

/// Import a Stripe invoices CSV export. Each paid invoice becomes an
/// `invoice.paid` event so historical MRR/charge totals populate the
/// timeseries. Ingestion is idempotent: re-uploading the same file is a
/// no-op.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/revenue/integrations/{integration_id}/import/invoices",
    operation_id = "revenue_import_invoices_csv",
    responses(
        (status = 200, body = ImportOutcomeResponse),
        (status = 400, description = "Malformed CSV or wrong provider"),
        (status = 404, description = "Integration not found"),
        (status = 413, description = "CSV too large"),
    ),
    params(
        ("project_id" = i32, Path),
        ("integration_id" = i32, Path),
    ),
    tag = "Revenue",
    security(("bearer_auth" = []))
)]
async fn revenue_import_invoices_csv(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ManagementState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, integration_id)): Path<(i32, i32)>,
    multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let bytes = read_csv_field(multipart).await?;
    let outcome = state
        .import
        .import_invoices_csv(project_id, integration_id, &bytes)
        .await
        .map_err(revenue_error_to_problem)?;

    let provider = state
        .integrations
        .get(project_id, integration_id)
        .await
        .map(|m| m.provider)
        .unwrap_or_else(|_| "stripe".to_string());

    let audit = RevenueCsvImportedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        integration_id,
        project_id,
        provider,
        kind: "invoices".into(),
        rows_read: outcome.rows_read,
        inserted: outcome.inserted,
        updated: outcome.updated,
        skipped: outcome.skipped_stale + outcome.skipped_invalid,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        error!("Failed to create revenue CSV import audit log: {}", e);
    }

    Ok(Json(ImportOutcomeResponse::from(outcome)))
}

fn default_range(
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let now = Utc::now();
    let to = to.unwrap_or(now);
    let from = from.unwrap_or(now - chrono::Duration::days(30));
    (from, to)
}

pub fn configure_management_routes() -> Router<Arc<ManagementState>> {
    Router::new()
        .route("/revenue/providers", get(revenue_list_providers))
        .route(
            "/projects/{project_id}/revenue/integrations",
            get(revenue_list_integrations).post(revenue_create_integration),
        )
        .route(
            "/projects/{project_id}/revenue/integrations/{integration_id}",
            axum::routing::delete(revenue_delete_integration),
        )
        .route(
            "/projects/{project_id}/revenue/integrations/{integration_id}/rotate-token",
            post(revenue_rotate_token),
        )
        .route(
            "/projects/{project_id}/revenue/integrations/{integration_id}/update-secret",
            post(revenue_update_secret),
        )
        .route(
            "/projects/{project_id}/revenue/integrations/{integration_id}/config",
            post(revenue_update_config),
        )
        .route(
            "/revenue/metrics/global-mrr",
            get(revenue_metrics_global_mrr),
        )
        .route(
            "/revenue/metrics/global-summary",
            get(revenue_metrics_global_summary),
        )
        .route("/revenue/events", get(revenue_global_events))
        .route(
            "/projects/{project_id}/revenue/metrics/summary",
            get(revenue_metrics_summary),
        )
        .route(
            "/projects/{project_id}/revenue/metrics/mrr",
            get(revenue_metrics_mrr),
        )
        .route(
            "/projects/{project_id}/revenue/metrics/customers",
            get(revenue_metrics_customers),
        )
        .route(
            "/projects/{project_id}/revenue/events",
            get(revenue_recent_events),
        )
        .route(
            "/projects/{project_id}/revenue/integrations/{integration_id}/import/subscriptions",
            post(revenue_import_subscriptions_csv).layer(DefaultBodyLimit::max(MAX_CSV_BYTES)),
        )
        .route(
            "/projects/{project_id}/revenue/integrations/{integration_id}/import/invoices",
            post(revenue_import_invoices_csv).layer(DefaultBodyLimit::max(MAX_CSV_BYTES)),
        )
}
