// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime AI model catalog endpoints (ADR 112): status, settings, "Refresh
//! now", the stored catalog, and the enabled models no price covers.
//!
//! Reading needs `ai_gateway:read`. Changing the catalog changes the prices
//! every project's spend is estimated at, so writes need the same operator
//! standing as spend limits: `ai_gateway:write` and `system:admin`.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, AuthContext, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::{IntoParams, OpenApi};

use crate::handlers::types::AiGatewayAppState;
use crate::services::model_catalog::{
    ModelCatalogEntry, ModelCatalogEntryFilter, ModelCatalogEntryPage, ModelCatalogError,
    ModelCatalogRefreshResult, ModelCatalogSettings, ModelCatalogSettingsUpdate,
    ModelCatalogStatus, RefreshTrigger, UnpricedModel, UnpricedModelReport,
};

pub(crate) const MODEL_CATALOG_SETTINGS_UPDATED: &str = "AI_MODEL_CATALOG_SETTINGS_UPDATED";
pub(crate) const MODEL_CATALOG_REFRESHED: &str = "AI_MODEL_CATALOG_REFRESHED";

#[derive(OpenApi)]
#[openapi(
    paths(
        get_model_catalog,
        update_model_catalog_settings,
        refresh_model_catalog,
        list_model_catalog_entries,
        list_unpriced_models
    ),
    components(schemas(
        ModelCatalogStatus,
        ModelCatalogSettings,
        ModelCatalogSettingsUpdate,
        ModelCatalogRefreshResult,
        ModelCatalogEntry,
        ModelCatalogEntryPage,
        UnpricedModel,
        UnpricedModelReport
    )),
    info(
        title = "AI Gateway Model Catalog API",
        description = "Runtime model catalog: prices and models newer than this release",
        version = "1.0.0"
    ),
    tags((name = "AI Gateway Model Catalog", description = "Runtime model catalog endpoints"))
)]
pub struct AiGatewayModelCatalogApiDoc;

pub fn configure_model_catalog_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/model-catalog", get(get_model_catalog))
        .route(
            "/ai/model-catalog/settings",
            patch(update_model_catalog_settings),
        )
        .route("/ai/model-catalog/refresh", post(refresh_model_catalog))
        .route("/ai/model-catalog/models", get(list_model_catalog_entries))
        .route("/ai/model-catalog/unpriced", get(list_unpriced_models))
}

