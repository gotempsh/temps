// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP API behind Settings › Telemetry.
//!
//! - `GET /settings/telemetry` — whether anonymous telemetry is on, what
//!   decided it, and exactly which events this binary can send.
//! - `PATCH /settings/telemetry` — admin-only on/off switch, persisted in the
//!   settings row, audit-logged, applied without a restart.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, Permission, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::{self, Problem};
use temps_core::telemetry::{TelemetryEventCategory, TelemetryEventKind};
use temps_core::{AuditLogger, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::settings::{
    TelemetrySettingsError, TelemetrySettingsService, TelemetryStatus, TelemetryStatusSource,
};

/// Public documentation of what is collected and how to opt out.
pub const TELEMETRY_PRIVACY_DOC_URL: &str =
    "https://temps.sh/docs/data-ownership-and-privacy#anonymous-telemetry";

/// The host-level kill switch, named in the response so the console can tell
/// operators exactly what to unset.
pub const TELEMETRY_ENV_VAR: &str = "TEMPS_TELEMETRY";

/// Handler state.
#[derive(Clone)]
pub struct TelemetryAppState {
    pub settings_service: Arc<TelemetrySettingsService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

/// One event this binary can send.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TelemetryEventInfo {
    /// Wire name, e.g. `deploy_succeeded`.
    #[schema(example = "deploy_succeeded")]
    pub name: String,
    pub category: TelemetryEventCategory,
}

/// Current anonymous telemetry state and disclosure.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TelemetryStatusResponse {
    /// Whether this server is sending anonymous telemetry right now.
    pub enabled: bool,
    /// What decided `enabled`.
    pub source: TelemetryStatusSource,
    /// `TEMPS_TELEMETRY` forces telemetry off on this server. The console
    /// cannot override it; remove the variable and restart to change.
    pub env_opted_out: bool,
    /// Name of the environment variable that forces telemetry off.
    #[schema(example = "TEMPS_TELEMETRY")]
    pub env_var: String,
    /// The admin's stored choice; `null` when nobody has chosen yet.
    pub admin_preference: Option<bool>,
    /// The built-in default that applies when nobody has chosen.
    pub default_enabled: bool,
    /// Whether the caller may change the setting (instance admins only).
    pub can_manage: bool,
    /// Random identifier events are reported under. Not derived from the
    /// host, domain or any account. `null` if the reporter could not start.
    pub anonymous_id: Option<String>,
    /// Host events are sent to.
    #[schema(example = "telemetry.temps.sh")]
    pub endpoint_host: Option<String>,
    /// Version string stamped on every event.
    pub temps_version: Option<String>,
    /// Every event this binary can send.
    pub events: Vec<TelemetryEventInfo>,
    /// Documentation of what is collected and what is never collected.
    pub privacy_doc_url: String,
}

/// Turn anonymous telemetry on or off.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateTelemetrySettingsRequest {
    pub enabled: bool,
}

/// Audit record for a telemetry preference change.
#[derive(Debug, Clone, Serialize)]
struct TelemetrySettingUpdatedAudit {
    context: AuditContext,
    previous_preference: Option<bool>,
    enabled: bool,
    effective_enabled: bool,
    env_opted_out: bool,
}

impl AuditOperation for TelemetrySettingUpdatedAudit {
    fn operation_type(&self) -> String {
        "TELEMETRY_SETTING_UPDATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|error| {
            anyhow::anyhow!("failed to serialize telemetry setting audit event: {error}")
        })
    }
}

impl From<TelemetrySettingsError> for Problem {
    fn from(error: TelemetrySettingsError) -> Self {
        match error {
            TelemetrySettingsError::PreferenceRead { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Telemetry Setting Unavailable")
                    .with_detail(error.to_string())
            }
            TelemetrySettingsError::PreferenceWrite { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Telemetry Setting Not Saved")
                    .with_detail(error.to_string())
            }
        }
    }
}

