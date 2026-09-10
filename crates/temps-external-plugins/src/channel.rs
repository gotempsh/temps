// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Platform-side channel connector.
//!
//! After a plugin completes its handshake, Temps opens a WebSocket to the
//! plugin's `/_temps/channel` endpoint.  The connection is kept alive for the
//! plugin's lifetime and serves two purposes:
//!
//! 1. **Plugin → Platform requests** — the plugin asks for data (projects,
//!    environments, deployments) and the platform responds using its own
//!    database connection.  The plugin never sees the database.
//! 2. **Platform → Plugin events** — replaces the old `POST /_events`
//!    mechanism with a push over the already-open connection.
//!
//! Each plugin gets its own [`PluginChannel`] which tracks the WebSocket
//! sender half and the background reader task.

use std::path::Path;
use std::sync::Arc;

use futures::stream::StreamExt;
use futures::SinkExt;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use temps_core::external_plugin::channel::*;
use temps_core::external_plugin::manifest::PluginCapability;
use temps_core::external_plugin::PluginEvent;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

/// A live channel connection to a single plugin.
pub struct PluginChannel {
    /// Plugin name (for logging).
    plugin_name: String,
    /// Sender for outgoing messages (responses + events).
    tx: mpsc::UnboundedSender<ChannelMessage>,
    /// Background task reading from the WebSocket.
    reader_task: JoinHandle<()>,
}

