// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox driver mode per node (ADR-055 phase 6).
//!
//! `GET/PUT /internal/nodes/{node_id}/sandbox-driver-mode`: which code path
//! serves the sandboxes on a node. Node `0` is the control plane.
//!
//! - `legacy` (the default): the sandbox providers serve every call exactly
//!   as before; sandboxes on worker nodes keep refusing terminals, the
//!   retained agent runtime, snapshots and disk resize with a 422.
//! - `shadow`: the providers still serve; the driver facade plans every
//!   create next to them and logs where it would differ.
//! - `enforce`: the driver facade serves. Requests are filtered on driver
//!   capabilities, unapplied limits are reported as `LimitEnforced = False`,
//!   and sandboxes on a worker node forward terminals, the retained agent
//!   runtime, disk resize, image identity and harness recovery to the
//!   worker's driver. Snapshots and restore stay refused on worker nodes.
//!
//! Setting `legacy` again is the rollback. Every change is audit-logged.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditLogger, RequestMetadata};
use temps_entities::sandbox_driver_mode::SandboxDriverMode;
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::audit::NodeSandboxDriverModeChangedAudit;
use crate::services::node_service::{NodeService, SandboxDriverModeSetting};

/// How long the control plane may keep serving its own sandboxes in the
/// previous mode after a change: it re-reads node 0's mode at most this
/// often. Worker nodes switch on their next sandbox call.
pub const CONTROL_PLANE_MODE_PROPAGATION_SECS: u64 = 5;

pub struct SandboxDriverModeAppState {
    pub node_service: Arc<NodeService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SandboxDriverModeResponse {
    /// `0` is the control plane.
    pub node_id: i32,
    pub node_name: String,
    /// `legacy`, `shadow` or `enforce`.
    #[schema(value_type = String, example = "legacy")]
    pub mode: SandboxDriverMode,
    /// Whether the driver facade serves this node's sandboxes (only in
    /// `enforce`).
    pub uses_drivers: bool,
    /// Longest delay before a change reaches calls already being served.
    pub propagation_secs: u64,
}

impl From<SandboxDriverModeSetting> for SandboxDriverModeResponse {
    fn from(setting: SandboxDriverModeSetting) -> Self {
        Self {
            node_id: setting.node_id,
            node_name: setting.node_name,
            uses_drivers: setting.mode.uses_drivers(),
            mode: setting.mode,
            propagation_secs: CONTROL_PLANE_MODE_PROPAGATION_SECS,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetSandboxDriverModeRequest {
    /// `legacy`, `shadow` or `enforce`.
    #[schema(value_type = String, example = "enforce")]
    pub mode: SandboxDriverMode,
}

#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/internal/nodes/{node_id}/sandbox-driver-mode",
    operation_id = "get_node_sandbox_driver_mode",
    params(("node_id" = i32, Path, description = "Worker node id; 0 for the control plane")),
    responses(
        (status = 200, description = "The node's sandbox driver mode", body = SandboxDriverModeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_sandbox_driver_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxDriverModeAppState>>,
    Path(node_id): Path<i32>,
) -> Result<Json<SandboxDriverModeResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let setting = state
        .node_service
        .sandbox_driver_mode(node_id)
        .await
        .map_err(Problem::from)?;
    Ok(Json(setting.into()))
}

#[utoipa::path(
    tag = "Nodes",
    put,
    path = "/internal/nodes/{node_id}/sandbox-driver-mode",
    operation_id = "set_node_sandbox_driver_mode",
    params(("node_id" = i32, Path, description = "Worker node id; 0 for the control plane")),
    request_body = SetSandboxDriverModeRequest,
    responses(
        (status = 200, description = "The node's new sandbox driver mode", body = SandboxDriverModeResponse),
        (status = 400, description = "The node does not host sandboxes"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Node not found"),
        (status = 422, description = "Unknown mode"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_sandbox_driver_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxDriverModeAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node_id): Path<i32>,
    Json(request): Json<SetSandboxDriverModeRequest>,
) -> Result<Json<SandboxDriverModeResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let (before, after) = state
        .node_service
        .set_sandbox_driver_mode(node_id, request.mode)
        .await
        .map_err(Problem::from)?;
    info!(
        node_id,
        node_name = %after.node_name,
        previous_mode = %before.mode,
        mode = %after.mode,
        "sandbox driver mode changed"
    );
    let audit = NodeSandboxDriverModeChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        node_id,
        node_name: after.node_name.clone(),
        previous_mode: before.mode.to_string(),
        mode: after.mode.to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            node_id,
            "sandbox driver mode changed but the audit record failed: {}", e
        );
    }
    Ok(Json(after.into()))
}

pub fn configure_routes() -> Router<Arc<SandboxDriverModeAppState>> {
    Router::new().route(
        "/internal/nodes/{node_id}/sandbox-driver-mode",
        get(get_sandbox_driver_mode).put(set_sandbox_driver_mode),
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(get_sandbox_driver_mode, set_sandbox_driver_mode),
    components(schemas(SandboxDriverModeResponse, SetSandboxDriverModeRequest))
)]
pub struct SandboxDriverModeApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::wireguard_mesh::admin_test_support::{
        admin, metadata, network_config, node, outsider, settings_reader, RecordingAuditLogger,
    };
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::StatusCode;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_auth::AuthContext;
    use tower::ServiceExt;

