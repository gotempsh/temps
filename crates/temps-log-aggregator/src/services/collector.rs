//! Docker log collector service
//!
//! Opens a streaming log connection to the Docker daemon for each running container.
//! Enriches every log line with project_id, env, service, deploy_id, container_id
//! from Docker labels set by temps.sh at container creation.
//!
//! Resilience:
//! - On stream error: exponential backoff (1s → 30s cap), reconnects using the
//!   timestamp of the last successfully received line to avoid gaps.
//! - On container gone (404): gives up immediately instead of retrying forever.
//! - Max consecutive failures threshold: after 20 consecutive errors the streaming
//!   task for that container exits to avoid wasting resources.

use std::collections::HashMap;
use std::sync::Arc;

use bollard::query_parameters::LogsOptionsBuilder;
use bollard::Docker;

use futures_util::StreamExt;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::error::LogAggregatorError;
use crate::parser::{parse_docker_timestamp, parse_log_line};
use crate::services::{ChunkWriterService, LogMetadataService};
use crate::types::{ContainerContext, LogLine, LogStream};

/// Docker label keys set by temps.sh at container creation
const LABEL_PROJECT_ID: &str = "sh.temps.project_id";
const LABEL_ENV: &str = "sh.temps.environment";
const LABEL_SERVICE: &str = "sh.temps.service";
const LABEL_DEPLOY_ID: &str = "sh.temps.deploy_id";

/// Maximum consecutive stream errors before giving up on a container.
/// At 30s max backoff this is roughly 10 minutes of retrying.
const MAX_CONSECUTIVE_ERRORS: u32 = 20;

/// Callback type invoked when a chunk is flushed during streaming.
type OnChunkFlushedCallback =
    Arc<dyn Fn(crate::types::ChunkMeta, Vec<LogLine>) + Send + Sync + 'static>;

/// State of a streaming task for a single container
struct StreamTask {
    handle: JoinHandle<()>,
}

/// Service that manages Docker container log streaming.
///
/// For each running container, it opens a `follow: true` streaming connection
/// to the Docker daemon. Tracks the last seen timestamp per container so
/// reconnections resume without gaps.
pub struct CollectorService {
    docker: Arc<Docker>,
    chunk_writer: Arc<ChunkWriterService>,
    metadata_service: Arc<LogMetadataService>,
    /// Broadcast channel for live tail subscribers
    tail_tx: broadcast::Sender<LogLine>,
    /// Active streaming tasks per container_id
    active_streams: Mutex<HashMap<String, StreamTask>>,
    /// Callback for chunk metadata (to write to DB)
    on_chunk_flushed: Option<OnChunkFlushedCallback>,
}

impl CollectorService {
    pub fn new(
        docker: Arc<Docker>,
        chunk_writer: Arc<ChunkWriterService>,
        metadata_service: Arc<LogMetadataService>,
        tail_capacity: usize,
    ) -> Self {
        let (tail_tx, _) = broadcast::channel(tail_capacity);
        Self {
            docker,
            chunk_writer,
            metadata_service,
            tail_tx,
            active_streams: Mutex::new(HashMap::new()),
            on_chunk_flushed: None,
        }
    }

    /// Set a callback that is invoked whenever a chunk is flushed.
    ///
    /// The callback receives the chunk metadata and the lines that were written,
    /// allowing the caller to insert metadata into the database and create log_events.
    pub fn set_on_chunk_flushed(&mut self, callback: OnChunkFlushedCallback) {
        self.on_chunk_flushed = Some(callback);
    }

    /// Get a broadcast receiver for live tail subscriptions.
    pub fn subscribe_tail(&self) -> broadcast::Receiver<LogLine> {
        self.tail_tx.subscribe()
    }

    /// Get the broadcast sender for creating a TailService.
    pub fn tail_sender(&self) -> broadcast::Sender<LogLine> {
        self.tail_tx.clone()
    }