impl PluginChannel {
    /// Open a WebSocket channel to a plugin's Unix socket.
    ///
    /// Returns `None` if the connection fails (the plugin may not support
    /// the channel yet — this is not fatal).
    pub async fn connect(
        socket_path: &Path,
        plugin_name: String,
        db: Arc<DatabaseConnection>,
        host_api: Arc<HostApiSlot>,
        capabilities: Vec<PluginCapability>,
        auth_secret: &str,
    ) -> Option<Self> {
        let socket_path_str = socket_path.to_string_lossy().to_string();

        // Connect to the plugin's Unix socket and upgrade to WebSocket.
        // tokio-tungstenite doesn't support Unix sockets directly, so we
        // perform the WebSocket handshake over a raw UnixStream.
        info!(
            plugin = %plugin_name,
            socket = %socket_path_str,
            "Opening platform channel to plugin"
        );

        let stream = match tokio::net::UnixStream::connect(socket_path).await {
            Ok(s) => {
                debug!(
                    plugin = %plugin_name,
                    "Unix socket connected for channel"
                );
                s
            }
            Err(e) => {
                warn!(
                    plugin = %plugin_name,
                    "Cannot connect to plugin socket for channel: {} ({})",
                    socket_path_str, e
                );
                return None;
            }
        };

        let uri = format!("ws://localhost{}", PLUGIN_CHANNEL_PATH);
        let mut request = match uri.into_client_request() {
            Ok(request) => request,
            Err(error) => {
                warn!(plugin = %plugin_name, "Cannot build channel handshake: {error}");
                return None;
            }
        };
        let auth_header = match auth_secret.parse() {
            Ok(value) => value,
            Err(error) => {
                warn!(plugin = %plugin_name, "Cannot encode channel authentication: {error}");
                return None;
            }
        };
        request.headers_mut().insert(
            temps_core::external_plugin::headers::AUTH_SIGNATURE,
            auth_header,
        );
        let ws_stream = match tokio_tungstenite::client_async(request, stream).await {
            Ok((ws, _resp)) => {
                debug!(
                    plugin = %plugin_name,
                    "WebSocket handshake succeeded for channel"
                );
                ws
            }
            Err(e) => {
                warn!(
                    plugin = %plugin_name,
                    "WebSocket handshake failed for channel: {} ({})",
                    socket_path_str, e
                );
                return None;
            }
        };

        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        // Outgoing message channel — lets the reader task and event pushes
        // share a single send path without locking.
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<ChannelMessage>();

        // Spawn writer task: forwards from mpsc channel to WebSocket.
        let writer_plugin_name = plugin_name.clone();
        tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        error!(
                            plugin = %writer_plugin_name,
                            "Failed to serialize channel message: {}", e
                        );
                        continue;
                    }
                };
                if let Err(e) = ws_tx.send(Message::Text(json.into())).await {
                    debug!(
                        plugin = %writer_plugin_name,
                        "Channel WebSocket write error (plugin may have shut down): {}", e
                    );
                    break;
                }
            }
        });

        // Spawn reader task: reads requests from the plugin, dispatches
        // them, and sends responses through msg_tx.
        let reader_tx = msg_tx.clone();
        let reader_plugin_name = plugin_name.clone();
        let reader_task = tokio::spawn(async move {
            while let Some(frame) = ws_rx.next().await {
                let text = match frame {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) => {
                        debug!(plugin = %reader_plugin_name, "Plugin closed channel");
                        break;
                    }
                    Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        debug!(
                            plugin = %reader_plugin_name,
                            "Channel read error: {}", e
                        );
                        break;
                    }
                };

                let msg: ChannelMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        // A plugin built against a newer Temps can name a call
                        // this build has no variant for, and the envelope then
                        // fails to deserialize as a whole. Answer it: leaving
                        // the plugin's request unreplied hangs its caller until
                        // the channel closes, which looks like a freeze rather
                        // than an incompatibility.
                        match peek_request_id(&text) {
                            Some(id) => {
                                warn!(
                                    plugin = %reader_plugin_name,
                                    "Unsupported channel request {}: {}", id, e
                                );
                                let _ =
                                    reader_tx.send(ChannelMessage::Response(ChannelResponse::err(
                                        id,
                                        ChannelErrorCode::MethodNotFound,
                                        format!(
                                            "This Temps build does not support that channel \
                                             call ({e}); rebuild the plugin against this version"
                                        ),
                                    )));
                            }
                            None => warn!(
                                plugin = %reader_plugin_name,
                                "Invalid channel message from plugin: {}", e
                            ),
                        }
                        continue;
                    }
                };

                match msg {
                    ChannelMessage::Request(req) => {
                        let response = dispatch_request(
                            &reader_plugin_name,
                            &db,
                            &host_api,
                            &capabilities,
                            &req,
                        )
                        .await;
                        let _ = reader_tx.send(ChannelMessage::Response(response));
                    }
                    _ => {
                        warn!(
                            plugin = %reader_plugin_name,
                            "Unexpected message type from plugin (expected Request)"
                        );
                    }
                }
            }

            debug!(plugin = %reader_plugin_name, "Channel reader task exiting");
        });

        info!(
            plugin = %plugin_name,
            "Platform channel connected"
        );

        Some(Self {
            plugin_name,
            tx: msg_tx,
            reader_task,
        })
    }

    /// Push a platform event to the plugin over the channel.
    pub fn send_event(&self, event: PluginEvent) -> Result<(), String> {
        self.tx
            .send(ChannelMessage::Event(ChannelEvent { event }))
            .map_err(|_| format!("Channel closed for plugin '{}'", self.plugin_name))
    }

    /// Check if the channel is still alive.
    pub fn is_alive(&self) -> bool {
        !self.reader_task.is_finished()
    }
}

impl Drop for PluginChannel {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

/// Recover just the correlation id from a request this build cannot parse.
///
/// Deliberately minimal: it must succeed on messages the strongly-typed
/// deserializer rejected, so it looks at nothing but `id`.
fn peek_request_id(text: &str) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct IdOnly {
        id: u64,
    }
    serde_json::from_str::<IdOnly>(text).ok().map(|p| p.id)
}

// ── Host API bridge ────────────────────────────────────────────────────

