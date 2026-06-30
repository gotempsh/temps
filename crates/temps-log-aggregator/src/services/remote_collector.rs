//! Remote worker-node log collector.
//!
//! The local [`CollectorService`](crate::services::CollectorService) streams
//! logs from the control plane's own Docker daemon. Containers running on remote
//! worker nodes are invisible to it, so their logs never reached searchable
//! history.
//!
//! This service closes that gap by *pulling* each remote container's logs over
//! the agent's existing mTLS `/logs/stream` endpoint and feeding the parsed
//! lines into the **same** [`ChunkWriterService`] the local collector uses — so
//! remote logs land in `log_chunks` and become searchable identically, tagged
//! with the `node_id`/`node_name` they came from.
//!
//! ## Why pull (CP-side) rather than push (agent-side)
//!
//! This is symmetric with how local logs already work (the control plane is
//! already the sink for the full local Docker firehose), reuses the proven
//! agent stream + chunk pipeline, and keeps intake CP-controlled so the control
//! plane can bound its own concurrency. The agent-push + CP-pressure-broadcast
//! design (ADR-021) is the scale-out evolution once the CP-as-sink saturates.
//!
//! ## Scale safety
//!
//! - Concurrency is bounded by the number of running remote containers (replica
//!   count), not by request/log volume.
//! - Buffering and flush are handled by the shared `ChunkWriterService` (1 MiB /
//!   30 s caps) — identical to local logs.
//! - Per-container streams back off exponentially on error and give up after a
//!   bounded number of consecutive failures, exactly like the local collector.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use futures_util::StreamExt;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::parser::{parse_docker_timestamp, parse_log_line};
use crate::services::{ChunkWriterService, LogMetadataService};
use crate::types::{ContainerContext, LogLine, LogStream};

/// Maximum consecutive stream errors before giving up on a remote container.
/// Mirrors the local collector; at 30 s max backoff this is ~10 minutes.
const MAX_CONSECUTIVE_ERRORS: u32 = 20;

/// A running remote container the collector should ship logs for.
#[derive(Debug, Clone)]
pub struct RemoteContainerInfo {
    /// Worker node the container runs on.
    pub node_id: i32,
    /// Human-readable node name (denormalized into each log line for display).
    pub node_name: String,
    /// Docker container ID on the remote node.
    pub container_id: String,
    /// Platform project ID.
    pub project_id: i32,
    /// Environment (ID as string, matching `LogLine.env`).
    pub env: String,
    /// Service name.
    pub service: String,
    /// Active deployment ID (`deployments.id`), if known.
    pub deploy_id: Option<i32>,
}

/// A stream of raw, timestamp-prefixed log lines from a remote container — one
/// item per line, formatted exactly as the agent's `/logs/stream` emits them
/// (`<rfc3339-ts> <message>`), without a trailing newline.
pub type RemoteLogStream = Pin<Box<dyn Stream<Item = Result<String, RemoteLogSourceError>> + Send>>;

/// Errors from the remote log source (the adapter that talks to worker agents).
#[derive(Debug, thiserror::Error)]
pub enum RemoteLogSourceError {
    #[error("remote log source error for node {node_id}: {reason}")]
    Source { node_id: i32, reason: String },

    #[error("remote container {container_id} on node {node_id} not found")]
    NotFound { node_id: i32, container_id: String },

    #[error("remote log source database error: {0}")]
    Database(String),
}

/// Port for fetching logs from remote worker nodes.
///
/// Defined here (the log-aggregator owns the sink) and implemented by
/// `temps-deployments` (which owns node records and the per-node mTLS clients).
/// This keeps the aggregator independent of deployment/cluster internals.
#[async_trait]
pub trait RemoteContainerLogSource: Send + Sync {
    /// List the running remote containers that should be collected into history.
    async fn list_remote_containers(
        &self,
    ) -> Result<Vec<RemoteContainerInfo>, RemoteLogSourceError>;

    /// Open a `follow` stream of raw timestamped log lines for one remote
    /// container, resuming after `since_unix` seconds (0 = from whatever the
    /// remote daemon still retains).
    async fn open_log_stream(
        &self,
        node_id: i32,
        container_id: &str,
        since_unix: i64,
    ) -> Result<RemoteLogStream, RemoteLogSourceError>;
}

struct StreamTask {
    handle: JoinHandle<()>,
}

/// Collects logs from remote worker-node containers into the shared chunk pipeline.
pub struct RemoteLogCollectorService {
    source: Arc<dyn RemoteContainerLogSource>,
    chunk_writer: Arc<ChunkWriterService>,
    metadata_service: Arc<LogMetadataService>,
    /// Shared with the local collector so remote lines also reach the live
    /// SSE tail — the structured `/logs/tail` becomes multi-node complete.
    tail_tx: broadcast::Sender<LogLine>,
    /// Active streaming tasks per remote container_id.
    active: Mutex<HashMap<String, StreamTask>>,
}