fn endpoint_host(endpoint: &str) -> Option<String> {
    let url = url::Url::parse(endpoint).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn event_catalog() -> Vec<TelemetryEventInfo> {
    TelemetryEventKind::all()
        .iter()
        .map(|kind| TelemetryEventInfo {
            name: kind.as_str().to_string(),
            category: kind.category(),
        })
        .collect()
}

fn status_response(
    status: TelemetryStatus,
    default_enabled: bool,
    can_manage: bool,
) -> TelemetryStatusResponse {
    TelemetryStatusResponse {
        enabled: status.enabled,
        source: status.source,
        env_opted_out: status.env_opted_out,
        env_var: TELEMETRY_ENV_VAR.to_string(),
        admin_preference: status.admin_preference,
        default_enabled,
        can_manage,
        anonymous_id: status.anonymous_id,
        endpoint_host: status.endpoint.as_deref().and_then(endpoint_host),
        temps_version: status.temps_version,
        events: event_catalog(),
        privacy_doc_url: TELEMETRY_PRIVACY_DOC_URL.to_string(),
    }
}

/// Anonymous telemetry status and disclosure
///
/// Reports whether this server sends anonymous product telemetry, what decided
/// it (environment kill switch, admin setting or default), the random instance
/// ID, the destination host, and every event type the binary can send.
#[utoipa::path(
    tag = "Telemetry",
    get,
    path = "/settings/telemetry",
    responses(
        (status = 200, description = "Current telemetry state", body = TelemetryStatusResponse),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 500, description = "Stored preference could not be read", body = temps_core::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_telemetry_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<TelemetryAppState>,
) -> Result<Json<TelemetryStatusResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let status = state.settings_service.status().await?;
    Ok(Json(status_response(
        status,
        state.settings_service.default_enabled(),
        auth.has_permission(&Permission::SystemAdmin),
    )))
}

/// Turn anonymous telemetry on or off
///
/// Instance admins only. Persisted in the settings row, audit-logged as
/// `TELEMETRY_SETTING_UPDATED`, and applied without a restart. When
/// `TEMPS_TELEMETRY` forces telemetry off, the choice is still saved but has
/// no effect until the variable is removed; the response says so via
/// `source = environment`.
#[utoipa::path(
    tag = "Telemetry",
    patch,
    path = "/settings/telemetry",
    request_body = UpdateTelemetrySettingsRequest,
    responses(
        (status = 200, description = "Setting saved; returns the new state", body = TelemetryStatusResponse),
        (status = 400, description = "Invalid request body", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Instance admin required", body = temps_core::ProblemDetails),
        (status = 500, description = "Setting could not be saved", body = temps_core::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_telemetry_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<TelemetryAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateTelemetrySettingsRequest>,
) -> Result<Json<TelemetryStatusResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);

    // Best effort: the previous value only enriches the audit record, so a
    // read failure must not block the admin from turning telemetry off.
    let previous_preference = match state.settings_service.status().await {
        Ok(status) => status.admin_preference,
        Err(read_error) => {
            error!(
                error = %read_error,
                "Could not read the previous telemetry preference for the audit record"
            );
            None
        }
    };

    let status = state.settings_service.set_enabled(request.enabled).await?;

    let audit = TelemetrySettingUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        previous_preference,
        enabled: request.enabled,
        effective_enabled: status.enabled,
        env_opted_out: status.env_opted_out,
    };
    if let Err(audit_error) = state.audit_service.create_audit_log(&audit).await {
        error!(
            error = %audit_error,
            "Failed to create audit log for TELEMETRY_SETTING_UPDATED"
        );
    }

    Ok(Json(status_response(
        status,
        state.settings_service.default_enabled(),
        true,
    )))
}

