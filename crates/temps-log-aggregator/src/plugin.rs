// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use crate::chunk::cache::ChunkCache;
use crate::handlers::{self, create_log_aggregator_app_state, LogAggregatorAppState};
use crate::index::clickhouse::{ClickHouseLineIndex, LineIndexTarget};
use crate::index::timescale::TimescaleLineIndex;
use crate::index::{LineIndex, NoLineIndex};
use crate::services::{
    ChunkWriterService, CollectorService, CompactorService, LogMetadataService, LogSearchService,
    RemoteContainerLogSource, RemoteLogCollectorService, RetentionService, TailService,
};
use crate::services::{
    ForgetSweeper, ReindexService, DEFAULT_FORGET_BATCH, DEFAULT_REINDEX_BATCH,
    FORGET_SWEEP_BURST_PAUSE, FORGET_SWEEP_INTERVAL,
};
use crate::storage::{FilesystemStorage, LogStorage, S3Storage};
use crate::store::chunk_store::ChunkStore;
use crate::store::manifest::ManifestRepo;
use crate::store::LogLineStore;
use crate::types::StorageConfig;
use temps_clickhouse::ClickHouseConfig;

/// The read cache and head-buffer budgets live in Settings → Monitoring
/// (`container_logs.cache_mb` / `head_buffer_mb`, persisted and audited);
/// this is how often a change is picked up without a restart.
const BUDGET_SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// Interval for the periodic flush ticker (10 seconds)
const FLUSH_TICKER_INTERVAL: Duration = Duration::from_secs(10);

/// Retention runs hourly: cheap (one indexed query per project) and it keeps
/// the line index TTL within an hour of a settings change.
const RETENTION_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Tombstoned chunk objects are removed this often (grace period applies).
const GC_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Yesterday's fragmented chunks are merged this often.
const COMPACTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Delay before the first compaction pass after boot.
const COMPACTION_STARTUP_DELAY: Duration = Duration::from_secs(5 * 60);

/// Delay before the reindexer's first pass after boot (let ingest settle).
const REINDEX_STARTUP_DELAY: Duration = Duration::from_secs(30);
/// Idle interval between reindex passes when the queue is empty.
const REINDEX_INTERVAL: Duration = Duration::from_secs(60);
/// Pause between passes while a backlog is being drained (rate limit).
const REINDEX_BURST_PAUSE: Duration = Duration::from_secs(2);

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
    /// ClickHouse connection for the ADR-047 line index, resolved once by
    /// the composition root from `ServerConfig` (the single home of the
    /// instance's ClickHouse connection, shared with analytics, OTel, proxy
    /// logs and metrics — ADR-012). `None` = no local ClickHouse; the index
    /// then goes to Temps Cloud's ClickHouse when the instance is linked,
    /// else to the control-plane TimescaleDB (ADR-047 §8).
    line_index_config: Option<ClickHouseConfig>,
}

impl LogAggregatorPlugin {
    pub fn new(storage_config: StorageConfig) -> Self {
        Self {
            storage_config,
            line_index_config: None,
        }
    }