impl From<ModelCatalogError> for Problem {
    fn from(error: ModelCatalogError) -> Self {
        let (status, title) = match &error {
            ModelCatalogError::InvalidSourceUrl { .. } => {
                (StatusCode::BAD_REQUEST, "Invalid Catalog Source")
            }
            ModelCatalogError::InvalidSettings { .. } => {
                (StatusCode::BAD_REQUEST, "Invalid Catalog Settings")
            }
            ModelCatalogError::Fetch { .. } => (StatusCode::BAD_GATEWAY, "Catalog Unreachable"),
            ModelCatalogError::Timeout { .. } => {
                (StatusCode::GATEWAY_TIMEOUT, "Catalog Fetch Timed Out")
            }
            ModelCatalogError::HttpStatus { .. } => {
                (StatusCode::BAD_GATEWAY, "Catalog Source Refused")
            }
            ModelCatalogError::TooLarge { .. } => (StatusCode::BAD_GATEWAY, "Catalog Too Large"),
            ModelCatalogError::Parse { .. } => (StatusCode::BAD_GATEWAY, "Invalid Catalog"),
            ModelCatalogError::Empty { .. } => (StatusCode::BAD_GATEWAY, "Empty Catalog"),
            ModelCatalogError::TooManyEntries { .. } => {
                (StatusCode::BAD_GATEWAY, "Catalog Too Large")
            }
            ModelCatalogError::RefreshInProgress => (StatusCode::CONFLICT, "Refresh In Progress"),
            ModelCatalogError::Database(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Model Catalog Error")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

/// Catalog writes reprice every project's usage, so they need operator
/// standing, as spend-limit writes do: `ai_gateway:write` alone is grantable
/// to a role or key confined to one project.
fn require_catalog_writer(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, AiGatewayWrite);
    permission_guard!(auth, SystemAdmin);
    Ok(())
}

/// `host[:port]` of a catalog URL, for the audit log: a custom URL's path or
/// query may carry an access token.
fn source_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

#[derive(Debug, Clone, Serialize)]
struct ModelCatalogAudit {
    #[serde(skip)]
    operation: &'static str,
    user_id: Option<i32>,
    #[serde(skip)]
    ip_address: Option<String>,
    #[serde(skip)]
    user_agent: String,
    /// Host of the catalog source, never its path or query.
    source_host: Option<String>,
    /// Whether the source is the catalog published in the Temps repository.
    default_source: bool,
    /// Settings fields the change touched.
    changed_fields: Vec<&'static str>,
    auto_refresh: Option<bool>,
    refresh_interval_hours: Option<i32>,
    discover_provider_models: Option<bool>,
    /// For a refresh: models stored, prices applied, entries rejected.
    stored: Option<usize>,
    applied_prices: Option<usize>,
    skipped: Option<usize>,
}

impl AuditOperation for ModelCatalogAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

impl ModelCatalogAudit {
    fn new(
        operation: &'static str,
        auth: &AuthContext,
        metadata: &RequestMetadata,
        settings: &ModelCatalogSettings,
    ) -> Self {
        Self {
            operation,
            user_id: auth.user_id_opt(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
            source_host: source_host(&settings.effective_source_url),
            default_source: settings.source_url.is_none(),
            changed_fields: Vec::new(),
            auto_refresh: None,
            refresh_interval_hours: None,
            discover_provider_models: None,
            stored: None,
            applied_prices: None,
            skipped: None,
        }
    }
}

/// Write an audit record. A failed write is logged; the change already
/// happened.
async fn write_audit(app_state: &AiGatewayAppState, audit: ModelCatalogAudit) {
    if let Err(audit_error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            operation = audit.operation,
            error = %audit_error,
            "Failed to create audit log for the AI model catalog"
        );
    }
}

/// The runtime model catalog's settings and the outcome of its last refresh.
#[utoipa::path(
    tag = "AI Gateway Model Catalog",
    get,
    path = "/ai/model-catalog",
    responses(
        (status = 200, description = "Catalog status", body = ModelCatalogStatus),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Database error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_model_catalog(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);
    Ok(Json(app_state.model_catalog_service.status().await?))
}

/// Change where the catalog comes from and how it is refreshed.
#[utoipa::path(
    tag = "AI Gateway Model Catalog",
    patch,
    path = "/ai/model-catalog/settings",
    request_body = ModelCatalogSettingsUpdate,
    responses(
        (status = 200, description = "Settings saved", body = ModelCatalogSettings),
        (status = 400, description = "Invalid source URL or interval", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Database error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn update_model_catalog_settings(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(update): Json<ModelCatalogSettingsUpdate>,
) -> Result<impl IntoResponse, Problem> {
    require_catalog_writer(&auth)?;
    let (settings, changed) = app_state
        .model_catalog_service
        .update_settings(update)
        .await?;
    if !changed.is_empty() {
        let mut audit =
            ModelCatalogAudit::new(MODEL_CATALOG_SETTINGS_UPDATED, &auth, &metadata, &settings);
        audit.auto_refresh = Some(settings.auto_refresh);
        audit.refresh_interval_hours = Some(settings.refresh_interval_hours);
        audit.discover_provider_models = Some(settings.discover_provider_models);
        audit.changed_fields = changed;
        write_audit(&app_state, audit).await;
    }
    Ok(Json(settings))
}

/// Fetch the catalog now, store it and apply its prices. On failure the
/// previous catalog stays in use and the reason is recorded in the status.
#[utoipa::path(
    tag = "AI Gateway Model Catalog",
    post,
    path = "/ai/model-catalog/refresh",
    responses(
        (status = 200, description = "Catalog refreshed", body = ModelCatalogRefreshResult),
        (status = 400, description = "The configured source URL is not allowed", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "A refresh is already running", body = ProblemDetails),
        (status = 502, description = "The source failed or served an invalid catalog", body = ProblemDetails),
        (status = 504, description = "The source did not answer in time", body = ProblemDetails),
        (status = 500, description = "Database error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn refresh_model_catalog(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    require_catalog_writer(&auth)?;
    let result = app_state
        .model_catalog_service
        .refresh(RefreshTrigger::Manual)
        .await?;
    let mut audit = ModelCatalogAudit::new(
        MODEL_CATALOG_REFRESHED,
        &auth,
        &metadata,
        &result.status.settings,
    );
    audit.stored = Some(result.stored);
    audit.applied_prices = Some(result.applied_prices);
    audit.skipped = Some(result.skipped);
    write_audit(&app_state, audit).await;
    Ok(Json(result))
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ModelCatalogEntryQuery {
    /// Temps provider id (`openai`, `anthropic`, `gemini`, `xai`, `openrouter`).
    pub provider: Option<String>,
    /// Case-insensitive part of the model id or name.
    pub search: Option<String>,
    /// Page number, from 1 (default 1).
    pub page: Option<u64>,
    /// Page size (default 20, max 100).
    pub page_size: Option<u64>,
}

/// The stored catalog's models, newest release first.
#[utoipa::path(
    tag = "AI Gateway Model Catalog",
    get,
    path = "/ai/model-catalog/models",
    params(ModelCatalogEntryQuery),
    responses(
        (status = 200, description = "A page of catalog models", body = ModelCatalogEntryPage),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Database error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_model_catalog_entries(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(query): Query<ModelCatalogEntryQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);
    let page = app_state
        .model_catalog_service
        .list_entries(
            ModelCatalogEntryFilter {
                provider: query.provider,
                search: query.search,
            },
            query.page.unwrap_or(1),
            query.page_size.unwrap_or(20),
        )
        .await?;
    Ok(Json(page))
}

/// Models enabled on a provider key that no price covers. Their usage is
/// charged the worst-case rate, so spend caps still hold, and is reported as
/// "price unknown".
#[utoipa::path(
    tag = "AI Gateway Model Catalog",
    get,
    path = "/ai/model-catalog/unpriced",
    responses(
        (status = 200, description = "Enabled models without a price", body = UnpricedModelReport),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Database error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_unpriced_models(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);
    Ok(Json(
        app_state.model_catalog_service.unpriced_models().await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Mutex;
    use temps_authz::permissions::Role;
    use temps_entities::users;
    use tower::ServiceExt;

    #[derive(Default)]
    struct RecordingAuditLogger {
        records: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.records
                .lock()
                .unwrap()
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    fn auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 7,
                name: "Catalog Tester".to_string(),
                email: "catalog-tester@example.com".to_string(),
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

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "192.0.2.10".to_string(),
            user_agent: "catalog-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.com".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.com".to_string(),
            is_secure: true,
        }
    }

    fn app(
        db: Arc<sea_orm::DatabaseConnection>,
        audit: Arc<RecordingAuditLogger>,
        auth: Option<AuthContext>,
    ) -> Router {
        let state = crate::handlers::types::test_app_state(db, audit);
        let router = configure_model_catalog_routes()
            .with_state(state)
            .layer(Extension(metadata()));
        match auth {
            Some(auth) => router.layer(Extension(auth)),
            None => router,
        }
    }

    async fn call(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let response = app
            .oneshot(
                request
                    .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn disconnected() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::DatabaseConnection::Disconnected)
    }

    #[tokio::test]
    async fn every_route_needs_a_signed_in_caller() {
        for (method, uri, body) in [
            ("GET", "/ai/model-catalog", None),
            ("PATCH", "/ai/model-catalog/settings", Some("{}")),
            ("POST", "/ai/model-catalog/refresh", None),
            ("GET", "/ai/model-catalog/models", None),
            ("GET", "/ai/model-catalog/unpriced", None),
        ] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let (status, _) = call(app(disconnected(), audit, None), method, uri, body).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn only_operators_may_change_or_refresh_the_catalog() {
        for role in [Role::User, Role::Reader] {
            for (method, uri, body) in [
                (
                    "PATCH",
                    "/ai/model-catalog/settings",
                    Some(r#"{"auto_refresh":false}"#),
                ),
                ("POST", "/ai/model-catalog/refresh", None),
            ] {
                let audit = Arc::new(RecordingAuditLogger::default());
                let (status, body) = call(
                    app(disconnected(), audit.clone(), Some(auth(role.clone()))),
                    method,
                    uri,
                    body,
                )
                .await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{role:?} {method} {uri}");
                assert!(body.contains("Insufficient Permissions"), "{body}");
                assert!(audit.records.lock().unwrap().is_empty());
            }
        }
        // `ai_gateway:write` alone, as a project-confined key would hold it.
        let mut confined = auth(Role::Reader);
        confined.custom_permissions =
            Some(vec![temps_authz::permissions::Permission::AiGatewayWrite]);
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _) = call(
            app(disconnected(), audit, Some(confined)),
            "POST",
            "/ai/model-catalog/refresh",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_invalid_source_is_a_problem_document_and_nothing_is_audited() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, body) = call(
            app(disconnected(), audit.clone(), Some(auth(Role::Admin))),
            "PATCH",
            "/ai/model-catalog/settings",
            Some(r#"{"source_url":"http://10.0.0.5/catalog.json"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("Invalid Catalog Source"), "{body}");
        assert!(body.contains("only https:// URLs are allowed"), "{body}");
        assert!(audit.records.lock().unwrap().is_empty());
    }

    #[test]
    fn every_catalog_error_maps_to_its_status() {
        let url = || "https://catalog.example.com/api.json".to_string();
        for (error, status) in [
            (
                ModelCatalogError::InvalidSourceUrl {
                    url: url(),
                    reason: "x".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                ModelCatalogError::InvalidSettings {
                    field: "refresh_interval_hours",
                    reason: "x".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                ModelCatalogError::Fetch {
                    url: url(),
                    reason: "x".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                ModelCatalogError::Timeout {
                    url: url(),
                    seconds: 30,
                },
                StatusCode::GATEWAY_TIMEOUT,
            ),
            (
                ModelCatalogError::HttpStatus {
                    url: url(),
                    status: 503,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                ModelCatalogError::TooLarge {
                    url: url(),
                    limit_bytes: 1,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                ModelCatalogError::Parse {
                    url: url(),
                    reason: "x".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                ModelCatalogError::Empty {
                    url: url(),
                    skipped: 0,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                ModelCatalogError::TooManyEntries {
                    url: url(),
                    limit: 1,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (ModelCatalogError::RefreshInProgress, StatusCode::CONFLICT),
            (
                ModelCatalogError::Database(sea_orm::DbErr::Custom("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            let detail = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status, "{detail}");
        }
    }

    #[test]
    fn the_audit_records_only_the_source_host() {
        assert_eq!(
            source_host("https://catalog.example.com:8443/v1/api.json?token=secret").as_deref(),
            Some("catalog.example.com:8443")
        );
        assert_eq!(source_host("not a url"), None);
    }

    #[tokio::test]
    async fn an_operator_reads_status_and_a_settings_change_is_audited() {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&format!("{error:#}")) =>
            {
                println!(
                    "Skipping: no container runtime for the PostgreSQL test database: {error:#}"
                );
                return;
            }
            Err(error) => panic!("test database setup failed: {error:#}"),
        };
        let db = test_db.connection_arc();
        let audit = Arc::new(RecordingAuditLogger::default());

        let (status, body) = call(
            app(db.clone(), audit.clone(), Some(auth(Role::Reader))),
            "GET",
            "/ai/model-catalog",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let status_body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status_body["settings"]["auto_refresh"], true);
        assert_eq!(status_body["entry_count"], 0);

        let (status, body) = call(
            app(db.clone(), audit.clone(), Some(auth(Role::Admin))),
            "PATCH",
            "/ai/model-catalog/settings",
            Some(r#"{"source_url":"https://catalog.example.com/v1/api.json?token=secret","refresh_interval_hours":12}"#),
        )
        .await;
        // A token-bearing URL would be returned to every gateway reader and
        // logged on refresh, so it is refused and not echoed back.
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!body.contains("secret"), "{body}");
        assert!(audit.records.lock().unwrap().is_empty());

        let (status, body) = call(
            app(db.clone(), audit.clone(), Some(auth(Role::Admin))),
            "PATCH",
            "/ai/model-catalog/settings",
            Some(r#"{"source_url":"https://catalog.example.com/v1/api.json","refresh_interval_hours":12}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let saved: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(saved["refresh_interval_hours"], 12);

        let records = audit.records.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, MODEL_CATALOG_SETTINGS_UPDATED);
        assert!(
            records[0].1.contains("catalog.example.com"),
            "{}",
            records[0].1
        );
        assert!(!records[0].1.contains("secret"), "{}", records[0].1);
        assert!(
            records[0].1.contains("refresh_interval_hours"),
            "{}",
            records[0].1
        );

        // Saving the same values again changes nothing and is not audited.
        let (status, _) = call(
            app(db.clone(), audit.clone(), Some(auth(Role::Admin))),
            "PATCH",
            "/ai/model-catalog/settings",
            Some(r#"{"refresh_interval_hours":12}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit.records.lock().unwrap().len(), 1);

        let (status, body) = call(
            app(db.clone(), audit.clone(), Some(auth(Role::Reader))),
            "GET",
            "/ai/model-catalog/models?page=1&page_size=500",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(page["page_size"], 100);

        let (status, body) = call(
            app(db, audit, Some(auth(Role::Reader))),
            "GET",
            "/ai/model-catalog/unpriced",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"total\":0"), "{body}");
    }
}
