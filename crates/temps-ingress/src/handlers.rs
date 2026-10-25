// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin endpoints for ingress connectors, and the two node endpoints the
//! agent pulls specs from and reports status to.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use sea_orm::DatabaseConnection;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditLogger, AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::OpenApi;

use crate::registry::{DnsHandling, FieldKind, ProviderInfo, SetupField};
use crate::service::{
    ConnectorLinksView, ConnectorList, ConnectorNodeView, ConnectorView, CreateConnectorRequest,
    CreateError, HostnameCandidate, HostnameMode, HostnameView, IngressConnectorService, LinkView,
    PublicationView, SetConnectorNodesRequest, SetHostnameChoiceRequest, UpdateConnectorRequest,
};
use crate::wire::NodeConnectorStatusReport;

pub struct IngressAppState {
    pub service: Arc<IngressConnectorService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

/// One audit record per ingress write. Setup answers are never recorded:
/// they can hold provider credentials.
#[derive(Debug, serde::Serialize)]
struct IngressAudit {
    #[serde(skip)]
    action: &'static str,
    context: AuditContext,
    connector_id: Option<i32>,
    connector_name: Option<String>,
    hostname: Option<String>,
    details: serde_json::Value,
}

impl AuditOperation for IngressAudit {
    fn operation_type(&self) -> String {
        self.action.to_string()
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
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

impl IngressAudit {
    fn new(
        action: &'static str,
        auth: &temps_auth::AuthContext,
        metadata: &RequestMetadata,
    ) -> Self {
        Self {
            action,
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            connector_id: None,
            connector_name: None,
            hostname: None,
            details: serde_json::Value::Null,
        }
    }

    fn connector(mut self, view: &ConnectorView) -> Self {
        self.connector_id = Some(view.id);
        self.connector_name = Some(view.name.clone());
        self
    }

    fn details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    async fn record(self, state: &IngressAppState) {
        if let Err(error) = state.audit_service.create_audit_log(&self).await {
            error!(%error, action = self.action, "ingress change made but audit record failed");
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_ingress_providers,
        list_ingress_connectors,
        create_ingress_connector,
        get_ingress_connector,
        update_ingress_connector,
        set_ingress_connector_nodes,
        list_ingress_hostnames,
        get_ingress_hostname,
        set_ingress_hostname_choice,
        sync_ingress_connector,
        get_ingress_connector_links,
        delete_ingress_connector,
    ),
    components(schemas(
        ConnectorList,
        ConnectorView,
        ConnectorNodeView,
        PublicationView,
        ConnectorLinksView,
        LinkView,
        CreateConnectorRequest,
        SetConnectorNodesRequest,
        UpdateConnectorRequest,
        HostnameView,
        HostnameMode,
        HostnameCandidate,
        SetHostnameChoiceRequest,
        ProviderInfo,
        SetupField,
        FieldKind,
        DnsHandling,
    )),
    tags((name = "Ingress", description = "Ingress connectors: tunnels into the cluster (ADR 058)"))
)]
pub struct IngressApiDoc;

/// Which page of tunnels to return, newest first.
#[derive(Debug, Clone, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListConnectorsParams {
    /// Page number (1-indexed)
    #[param(example = 1)]
    pub page: Option<u64>,
    /// Number of items per page (default 20, max 100)
    #[param(example = 20)]
    pub page_size: Option<u64>,
}

pub fn configure_routes() -> Router<Arc<IngressAppState>> {
    Router::new()
        .route("/ingress/providers", get(list_ingress_providers))
        .route(
            "/ingress/connectors",
            get(list_ingress_connectors).post(create_ingress_connector),
        )
        .route(
            "/ingress/connectors/{id}",
            get(get_ingress_connector)
                .patch(update_ingress_connector)
                .delete(delete_ingress_connector),
        )
        .route(
            "/ingress/connectors/{id}/nodes",
            put(set_ingress_connector_nodes),
        )
        .route(
            "/ingress/connectors/{id}/sync",
            post(sync_ingress_connector),
        )
        .route(
            "/ingress/connectors/{id}/links",
            get(get_ingress_connector_links),
        )
        .route("/ingress/hostnames", get(list_ingress_hostnames))
        .route(
            "/ingress/hostnames/{hostname}",
            get(get_ingress_hostname).put(set_ingress_hostname_choice),
        )
}

/// List the tunnel providers this control plane offers.
///
/// Each entry describes its setup fields; answer them by key in
/// `create_ingress_connector`.
#[utoipa::path(
    tag = "Ingress",
    get,
    path = "/ingress/providers",
    operation_id = "list_ingress_providers",
    responses((status = 200, body = Vec<ProviderInfo>)),
    security(("bearer_auth" = []))
)]
async fn list_ingress_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.providers()))
}

