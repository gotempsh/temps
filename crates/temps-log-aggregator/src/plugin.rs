//! Plugin registration for the log aggregator

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{self, create_log_aggregator_app_state, LogAggregatorAppState};
use crate::services::{
    ChunkWriterService, CollectorService, LogMetadataService, LogSearchService,
    RemoteContainerLogSource, RemoteLogCollectorService, RetentionService, TailService,
};
use crate::storage::{FilesystemStorage, LogStorage, S3Storage};
use crate::types::StorageConfig;

/// Interval for the periodic flush ticker (10 seconds)
const FLUSH_TICKER_INTERVAL: Duration = Duration::from_secs(10);

/// Interval for retention cleanup (24 hours)
const RETENTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the remote log collector reconciles its open streams against the
/// set of running remote containers (start new, drop gone).
const REMOTE_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum retries for the startup container scan
const STARTUP_SCAN_MAX_RETRIES: u32 = 5;

/// Base delay for startup scan retries (doubles each time)
const STARTUP_SCAN_BASE_DELAY: Duration = Duration::from_secs(2);

/// Delay before restarting the Docker events listener after it exits
const EVENTS_RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Log Aggregator Plugin for structured log collection, storage, search, and streaming
pub struct LogAggregatorPlugin {
    storage_config: StorageConfig,
}

impl LogAggregatorPlugin {
    pub fn new(storage_config: StorageConfig) -> Self {
        Self { storage_config }
    }
}

impl TempsPlugin for LogAggregatorPlugin {
    fn name(&self) -> &'static str {
        "log-aggregator"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Create storage backend based on config
            let storage: Arc<dyn LogStorage> = match &self.storage_config {
                StorageConfig::Filesystem { base_path } => {
                    Arc::new(FilesystemStorage::new(base_path.clone()).map_err(|e| {
                        PluginError::PluginRegistrationFailed {
                            plugin_name: "log-aggregator".to_string(),
                            error: format!("Failed to initialize filesystem storage: {}", e),
                        }
                    })?)
                }
                StorageConfig::S3 { .. } => {
                    Arc::new(S3Storage::new(&self.storage_config).map_err(|e| {
                        PluginError::PluginRegistrationFailed {
                            plugin_name: "log-aggregator".to_string(),
                            error: format!("Failed to initialize S3 storage: {}", e),
                        }
                    })?)
                }
            };
            context.register_service(storage.clone());

            // Database connection
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Chunk writer
            let chunk_writer = Arc::new(ChunkWriterService::new(storage.clone()));
            context.register_service(chunk_writer.clone());

            // Docker (required for collector)
            let docker = context.require_service::<bollard::Docker>();

            // Metadata service (used by collector to resume from last known position on restart)
            let collector_metadata = Arc::new(LogMetadataService::new(db.clone()));

            // Collector service — set the on_chunk_flushed callback before wrapping in Arc
            let mut collector = CollectorService::new(
                docker,
                chunk_writer.clone(),
                collector_metadata.clone(),
                10_000,
            )
            .with_db(db.clone());

            // Wire callback: when a chunk is flushed during streaming, insert chunk metadata into DB.
            // This callback runs in the collector's streaming task, so it must be Send + Sync.
            // Note: we do NOT insert into log_events — all searches read directly from chunk files.
            let cb_metadata = collector_metadata;
            collector.set_on_chunk_flushed(Arc::new(move |meta, _lines| {
                let metadata_svc = cb_metadata.clone();
                tokio::spawn(async move {
                    if let Err(e) = metadata_svc.insert_chunk_meta(&meta).await {
                        tracing::error!(
                            chunk_id = %meta.id,
                            error = %e,
                            "Failed to insert chunk metadata from collector callback"
                        );
                    }
                });
            }));

            let collector = Arc::new(collector);
            let tail_tx_sender = collector.tail_sender();
            context.register_service(collector.clone());

            // Metadata service
            let metadata_service = Arc::new(LogMetadataService::new(db.clone()));
            context.register_service(metadata_service.clone());

            // Search service
            let search_service = Arc::new(LogSearchService::new(
                storage.clone(),
                metadata_service.clone(),
            ));
            context.register_service(search_service.clone());

            // Tail service
            let tail_service = Arc::new(TailService::new(tail_tx_sender));
            context.register_service(tail_service.clone());