/// Runs an [`ApiCall`] against the platform's own HTTP router.
///
/// A trait rather than a concrete type because this crate must not know how
/// the console assembles its router — and cannot, since the router contains
/// this crate's own routes. The console builds the router, then hands an
/// implementation back through
/// [`crate::service::ExternalPluginsService::set_host_api`].
#[async_trait::async_trait]
pub trait HostApiBridge: Send + Sync {
    /// Dispatch `call` as `plugin`, resolving the acting user from the
    /// call's actor token.
    async fn call(&self, plugin: &str, call: ApiCall) -> Result<ApiCallResult, ChannelError>;
}

/// Late-bound holder for the [`HostApiBridge`].
///
/// Channels are opened while plugins start, which is *before* the console
/// has finished assembling the router they will call into — the router
/// contains this crate's own routes, so it cannot exist first. The slot is
/// therefore shared at connect time and read on each call, so a plugin that
/// connected during startup still reaches the router once it exists.
pub type HostApiSlot = tokio::sync::RwLock<Option<Arc<dyn HostApiBridge>>>;

// ── Request dispatch ───────────────────────────────────────────────────

/// Route a plugin request to the handler that serves it.
///
/// The match is exhaustive over [`PlatformCallRequest`] with no catch-all
/// arm: adding a call to the protocol without handling it here is a compile
/// error, which is the point of the typed protocol. A method this build does
/// not know cannot reach here at all — it fails to deserialize, and the
/// reader loop answers `MethodNotFound`.
async fn dispatch_request(
    plugin_name: &str,
    db: &DatabaseConnection,
    host_api: &HostApiSlot,
    capabilities: &[PluginCapability],
    req: &ChannelRequest,
) -> ChannelResponse {
    debug!(
        plugin = %plugin_name,
        method = %req.call.method_name(),
        id = req.id,
        "Dispatching channel request"
    );

    let outcome = match &req.call {
        PlatformCallRequest::GetProject(p) => handle_get_project(db, p).await,
        PlatformCallRequest::ListProjects(p) => handle_list_projects(db, p).await,
        PlatformCallRequest::GetEnvironment(p) => handle_get_environment(db, p).await,
        PlatformCallRequest::ListEnvironments(p) => handle_list_environments(db, p).await,
        PlatformCallRequest::GetDeployment(p) => handle_get_deployment(db, p).await,
        PlatformCallRequest::GetLastDeployment(p) => handle_get_last_deployment(db, p).await,
        PlatformCallRequest::ListDeployments(p) => handle_list_deployments(db, p).await,
        PlatformCallRequest::ApiCall(call) => {
            handle_api_call(plugin_name, host_api, capabilities, call.clone()).await
        }
    };

    match outcome {
        Ok(result) => ChannelResponse::ok(req.id, result),
        Err(err) => ChannelResponse {
            id: req.id,
            outcome: CallOutcome::Err(err),
        },
    }
}

/// Serve an [`ApiCall`] through the platform's own router.
async fn handle_api_call(
    plugin_name: &str,
    host_api: &HostApiSlot,
    capabilities: &[PluginCapability],
    call: ApiCall,
) -> Result<PlatformCallResponse, ChannelError> {
    // Capability first: refuse before the request reaches the router, and
    // name what is missing. A plugin author debugging this alone cannot
    // guess "add api_write to your manifest" from a bare 403.
    let required = PluginCapability::for_method(call.method);
    if !capabilities.contains(&required) {
        return Err(ChannelError::new(
            ChannelErrorCode::PermissionDenied,
            format!(
                "Plugin '{plugin_name}' called {} {} but does not declare the '{}' capability;                  add it to the plugin manifest",
                call.method.as_str(),
                call.path,
                required.as_str()
            ),
        ));
    }

    // Say which capability is missing rather than a bare "unavailable": an
    // operator running an embedding build that never wired the bridge has no
    // other way to find out why every plugin API call fails.
    let Some(api) = host_api.read().await.clone() else {
        return Err(ChannelError::new(
            ChannelErrorCode::Internal,
            format!(
                "This Temps build did not wire the host API bridge, so plugin '{plugin_name}' \
                 cannot call the platform API over the channel"
            ),
        ));
    };
    api.call(plugin_name, call)
        .await
        .map(PlatformCallResponse::ApiCall)
}