/// List tunnels, the workers each runs on and their status.
///
/// A tunnel publishes apps under its `domain` without certificates or open
/// ports. Each node entry says whether that worker's connector is connected.
#[utoipa::path(
    tag = "Ingress",
    get,
    path = "/ingress/connectors",
    operation_id = "list_ingress_connectors",
    params(ListConnectorsParams),
    responses((status = 200, body = ConnectorList)),
    security(("bearer_auth" = []))
)]
async fn list_ingress_connectors(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Query(params): Query<ListConnectorsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(
        state.service.list(params.page, params.page_size).await?,
    ))
}

/// Create a tunnel for a managed domain and run it on workers.
///
/// Cloudflare Tunnel takes `values.dns_provider_id` (a Cloudflare DNS
/// provider) and `values.domain` (a zone managed by it, or a subdomain of
/// one). Every worker in `node_ids` must allow ingress
/// (`temps agent ingress delegated`). Apps under the domain are then
/// published automatically: adding `app.<domain>` to an environment is
/// enough.
#[utoipa::path(
    tag = "Ingress",
    post,
    path = "/ingress/connectors",
    operation_id = "create_ingress_connector",
    request_body = CreateConnectorRequest,
    responses(
        (status = 201, body = ConnectorView),
        (status = 400, description = "Invalid name, setup answers or nodes"),
        (status = 409, description = "A worker has not delegated public ingress"),
        (status = 502, description = "The provider refused; the detail says what to fix")
    ),
    security(("bearer_auth" = []))
)]
async fn create_ingress_connector(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateConnectorRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let provider = request.provider.clone();
    let node_ids = request.node_ids.clone();
    let view = match state.service.create(request).await {
        Ok(view) => view,
        Err(CreateError::NotSaved(error)) => return Err((*error).into()),
        // Saved, then the provider or the worker assignment failed: still a
        // write, so it is audited with the failure.
        Err(CreateError::Saved {
            connector_id,
            connector_name,
            error,
        }) => {
            let mut audit = IngressAudit::new("INGRESS_CONNECTOR_CREATED", &auth, &metadata)
                .details(serde_json::json!({
                    "provider": provider,
                    "node_ids": node_ids,
                    "outcome": "setup_failed",
                    "error": error.to_string(),
                }));
            audit.connector_id = Some(connector_id);
            audit.connector_name = Some(connector_name);
            audit.record(&state).await;
            return Err((*error).into());
        }
    };
    IngressAudit::new("INGRESS_CONNECTOR_CREATED", &auth, &metadata)
        .connector(&view)
        .details(serde_json::json!({
            "provider": view.provider,
            "domain": view.domain,
            "node_ids": view.nodes.iter().map(|n| n.node_id).collect::<Vec<_>>(),
            "auto_publish": view.auto_publish,
            "outcome": "created",
        }))
        .record(&state)
        .await;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Get one tunnel.
#[utoipa::path(
    tag = "Ingress",
    get,
    path = "/ingress/connectors/{id}",
    operation_id = "get_ingress_connector",
    params(("id" = i32, Path)),
    responses((status = 200, body = ConnectorView), (status = 404)),
    security(("bearer_auth" = []))
)]
async fn get_ingress_connector(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.get(id).await?))
}