impl RemoteLogCollectorService {
    pub fn new(
        source: Arc<dyn RemoteContainerLogSource>,
        chunk_writer: Arc<ChunkWriterService>,
        metadata_service: Arc<LogMetadataService>,
        tail_tx: broadcast::Sender<LogLine>,
    ) -> Self {
        Self {
            source,
            chunk_writer,
            metadata_service,
            tail_tx,
            active: Mutex::new(HashMap::new()),
        }
    }

    /// Reconcile active streams against the current set of running remote
    /// containers: start streams for new containers, drop streams for ones that
    /// are gone or whose task has exited.
    pub async fn reconcile(&self) -> Result<(), RemoteLogSourceError> {
        let containers = self.source.list_remote_containers().await?;
        let desired: HashSet<String> = containers.iter().map(|c| c.container_id.clone()).collect();

        // Drop tasks that finished on their own (stream ended) or whose
        // container is no longer running, so they can be restarted/cleaned up.
        {
            let mut active = self.active.lock().await;
            let to_remove: Vec<String> = active
                .iter()
                .filter(|(id, task)| task.handle.is_finished() || !desired.contains(*id))
                .map(|(id, _)| id.clone())
                .collect();
            for id in to_remove {
                if let Some(task) = active.remove(&id) {
                    task.handle.abort();
                }
                // Flush whatever the container buffered so its tail isn't lost.
                if let Some(Ok(flush)) = self.chunk_writer.remove_container(&id).await {
                    if let Err(e) = self.metadata_service.insert_chunk_meta(&flush.meta).await {
                        warn!(container_id = %id, error = %e,
                            "Failed to insert chunk metadata on remote stream stop");
                    }
                }
            }
        }

        // Start streams for newly-seen remote containers.
        for info in containers {
            let already = {
                let active = self.active.lock().await;
                active.contains_key(&info.container_id)
            };
            if already {
                continue;
            }
            self.start_stream(info).await;
        }

        Ok(())
    }

    /// Spawn a streaming task for a single remote container.
    async fn start_stream(&self, info: RemoteContainerInfo) {
        // Resume from the last chunk we stored for this container so a restart
        // doesn't replay the whole history (and +1s avoids re-serving the
        // boundary second, matching the local collector's reasoning).
        let resume_after = self
            .metadata_service
            .get_latest_chunk_end_for_container(&info.container_id)
            .await
            .unwrap_or(None)
            .map(|ts| ts.timestamp().saturating_add(1))
            .unwrap_or(0);

        let source = self.source.clone();
        let chunk_writer = self.chunk_writer.clone();
        let metadata_service = self.metadata_service.clone();
        let tail_tx = self.tail_tx.clone();
        let container_id = info.container_id.clone();

        let handle = tokio::spawn(async move {
            Self::stream_remote_container(
                source,
                chunk_writer,
                metadata_service,
                tail_tx,
                info,
                resume_after,
            )
            .await;
        });

        let mut active = self.active.lock().await;
        active.insert(container_id.clone(), StreamTask { handle });
        info!(container_id = %container_id, "Started remote log streaming");
    }

    /// Stop all active streams (called on shutdown).
    pub async fn stop_all(&self) {
        let mut active = self.active.lock().await;
        for (_, task) in active.drain() {
            task.handle.abort();
        }
    }

    /// Number of active per-container streaming tasks (for tests/observability).
    pub async fn active_count(&self) -> usize {
        self.active.lock().await.len()
    }

    /// Internal: stream one remote container's logs into the chunk pipeline,
    /// reconnecting with exponential backoff and resuming from the last line.
    async fn stream_remote_container(
        source: Arc<dyn RemoteContainerLogSource>,
        chunk_writer: Arc<ChunkWriterService>,
        metadata_service: Arc<LogMetadataService>,
        tail_tx: broadcast::Sender<LogLine>,
        info: RemoteContainerInfo,
        resume_after: i64,
    ) {
        let ctx = ContainerContext {
            project_id: info.project_id,
            env: info.env.clone(),
            service: info.service.clone(),
            container_id: info.container_id.clone(),
            deploy_id: info.deploy_id,
        };

        let mut last_seen_ts: i64 = resume_after;
        let mut consecutive_errors: u32 = 0;
        let mut retry_delay = std::time::Duration::from_secs(1);
        let max_retry_delay = std::time::Duration::from_secs(30);

        loop {
            let stream = match source
                .open_log_stream(info.node_id, &info.container_id, last_seen_ts)
                .await
            {
                Ok(s) => s,
                Err(RemoteLogSourceError::NotFound { .. }) => {
                    info!(container_id = %info.container_id, node_id = info.node_id,
                        "Remote container gone, stopping log stream");
                    break;
                }
                Err(e) => {
                    consecutive_errors += 1;
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        error!(container_id = %info.container_id, error = %e,
                            "Max consecutive errors opening remote stream, giving up");
                        break;
                    }
                    warn!(container_id = %info.container_id, error = %e,
                        retry_delay_secs = retry_delay.as_secs(),
                        "Failed to open remote log stream, retrying");
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = std::cmp::min(retry_delay * 2, max_retry_delay);
                    continue;
                }
            };

            let mut stream = stream;
            let mut clean_eof = true;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(raw) => {
                        consecutive_errors = 0;
                        retry_delay = std::time::Duration::from_secs(1);

                        let (ts, msg) = parse_docker_timestamp(&raw);
                        // Lines from a torn-down resume window are skipped: only
                        // accept lines strictly newer than where we resumed.
                        if ts.timestamp() < last_seen_ts {
                            continue;
                        }
                        let mut line = parse_log_line(msg, ts, LogStream::Stdout, &ctx);
                        line.node_id = Some(info.node_id);
                        line.node_name = Some(info.node_name.clone());
                        last_seen_ts = line.ts.timestamp();

                        let _ = tail_tx.send(line.clone());

                        match chunk_writer.write_line(line).await {
                            Ok(Some(flush)) => {
                                if let Err(e) =
                                    metadata_service.insert_chunk_meta(&flush.meta).await
                                {
                                    error!(container_id = %info.container_id, error = %e,
                                        "Failed to insert chunk metadata for remote container");
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                error!(container_id = %info.container_id, error = %e,
                                    "Failed to write remote log line to chunk buffer");
                            }
                        }
                    }
                    Err(e) => {
                        clean_eof = false;
                        consecutive_errors += 1;
                        warn!(container_id = %info.container_id, error = %e,
                            consecutive_errors,
                            "Remote log stream error, will reconnect");
                        break;
                    }
                }
            }

