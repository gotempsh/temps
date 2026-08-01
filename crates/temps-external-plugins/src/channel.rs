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
use temps_core::external_plugin::PluginEvent;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
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
        let ws_stream = match tokio_tungstenite::client_async(&uri, stream).await {
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
                        warn!(
                            plugin = %reader_plugin_name,
                            "Invalid channel message from plugin: {}", e
                        );
                        continue;
                    }
                };

                match msg {
                    ChannelMessage::Request(req) => {
                        let response = dispatch_request(&reader_plugin_name, &db, &req).await;
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

// ── Request dispatch ───────────────────────────────────────────────────

/// Route a plugin request to the appropriate query handler.
async fn dispatch_request(
    plugin_name: &str,
    db: &DatabaseConnection,
    req: &ChannelRequest,
) -> ChannelResponse {
    debug!(
        plugin = %plugin_name,
        method = %req.method,
        id = req.id,
        "Dispatching channel request"
    );

    match req.method.as_str() {
        "get_project" => handle_get_project(db, req).await,
        "list_projects" => handle_list_projects(db, req).await,
        "get_environment" => handle_get_environment(db, req).await,
        "list_environments" => handle_list_environments(db, req).await,
        "get_deployment" => handle_get_deployment(db, req).await,
        "get_last_deployment" => handle_get_last_deployment(db, req).await,
        "list_deployments" => handle_list_deployments(db, req).await,
        _ => ChannelResponse::err(
            req.id,
            ChannelErrorCode::MethodNotFound,
            format!("Unknown method: {}", req.method),
        ),
    }
}

// ── Method handlers ────────────────────────────────────────────────────

async fn handle_get_project(db: &DatabaseConnection, req: &ChannelRequest) -> ChannelResponse {
    let project_id = match req.params.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id as i32,
        None => {
            return ChannelResponse::err(
                req.id,
                ChannelErrorCode::InvalidParams,
                "Missing required parameter: project_id",
            )
        }
    };

    use temps_entities::projects;

    match projects::Entity::find_by_id(project_id)
        .filter(projects::Column::IsDeleted.eq(false))
        .one(db)
        .await
    {
        Ok(Some(project)) => {
            let info = project_to_info(&project);
            ChannelResponse::ok(req.id, serde_json::to_value(info).unwrap())
        }
        Ok(None) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::NotFound,
            format!("Project {} not found", project_id),
        ),
        Err(e) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::Internal,
            format!("Database error: {}", e),
        ),
    }
}

async fn handle_list_projects(db: &DatabaseConnection, req: &ChannelRequest) -> ChannelResponse {
    use temps_entities::projects;

    let limit = req
        .params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(100)
        .min(100);

    match projects::Entity::find()
        .filter(projects::Column::IsDeleted.eq(false))
        .order_by_asc(projects::Column::Name)
        .all(db)
        .await
    {
        Ok(projects) => {
            let infos: Vec<ProjectInfo> = projects
                .iter()
                .take(limit as usize)
                .map(project_to_info)
                .collect();
            ChannelResponse::ok(req.id, serde_json::to_value(infos).unwrap())
        }
        Err(e) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::Internal,
            format!("Database error: {}", e),
        ),
    }
}

async fn handle_get_environment(db: &DatabaseConnection, req: &ChannelRequest) -> ChannelResponse {
    let environment_id = match req.params.get("environment_id").and_then(|v| v.as_i64()) {
        Some(id) => id as i32,
        None => {
            return ChannelResponse::err(
                req.id,
                ChannelErrorCode::InvalidParams,
                "Missing required parameter: environment_id",
            )
        }
    };

    use temps_entities::environments;

    match environments::Entity::find_by_id(environment_id)
        .filter(environments::Column::DeletedAt.is_null())
        .one(db)
        .await
    {
        Ok(Some(env)) => {
            let info = environment_to_info(&env);
            ChannelResponse::ok(req.id, serde_json::to_value(info).unwrap())
        }
        Ok(None) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::NotFound,
            format!("Environment {} not found", environment_id),
        ),
        Err(e) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::Internal,
            format!("Database error: {}", e),
        ),
    }
}