    /// Start streaming logs for a container.
    ///
    /// Extracts context from Docker labels. If the container has no temps.sh labels,
    /// it is silently skipped.
    pub async fn start_streaming(&self, container_id: &str) -> Result<(), LogAggregatorError> {
        // Check if already streaming
        {
            let streams = self.active_streams.lock().await;
            if streams.contains_key(container_id) {
                debug!(container_id = container_id, "Already streaming, skipping");
                return Ok(());
            }
        }

        // Inspect container for labels
        let ctx = self.extract_context(container_id).await?;
        let ctx = match ctx {
            Some(c) => c,
            None => {
                debug!(
                    container_id = container_id,
                    "Container has no temps.sh labels, skipping"
                );
                return Ok(());
            }
        };

        // Query the DB for the latest chunk end timestamp for this container.
        // On server restart, this prevents replaying the entire container history.
        //
        // We add +1 second because Docker's `since` parameter is second-resolution
        // AND inclusive — passing the raw `ended_at.timestamp()` would re-serve every
        // line stamped in that same second (the boot-time burst in crash-loop
        // scenarios was producing 10x duplicate rows per restart). Trading <1s of
        // coverage for zero duplicates is the right call; the dedup layer in
        // archive_search is the backstop for anything that still slips through.
        let resume_after = self
            .metadata_service
            .get_latest_chunk_end_for_container(container_id)
            .await
            .unwrap_or(None)
            .map(|ts| ts.timestamp().saturating_add(1))
            .unwrap_or(0);

        if resume_after > 0 {
            info!(
                container_id = container_id,
                resume_after_ts = resume_after,
                "Resuming log stream from last known position"
            );
        }

        let docker = self.docker.clone();
        let chunk_writer = self.chunk_writer.clone();
        let tail_tx = self.tail_tx.clone();
        let container_id_owned = container_id.to_string();
        let on_chunk_flushed = self.on_chunk_flushed.clone();

        let handle = tokio::spawn(async move {
            Self::stream_container_logs(
                docker,
                chunk_writer,
                tail_tx,
                container_id_owned.clone(),
                ctx,
                on_chunk_flushed,
                resume_after,
            )
            .await;
        });

        let mut streams = self.active_streams.lock().await;
        streams.insert(container_id.to_string(), StreamTask { handle });

        info!(container_id = container_id, "Started log streaming");
        Ok(())
    }

    /// Stop streaming logs for a container and flush remaining buffer.
    pub async fn stop_streaming(&self, container_id: &str) {
        let task = {
            let mut streams = self.active_streams.lock().await;
            streams.remove(container_id)
        };

        if let Some(task) = task {
            task.handle.abort();
            // Flush remaining lines
            if let Some(result) = self.chunk_writer.remove_container(container_id).await {
                match result {
                    Ok(flush_result) => {
                        debug!(
                            container_id = container_id,
                            chunk_id = %flush_result.meta.id,
                            "Flushed remaining lines on stop"
                        );
                        // Invoke callback for final chunk
                        if let Some(ref callback) = self.on_chunk_flushed {
                            callback(flush_result.meta, flush_result.lines);
                        }
                    }
                    Err(e) => {
                        warn!(
                            container_id = container_id,
                            error = %e,
                            "Failed to flush remaining lines on stop"
                        );
                    }
                }
            }
            info!(container_id = container_id, "Stopped log streaming");
        }
    }

    /// Stop all active streams.
    pub async fn stop_all(&self) {
        let container_ids: Vec<String> = {
            let streams = self.active_streams.lock().await;
            streams.keys().cloned().collect()
        };

        for container_id in container_ids {
            self.stop_streaming(&container_id).await;
        }
    }

    /// Get the list of currently streaming container IDs.
    pub async fn active_containers(&self) -> Vec<String> {
        let streams = self.active_streams.lock().await;
        streams.keys().cloned().collect()
    }