            if clean_eof {
                // The stream ended without error — usually the container stopped
                // or the agent closed the follow. Reconcile will restart it if
                // it's still running; exit this task so a finished handle is
                // visible to the next reconcile.
                debug!(container_id = %info.container_id, "Remote log stream ended");
                break;
            }

            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                error!(container_id = %info.container_id,
                    "Max consecutive errors on remote stream, giving up");
                break;
            }
            tokio::time::sleep(retry_delay).await;
            retry_delay = std::cmp::min(retry_delay * 2, max_retry_delay);
            // Resume one second past the last line to avoid duplicate boundary
            // lines (the dedup pass in archive_search is the backstop).
            last_seen_ts = last_seen_ts.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ChunkWriterService;
    use crate::storage::{FilesystemStorage, LogStorage};
    use std::sync::Mutex as StdMutex;

    /// Mock source with a swappable container list. Streams are `pending()` so
    /// the spawned tasks stay alive (never finish), letting the reconcile diff
    /// be observed deterministically via `active_count()`.
    struct MockSource {
        containers: StdMutex<Vec<RemoteContainerInfo>>,
    }

    #[async_trait]
    impl RemoteContainerLogSource for MockSource {
        async fn list_remote_containers(
            &self,
        ) -> Result<Vec<RemoteContainerInfo>, RemoteLogSourceError> {
            Ok(self.containers.lock().unwrap().clone())
        }
        async fn open_log_stream(
            &self,
            _node_id: i32,
            _container_id: &str,
            _since_unix: i64,
        ) -> Result<RemoteLogStream, RemoteLogSourceError> {
            Ok(Box::pin(futures::stream::pending::<
                Result<String, RemoteLogSourceError>,
            >()))
        }
    }

    fn info(container_id: &str) -> RemoteContainerInfo {
        RemoteContainerInfo {
            node_id: 1,
            node_name: "worker-1".into(),
            container_id: container_id.into(),
            project_id: 42,
            env: "1".into(),
            service: "web".into(),
            deploy_id: Some(7),
        }
    }

    fn collector(source: Arc<MockSource>) -> RemoteLogCollectorService {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let chunk_writer = Arc::new(ChunkWriterService::new(storage));
        // start_stream queries get_latest_chunk_end_for_container once per new
        // container; return empty results (→ resume from 0). A few extra empty
        // result sets cover any incidental queries. The pending streams produce
        // no lines, so nothing is written back.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::log_chunks::Model>::new(); 8])
            .into_connection();
        let metadata = Arc::new(LogMetadataService::new(Arc::new(db)));
        let (tail_tx, _) = broadcast::channel(16);
        RemoteLogCollectorService::new(source, chunk_writer, metadata, tail_tx)
    }

    #[tokio::test]
    async fn test_reconcile_starts_and_stops_streams() {
        let source = Arc::new(MockSource {
            containers: StdMutex::new(vec![info("cnt-a"), info("cnt-b")]),
        });
        let collector = collector(source.clone());

        // Two remote containers → two active streams.
        collector.reconcile().await.unwrap();
        assert_eq!(collector.active_count().await, 2);

        // Reconcile is idempotent: same set → still two (no duplicate streams).
        collector.reconcile().await.unwrap();
        assert_eq!(collector.active_count().await, 2);

        // One container goes away → its stream is dropped.
        *source.containers.lock().unwrap() = vec![info("cnt-a")];
        collector.reconcile().await.unwrap();
        assert_eq!(collector.active_count().await, 1);

        // All gone → no active streams.
        source.containers.lock().unwrap().clear();
        collector.reconcile().await.unwrap();
        assert_eq!(collector.active_count().await, 0);

        collector.stop_all().await;
    }
}