async fn handle_list_environments(
    db: &DatabaseConnection,
    req: &ChannelRequest,
) -> ChannelResponse {
    let project_id = match req.params.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id as i32,
        None => {
            return ChannelResponse::err(
                req.id,
                ChannelErrorCode::InvalidParams,
                "Missing required parameter: project_id",
            )
        }
    };

    use temps_entities::environments;

    match environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project_id))
        .filter(environments::Column::DeletedAt.is_null())
        .order_by_asc(environments::Column::Name)
        .all(db)
        .await
    {
        Ok(envs) => {
            let infos: Vec<EnvironmentInfo> = envs.iter().map(environment_to_info).collect();
            ChannelResponse::ok(req.id, serde_json::to_value(infos).unwrap())
        }
        Err(e) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::Internal,
            format!("Database error: {}", e),
        ),
    }
}

async fn handle_get_deployment(db: &DatabaseConnection, req: &ChannelRequest) -> ChannelResponse {
    let deployment_id = match req.params.get("deployment_id").and_then(|v| v.as_i64()) {
        Some(id) => id as i32,
        None => {
            return ChannelResponse::err(
                req.id,
                ChannelErrorCode::InvalidParams,
                "Missing required parameter: deployment_id",
            )
        }
    };

    use temps_entities::deployments;

    match deployments::Entity::find_by_id(deployment_id).one(db).await {
        Ok(Some(dep)) => {
            let info = deployment_to_info(&dep);
            ChannelResponse::ok(req.id, serde_json::to_value(info).unwrap())
        }
        Ok(None) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::NotFound,
            format!("Deployment {} not found", deployment_id),
        ),
        Err(e) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::Internal,
            format!("Database error: {}", e),
        ),
    }
}

async fn handle_get_last_deployment(
    db: &DatabaseConnection,
    req: &ChannelRequest,
) -> ChannelResponse {
    let project_id = match req.params.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id as i32,
        None => {
            return ChannelResponse::err(
                req.id,
                ChannelErrorCode::InvalidParams,
                "Missing required parameter: project_id",
            )
        }
    };

    let environment_id = req
        .params
        .get("environment_id")
        .and_then(|v| v.as_i64())
        .map(|id| id as i32);

    use temps_entities::deployments;

    let mut query = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .order_by_desc(deployments::Column::CreatedAt);

    if let Some(env_id) = environment_id {
        query = query.filter(deployments::Column::EnvironmentId.eq(env_id));
    }

    match query.one(db).await {
        Ok(Some(dep)) => {
            let info = deployment_to_info(&dep);
            ChannelResponse::ok(req.id, serde_json::to_value(info).unwrap())
        }
        Ok(None) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::NotFound,
            format!(
                "No deployments found for project {}{}",
                project_id,
                environment_id
                    .map(|e| format!(" environment {}", e))
                    .unwrap_or_default()
            ),
        ),
        Err(e) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::Internal,
            format!("Database error: {}", e),
        ),
    }
}

async fn handle_list_deployments(db: &DatabaseConnection, req: &ChannelRequest) -> ChannelResponse {
    let project_id = match req.params.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id as i32,
        None => {
            return ChannelResponse::err(
                req.id,
                ChannelErrorCode::InvalidParams,
                "Missing required parameter: project_id",
            )
        }
    };

    let environment_id = req
        .params
        .get("environment_id")
        .and_then(|v| v.as_i64())
        .map(|id| id as i32);

    let limit = req
        .params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .min(100);

    use sea_orm::QuerySelect;
    use temps_entities::deployments;

    let mut query = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .order_by_desc(deployments::Column::CreatedAt)
        .limit(limit);

    if let Some(env_id) = environment_id {
        query = query.filter(deployments::Column::EnvironmentId.eq(env_id));
    }

    match query.all(db).await {
        Ok(deps) => {
            let infos: Vec<DeploymentInfo> = deps.iter().map(deployment_to_info).collect();
            ChannelResponse::ok(req.id, serde_json::to_value(infos).unwrap())
        }
        Err(e) => ChannelResponse::err(
            req.id,
            ChannelErrorCode::Internal,
            format!("Database error: {}", e),
        ),
    }
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