    fn app(db: MockDatabase, auth: Option<AuthContext>) -> (Router, Arc<RecordingAuditLogger>) {
        let audits = Arc::new(RecordingAuditLogger::default());
        let state = Arc::new(SandboxDriverModeAppState {
            node_service: Arc::new(NodeService::new(Arc::new(db.into_connection()))),
            audit_service: audits.clone(),
        });
        let router = configure_routes()
            .layer(axum::middleware::from_fn(
                move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                    let auth = auth.clone();
                    async move {
                        if let Some(auth) = auth {
                            request.extensions_mut().insert(auth);
                        }
                        request.extensions_mut().insert(metadata());
                        next.run(request).await
                    }
                },
            ))
            .with_state(state);
        (router, audits)
    }

    fn put(node_id: i32, mode: &str) -> Request<Body> {
        Request::put(format!("/internal/nodes/{node_id}/sandbox-driver-mode"))
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"mode":"{mode}"}}"#)))
            .expect("request")
    }

    fn get(node_id: i32) -> Request<Body> {
        Request::get(format!("/internal/nodes/{node_id}/sandbox-driver-mode"))
            .body(Body::empty())
            .expect("request")
    }

    async fn body(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test]
    async fn no_caller_is_unauthorized() {
        let (app, audits) = app(MockDatabase::new(DatabaseBackend::Postgres), None);
        let response = app.oneshot(put(3, "enforce")).await.expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(audits.records().is_empty());
    }

    #[tokio::test]
    async fn changing_the_mode_needs_settings_write() {
        for caller in [settings_reader(), outsider()] {
            let (app, audits) = app(MockDatabase::new(DatabaseBackend::Postgres), Some(caller));
            let response = app.oneshot(put(3, "enforce")).await.expect("response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert!(audits.records().is_empty());
        }
        let (app, _) = app(
            MockDatabase::new(DatabaseBackend::Postgres),
            Some(outsider()),
        );
        let response = app.oneshot(get(3)).await.expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_worker_moves_to_enforce_and_the_change_is_audited() {
        let worker = node(3, "worker-a", "token");
        let mut updated = worker.clone();
        updated.sandbox_driver_mode = SandboxDriverMode::Enforce;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![worker.clone()]])
            .append_query_results(vec![vec![worker]])
            .append_query_results(vec![vec![updated]]);
        let (app, audits) = app(db, Some(admin()));
        let response = app.oneshot(put(3, "enforce")).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let json = body(response).await;
        assert_eq!(json["node_id"], 3);
        assert_eq!(json["node_name"], "worker-a");
        assert_eq!(json["mode"], "enforce");
        assert_eq!(json["uses_drivers"], true);

        let records = audits.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].operation, "NODE_SANDBOX_DRIVER_MODE_CHANGED");
        let audit: serde_json::Value = serde_json::from_str(&records[0].body).expect("audit json");
        assert_eq!(audit["previous_mode"], "legacy");
        assert_eq!(audit["mode"], "enforce");
        assert_eq!(audit["node_name"], "worker-a");
        assert_eq!(
            records[0].ip_address.as_deref(),
            Some(crate::handlers::wireguard_mesh::admin_test_support::CLIENT_IP)
        );
    }

    #[tokio::test]
    async fn node_zero_is_the_control_plane() {
        let mut row = network_config(false, false);
        row.control_plane_sandbox_driver_mode = SandboxDriverMode::Shadow;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }]);
        let (app, audits) = app(db, Some(admin()));
        let response = app.oneshot(put(0, "legacy")).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let json = body(response).await;
        assert_eq!(json["node_id"], 0);
        assert_eq!(json["node_name"], "control-plane");
        assert_eq!(json["mode"], "legacy");
        let audit: serde_json::Value =
            serde_json::from_str(&audits.records()[0].body).expect("audit json");
        assert_eq!(audit["previous_mode"], "shadow");
        assert_eq!(audit["mode"], "legacy");
    }

    #[tokio::test]
    async fn reading_works_with_settings_read_and_defaults_to_legacy() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::network_config::Model>::new()]);
        let (app, _) = app(db, Some(settings_reader()));
        let response = app.oneshot(get(0)).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let json = body(response).await;
        assert_eq!(json["mode"], "legacy");
        assert_eq!(json["uses_drivers"], false);
    }

    #[tokio::test]
    async fn an_unknown_node_is_not_found_and_a_non_worker_is_refused() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::nodes::Model>::new()]);
        let (router, audits) = app(db, Some(admin()));
        let response = router.oneshot(put(99, "enforce")).await.expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let json = body(response).await;
        assert!(
            json["detail"].as_str().unwrap_or_default().contains("99"),
            "{json}"
        );
        assert!(audits.records().is_empty());

        let mut edge = node(4, "edge-a", "token");
        edge.role = "edge".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![edge.clone()]])
            .append_query_results(vec![vec![edge]]);
        let (router, audits) = app(db, Some(admin()));
        let response = router.oneshot(put(4, "enforce")).await.expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body(response).await;
        assert!(
            json["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("has role 'edge'"),
            "{json}"
        );
        assert!(audits.records().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_mode_is_rejected_before_anything_is_read() {
        let (app, audits) = app(MockDatabase::new(DatabaseBackend::Postgres), Some(admin()));
        let response = app.oneshot(put(3, "enforced")).await.expect("response");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(audits.records().is_empty());
    }
}