/// Change what a tunnel publishes.
///
/// `auto_publish: true` publishes every app under its domain; `false`
/// publishes only hostnames chosen for it with
/// `set_ingress_hostname_choice`.
#[utoipa::path(
    tag = "Ingress",
    patch,
    path = "/ingress/connectors/{id}",
    operation_id = "update_ingress_connector",
    params(("id" = i32, Path)),
    request_body = UpdateConnectorRequest,
    responses((status = 200, body = ConnectorView), (status = 404)),
    security(("bearer_auth" = []))
)]
async fn update_ingress_connector(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateConnectorRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let view = state.service.update(id, request.clone()).await?;
    IngressAudit::new("INGRESS_CONNECTOR_UPDATED", &auth, &metadata)
        .connector(&view)
        .details(serde_json::json!({ "auto_publish": request.auto_publish }))
        .record(&state)
        .await;
    Ok(Json(view))
}

/// List hostnames tunnels publish or could publish, and why.
///
/// For each: the tunnel carrying it, its state (`published`, `pending`,
/// `error`, `not_published`) and, when not published, the reason and what to
/// do.
#[utoipa::path(
    tag = "Ingress",
    get,
    path = "/ingress/hostnames",
    operation_id = "list_ingress_hostnames",
    responses((status = 200, body = Vec<HostnameView>)),
    security(("bearer_auth" = []))
)]
async fn list_ingress_hostnames(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.hostnames().await?))
}

/// Explain how one hostname reaches visitors.
///
/// Works for any hostname, served or not: the tunnels that could carry it
/// (`candidates`), the one that does, and why.
#[utoipa::path(
    tag = "Ingress",
    get,
    path = "/ingress/hostnames/{hostname}",
    operation_id = "get_ingress_hostname",
    params(("hostname" = String, Path)),
    responses((status = 200, body = HostnameView), (status = 400)),
    security(("bearer_auth" = []))
)]
async fn get_ingress_hostname(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Path(hostname): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.hostname(&hostname).await?))
}

/// Choose how a hostname reaches visitors.
///
/// `automatic` lets the most specific tunnel covering it publish it;
/// `tunnel` with `connector_id` pins it to that tunnel (adding a domain
/// "through a tunnel"); `off` keeps every tunnel away from it, for DNS
/// managed elsewhere. It is published once an environment serves it.
#[utoipa::path(
    tag = "Ingress",
    put,
    path = "/ingress/hostnames/{hostname}",
    operation_id = "set_ingress_hostname_choice",
    params(("hostname" = String, Path)),
    request_body = SetHostnameChoiceRequest,
    responses(
        (status = 200, body = HostnameView),
        (status = 400, description = "Invalid hostname, or the tunnel cannot publish it"),
        (status = 404, description = "No such tunnel")
    ),
    security(("bearer_auth" = []))
)]
async fn set_ingress_hostname_choice(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(hostname): Path<String>,
    Json(request): Json<SetHostnameChoiceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let view = state
        .service
        .set_hostname_choice(&hostname, request.clone())
        .await?;
    let mut audit = IngressAudit::new("INGRESS_HOSTNAME_ROUTE_SET", &auth, &metadata)
        .details(serde_json::json!({ "mode": request.mode, "state": view.state }));
    audit.hostname = Some(view.hostname.clone());
    audit.connector_id = request.connector_id;
    audit.record(&state).await;
    Ok(Json(view))
}