/// Routes, to be given [`TelemetryAppState`].
pub fn configure_routes() -> Router<TelemetryAppState> {
    Router::new().route(
        "/settings/telemetry",
        get(get_telemetry_settings).patch(update_telemetry_settings),
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(get_telemetry_settings, update_telemetry_settings),
    components(schemas(
        TelemetryStatusResponse,
        TelemetryStatusSource,
        TelemetryEventInfo,
        TelemetryEventCategory,
        UpdateTelemetrySettingsRequest,
        temps_core::ProblemDetails,
    )),
    tags((name = "Telemetry", description = "Anonymous product telemetry status and opt-out"))
)]
pub struct TelemetryApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::MemoryStore;
    use crate::settings::TelemetryPreferenceStore;
    use crate::TelemetryService;
    use std::sync::Mutex;
    use temps_auth::{AuthContext, Role};

    #[derive(Default)]
    struct RecordingAuditLogger {
        records: Mutex<Vec<(String, serde_json::Value)>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            let body: serde_json::Value = serde_json::from_str(&operation.serialize()?)?;
            self.records
                .lock()
                .expect("audit lock")
                .push((operation.operation_type(), body));
            Ok(())
        }
    }

    fn test_user(id: i32) -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
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

    fn auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_persisted_session(test_user(7), role, 1))
    }

    fn metadata() -> Extension<RequestMetadata> {
        Extension(RequestMetadata {
            ip_address: "192.0.2.10".to_string(),
            user_agent: "telemetry-settings-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        })
    }

    /// Serializes tests that construct a reporter: `TEMPS_TELEMETRY` and the
    /// endpoint override are process-wide.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn reporter(env_opt_out: bool) -> TelemetryService {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "temps-telemetry-handler-{}",
            uuid::Uuid::new_v4().simple()
        ));
        if env_opt_out {
            std::env::set_var("TEMPS_TELEMETRY", "0");
        } else {
            std::env::remove_var("TEMPS_TELEMETRY");
        }
        // Never point a test reporter at the real ingest endpoint.
        std::env::set_var("TEMPS_TELEMETRY_ENDPOINT", "http://127.0.0.1:9/v1/events");
        let service = TelemetryService::new(&dir, "0.0.0-test").expect("reporter");
        std::env::remove_var("TEMPS_TELEMETRY");
        std::env::remove_var("TEMPS_TELEMETRY_ENDPOINT");
        service
    }

    fn state(
        reporter: Option<TelemetryService>,
        store: Arc<dyn TelemetryPreferenceStore>,
    ) -> (TelemetryAppState, Arc<RecordingAuditLogger>) {
        let audit = Arc::new(RecordingAuditLogger::default());
        (
            TelemetryAppState {
                settings_service: Arc::new(TelemetrySettingsService::new(reporter, store)),
                audit_service: audit.clone(),
            },
            audit,
        )
    }

    #[tokio::test]
    async fn status_reports_default_and_discloses_every_event() {
        let (state, _) = state(Some(reporter(false)), Arc::new(MemoryStore::default()));
        let Json(body) = get_telemetry_settings(auth(Role::Admin), State(state))
            .await
            .expect("status");
        assert!(body.enabled);
        assert_eq!(body.source, TelemetryStatusSource::Default);
        assert_eq!(body.admin_preference, None);
        assert!(body.default_enabled);
        assert!(body.can_manage);
        assert_eq!(body.env_var, "TEMPS_TELEMETRY");
        assert_eq!(body.endpoint_host.as_deref(), Some("127.0.0.1:9"));
        assert!(body
            .anonymous_id
            .as_deref()
            .is_some_and(|id| id.starts_with("inst_")));
        assert_eq!(body.events.len(), TelemetryEventKind::all().len());
        assert!(body
            .events
            .iter()
            .any(|event| event.name == "instance_heartbeat"));
        assert_eq!(body.privacy_doc_url, TELEMETRY_PRIVACY_DOC_URL);
    }

    #[tokio::test]
    async fn admin_can_turn_telemetry_off_with_audit_and_runtime_effect() {
        let reporter = reporter(false);
        let store = Arc::new(MemoryStore::default());
        let (state, audit) = state(Some(reporter.clone()), store.clone());
        assert!(temps_core::telemetry::TelemetryReporter::is_enabled(
            &reporter
        ));

        let Json(body) = update_telemetry_settings(
            auth(Role::Admin),
            State(state.clone()),
            metadata(),
            Json(UpdateTelemetrySettingsRequest { enabled: false }),
        )
        .await
        .expect("admin can change telemetry");

        assert!(!body.enabled);
        assert_eq!(body.source, TelemetryStatusSource::AdminSetting);
        assert_eq!(body.admin_preference, Some(false));
        assert_eq!(*store.value.lock().unwrap(), Some(false));
        // Applied to the live reporter, no restart.
        assert!(!temps_core::telemetry::TelemetryReporter::is_enabled(
            &reporter
        ));

        let records = audit.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        let (operation, record) = &records[0];
        assert_eq!(operation, "TELEMETRY_SETTING_UPDATED");
        assert_eq!(record["enabled"], false);
        assert_eq!(record["effective_enabled"], false);
        assert_eq!(record["previous_preference"], serde_json::Value::Null);
        assert_eq!(record["context"]["user_id"], 7);
    }

    #[tokio::test]
    async fn non_admin_cannot_change_telemetry_and_nothing_is_audited() {
        let store = Arc::new(MemoryStore::default());
        let (state, audit) = state(Some(reporter(false)), store.clone());

        for role in [Role::User, Role::Reader] {
            let error = update_telemetry_settings(
                auth(role),
                State(state.clone()),
                metadata(),
                Json(UpdateTelemetrySettingsRequest { enabled: false }),
            )
            .await
            .expect_err("only instance admins may change telemetry");
            assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        }
        assert_eq!(*store.value.lock().unwrap(), None);
        assert!(audit.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn environment_opt_out_wins_over_admin_opt_in() {
        let reporter = reporter(true);
        let (state, audit) = state(Some(reporter.clone()), Arc::new(MemoryStore::default()));

        let Json(body) = update_telemetry_settings(
            auth(Role::Admin),
            State(state),
            metadata(),
            Json(UpdateTelemetrySettingsRequest { enabled: true }),
        )
        .await
        .expect("preference is saved even while forced off");

        assert!(!body.enabled, "TEMPS_TELEMETRY=0 must keep it off");
        assert_eq!(body.source, TelemetryStatusSource::Environment);
        assert!(body.env_opted_out);
        assert_eq!(body.admin_preference, Some(true));
        assert!(!temps_core::telemetry::TelemetryReporter::is_enabled(
            &reporter
        ));
        let records = audit.records.lock().unwrap();
        assert_eq!(records[0].1["effective_enabled"], false);
        assert_eq!(records[0].1["env_opted_out"], true);
    }

    #[tokio::test]
    async fn failed_save_returns_problem_without_audit() {
        let store = Arc::new(MemoryStore {
            fail_writes: true,
            ..Default::default()
        });
        let (state, audit) = state(Some(reporter(false)), store);
        let error = update_telemetry_settings(
            auth(Role::Admin),
            State(state),
            metadata(),
            Json(UpdateTelemetrySettingsRequest { enabled: false }),
        )
        .await
        .expect_err("write failure surfaces");
        assert_eq!(error.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(audit.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_reports_unavailable_reporter_honestly() {
        let (state, _) = state(None, Arc::new(MemoryStore::default()));
        let Json(body) = get_telemetry_settings(auth(Role::PlatformAdmin), State(state))
            .await
            .expect("status");
        assert!(!body.enabled);
        assert_eq!(body.source, TelemetryStatusSource::Unavailable);
        assert_eq!(body.anonymous_id, None);
        assert_eq!(body.endpoint_host, None);
    }

    #[tokio::test]
    async fn status_requires_settings_read() {
        let (state, _) = state(Some(reporter(false)), Arc::new(MemoryStore::default()));
        let error = get_telemetry_settings(auth(Role::User), State(state))
            .await
            .expect_err("users without settings:read cannot read it");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn endpoint_host_strips_path_and_keeps_port() {
        assert_eq!(
            endpoint_host("https://telemetry.temps.sh/v1/events").as_deref(),
            Some("telemetry.temps.sh")
        );
        assert_eq!(
            endpoint_host("http://localhost:4318/v1/events").as_deref(),
            Some("localhost:4318")
        );
        assert_eq!(endpoint_host("not a url"), None);
    }
}