            // Retention service
            let retention_service = Arc::new(RetentionService::new(
                storage.clone(),
                metadata_service.clone(),
            ));
            context.register_service(retention_service.clone());

            // Audit service
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // App state for handlers
            let app_state = create_log_aggregator_app_state(
                search_service,
                metadata_service,
                tail_service,
                retention_service,
                audit_service,
            )
            .await;
            context.register_service(app_state);

            tracing::debug!("Log aggregator plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let chunk_writer = context.require_service::<ChunkWriterService>();
            let metadata_service = context.require_service::<LogMetadataService>();
            let collector = context.require_service::<CollectorService>();
            let docker = context.require_service::<bollard::Docker>();
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let retention_service = context.require_service::<RetentionService>();
            let retention_metadata = context.require_service::<LogMetadataService>();

            // ── Flush ticker ────────────────────────────────────────────
            // Periodically flushes expired buffers (those that have exceeded the 30s threshold)
            // and inserts chunk metadata into the database
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(FLUSH_TICKER_INTERVAL);
                loop {
                    interval.tick().await;
                    let results = chunk_writer.flush_expired().await;
                    for result in results {
                        match result {
                            Ok(flush_result) => {
                                if let Err(e) =
                                    metadata_service.insert_chunk_meta(&flush_result.meta).await
                                {
                                    tracing::error!(
                                        chunk_id = %flush_result.meta.id,
                                        error = %e,
                                        "Failed to insert chunk metadata from flush ticker"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Flush ticker encountered error"
                                );
                            }
                        }
                    }
                }
            });
            tracing::info!(
                "Log aggregator flush ticker started (interval: {:?})",
                FLUSH_TICKER_INTERVAL
            );

            // ── Remote worker-node log collector ────────────────────────
            // If a RemoteContainerLogSource is registered (multi-node setups —
            // temps-deployments provides it), run a reconcile loop that keeps a
            // log stream open for every running remote container and feeds the
            // lines into the SAME chunk pipeline as local logs. Single-node and
            // test setups register no source, so this is skipped entirely.
            if let Some(remote_source) = context.get_service::<dyn RemoteContainerLogSource>() {
                let remote_chunk_writer = context.require_service::<ChunkWriterService>();
                let remote_metadata = context.require_service::<LogMetadataService>();
                let remote_tail_tx = context.require_service::<CollectorService>().tail_sender();
                let remote_collector = Arc::new(RemoteLogCollectorService::new(
                    remote_source,
                    remote_chunk_writer,
                    remote_metadata,
                    remote_tail_tx,
                ));
                tokio::spawn(async move {
                    // Small initial delay so node registration / agent readiness
                    // settles before the first reconcile.
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    let mut interval = tokio::time::interval(REMOTE_RECONCILE_INTERVAL);
                    loop {
                        interval.tick().await;
                        if let Err(e) = remote_collector.reconcile().await {
                            tracing::warn!(
                                error = %e,
                                "Remote log collector reconcile failed; will retry"
                            );
                        }
                    }
                });
                tracing::info!(
                    "Remote log collector started (reconcile interval: {:?})",
                    REMOTE_RECONCILE_INTERVAL
                );
            } else {
                tracing::debug!(
                    "No remote container log source registered — remote log collection disabled"
                );
            }