/// Set the workers a tunnel runs on.
///
/// More than one keeps apps reachable when a worker goes down; an empty list
/// stops the tunnel everywhere without deleting it.
#[utoipa::path(
    tag = "Ingress",
    put,
    path = "/ingress/connectors/{id}/nodes",
    operation_id = "set_ingress_connector_nodes",
    params(("id" = i32, Path)),
    request_body = SetConnectorNodesRequest,
    responses((status = 200, body = ConnectorView), (status = 404)),
    security(("bearer_auth" = []))
)]
async fn set_ingress_connector_nodes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<SetConnectorNodesRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let view = state
        .service
        .set_nodes(id, request.node_ids.clone())
        .await?;
    IngressAudit::new("INGRESS_CONNECTOR_NODES_SET", &auth, &metadata)
        .connector(&view)
        .details(serde_json::json!({ "node_ids": request.node_ids }))
        .record(&state)
        .await;
    Ok(Json(view))
}

/// Re-check a tunnel with its provider and recreate it if it was deleted.
#[utoipa::path(
    tag = "Ingress",
    post,
    path = "/ingress/connectors/{id}/sync",
    operation_id = "sync_ingress_connector",
    params(("id" = i32, Path)),
    responses((status = 200, body = ConnectorView), (status = 404), (status = 502)),
    security(("bearer_auth" = []))
)]
async fn sync_ingress_connector(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let view = state.service.sync(id).await?;
    IngressAudit::new("INGRESS_CONNECTOR_SYNCED", &auth, &metadata)
        .connector(&view)
        .details(serde_json::json!({ "status": view.status }))
        .record(&state)
        .await;
    Ok(Json(view))
}

/// List a tunnel's live connections to the provider's edge.
///
/// Read from the provider on request: each link's data centre, when it
/// opened, whether it is reconnecting, and the worker holding it.
#[utoipa::path(
    tag = "Ingress",
    get,
    path = "/ingress/connectors/{id}/links",
    operation_id = "get_ingress_connector_links",
    params(("id" = i32, Path)),
    responses(
        (status = 200, body = ConnectorLinksView),
        (status = 400, description = "The tunnel is not created yet"),
        (status = 404),
        (status = 502, description = "The provider could not be asked")
    ),
    security(("bearer_auth" = []))
)]
async fn get_ingress_connector_links(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.links(id).await?))
}

/// Delete a tunnel at the provider and unpublish every hostname it carries.
#[utoipa::path(
    tag = "Ingress",
    delete,
    path = "/ingress/connectors/{id}",
    operation_id = "delete_ingress_connector",
    params(("id" = i32, Path)),
    responses((status = 204), (status = 404), (status = 502)),
    security(("bearer_auth" = []))
)]
async fn delete_ingress_connector(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IngressAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let before = state.service.get(id).await?;
    state.service.delete(id).await?;
    IngressAudit::new("INGRESS_CONNECTOR_DELETED", &auth, &metadata)
        .connector(&before)
        .details(serde_json::json!({ "provider": before.provider, "domain": before.domain }))
        .record(&state)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ----- node endpoints (node token) --------------------------------------

pub struct IngressNodeState {
    pub db: Arc<DatabaseConnection>,
    pub service: Arc<IngressConnectorService>,
}

/// Mounted next to route sync, outside the plugin router: these are
/// authenticated by the node token, not a user session.
pub fn node_routes(state: Arc<IngressNodeState>) -> Router {
    Router::new()
        .route(
            "/internal/nodes/{node_id}/ingress/connectors",
            get(node_connectors),
        )
        .route(
            "/internal/nodes/{node_id}/ingress/status",
            post(node_connector_status),
        )
        .with_state(state)
}

async fn node_connectors(
    State(state): State<Arc<IngressNodeState>>,
    Path(node_id): Path<i32>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    temps_routes::route_sync::authenticate_node(&state.db, &headers, node_id).await?;
    state
        .service
        .node_specs(node_id)
        .await
        .map(Json)
        .map_err(|e| {
            error!(node_id, error = %e, "could not build ingress connector specs");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

async fn node_connector_status(
    State(state): State<Arc<IngressNodeState>>,
    Path(node_id): Path<i32>,
    headers: HeaderMap,
    Json(report): Json<NodeConnectorStatusReport>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    temps_routes::route_sync::authenticate_node(&state.db, &headers, node_id).await?;
    state
        .service
        .record_status(node_id, report)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