// ── Method handlers ────────────────────────────────────────────────────
//
// Each takes its own parameter struct and returns the response variant that
// belongs to it. Nothing parses JSON by hand any more: a missing or
// mistyped field is a deserialization failure on the envelope, reported
// once, instead of every handler re-implementing "missing required
// parameter".

async fn handle_get_project(
    db: &DatabaseConnection,
    params: &GetProject,
) -> Result<PlatformCallResponse, ChannelError> {
    use temps_entities::projects;

    let project = projects::Entity::find_by_id(params.project_id)
        .filter(projects::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(|e| db_error("project", e))?
        .ok_or_else(|| {
            ChannelError::new(
                ChannelErrorCode::NotFound,
                format!("Project {} not found", params.project_id),
            )
        })?;

    Ok(PlatformCallResponse::GetProject(project_to_info(&project)))
}

async fn handle_list_projects(
    db: &DatabaseConnection,
    _params: &ListProjects,
) -> Result<PlatformCallResponse, ChannelError> {
    use temps_entities::projects;

    let projects = projects::Entity::find()
        .filter(projects::Column::IsDeleted.eq(false))
        .order_by_asc(projects::Column::Name)
        .all(db)
        .await
        .map_err(|e| db_error("projects", e))?;

    Ok(PlatformCallResponse::ListProjects(
        projects.iter().map(project_to_info).collect(),
    ))
}

async fn handle_get_environment(
    db: &DatabaseConnection,
    params: &GetEnvironment,
) -> Result<PlatformCallResponse, ChannelError> {
    use temps_entities::environments;

    let env = environments::Entity::find_by_id(params.environment_id)
        .filter(environments::Column::DeletedAt.is_null())
        .one(db)
        .await
        .map_err(|e| db_error("environment", e))?
        .ok_or_else(|| {
            ChannelError::new(
                ChannelErrorCode::NotFound,
                format!("Environment {} not found", params.environment_id),
            )
        })?;

    Ok(PlatformCallResponse::GetEnvironment(environment_to_info(
        &env,
    )))
}

async fn handle_list_environments(
    db: &DatabaseConnection,
    params: &ListEnvironments,
) -> Result<PlatformCallResponse, ChannelError> {
    use temps_entities::environments;

    let envs = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(params.project_id))
        .filter(environments::Column::DeletedAt.is_null())
        .order_by_asc(environments::Column::Name)
        .all(db)
        .await
        .map_err(|e| db_error("environments", e))?;

    Ok(PlatformCallResponse::ListEnvironments(
        envs.iter().map(environment_to_info).collect(),
    ))
}

async fn handle_get_deployment(
    db: &DatabaseConnection,
    params: &GetDeployment,
) -> Result<PlatformCallResponse, ChannelError> {
    use temps_entities::deployments;

    let dep = deployments::Entity::find_by_id(params.deployment_id)
        .one(db)
        .await
        .map_err(|e| db_error("deployment", e))?
        .ok_or_else(|| {
            ChannelError::new(
                ChannelErrorCode::NotFound,
                format!("Deployment {} not found", params.deployment_id),
            )
        })?;

    Ok(PlatformCallResponse::GetDeployment(deployment_to_info(
        &dep,
    )))
}

async fn handle_get_last_deployment(
    db: &DatabaseConnection,
    params: &GetLastDeployment,
) -> Result<PlatformCallResponse, ChannelError> {
    use temps_entities::deployments;

    let mut query = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(params.project_id))
        .order_by_desc(deployments::Column::CreatedAt);

    if let Some(env_id) = params.environment_id {
        query = query.filter(deployments::Column::EnvironmentId.eq(env_id));
    }

    let dep = query
        .one(db)
        .await
        .map_err(|e| db_error("deployment", e))?
        .ok_or_else(|| {
            ChannelError::new(
                ChannelErrorCode::NotFound,
                format!(
                    "No deployments found for project {}{}",
                    params.project_id,
                    params
                        .environment_id
                        .map(|e| format!(" environment {e}"))
                        .unwrap_or_default()
                ),
            )
        })?;

    Ok(PlatformCallResponse::GetLastDeployment(deployment_to_info(
        &dep,
    )))
}