            // ── Container discovery: startup scan ───────────────────────
            // Find already-running containers and start streaming. Two label
            // families are collected: deployment/application containers
            // (`sh.temps.project_id`) and imported/managed external-service
            // containers (`temps.service_type`). Docker's `label` filter ANDs
            // multiple values, so each family needs its own list call; the IDs
            // are unioned. Retries with exponential backoff if Docker is
            // temporarily unavailable.
            let startup_collector = collector.clone();
            let startup_docker = docker.clone();
            let startup_db = db.clone();
            tokio::spawn(async move {
                use bollard::query_parameters::ListContainersOptions;
                use std::collections::{HashMap, HashSet};

                let scan_labels = ["sh.temps.project_id", "temps.service_type"];

                let mut delay = STARTUP_SCAN_BASE_DELAY;
                for attempt in 0..=STARTUP_SCAN_MAX_RETRIES {
                    let mut scan_result: Result<HashSet<String>, bollard::errors::Error> =
                        Ok(HashSet::new());
                    for label in scan_labels {
                        let mut filters = HashMap::new();
                        filters.insert("status".to_string(), vec!["running".to_string()]);
                        filters.insert("label".to_string(), vec![label.to_string()]);
                        let options = ListContainersOptions {
                            all: false,
                            filters: Some(filters),
                            ..Default::default()
                        };
                        match startup_docker.list_containers(Some(options)).await {
                            Ok(containers) => {
                                if let Ok(ids) = scan_result.as_mut() {
                                    ids.extend(containers.into_iter().filter_map(|c| c.id));
                                }
                            }
                            Err(e) => {
                                scan_result = Err(e);
                                break;
                            }
                        }
                    }

                    // Imported external-service containers carry NO temps.*
                    // labels, so the label scans above miss them. Discover them
                    // by the plaintext container names recorded at import time
                    // and add any that are running by name filter.
                    if let Ok(ids) = scan_result.as_mut() {
                        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
                        let imported_names: Vec<String> =
                            temps_entities::external_services::Entity::find()
                                .filter(
                                    temps_entities::external_services::Column::ContainerName
                                        .is_not_null(),
                                )
                                .select_only()
                                .column(temps_entities::external_services::Column::ContainerName)
                                .into_tuple::<Option<String>>()
                                .all(startup_db.as_ref())
                                .await
                                .unwrap_or_default()
                                .into_iter()
                                .flatten()
                                .collect();
                        for name in imported_names {
                            let mut filters = HashMap::new();
                            filters.insert("status".to_string(), vec!["running".to_string()]);
                            filters.insert("name".to_string(), vec![name.clone()]);
                            let options = ListContainersOptions {
                                all: false,
                                filters: Some(filters),
                                ..Default::default()
                            };
                            if let Ok(containers) =
                                startup_docker.list_containers(Some(options)).await
                            {
                                ids.extend(containers.into_iter().filter_map(|c| c.id));
                            }
                        }
                    }

                    // Local cluster members (monitor/primary/replica) carry
                    // deployment-style `sh.temps.service.*` labels the scans
                    // above don't target, and their names live in
                    // `service_members`, not on the service row. Discover the
                    // control-plane-local ones (node_id IS NULL) by name — the
                    // collector resolves each to its owning service via
                    // `service_members.container_name`. Remote members are
                    // handled separately by the remote collector.
                    if let Ok(ids) = scan_result.as_mut() {
                        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
                        let member_names: Vec<String> =
                            temps_entities::service_members::Entity::find()
                                .filter(temps_entities::service_members::Column::NodeId.is_null())
                                .select_only()
                                .column(temps_entities::service_members::Column::ContainerName)
                                .into_tuple::<String>()
                                .all(startup_db.as_ref())
                                .await
                                .unwrap_or_default();
                        for name in member_names {
                            let mut filters = HashMap::new();
                            filters.insert("status".to_string(), vec!["running".to_string()]);
                            filters.insert("name".to_string(), vec![name.clone()]);
                            let options = ListContainersOptions {
                                all: false,
                                filters: Some(filters),
                                ..Default::default()
                            };
                            if let Ok(containers) =
                                startup_docker.list_containers(Some(options)).await
                            {
                                ids.extend(containers.into_iter().filter_map(|c| c.id));
                            }
                        }
                    }

                    match scan_result {
                        Ok(ids) => {
                            let count = ids.len();
                            for id in ids {
                                if let Err(e) = startup_collector.start_streaming(&id).await {
                                    tracing::warn!(
                                        container_id = %id,
                                        error = %e,
                                        "Failed to start streaming for existing container"
                                    );
                                }
                            }
                            tracing::info!(
                                container_count = count,
                                "Startup scan complete: discovered running containers"
                            );
                            return; // Success — exit the retry loop
                        }
                        Err(e) => {
                            if attempt < STARTUP_SCAN_MAX_RETRIES {
                                tracing::warn!(
                                    error = %e,
                                    attempt = attempt + 1,
                                    max_retries = STARTUP_SCAN_MAX_RETRIES,
                                    retry_delay_secs = delay.as_secs(),
                                    "Startup scan failed, retrying"
                                );
                                tokio::time::sleep(delay).await;
                                delay = std::cmp::min(delay * 2, Duration::from_secs(30));
                            } else {
                                tracing::error!(
                                    error = %e,
                                    "Startup scan failed after {} retries, giving up. \
                                     Running containers will be discovered via Docker events instead.",
                                    STARTUP_SCAN_MAX_RETRIES
                                );
                            }
                        }
                    }
                }
            });