    /// Enable the ClickHouse line index with the instance's connection.
    pub fn with_line_index(mut self, config: Option<ClickHouseConfig>) -> Self {
        self.line_index_config = config;
        self
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

            // ── ADR-046 chunk read-through cache ─────────────────────────
            // Built before the chunk writer so the writer can write-through
            // into the same cache a sealing node reads from (ADR-046 §6a).
            let data_dir = std::env::var("TEMPS_DATA_DIR")
                .map(std::path::PathBuf::from)
                .ok();
            let cache_dir = data_dir.as_ref().map(|dir| dir.join("logs").join("cache"));
            // WIRE: if TEMPS_DATA_DIR cannot be resolved, fall back to an
            // in-memory-only cache rather than failing plugin registration —
            // a missing cache only costs latency, never correctness.
            // Opened at the default budget; the persisted setting is applied
            // as soon as the plugin starts (and re-applied every minute).
            let cache_bytes =
                u64::from(temps_core::ContainerLogSettings::default().cache_mb) * 1024 * 1024;
            let cache = ChunkCache::open(cache_dir, cache_bytes)
                .await
                .map_err(|e| PluginError::PluginRegistrationFailed {
                    plugin_name: "log-aggregator".to_string(),
                    error: format!("Failed to open log chunk cache: {e}"),
                })?;
            context.register_service(Arc::new(cache.clone()));

            // Chunk writer (ADR-046): owns the whole seal pipeline (encode →
            // object write → manifest insert → WAL truncate). The WAL gives
            // crash-safety for the unflushed head window; when
            // `TEMPS_DATA_DIR` cannot be resolved the writer simply runs
            // without WAL protection rather than failing to start.
            let wal_dir = data_dir.map(|dir| dir.join("logs").join("wal"));

            // ADR-047 §8: the per-line index lives in the first store that
            // is available — the instance's ClickHouse, then Temps Cloud's
            // ClickHouse (through the telemetry proxies), then the
            // control-plane TimescaleDB. Any reason a preferred store is
            // skipped is logged; the chosen store is reported by the
            // capabilities endpoint so the operator always knows where the
            // index is (never a silent downgrade).
            let selected_index = select_line_index(
                self.line_index_config.as_ref(),
                context.get_service::<temps_cloud_client::CloudLink>(),
                db.clone(),
            )
            .await;

            // A different store than last start means the existing
            // `indexed_at` marks describe rows queries will never read
            // again: clear them so the reindexer rebuilds the index here.
            // This must succeed *before* the new store is exposed to any
            // reader or writer — if it silently failed, chunks that really
            // do need reindexing would keep their old `indexed_at` marks,
            // the reindexer would skip them (it only looks at
            // `indexed_at IS NULL`), and facets/histograms/aggregates/
            // attribute search would read as complete while actually
            // missing everything not yet in the new store, with nothing
            // short of a lucky future backend flap ever correcting it.
            // Retried with the seal path's backoff (a transient control-
            // plane blip should not be treated the same as "will never
            // work"); if it still fails, the index stays disabled rather
            // than exposed in a state nothing can distinguish from correct.
            let line_index: Arc<dyn LineIndex> = if let Some(backend) = selected_index.backend() {
                let manifests = ManifestRepo::new(db.clone());
                match crate::services::retry_with_backoff(|| {
                    manifests.activate_index_backend(backend.as_str())
                })
                .await
                {
                    Ok(true) => {
                        tracing::info!(
                            backend = backend.as_str(),
                            "log line index store changed; every live chunk queued for reindex"
                        );
                        selected_index
                    }
                    Ok(false) => selected_index,
                    Err(e) => {
                        let reason = format!(
                            "log line index disabled: could not durably record the active \
                             backend ({backend}: {e}) — refusing to serve or write to a store \
                             whose transition from the previous one is not confirmed; this \
                             will retry on the next restart",
                            backend = backend.as_str(),
                        );
                        tracing::error!(error = %e, backend = backend.as_str(), "{reason}");
                        Arc::new(NoLineIndex::new(reason))
                    }
                }
            } else {
                selected_index
            };

            let chunk_writer = ChunkWriterService::open_deferred_with_index(
                storage.clone(),
                Arc::new(ManifestRepo::new(db.clone())),
                wal_dir,
                Some(cache.clone()),
                line_index.clone(),
            )
            .await
            .map_err(|e| PluginError::PluginRegistrationFailed {
                plugin_name: "log-aggregator".to_string(),
                error: format!("Failed to open chunk writer: {e}"),
            })?;
            context.register_service(chunk_writer.clone());

            // DockerHandle — always registered; CollectorService holds it and
            // resolves the daemon at streaming time via require(), returning a typed
            // error on a control-plane process instead of failing at startup.
            let docker_handle = context.require_service::<temps_core::DockerHandle>();

            // Metadata service (used by collector to resume from last known position on restart)
            let collector_metadata = Arc::new(LogMetadataService::new(db.clone()));

            // Collector service. The chunk writer now owns the whole seal
            // pipeline (object write + manifest insert) itself, so the
            // collector no longer needs an on-chunk-flushed callback.
            let collector = CollectorService::new(
                docker_handle,
                chunk_writer.clone(),
                collector_metadata,
                10_000,
            )
            .with_db(db.clone());

            let collector = Arc::new(collector);
            let tail_tx_sender = collector.tail_sender();
            context.register_service(collector.clone());

            // Metadata service
            let metadata_service = Arc::new(LogMetadataService::new(db.clone()));
            context.register_service(metadata_service.clone());

            // ── ADR-046 chunk-backed log line store ─────────────────────
            // `log_chunks` manifests (Postgres, one row per chunk — never per
            // line) plus the read-through cache over the object-storage
            // bytes built above. `ManifestRepo` is a thin wrapper over the
            // shared DB connection, so a second instance for
            // `RetentionService` is cheap — it is not `Clone` because it has
            // no state worth sharing beyond that.
            //
            // `chunk_writer` doubles as the store's `HeadSource`: unsealed
            // head-buffer lines become visible to search before they are
            // flushed to object storage (ADR-046 §4).
            let store: Arc<dyn LogLineStore> = Arc::new(
                ChunkStore::new(
                    ManifestRepo::new(db.clone()),
                    storage.clone(),
                    cache,
                    chunk_writer.clone(),
                )
                .with_line_index(line_index.clone()),
            );
            context.register_service(line_index.clone());
            context.register_service(store.clone());

            // Search service
            let search_service = Arc::new(LogSearchService::new(
                store.clone(),
                metadata_service.clone(),
            ));
            context.register_service(search_service.clone());

            // Tail service
            let tail_service = Arc::new(TailService::new(tail_tx_sender));
            context.register_service(tail_service.clone());

            // Retention service
            let retention_service = Arc::new(
                RetentionService::new(
                    Arc::new(ManifestRepo::new(db.clone())),
                    metadata_service.clone(),
                )
                .with_chunk_writer(chunk_writer.clone())
                .with_line_index(line_index.clone()),
            );
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
                store,
                db.clone(),
                line_index,
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
            chunk_writer.start_background_recovery();
            let collector = context.require_service::<CollectorService>();
            let docker_handle = context.require_service::<temps_core::DockerHandle>();
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let retention_service = context.require_service::<RetentionService>();
            let compactor_storage = context.require_service::<dyn LogStorage>();
            let compactor_cache = (*context.require_service::<ChunkCache>()).clone();
            let retention_metadata = context.require_service::<LogMetadataService>();

            // ── Flush ticker ────────────────────────────────────────────
            // Seals every head buffer whose flush policy (ADR-046 §1) says
            // it's due. The writer owns the whole seal pipeline itself now
            // (object write + manifest insert + WAL truncate), so there is
            // nothing left for the ticker to do with a result.
            let flush_chunk_writer = chunk_writer.clone();
            let recovery_writer = chunk_writer.clone();
            tokio::spawn(async move {
                recovery_writer.wait_for_recovery().await;
                let mut interval = tokio::time::interval(FLUSH_TICKER_INTERVAL);
                loop {
                    interval.tick().await;
                    flush_chunk_writer.flush_expired().await;
                }
            });
            tracing::info!(
                "Log aggregator flush ticker started (interval: {:?})",
                FLUSH_TICKER_INTERVAL
            );

            // ── WAL sync ticker ─────────────────────────────────────────
            // Flushes and fsyncs every open per-container WAL file so at
            // most ~1s of ingest is unsynced at any time (ADR-046 §1).
            let sync_chunk_writer = chunk_writer.clone();
            let recovery_writer = chunk_writer.clone();
            tokio::spawn(async move {
                recovery_writer.wait_for_recovery().await;
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                loop {
                    interval.tick().await;
                    sync_chunk_writer.sync_wals().await;
                }
            });
            tracing::info!("Log aggregator WAL sync ticker started (interval: 1s)");

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
                let recovery_writer = chunk_writer.clone();
                tokio::spawn(async move {
                    recovery_writer.wait_for_recovery().await;
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

            // ── Container discovery (local daemon only) ─────────────────
            // The startup scan and the Docker events listener stream logs
            // from containers on THIS host's daemon. On a `control-plane`
            // profile there is no daemon and no local workload to tail; the
            // remote collector above already covers worker-node containers.
            match local_discovery_plan(&docker_handle) {
                LocalDiscoveryPlan::Start(docker) => {
                    let recovery_writer = chunk_writer.clone();
                    let collector = collector.clone();
                    let db = db.clone();
                    tokio::spawn(async move {
                        recovery_writer.wait_for_recovery().await;
                        spawn_local_container_discovery(docker, collector, db);
                    });
                }
                LocalDiscoveryPlan::Skip { reason } => tracing::info!("{}", reason),
            }

            // ── Budget sync ─────────────────────────────────────────────
            // Settings → Monitoring → container logs: read cache size and
            // per-container head buffer. Applied now and re-read every
            // minute so a saved setting never needs a restart.
            if let Some(config_service) = context.get_service::<temps_config::ConfigService>() {
                let cache = context.require_service::<ChunkCache>();
                let writer = context.require_service::<ChunkWriterService>();
                let recovery_writer = chunk_writer.clone();
                tokio::spawn(async move {
                    recovery_writer.wait_for_recovery().await;
                    let mut interval = tokio::time::interval(BUDGET_SYNC_INTERVAL);
                    loop {
                        interval.tick().await;
                        match config_service.get_settings().await {
                            Ok(settings) => {
                                let logs = &settings.container_logs;
                                let cache_bytes = u64::from(logs.cache_mb) * 1024 * 1024;
                                let head_bytes = logs.head_buffer_mb as usize * 1024 * 1024;
                                if cache.max_bytes() != cache_bytes {
                                    tracing::info!(
                                        cache_mb = logs.cache_mb,
                                        "log read cache budget applied"
                                    );
                                    cache.set_max_bytes(cache_bytes).await;
                                }
                                if writer.head_max_bytes() != head_bytes {
                                    tracing::info!(
                                        head_buffer_mb = logs.head_buffer_mb,
                                        "log head buffer cap applied"
                                    );
                                    writer.set_head_max_bytes(head_bytes);
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "could not read container log budgets; keeping current")
                            }
                        }
                    }
                });
            }

            // ── Retention scheduler ─────────────────────────────────────
            // Run retention cleanup once every 24 hours
            let retention_settings = context.get_service::<temps_config::ConfigService>();
            let recovery_writer = chunk_writer.clone();
            tokio::spawn(async move {
                recovery_writer.wait_for_recovery().await;
                let mut interval = tokio::time::interval(RETENTION_INTERVAL);
                loop {
                    interval.tick().await;

                    // Settings → Monitoring → retention. Falls back to the
                    // default window when the config service is absent
                    // (tests) or unreadable, and says so.
                    let mut retention_config = crate::types::RetentionConfig::default();
                    match &retention_settings {
                        Some(config_service) => match config_service.get_settings().await {
                            Ok(settings) => {
                                retention_config.chunk_retention_days =
                                    settings.observability_retention.container_logs_days;
                            }
                            Err(e) => tracing::warn!(
                                error = %e,
                                default_days = retention_config.chunk_retention_days,
                                "could not read container log retention setting; using default"
                            ),
                        },
                        None => tracing::debug!("config service not registered; default retention"),
                    }
                    retention_service
                        .sync_index_retention(&retention_config)
                        .await;

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

            // ── Compaction + GC (ADR-046 §8a.1, §8a.3) ─────────────────
            // GC deletes objects whose manifest tombstone is older than the
            // grace period; compaction merges yesterday's fragmented chunks.
            // Both are idempotent, so a restart mid-run is harmless.
            let line_index = context.require_service::<dyn LineIndex>();
            let compactor = Arc::new(
                CompactorService::new(
                    Arc::new(ManifestRepo::new(db.clone())),
                    compactor_storage,
                    Some(compactor_cache),
                )
                .with_line_index(line_index.clone()),
            );
            let gc = compactor.clone();
            let recovery_writer = chunk_writer.clone();
            tokio::spawn(async move {
                recovery_writer.wait_for_recovery().await;
                let mut interval = tokio::time::interval(GC_INTERVAL);
                loop {
                    interval.tick().await;
                    gc.gc_once().await;
                }
            });
            let recovery_writer = chunk_writer.clone();
            tokio::spawn(async move {
                recovery_writer.wait_for_recovery().await;
                // First pass shortly after boot so an upgrade picks up the
                // backlog; then daily.
                tokio::time::sleep(COMPACTION_STARTUP_DELAY).await;
                let mut interval = tokio::time::interval(COMPACTION_INTERVAL);
                loop {
                    interval.tick().await;
                    compactor.reconcile_once().await;
                    compactor.run_once().await;
                }
            });
            tracing::info!(
                "Log chunk compaction scheduled (interval: {:?}); gc interval {:?}",
                COMPACTION_INTERVAL,
                GC_INTERVAL
            );

            // ── Reindexer (ADR-047 §6) ─────────────────────────────────
            // Drains `indexed_at IS NULL` chunks into the line index: the
            // backlog from before ClickHouse was configured, chunks sealed
            // while it was down, and compactor output. Runs in short bursts
            // while there is work, then idles on the interval. Scheduled
            // whenever a store is selected — not on `unavailable_reason()`,
            // which is dynamic for Temps Cloud (telemetry export is switched
            // on after startup) and would leave the backlog undrained.
            if line_index.backend().is_some() {
                let reindexer = ReindexService::new(
                    Arc::new(ManifestRepo::new(db.clone())),
                    context.require_service::<dyn LogStorage>(),
                    line_index.clone(),
                );
                let recovery_writer = chunk_writer.clone();
                tokio::spawn(async move {
                    recovery_writer.wait_for_recovery().await;
                    tokio::time::sleep(REINDEX_STARTUP_DELAY).await;
                    loop {
                        let report = reindexer.run_once(DEFAULT_REINDEX_BATCH).await;
                        let pause = if report.more && report.failed == 0 {
                            REINDEX_BURST_PAUSE
                        } else {
                            REINDEX_INTERVAL
                        };
                        tokio::time::sleep(pause).await;
                    }
                });
                tracing::info!(
                    "Log line reindexer scheduled (interval: {:?})",
                    REINDEX_INTERVAL
                );
            }

            // ── Forget sweeper (ADR-047 §8a) ────────────────────────────
            // Drains `log_line_forget_backlog`: chunks compaction, purge and
            // retention retired but could not immediately confirm forgotten
            // from the line index (a transient ClickHouse error, or a
            // rejected Cloud insert during an outage). Without this,
            // that first failure was the end of the story and the rows
            // stayed queryable — double-counted in facets/histograms/
            // aggregates, or pointing at chunks the reader can no longer
            // resolve — for the rest of the index's retention window.
            // Gated the same way the reindexer is.
            if line_index.backend().is_some() {
                let sweeper =
                    ForgetSweeper::new(Arc::new(ManifestRepo::new(db.clone())), line_index.clone());
                let recovery_writer = chunk_writer.clone();
                tokio::spawn(async move {
                    recovery_writer.wait_for_recovery().await;
                    loop {
                        let report = sweeper.run_once(DEFAULT_FORGET_BATCH).await;
                        let pause = if report.more && report.failed == 0 {
                            FORGET_SWEEP_BURST_PAUSE
                        } else {
                            FORGET_SWEEP_INTERVAL
                        };
                        tokio::time::sleep(pause).await;
                    }
                });
                tracing::info!(
                    "Log line forget sweeper scheduled (interval: {:?})",
                    FORGET_SWEEP_INTERVAL
                );
            }

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let old = context.require_service::<LogAggregatorAppState>();
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let app_state = Arc::new(LogAggregatorAppState {
            search_service: old.search_service.clone(),
            metadata_service: old.metadata_service.clone(),
            tail_service: old.tail_service.clone(),
            retention_service: old.retention_service.clone(),
            audit_service: old.audit_service.clone(),
            store: old.store.clone(),
            db: old.db.clone(),
            project_access_checker,
            line_index: old.line_index.clone(),
            manifests: old.manifests.clone(),
        });
        let routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<handlers::LogAggregatorApiDoc as OpenApiTrait>::openapi())
    }
}

/// Discover containers on the local Docker daemon and stream their logs:
/// a one-shot startup scan of already-running containers plus a
/// self-restarting Docker events listener. Only ever spawned when the
/// process actually has a daemon (see `initialize_plugin_services`).
/// What `initialize_plugin_services` should do about local container-log
/// discovery in this process.
///
/// A decision value rather than an inline branch so the "no daemon here" case
/// is testable, and so the explanation an operator reads in the logs is one
/// string produced in one place instead of prose stranded inside a `match`.
#[derive(Debug)]
enum LocalDiscoveryPlan {
    /// A daemon exists: tail this host's containers.
    Start(Arc<bollard::Docker>),
    /// No daemon exists, and this is what that means for log collection.
    Skip { reason: String },
}

/// Decide whether to run the startup scan + Docker events listener.
///
/// Both stream logs from containers on *this* host's daemon. On a profile with
/// no daemon there is neither a daemon to ask nor a local workload to tail —
/// but worker-node containers are still collected, by the remote collector
/// started above, so the skip must say so rather than read like logs are off.
fn local_discovery_plan(handle: &temps_core::DockerHandle) -> LocalDiscoveryPlan {
    match handle.cloned() {
        Some(docker) => LocalDiscoveryPlan::Start(docker),
        None => {
            let cause = handle
                .unavailable_error()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "this process has no local Docker daemon".to_string());
            LocalDiscoveryPlan::Skip {
                reason: format!(
                    "Local container log discovery disabled: {cause}. Logs from containers on \
                     worker nodes are still collected, by the remote log collector"
                ),
            }
        }
    }
}

fn spawn_local_container_discovery(
    docker: Arc<bollard::Docker>,
    collector: Arc<CollectorService>,
    db: Arc<sea_orm::DatabaseConnection>,
) {
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
                let imported_names: Vec<String> = temps_entities::external_services::Entity::find()
                    .filter(temps_entities::external_services::Column::ContainerName.is_not_null())
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
                    if let Ok(containers) = startup_docker.list_containers(Some(options)).await {
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
                let member_names: Vec<String> = temps_entities::service_members::Entity::find()
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
                    if let Ok(containers) = startup_docker.list_containers(Some(options)).await {
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
                        let is_container = event.typ == Some(EventMessageTypeEnum::CONTAINER);
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
                                if let Err(e) = events_collector.start_streaming(container_id).await
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
}

/// Pick the line index store in preference order (ADR-047 §8).
///
/// A *configured* local ClickHouse that cannot be reached is not a reason
/// to index somewhere else: a restart during a ClickHouse blip would
/// otherwise move the index (and, with Cloud next in line, export the whole
/// retention window) on nobody's decision. Configured-but-unavailable fails
/// closed with the verbatim reason; only an *unconfigured* store is skipped.
async fn select_line_index(
    local: Option<&ClickHouseConfig>,
    cloud: Option<Arc<temps_cloud_client::CloudLink>>,
    db: Arc<sea_orm::DatabaseConnection>,
) -> Arc<dyn LineIndex> {
    if let Some(config) = local {
        return match ClickHouseLineIndex::connect(LineIndexTarget::Local(config.clone())).await {
            Ok(index) => index,
            Err(reason) => {
                tracing::warn!(
                    %reason,
                    "log line index disabled: the configured ClickHouse is unavailable \
                     (not falling back — fix the connection or unset TEMPS_CLICKHOUSE_*)"
                );
                Arc::new(NoLineIndex::new(reason.to_string()))
            }
        };
    }
    if let Some(link) = cloud.filter(|link| link.is_linked()) {
        match ClickHouseLineIndex::connect(LineIndexTarget::Cloud {
            link,
            db: db.clone(),
        })
        .await
        {
            Ok(index) => {
                tracing::info!("log line index ready (Temps Cloud ClickHouse)");
                return index;
            }
            Err(reason) => tracing::warn!(
                %reason,
                "Temps Cloud cannot host the log line index; falling back to TimescaleDB"
            ),
        }
    }
    tracing::info!(
        "log line index ready (TimescaleDB) — configure ClickHouse or link Temps Cloud for \
         volume beyond a few million lines a day"
    );
    TimescaleLineIndex::new(db)
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

    /// A process with no local daemon must skip local discovery instead of
    /// trying to reach one — and must say why, plus that remote logs are
    /// unaffected, because "my logs are empty" is otherwise indistinguishable
    /// from a broken install.
    #[test]
    fn local_discovery_is_skipped_with_an_explanation_when_there_is_no_daemon() {
        let handle = temps_core::DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            temps_core::CONTROL_PLANE_DOCKER_REASON,
        );

        match local_discovery_plan(&handle) {
            LocalDiscoveryPlan::Skip { reason } => {
                assert!(reason.contains("control-plane"), "{reason}");
                assert!(
                    reason.contains("remote log collector"),
                    "the operator must learn worker logs still arrive: {reason}",
                );
            }
            LocalDiscoveryPlan::Start(_) => {
                panic!("a disabled handle must never start local discovery")
            }
        }
    }

    /// The other half of the branch: a handle with a client still tails this
    /// host, exactly as before the profile split.
    #[test]
    fn local_discovery_starts_when_a_client_exists() {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            // No socket path configured on this machine. A bollard client is a
            // lazy descriptor, so this only happens when there is nothing to
            // describe; the disabled arm above is the one under test anyway.
            println!("No Docker socket path available, skipping");
            return;
        };
        let handle = temps_core::DockerHandle::available(Arc::new(docker));

        assert!(matches!(
            local_discovery_plan(&handle),
            LocalDiscoveryPlan::Start(_)
        ));
    }
}