async fn handle_list_deployments(
    db: &DatabaseConnection,
    params: &ListDeployments,
) -> Result<PlatformCallResponse, ChannelError> {
    use sea_orm::QuerySelect;
    use temps_entities::deployments;

    // Bounded regardless of what the plugin asks for: this runs against the
    // control-plane database, and an unbounded fetch on a busy project is a
    // memory spike on a small box.
    let limit = params.limit.unwrap_or(20).min(100);

    let mut query = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(params.project_id))
        .order_by_desc(deployments::Column::CreatedAt)
        .limit(limit);

    if let Some(env_id) = params.environment_id {
        query = query.filter(deployments::Column::EnvironmentId.eq(env_id));
    }

    let deps = query
        .all(db)
        .await
        .map_err(|e| db_error("deployments", e))?;

    Ok(PlatformCallResponse::ListDeployments(
        deps.iter().map(deployment_to_info).collect(),
    ))
}

/// Database failures are internal errors, and the plugin is told which
/// lookup failed so a plugin author can tell "no such project" from "the
/// control plane is unwell".
fn db_error(what: &str, e: sea_orm::DbErr) -> ChannelError {
    ChannelError::new(
        ChannelErrorCode::Internal,
        format!("Failed to read {what} from the control-plane database: {e}"),
    )
}

// ── Entity → DTO converters ────────────────────────────────────────────

fn format_dt(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn format_dt_opt(dt: &Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    dt.as_ref().map(format_dt)
}

fn project_to_info(p: &temps_entities::projects::Model) -> ProjectInfo {
    ProjectInfo {
        id: p.id,
        name: p.name.clone(),
        slug: p.slug.clone(),
        repo_name: p.repo_name.clone(),
        repo_owner: p.repo_owner.clone(),
        main_branch: p.main_branch.clone(),
        preset: temps_presets::runtime_slug(p.preset, p.preset_config.as_ref()),
        source_type: format!("{:?}", p.source_type),
        created_at: format_dt(&p.created_at),
        updated_at: format_dt(&p.updated_at),
        last_deployment: format_dt_opt(&p.last_deployment),
        enable_preview_environments: p.enable_preview_environments,
    }
}

fn environment_to_info(e: &temps_entities::environments::Model) -> EnvironmentInfo {
    EnvironmentInfo {
        id: e.id,
        project_id: e.project_id,
        name: e.name.clone(),
        slug: e.slug.clone(),
        branch: e.branch.clone(),
        is_preview: e.is_preview,
        current_deployment_id: e.current_deployment_id,
        created_at: format_dt(&e.created_at),
        updated_at: format_dt(&e.updated_at),
    }
}

fn deployment_to_info(d: &temps_entities::deployments::Model) -> DeploymentInfo {
    DeploymentInfo {
        id: d.id,
        project_id: d.project_id,
        environment_id: d.environment_id,
        state: d.state.clone(),
        branch: d.branch_ref.clone(),
        tag: d.tag_ref.clone(),
        commit_sha: d.commit_sha.clone(),
        commit_message: d.commit_message.clone(),
        commit_author: d.commit_author.clone(),
        created_at: format_dt(&d.created_at),
        started_at: format_dt_opt(&d.started_at),
        finished_at: format_dt_opt(&d.finished_at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_dt() {
        let dt = chrono::DateTime::parse_from_rfc3339("2025-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(format_dt(&dt), "2025-01-15T10:30:00Z");
    }

    #[test]
    fn test_format_dt_opt_some() {
        let dt = chrono::DateTime::parse_from_rfc3339("2025-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            format_dt_opt(&Some(dt)),
            Some("2025-06-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn test_format_dt_opt_none() {
        assert_eq!(format_dt_opt(&None), None);
    }
}