            // ── Container discovery: Docker events listener ─────────────
            // Listen for container start/stop events to dynamically start/stop streaming.
            //
            // Outer loop: if the events stream ends (returns None) or Docker goes
            // down, we wait and restart the stream. This task never exits unless
            // the tokio runtime is shut down.
            let events_collector = collector.clone();
            let events_docker = docker.clone();
            tokio::spawn(async move {
                use bollard::models::EventMessageTypeEnum;

                loop {
                    tracing::debug!("Opening Docker events stream");
                    let options = bollard::query_parameters::EventsOptionsBuilder::new().build();
                    let mut stream = events_docker.events(Some(options));

                    // Inner loop: process events from this stream instance
                    loop {
                        match stream.next().await {
                            Some(Ok(event)) => {
                                let is_container =
                                    event.typ == Some(EventMessageTypeEnum::CONTAINER);
                                if !is_container {
                                    continue;
                                }
                                let action = event.action.as_deref().unwrap_or("");
                                let container_id = event
                                    .actor
                                    .as_ref()
                                    .and_then(|a| a.id.as_deref())
                                    .unwrap_or("");

                                if container_id.is_empty() {
                                    continue;
                                }

                                match action {
                                    "start" => {
                                        tracing::debug!(
                                            container_id = container_id,
                                            "Docker event: container started"
                                        );
                                        if let Err(e) =
                                            events_collector.start_streaming(container_id).await
                                        {
                                            tracing::debug!(
                                                container_id = container_id,
                                                error = %e,
                                                "Failed to start streaming (may not have temps labels)"
                                            );
                                        }
                                    }
                                    "stop" | "die" | "kill" => {
                                        tracing::debug!(
                                            container_id = container_id,
                                            action = action,
                                            "Docker event: container stopped"
                                        );
                                        events_collector.stop_streaming(container_id).await;
                                    }
                                    _ => {}
                                }
                            }
                            Some(Err(e)) => {
                                tracing::warn!(
                                    error = %e,
                                    "Docker events stream error, reconnecting in {:?}",
                                    EVENTS_RECONNECT_DELAY
                                );
                                // Break inner loop to restart from outer loop
                                break;
                            }
                            None => {
                                // Stream returned None — Docker may have closed the connection.
                                tracing::warn!(
                                    "Docker events stream ended, reconnecting in {:?}",
                                    EVENTS_RECONNECT_DELAY
                                );
                                break;
                            }
                        }
                    }

                    // Wait before restarting the events stream
                    tokio::time::sleep(EVENTS_RECONNECT_DELAY).await;
                }
            });
            tracing::info!("Container discovery started (events listener + startup scan)");

            // ── Retention scheduler ─────────────────────────────────────
            // Run retention cleanup once every 24 hours
            tokio::spawn(async move {
                let retention_config = crate::types::RetentionConfig::default();
                let mut interval = tokio::time::interval(RETENTION_INTERVAL);
                loop {
                    interval.tick().await;

                    // Find all distinct project_ids that have log_chunks
                    match retention_metadata.list_distinct_projects().await {
                        Ok(project_ids) => {
                            tracing::info!(
                                project_count = project_ids.len(),
                                "Running retention cleanup"
                            );
                            for project_id in project_ids {
                                if let Err(e) = retention_service
                                    .cleanup_project(project_id, &retention_config)
                                    .await
                                {
                                    tracing::error!(
                                        project_id = %project_id,
                                        error = %e,
                                        "Retention cleanup failed for project"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to list projects for retention cleanup");
                        }
                    }
                }
            });
            tracing::info!(
                "Retention scheduler started (interval: {:?})",
                RETENTION_INTERVAL
            );

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.require_service::<LogAggregatorAppState>();
        let routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<handlers::LogAggregatorApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_log_aggregator_plugin_name() {
        let plugin = LogAggregatorPlugin::new(StorageConfig::Filesystem {
            base_path: PathBuf::from("/tmp/test-logs"),
        });
        assert_eq!(plugin.name(), "log-aggregator");
    }
}