    /// Extract container context from Docker labels.
    async fn extract_context(
        &self,
        container_id: &str,
    ) -> Result<Option<ContainerContext>, LogAggregatorError> {
        let inspect = self
            .docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                if e.to_string().contains("404") || e.to_string().contains("No such container") {
                    LogAggregatorError::ContainerNotFound {
                        container_id: container_id.to_string(),
                    }
                } else {
                    LogAggregatorError::DockerStreamFailed {
                        container_id: container_id.to_string(),
                        reason: format!("Failed to inspect container: {}", e),
                    }
                }
            })?;

        let labels = inspect.config.as_ref().and_then(|c| c.labels.as_ref());

        let labels = match labels {
            Some(l) => l,
            None => return Ok(None),
        };

        let project_id = match labels.get(LABEL_PROJECT_ID) {
            Some(id) => match id.parse::<i32>() {
                Ok(pid) => pid,
                Err(_) => return Ok(None),
            },
            None => return Ok(None),
        };

        let env = labels
            .get(LABEL_ENV)
            .cloned()
            .unwrap_or_else(|| "default".to_string());
        let service = labels
            .get(LABEL_SERVICE)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let deploy_id = labels
            .get(LABEL_DEPLOY_ID)
            .and_then(|id| id.parse::<i32>().ok());

        Ok(Some(ContainerContext {
            project_id,
            env,
            service,
            container_id: container_id.to_string(),
            deploy_id,
        }))
    }

    /// Returns true if the error string indicates the container no longer exists.
    fn is_container_gone(error_msg: &str) -> bool {
        error_msg.contains("404")
            || error_msg.contains("No such container")
            || error_msg.contains("is not running")
            || error_msg.contains("removal in progress")
    }

    /// Build a `follow: true` log options set, resuming from `since_ts`.
    fn build_log_options(since_ts: i64) -> bollard::query_parameters::LogsOptions {
        LogsOptionsBuilder::new()
            .follow(true)
            .stdout(true)
            .stderr(true)
            .timestamps(true)
            .since(since_ts as i32)
            .build()
    }

    /// Internal: stream logs from a Docker container via the daemon API.
    ///
    /// On error: retries with exponential backoff, using the timestamp of the
    /// last successfully received line as `since` so reconnections don't create
    /// gaps (at worst, the same second is re-fetched — but not the full history).
    ///
    /// Gives up when:
    /// - The container is gone (404 / not running)
    /// - MAX_CONSECUTIVE_ERRORS consecutive failures are hit
    /// - The stream returns None (container stopped normally)
    async fn stream_container_logs(
        docker: Arc<Docker>,
        chunk_writer: Arc<ChunkWriterService>,
        tail_tx: broadcast::Sender<LogLine>,
        container_id: String,
        ctx: ContainerContext,
        on_chunk_flushed: Option<OnChunkFlushedCallback>,
        resume_after: i64,
    ) {
        // Track the timestamp of the last successfully received line.
        // On reconnect we use this so we only re-fetch at most one second of overlap
        // instead of the entire history. On fresh startup, resume_after comes from
        // the DB (latest chunk ended_at), preventing replay of already-collected logs.
        let mut last_seen_ts: i64 = resume_after;
        let mut consecutive_errors: u32 = 0;
        let mut retry_delay = std::time::Duration::from_secs(1);
        let max_retry_delay = std::time::Duration::from_secs(30);

        let options = Self::build_log_options(last_seen_ts);
        let mut stream = docker.logs(&container_id, Some(options));

        loop {
            match stream.next().await {
                Some(Ok(output)) => {
                    // Reset error state on success
                    consecutive_errors = 0;
                    retry_delay = std::time::Duration::from_secs(1);

                    let (stream_type, raw) = match output {
                        bollard::container::LogOutput::StdOut { message } => (
                            LogStream::Stdout,
                            String::from_utf8_lossy(&message).to_string(),
                        ),
                        bollard::container::LogOutput::StdErr { message } => (
                            LogStream::Stderr,
                            String::from_utf8_lossy(&message).to_string(),
                        ),
                        _ => continue,
                    };

                    let (ts, msg) = parse_docker_timestamp(&raw);
                    let line = parse_log_line(msg, ts, stream_type, &ctx);

                    // Update last seen timestamp for reconnection
                    last_seen_ts = line.ts.timestamp();

                    // Send to live tail subscribers (ignore errors if no subscribers)
                    let _ = tail_tx.send(line.clone());

                    // Buffer the line
                    match chunk_writer.write_line(line).await {
                        Ok(Some(flush_result)) => {
                            if let Some(ref callback) = on_chunk_flushed {
                                callback(flush_result.meta, flush_result.lines);
                            }
                        }
                        Ok(None) => {} // Buffered, not flushed
                        Err(e) => {
                            error!(
                                container_id = container_id,
                                error = %e,
                                "Failed to write log line to chunk buffer"
                            );
                        }
                    }
                }
                Some(Err(e)) => {
                    let err_msg = e.to_string();

                    // If the container no longer exists, stop immediately
                    if Self::is_container_gone(&err_msg) {
                        info!(
                            container_id = container_id,
                            error = %e,
                            "Container is gone, stopping log stream"
                        );
                        break;
                    }

                    consecutive_errors += 1;
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        error!(
                            container_id = container_id,
                            consecutive_errors = consecutive_errors,
                            "Max consecutive errors reached, giving up on container"
                        );
                        break;
                    }

                    warn!(
                        container_id = container_id,
                        error = %e,
                        retry_delay_secs = retry_delay.as_secs(),
                        consecutive_errors = consecutive_errors,
                        last_seen_ts = last_seen_ts,
                        "Docker log stream error, retrying"
                    );
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = std::cmp::min(retry_delay * 2, max_retry_delay);

                    // Reconnect one second past the last line we successfully received.
                    // Docker's `since` parameter is integer seconds and is inclusive, so
                    // passing `last_seen_ts` would re-serve every line stamped at exactly
                    // that second — producing visible duplicates in the UI. Trading a
                    // worst-case <1s gap for no duplicates is the right default here;
                    // the dedup layer in archive_search catches anything that slips through.
                    let resume_from = last_seen_ts.saturating_add(1);
                    let reconnect_options = Self::build_log_options(resume_from);
                    stream = docker.logs(&container_id, Some(reconnect_options));
                }
                None => {
                    // Stream ended normally — the container stopped.
                    info!(
                        container_id = container_id,
                        "Docker log stream ended (container stopped)"
                    );
                    break;
                }
            }
        }
    }
}
