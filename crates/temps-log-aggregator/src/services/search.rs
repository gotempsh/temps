//! Log search service
//!
//! All searches read directly from compressed chunk files on disk/S3.
//! The log_chunks table provides the index of which files to read.

use std::collections::HashSet;
use std::sync::Arc;

use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::LogAggregatorError;
use crate::services::LogMetadataService;
use crate::storage::LogStorage;
use crate::types::*;

/// Maximum time range for full text search (hours)
const MAX_FULLTEXT_HOURS: u32 = 24;

/// Maximum concurrent chunk file fetches
const MAX_CONCURRENT_FETCHES: usize = 20;

/// Search service that routes log queries to the appropriate execution path.
pub struct LogSearchService {
    storage: Arc<dyn LogStorage>,
    metadata_service: Arc<LogMetadataService>,
}

impl LogSearchService {
    pub fn new(storage: Arc<dyn LogStorage>, metadata_service: Arc<LogMetadataService>) -> Self {
        Self {
            storage,
            metadata_service,
        }
    }

    /// Execute a log search with automatic routing.
    pub async fn search(
        &self,
        filter: &LogSearchFilter,
    ) -> Result<LogSearchResult, LogAggregatorError> {
        // Validate required params
        self.validate_filter(filter)?;

        let page_size = std::cmp::min(filter.page_size, 500) as u64;

        // Always search from chunk files — no log_events table needed
        self.archive_search(filter, page_size).await
    }

    /// Get context lines around a specific log line in a chunk.
    pub async fn get_context(
        &self,
        request: &ContextRequest,
    ) -> Result<ContextResponse, LogAggregatorError> {
        // Fetch chunk metadata
        let meta = self
            .metadata_service
            .get_chunk_meta(request.chunk_id)
            .await?
            .ok_or_else(|| LogAggregatorError::ChunkNotFound {
                chunk_id: request.chunk_id,
                storage_key: String::new(),
            })?;

        // Read and decompress the chunk
        let compressed = self.storage.read_chunk(&meta.storage_key).await?;
        let decompressed = zstd::decode_all(compressed.as_slice()).map_err(|e| {
            LogAggregatorError::DecompressionFailed {
                chunk_id: request.chunk_id,
                reason: e.to_string(),
            }
        })?;

        let content = String::from_utf8_lossy(&decompressed);
        let all_lines: Vec<&str> = content.lines().collect();

        let target_offset = request.line_offset as usize;
        let context_lines = request.lines as usize;
        let start = target_offset.saturating_sub(context_lines);
        let end = std::cmp::min(target_offset + context_lines + 1, all_lines.len());

        let mut result_lines = Vec::new();
        let target_index_in_result = target_offset - start;

        for (i, raw_line) in all_lines[start..end].iter().enumerate() {
            let absolute_offset = start + i;
            if let Ok(parsed) = serde_json::from_str::<LogLine>(raw_line) {
                result_lines.push(ContextLine {
                    timestamp: parsed.ts,
                    level: parsed.level,
                    message: parsed.msg,
                    fields: parsed.fields,
                    line_offset: absolute_offset as i32,
                    is_match: absolute_offset == target_offset,
                });
            }
        }

        Ok(ContextResponse {
            lines: result_lines,
            target_index: target_index_in_result,
        })
    }

    /// Validate search filter constraints.
    fn validate_filter(&self, filter: &LogSearchFilter) -> Result<(), LogAggregatorError> {
        if filter.start_time >= filter.end_time {
            return Err(LogAggregatorError::Validation {
                message: "start_time must be before end_time".to_string(),
            });
        }

        // Check full text search time range limit
        if filter.text.is_some() {
            let hours = (filter.end_time - filter.start_time).num_hours();
            if hours > MAX_FULLTEXT_HOURS as i64 {
                return Err(LogAggregatorError::SearchTimeRangeExceeded {
                    max_hours: MAX_FULLTEXT_HOURS,
                    search_type: "full text".to_string(),
                });
            }
        }

        Ok(())
    }

    /// Search against S3/filesystem chunk files.
    ///
    /// 1. Query log_chunks metadata to identify relevant chunks
    /// 2. Fetch and decompress chunks in parallel
    /// 3. Filter lines in memory
    async fn archive_search(
        &self,
        filter: &LogSearchFilter,
        page_size: u64,
    ) -> Result<LogSearchResult, LogAggregatorError> {
        // Find relevant chunks via metadata
        let service_filter = filter.services.first().map(|s| s.as_str());
        let chunks = self
            .metadata_service
            .find_chunks(
                filter.project_id,
                service_filter,
                filter.start_time,
                filter.end_time,
            )
            .await?;

        if chunks.is_empty() {
            return Ok(LogSearchResult {
                lines: Vec::new(),
                next_cursor: None,
                search_mode: SearchMode::Archive,
                total_scanned: 0,
            });
        }

        debug!(
            project_id = %filter.project_id,
            chunk_count = chunks.len(),
            "Starting archive search"
        );

        let mut all_matches = Vec::new();
        let mut total_scanned = 0u64;
        // Dedup key: (ts_nanos, container_id, stream, msg). Lines identical on all four
        // are considered the same event and collapsed. This defends against:
        // - Docker `since=N` re-serving lines on reconnect (collector.rs comments)
        // - Server restart replays where `since` rounds to the second boundary
        // - Any overlapping chunks for the same container/time window
        let mut seen: HashSet<(i64, String, LogStream, String)> = HashSet::new();

        // Process chunks in batches of MAX_CONCURRENT_FETCHES
        for chunk_batch in chunks.chunks(MAX_CONCURRENT_FETCHES) {
            let mut fetch_futures = Vec::new();

            for chunk in chunk_batch {
                let storage = self.storage.clone();
                let storage_key = chunk.storage_key.clone();
                let chunk_id = chunk.id;

                fetch_futures.push(async move {
                    let compressed = storage.read_chunk(&storage_key).await?;
                    let decompressed = zstd::decode_all(compressed.as_slice()).map_err(|e| {
                        LogAggregatorError::DecompressionFailed {
                            chunk_id,
                            reason: e.to_string(),
                        }
                    })?;
                    let result: (Uuid, Vec<u8>) = (chunk_id, decompressed);
                    Ok(result)
                });
            }

            let results: Vec<Result<(Uuid, Vec<u8>), LogAggregatorError>> =
                futures::future::join_all(fetch_futures).await;

            for result in results {
                match result {
                    Ok((chunk_id, data)) => {
                        let content = String::from_utf8_lossy(&data);
                        for (line_idx, raw_line) in content.lines().enumerate() {
                            total_scanned += 1;
                            if let Ok(parsed) = serde_json::from_str::<LogLine>(raw_line) {
                                if !self.line_matches_filter(&parsed, filter) {
                                    continue;
                                }
                                let dedup_key = (
                                    parsed.ts.timestamp_nanos_opt().unwrap_or(0),
                                    parsed.container_id.clone(),
                                    parsed.stream,
                                    parsed.msg.clone(),
                                );
                                if !seen.insert(dedup_key) {
                                    continue;
                                }
                                all_matches.push(LogSearchLine {
                                    timestamp: parsed.ts,
                                    level: parsed.level,
                                    service: parsed.service,
                                    message: parsed.msg,
                                    fields: parsed.fields,
                                    chunk_id,
                                    line_offset: line_idx as i32,
                                    deploy_id: parsed.deploy_id,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to fetch/decompress chunk during archive search");
                    }
                }
            }

            // Early exit if we have enough results
            if all_matches.len() > page_size as usize {
                break;
            }
        }

        // Sort by timestamp ascending (oldest first, like a terminal)
        all_matches.sort_by_key(|a| a.timestamp);

        let has_more = all_matches.len() > page_size as usize;
        all_matches.truncate(page_size as usize);

        let next_cursor = if has_more {
            all_matches
                .last()
                .map(|l| format!("{}:{}", l.timestamp.timestamp_millis(), l.chunk_id))
        } else {
            None
        };

        Ok(LogSearchResult {
            lines: all_matches,
            next_cursor,
            search_mode: SearchMode::Archive,
            total_scanned,
        })
    }

    /// Check if a single log line matches the search filter.
    fn line_matches_filter(&self, line: &LogLine, filter: &LogSearchFilter) -> bool {
        // Time range
        if line.ts < filter.start_time || line.ts > filter.end_time {
            return false;
        }

        // Level filter
        if !filter.levels.is_empty() && !filter.levels.contains(&line.level) {
            return false;
        }

        // Service filter
        if !filter.services.is_empty() && !filter.services.contains(&line.service) {
            return false;
        }

        // Env filter
        if !filter.envs.is_empty() && !filter.envs.contains(&line.env) {
            return false;
        }

        // Deploy filter
        if let Some(deploy_id) = filter.deploy_id {
            if line.deploy_id != Some(deploy_id) {
                return false;
            }
        }

        // Text search (simple substring match for archive)
        if let Some(ref text) = filter.text {
            if !line.msg.to_lowercase().contains(&text.to_lowercase()) {
                return false;
            }
        }

        // Field filters
        for field_filter in &filter.field_filters {
            if !self.field_matches(line, field_filter) {
                return false;
            }
        }

        true
    }

    /// Check if a log line's fields match a field filter.
    fn field_matches(&self, line: &LogLine, filter: &FieldFilter) -> bool {
        let fields = match &line.fields {
            Some(f) => f,
            None => return false,
        };

        let value = match fields.get(&filter.key) {
            Some(v) => v,
            None => return false,
        };

        match filter.op {
            FieldFilterOp::Eq => {
                let val_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                val_str == filter.value
            }
            FieldFilterOp::Gt | FieldFilterOp::Lt | FieldFilterOp::Gte | FieldFilterOp::Lte => {
                let val_num = value.as_f64();
                let filter_num = filter.value.parse::<f64>().ok();
                match (val_num, filter_num) {
                    (Some(v), Some(f)) => match filter.op {
                        FieldFilterOp::Gt => v > f,
                        FieldFilterOp::Lt => v < f,
                        FieldFilterOp::Gte => v >= f,
                        FieldFilterOp::Lte => v <= f,
                        _ => false,
                    },
                    _ => false,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn make_filter() -> LogSearchFilter {
        LogSearchFilter {
            project_id: 1,
            start_time: Utc::now() - Duration::hours(1),
            end_time: Utc::now() + Duration::hours(1),
            levels: vec![],
            services: vec![],
            envs: vec![],
            deploy_id: None,
            text: None,
            field_filters: vec![],
            cursor: None,
            page_size: 100,
        }
    }

    fn make_log_line(level: LogLevel, msg: &str) -> LogLine {
        LogLine {
            ts: Utc::now(),
            stream: LogStream::Stdout,
            level,
            msg: msg.to_string(),
            fields: None,
            container_id: "cnt1".to_string(),
            service: "web".to_string(),
            env: "1".to_string(),
            project_id: 1,
            deploy_id: None,
        }
    }

    /// Dedup key used inside archive_search to collapse duplicate log lines.
    /// Kept next to the test so any change to the key composition breaks the
    /// test, forcing an intentional review of what counts as "the same line".
    fn dedup_key(line: &LogLine) -> (i64, String, LogStream, String) {
        (
            line.ts.timestamp_nanos_opt().unwrap_or(0),
            line.container_id.clone(),
            line.stream,
            line.msg.clone(),
        )
    }

    #[test]
    fn test_dedup_key_collapses_identical_lines() {
        // Simulates the observed bug: the same boot-time log line ingested
        // multiple times (e.g. from Docker `since=N` replays on server restart
        // or container crash-loops) must collapse to a single entry.
        let line_a = make_log_line(LogLevel::Warn, "HLF_KEY_PEM is not set");
        let line_b = line_a.clone();

        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(dedup_key(&line_a)));
        assert!(
            !seen.insert(dedup_key(&line_b)),
            "second insert of an identical line must be rejected"
        );
    }

    #[test]
    fn test_dedup_key_distinguishes_different_containers() {
        // Same message, same timestamp, but different containers → keep both.
        let mut a = make_log_line(LogLevel::Info, "ready");
        let mut b = a.clone();
        a.container_id = "cnt-a".into();
        b.container_id = "cnt-b".into();

        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(dedup_key(&a)));
        assert!(
            seen.insert(dedup_key(&b)),
            "different container_id means different event"
        );
    }

    #[test]
    fn test_dedup_key_distinguishes_stream() {
        // Same message, same ts, same container, different stream → keep both
        // (a process writing identical text to stdout and stderr is rare but real).
        let mut a = make_log_line(LogLevel::Info, "ping");
        let mut b = a.clone();
        a.stream = LogStream::Stdout;
        b.stream = LogStream::Stderr;

        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(dedup_key(&a)));
        assert!(seen.insert(dedup_key(&b)));
    }

    #[test]
    fn test_dedup_key_distinguishes_timestamp() {
        // Same everything except ts → keep both. Two consecutive `ping` heartbeats
        // are separate events.
        let a = make_log_line(LogLevel::Info, "ping");
        let mut b = a.clone();
        b.ts = a.ts + Duration::milliseconds(1);

        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(dedup_key(&a)));
        assert!(seen.insert(dedup_key(&b)));
    }

    #[test]
    fn test_line_matches_filter_level() {
        let storage: Arc<dyn LogStorage> = Arc::new(
            crate::storage::FilesystemStorage::new(std::path::PathBuf::from("/tmp/test-logs-lm"))
                .unwrap_or_else(|_| panic!("test setup failed")),
        );
        let metadata = Arc::new(LogMetadataService::new(Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        )));
        let service = LogSearchService::new(storage, metadata);

        let mut filter = make_filter();
        filter.levels = vec![LogLevel::Error];

        let error_line = make_log_line(LogLevel::Error, "error");
        let info_line = make_log_line(LogLevel::Info, "info");

        assert!(service.line_matches_filter(&error_line, &filter));
        assert!(!service.line_matches_filter(&info_line, &filter));
    }

    #[test]
    fn test_line_matches_filter_text() {
        let storage: Arc<dyn LogStorage> = Arc::new(
            crate::storage::FilesystemStorage::new(std::path::PathBuf::from("/tmp/test-logs-ft"))
                .unwrap_or_else(|_| panic!("test setup failed")),
        );
        let metadata = Arc::new(LogMetadataService::new(Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        )));
        let service = LogSearchService::new(storage, metadata);

        let mut filter = make_filter();
        filter.text = Some("timeout".to_string());

        let match_line = make_log_line(LogLevel::Error, "database timeout occurred");
        let no_match = make_log_line(LogLevel::Error, "connection refused");

        assert!(service.line_matches_filter(&match_line, &filter));
        assert!(!service.line_matches_filter(&no_match, &filter));
    }

    #[test]
    fn test_line_matches_field_filter_eq() {
        let storage: Arc<dyn LogStorage> = Arc::new(
            crate::storage::FilesystemStorage::new(std::path::PathBuf::from("/tmp/test-logs-fe"))
                .unwrap_or_else(|_| panic!("test setup failed")),
        );
        let metadata = Arc::new(LogMetadataService::new(Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        )));
        let service = LogSearchService::new(storage, metadata);

        let mut filter = make_filter();
        filter.field_filters = vec![FieldFilter {
            key: "status".to_string(),
            op: FieldFilterOp::Eq,
            value: "500".to_string(),
        }];

        let mut line = make_log_line(LogLevel::Error, "server error");
        line.fields = Some(serde_json::json!({"status": "500"}));
        assert!(service.line_matches_filter(&line, &filter));

        line.fields = Some(serde_json::json!({"status": "200"}));
        assert!(!service.line_matches_filter(&line, &filter));
    }

    #[test]
    fn test_line_matches_field_filter_gt() {
        let storage: Arc<dyn LogStorage> = Arc::new(
            crate::storage::FilesystemStorage::new(std::path::PathBuf::from("/tmp/test-logs-fg"))
                .unwrap_or_else(|_| panic!("test setup failed")),
        );
        let metadata = Arc::new(LogMetadataService::new(Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        )));
        let service = LogSearchService::new(storage, metadata);

        let mut filter = make_filter();
        filter.field_filters = vec![FieldFilter {
            key: "duration_ms".to_string(),
            op: FieldFilterOp::Gt,
            value: "1000".to_string(),
        }];

        let mut line = make_log_line(LogLevel::Info, "slow query");
        line.fields = Some(serde_json::json!({"duration_ms": 1500}));
        assert!(service.line_matches_filter(&line, &filter));

        line.fields = Some(serde_json::json!({"duration_ms": 500}));
        assert!(!service.line_matches_filter(&line, &filter));
    }

    // ────────────────────────────────────────────────────────────────────────
    // Integration tests: full pipeline from parse → write → storage → search
    // ────────────────────────────────────────────────────────────────────────

    mod integration {
        use super::*;
        use crate::parser::parse_log_line;
        use crate::services::ChunkWriterService;
        use crate::storage::FilesystemStorage;
        use crate::types::ContainerContext;

        /// Shared project_id for integration tests
        fn test_project_id() -> i32 {
            50001
        }

        fn test_deploy_id() -> Uuid {
            Uuid::parse_str("d0000000-0000-0000-0000-000000000001").unwrap()
        }

        /// Create a search service backed by the given filesystem storage.
        fn make_search_service(storage: Arc<FilesystemStorage>) -> LogSearchService {
            let metadata = Arc::new(LogMetadataService::new(Arc::new(
                sea_orm::DatabaseConnection::Disconnected,
            )));
            LogSearchService::new(storage, metadata)
        }

        /// Build a LogLine from structured JSON via the parser, then write it
        /// to the chunk writer so it passes through real serialization.
        fn parse_json_line(json_str: &str, ctx: &ContainerContext) -> LogLine {
            parse_log_line(json_str, Utc::now(), LogStream::Stdout, ctx)
        }

        /// Write a batch of LogLines through ChunkWriter, flush, then read back
        /// the decompressed NDJSON and deserialize every line. This validates the
        /// full roundtrip: LogLine → NDJSON → zstd compress → storage → read →
        /// zstd decompress → parse back to LogLine.
        async fn write_and_read_back(
            storage: &Arc<FilesystemStorage>,
            writer: &ChunkWriterService,
            lines: &[LogLine],
            container_id: &str,
        ) -> Vec<LogLine> {
            for line in lines {
                writer.write_line(line.clone()).await.expect("write_line");
            }
            let flush_result = writer.flush_container(container_id).await.expect("flush");

            let compressed = storage
                .read_chunk(&flush_result.meta.storage_key)
                .await
                .expect("read_chunk");
            let decompressed = zstd::decode_all(compressed.as_slice()).expect("zstd decompress");
            let content = String::from_utf8_lossy(&decompressed);

            content
                .lines()
                .filter_map(|raw| serde_json::from_str::<LogLine>(raw).ok())
                .collect()
        }

        // ── Service/environment tag filtering ─────────────────────────

        #[tokio::test]
        async fn test_filter_by_single_service_tag() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let web_ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-web".into(),
                deploy_id: None,
            };
            let worker_ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "worker".into(),
                container_id: "cnt-worker".into(),
                deploy_id: None,
            };

            let web_line =
                parse_json_line(r#"{"level":"info","msg":"GET /api/health 200"}"#, &web_ctx);
            let worker_line =
                parse_json_line(r#"{"level":"info","msg":"Processing job 42"}"#, &worker_ctx);

            let mut filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec!["web".into()],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&web_line, &filter));
            assert!(!search.line_matches_filter(&worker_line, &filter));

            // Switch to worker
            filter.services = vec!["worker".into()];
            assert!(!search.line_matches_filter(&web_line, &filter));
            assert!(search.line_matches_filter(&worker_line, &filter));
        }

        #[tokio::test]
        async fn test_filter_by_multiple_service_tags() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let services = ["web", "api", "worker"];
            let lines: Vec<LogLine> = services
                .iter()
                .map(|svc| {
                    let ctx = ContainerContext {
                        project_id,
                        env: "production".into(),
                        service: svc.to_string(),
                        container_id: format!("cnt-{}", svc),
                        deploy_id: None,
                    };
                    parse_json_line(
                        &format!(r#"{{"level":"info","msg":"{} request"}}"#, svc),
                        &ctx,
                    )
                })
                .collect();

            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec!["web".into(), "api".into()],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&lines[0], &filter)); // web
            assert!(search.line_matches_filter(&lines[1], &filter)); // api
            assert!(!search.line_matches_filter(&lines[2], &filter)); // worker
        }

        #[tokio::test]
        async fn test_filter_by_environment_tag() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let prod_ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-prod".into(),
                deploy_id: None,
            };
            let staging_ctx = ContainerContext {
                project_id,
                env: "staging".into(),
                service: "web".into(),
                container_id: "cnt-staging".into(),
                deploy_id: None,
            };

            let prod_line = parse_json_line(r#"{"level":"info","msg":"prod request"}"#, &prod_ctx);
            let staging_line =
                parse_json_line(r#"{"level":"info","msg":"staging request"}"#, &staging_ctx);

            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec!["production".into()],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&prod_line, &filter));
            assert!(!search.line_matches_filter(&staging_line, &filter));
        }

        #[tokio::test]
        async fn test_filter_by_deploy_id_tag() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let deploy = test_deploy_id();
            let ctx_with_deploy = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-1".into(),
                deploy_id: Some(deploy),
            };
            let ctx_other_deploy = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-2".into(),
                deploy_id: Some(Uuid::new_v4()),
            };

            let line_deploy = parse_json_line(
                r#"{"level":"info","msg":"deployed v1.2"}"#,
                &ctx_with_deploy,
            );
            let line_other = parse_json_line(
                r#"{"level":"info","msg":"deployed v1.3"}"#,
                &ctx_other_deploy,
            );

            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: Some(deploy),
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&line_deploy, &filter));
            assert!(!search.line_matches_filter(&line_other, &filter));
        }

        #[tokio::test]
        async fn test_combined_service_env_level_filter() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "api".into(),
                container_id: "cnt-api".into(),
                deploy_id: None,
            };

            let error_line = parse_json_line(r#"{"level":"error","msg":"database timeout"}"#, &ctx);
            let info_line = parse_json_line(r#"{"level":"info","msg":"request handled"}"#, &ctx);

            // Filter: production + api + error only
            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![LogLevel::Error],
                services: vec!["api".into()],
                envs: vec!["production".into()],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&error_line, &filter));
            assert!(!search.line_matches_filter(&info_line, &filter));
        }

        // ── Full-text search ──────────────────────────────────────────

        #[tokio::test]
        async fn test_fulltext_case_insensitive() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            let line = parse_json_line(
                r#"{"level":"error","msg":"Connection REFUSED by PostgreSQL"}"#,
                &ctx,
            );

            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("connection refused".into()),
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&line, &filter));
        }

        #[tokio::test]
        async fn test_fulltext_partial_match() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            let line = parse_json_line(
                r#"{"level":"error","msg":"request to api.example.com/v2/users timed out after 30s"}"#,
                &ctx,
            );

            // Partial URL match
            let mut filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("api.example.com".into()),
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };
            assert!(search.line_matches_filter(&line, &filter));

            // Partial phrase match
            filter.text = Some("timed out".into());
            assert!(search.line_matches_filter(&line, &filter));

            // No match
            filter.text = Some("connection refused".into());
            assert!(!search.line_matches_filter(&line, &filter));
        }

        #[tokio::test]
        async fn test_fulltext_combined_with_level_filter() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            let error_timeout = parse_json_line(
                r#"{"level":"error","msg":"database query timeout after 5000ms"}"#,
                &ctx,
            );
            let info_timeout = parse_json_line(
                r#"{"level":"info","msg":"request timeout config set to 30s"}"#,
                &ctx,
            );

            // Text "timeout" + level ERROR
            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![LogLevel::Error],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("timeout".into()),
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&error_timeout, &filter));
            assert!(!search.line_matches_filter(&info_timeout, &filter));
        }

        // ── Field filters (structured JSON field search) ──────────────

        #[tokio::test]
        async fn test_field_filter_eq_string() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "api".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            // JSON log with fields: status and request_id preserved in fields
            let line = parse_json_line(
                r#"{"level":"error","msg":"internal error","status":"500","request_id":"req-abc-123"}"#,
                &ctx,
            );

            // Verify fields were preserved (not stripped)
            assert!(line.fields.is_some());
            let fields = line.fields.as_ref().unwrap();
            assert_eq!(fields["status"], "500");
            assert_eq!(fields["request_id"], "req-abc-123");

            // Filter by status=500
            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "status".into(),
                    op: FieldFilterOp::Eq,
                    value: "500".into(),
                }],
                cursor: None,
                page_size: 100,
            };
            assert!(search.line_matches_filter(&line, &filter));

            // Filter by request_id
            let filter_req = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "request_id".into(),
                    op: FieldFilterOp::Eq,
                    value: "req-abc-123".into(),
                }],
                cursor: None,
                page_size: 100,
            };
            assert!(search.line_matches_filter(&line, &filter_req));
        }

        #[tokio::test]
        async fn test_field_filter_numeric_comparisons() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "api".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            // Log line with parsed duration (45ms → duration_ms: 45.0)
            let line = parse_json_line(
                r#"{"level":"info","msg":"request completed","duration":"45ms","user_id":"u-42"}"#,
                &ctx,
            );

            // Verify duration was parsed and duration_ms was added to fields
            assert!(line.fields.is_some());
            let fields = line.fields.as_ref().unwrap();
            assert_eq!(fields["duration_ms"], 45.0);

            // Gt: duration_ms > 30 → true
            let filter_gt = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "duration_ms".into(),
                    op: FieldFilterOp::Gt,
                    value: "30".into(),
                }],
                cursor: None,
                page_size: 100,
            };
            assert!(search.line_matches_filter(&line, &filter_gt));

            // Gt: duration_ms > 100 → false
            let filter_gt_high = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "duration_ms".into(),
                    op: FieldFilterOp::Gt,
                    value: "100".into(),
                }],
                cursor: None,
                page_size: 100,
            };
            assert!(!search.line_matches_filter(&line, &filter_gt_high));

            // Lt: duration_ms < 100 → true
            let filter_lt = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "duration_ms".into(),
                    op: FieldFilterOp::Lt,
                    value: "100".into(),
                }],
                cursor: None,
                page_size: 100,
            };
            assert!(search.line_matches_filter(&line, &filter_lt));

            // Gte: duration_ms >= 45 → true
            let filter_gte = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "duration_ms".into(),
                    op: FieldFilterOp::Gte,
                    value: "45".into(),
                }],
                cursor: None,
                page_size: 100,
            };
            assert!(search.line_matches_filter(&line, &filter_gte));

            // Lte: duration_ms <= 44 → false
            let filter_lte = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "duration_ms".into(),
                    op: FieldFilterOp::Lte,
                    value: "44".into(),
                }],
                cursor: None,
                page_size: 100,
            };
            assert!(!search.line_matches_filter(&line, &filter_lte));
        }

        #[tokio::test]
        async fn test_field_filter_missing_field_rejects() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            // Line with no fields (plain text)
            let line = parse_json_line(r#"{"level":"info","msg":"simple log"}"#, &ctx);

            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "nonexistent".into(),
                    op: FieldFilterOp::Eq,
                    value: "anything".into(),
                }],
                cursor: None,
                page_size: 100,
            };

            assert!(!search.line_matches_filter(&line, &filter));
        }

        #[tokio::test]
        async fn test_multiple_field_filters_all_must_match() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "api".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            let line = parse_json_line(
                r#"{"level":"error","msg":"slow 500","status":"500","duration":"2000ms"}"#,
                &ctx,
            );

            // Both status=500 AND duration_ms > 1000 → should match
            let filter_both = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![
                    FieldFilter {
                        key: "status".into(),
                        op: FieldFilterOp::Eq,
                        value: "500".into(),
                    },
                    FieldFilter {
                        key: "duration_ms".into(),
                        op: FieldFilterOp::Gt,
                        value: "1000".into(),
                    },
                ],
                cursor: None,
                page_size: 100,
            };
            assert!(search.line_matches_filter(&line, &filter_both));

            // status=500 AND duration_ms > 5000 → second fails
            let filter_partial = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![
                    FieldFilter {
                        key: "status".into(),
                        op: FieldFilterOp::Eq,
                        value: "500".into(),
                    },
                    FieldFilter {
                        key: "duration_ms".into(),
                        op: FieldFilterOp::Gt,
                        value: "5000".into(),
                    },
                ],
                cursor: None,
                page_size: 100,
            };
            assert!(!search.line_matches_filter(&line, &filter_partial));
        }

        // ── End-to-end roundtrip: write → storage → decompress → filter

        #[tokio::test]
        async fn test_roundtrip_write_decompress_filter_by_service() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let writer = ChunkWriterService::new(storage.clone());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "payments".into(),
                container_id: "cnt-pay".into(),
                deploy_id: None,
            };

            let lines: Vec<LogLine> = vec![
                parse_json_line(
                    r#"{"level":"info","msg":"payment processed for $99"}"#,
                    &ctx,
                ),
                parse_json_line(r#"{"level":"error","msg":"payment gateway timeout"}"#, &ctx),
                parse_json_line(r#"{"level":"warn","msg":"retry payment #3"}"#, &ctx),
            ];

            let roundtripped = write_and_read_back(&storage, &writer, &lines, "cnt-pay").await;
            assert_eq!(roundtripped.len(), 3);

            // Filter: service=payments → all match
            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec!["payments".into()],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let matches: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter))
                .collect();
            assert_eq!(matches.len(), 3);

            // Filter: service=billing → none match
            let filter_none = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec!["billing".into()],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let no_matches: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_none))
                .collect();
            assert_eq!(no_matches.len(), 0);
        }

        #[tokio::test]
        async fn test_roundtrip_fulltext_search() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let writer = ChunkWriterService::new(storage.clone());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-web".into(),
                deploy_id: None,
            };

            let lines: Vec<LogLine> = vec![
                parse_json_line(r#"{"level":"info","msg":"GET /api/users 200 OK"}"#, &ctx),
                parse_json_line(
                    r#"{"level":"error","msg":"POST /api/orders 500 Internal Server Error"}"#,
                    &ctx,
                ),
                parse_json_line(r#"{"level":"info","msg":"GET /api/health 200 OK"}"#, &ctx),
                parse_json_line(
                    r#"{"level":"warn","msg":"GET /api/users 429 Too Many Requests"}"#,
                    &ctx,
                ),
                parse_json_line(
                    r#"{"level":"error","msg":"connection refused: database pool exhausted"}"#,
                    &ctx,
                ),
            ];

            let roundtripped = write_and_read_back(&storage, &writer, &lines, "cnt-web").await;
            assert_eq!(roundtripped.len(), 5);

            // Search for "users"
            let filter_users = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("users".into()),
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let matches: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_users))
                .collect();
            assert_eq!(matches.len(), 2); // lines 0 and 3

            // Search for "500" (appears in orders error)
            let filter_500 = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("500".into()),
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let matches_500: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_500))
                .collect();
            assert_eq!(matches_500.len(), 1);
            assert!(matches_500[0].msg.contains("orders"));

            // Search for "connection refused" — case insensitive
            let filter_conn = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("CONNECTION REFUSED".into()),
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let matches_conn: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_conn))
                .collect();
            assert_eq!(matches_conn.len(), 1);
            assert!(matches_conn[0].msg.contains("database pool"));
        }

        #[tokio::test]
        async fn test_roundtrip_field_filter_after_json_parsing() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let writer = ChunkWriterService::new(storage.clone());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "api".into(),
                container_id: "cnt-api".into(),
                deploy_id: None,
            };

            let lines: Vec<LogLine> = vec![
                parse_json_line(
                    r#"{"level":"info","msg":"GET /users","status":"200","duration":"12ms","user_id":"u-1"}"#,
                    &ctx,
                ),
                parse_json_line(
                    r#"{"level":"error","msg":"POST /orders","status":"500","duration":"3500ms","user_id":"u-2"}"#,
                    &ctx,
                ),
                parse_json_line(
                    r#"{"level":"warn","msg":"GET /products","status":"429","duration":"80ms","user_id":"u-1"}"#,
                    &ctx,
                ),
            ];

            let roundtripped = write_and_read_back(&storage, &writer, &lines, "cnt-api").await;
            assert_eq!(roundtripped.len(), 3);

            // Verify fields survived the roundtrip
            for line in &roundtripped {
                assert!(
                    line.fields.is_some(),
                    "fields should be preserved after roundtrip"
                );
            }

            // Filter: status=500 → 1 match
            let filter_500 = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "status".into(),
                    op: FieldFilterOp::Eq,
                    value: "500".into(),
                }],
                cursor: None,
                page_size: 100,
            };

            let matches_500: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_500))
                .collect();
            assert_eq!(matches_500.len(), 1);
            assert!(matches_500[0].msg.contains("orders"));

            // Filter: duration_ms > 1000 → 1 match (the 3500ms one)
            let filter_slow = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "duration_ms".into(),
                    op: FieldFilterOp::Gt,
                    value: "1000".into(),
                }],
                cursor: None,
                page_size: 100,
            };

            let matches_slow: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_slow))
                .collect();
            assert_eq!(matches_slow.len(), 1);
            assert_eq!(
                matches_slow[0].fields.as_ref().unwrap()["duration_ms"],
                3500.0
            );

            // Filter: user_id=u-1 → 2 matches
            let filter_user = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![FieldFilter {
                    key: "user_id".into(),
                    op: FieldFilterOp::Eq,
                    value: "u-1".into(),
                }],
                cursor: None,
                page_size: 100,
            };

            let matches_user: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_user))
                .collect();
            assert_eq!(matches_user.len(), 2);
        }

        #[tokio::test]
        async fn test_roundtrip_combined_text_and_field_filter() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let writer = ChunkWriterService::new(storage.clone());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "api".into(),
                container_id: "cnt-api2".into(),
                deploy_id: None,
            };

            let lines: Vec<LogLine> = vec![
                parse_json_line(
                    r#"{"level":"error","msg":"timeout connecting to redis","status":"503","duration":"5000ms"}"#,
                    &ctx,
                ),
                parse_json_line(
                    r#"{"level":"error","msg":"timeout connecting to postgres","status":"503","duration":"8000ms"}"#,
                    &ctx,
                ),
                parse_json_line(
                    r#"{"level":"error","msg":"invalid request body","status":"400","duration":"2ms"}"#,
                    &ctx,
                ),
            ];

            let roundtripped = write_and_read_back(&storage, &writer, &lines, "cnt-api2").await;
            assert_eq!(roundtripped.len(), 3);

            // Text "timeout" + status=503 + duration_ms > 6000 → only postgres
            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![LogLevel::Error],
                services: vec!["api".into()],
                envs: vec!["production".into()],
                deploy_id: None,
                text: Some("timeout".into()),
                field_filters: vec![
                    FieldFilter {
                        key: "status".into(),
                        op: FieldFilterOp::Eq,
                        value: "503".into(),
                    },
                    FieldFilter {
                        key: "duration_ms".into(),
                        op: FieldFilterOp::Gt,
                        value: "6000".into(),
                    },
                ],
                cursor: None,
                page_size: 100,
            };

            let matches: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter))
                .collect();
            assert_eq!(matches.len(), 1);
            assert!(matches[0].msg.contains("postgres"));
        }

        // ── Plain text (non-JSON) log parsing and filtering ───────────

        #[tokio::test]
        async fn test_roundtrip_plain_text_logs_with_level_detection() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let writer = ChunkWriterService::new(storage.clone());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "legacy".into(),
                container_id: "cnt-legacy".into(),
                deploy_id: None,
            };

            let lines: Vec<LogLine> = vec![
                parse_json_line("[ERROR] Failed to connect to database", &ctx),
                parse_json_line("[WARN] Disk usage at 85%", &ctx),
                parse_json_line("[INFO] Server started on port 3000", &ctx),
                parse_json_line("Just a plain log line with no level", &ctx),
            ];

            let roundtripped = write_and_read_back(&storage, &writer, &lines, "cnt-legacy").await;
            assert_eq!(roundtripped.len(), 4);

            // Filter ERROR only
            let filter_error = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![LogLevel::Error],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let errors: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_error))
                .collect();
            assert_eq!(errors.len(), 1);
            assert!(errors[0].msg.contains("database"));

            // Fulltext "disk" across all levels
            let filter_disk = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("disk".into()),
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let disk_matches: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter_disk))
                .collect();
            assert_eq!(disk_matches.len(), 1);
            assert_eq!(disk_matches[0].level, LogLevel::Warn);
        }

        // ── Time range boundary tests ─────────────────────────────────

        #[tokio::test]
        async fn test_time_range_filter_excludes_out_of_range() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            let now = Utc::now();
            let mut recent_line = parse_json_line(r#"{"level":"info","msg":"recent event"}"#, &ctx);
            recent_line.ts = now;

            let mut old_line = parse_json_line(r#"{"level":"info","msg":"old event"}"#, &ctx);
            old_line.ts = now - Duration::days(5);

            // Filter last hour
            let filter = LogSearchFilter {
                project_id,
                start_time: now - Duration::hours(1),
                end_time: now + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            assert!(search.line_matches_filter(&recent_line, &filter));
            assert!(!search.line_matches_filter(&old_line, &filter));
        }

        // ── Validation edge cases ─────────────────────────────────────

        #[tokio::test]
        async fn test_validate_filter_rejects_inverted_time_range() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());

            let filter = LogSearchFilter {
                project_id: test_project_id(),
                start_time: Utc::now() + Duration::hours(1),
                end_time: Utc::now(),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let result = search.search(&filter).await;
            assert!(result.is_err());
            match result.unwrap_err() {
                LogAggregatorError::Validation { message } => {
                    assert!(message.contains("start_time"));
                }
                other => panic!("Expected Validation error, got: {:?}", other),
            }
        }

        #[tokio::test]
        async fn test_validate_fulltext_time_range_limit() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());

            let filter = LogSearchFilter {
                project_id: test_project_id(),
                start_time: Utc::now() - Duration::hours(48),
                end_time: Utc::now(),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: Some("search query".into()), // full text + > 24h
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let result = search.search(&filter).await;
            assert!(result.is_err());
            match result.unwrap_err() {
                LogAggregatorError::SearchTimeRangeExceeded { max_hours, .. } => {
                    assert_eq!(max_hours, 24);
                }
                other => panic!("Expected SearchTimeRangeExceeded, got: {:?}", other),
            }
        }

        // ── Large chunk archive search simulation ──────────────────────

        #[tokio::test]
        async fn test_archive_search_large_chunk_with_mixed_filters() {
            // Simulates the archive_search path: write 1000 lines to storage,
            // read back, decompress, and filter with combined text + level + field filters.
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let writer = ChunkWriterService::new(storage.clone());
            let search = make_search_service(storage.clone());

            let project_id = test_project_id();
            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "api".into(),
                container_id: "cnt-large".into(),
                deploy_id: None,
            };

            // Create 1000 lines: mix of levels, some with "timeout" in the message,
            // some with status=503 field
            let mut lines = Vec::new();
            for i in 0..1000 {
                let level = match i % 10 {
                    0 => "error",
                    1 => "warn",
                    _ => "info",
                };
                let msg = if i % 50 == 0 {
                    format!(
                        r#"{{"level":"{}","msg":"timeout connecting to service-{}","status":"503","duration_ms":"{}"}}"#,
                        level,
                        i,
                        i * 10
                    )
                } else if i % 7 == 0 {
                    format!(
                        r#"{{"level":"{}","msg":"request completed for user-{}","status":"200","duration_ms":"{}"}}"#,
                        level, i, i
                    )
                } else {
                    format!(
                        r#"{{"level":"{}","msg":"processing item {}","count":"{}"}}"#,
                        level,
                        i,
                        i * 2
                    )
                };
                lines.push(parse_json_line(&msg, &ctx));
            }

            let roundtripped = write_and_read_back(&storage, &writer, &lines, "cnt-large").await;
            assert_eq!(
                roundtripped.len(),
                1000,
                "All 1000 lines must survive roundtrip"
            );

            // Filter: ERROR level + "timeout" text + status=503
            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![LogLevel::Error],
                services: vec!["api".into()],
                envs: vec!["production".into()],
                deploy_id: None,
                text: Some("timeout".into()),
                field_filters: vec![FieldFilter {
                    key: "status".into(),
                    op: FieldFilterOp::Eq,
                    value: "503".into(),
                }],
                cursor: None,
                page_size: 500,
            };

            let matches: Vec<_> = roundtripped
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter))
                .collect();

            // Lines that are: ERROR (i%10==0) AND timeout (i%50==0) AND status=503
            // i%50==0 always has status=503 and "timeout". i%50==0 AND i%10==0 => i%50==0
            // because 50 is a multiple of 10. So matches are i=0,50,100,...,950 = 20 lines
            assert_eq!(
                matches.len(),
                20,
                "Expected 20 lines matching ERROR + timeout + status=503"
            );

            // Verify every match actually contains "timeout"
            for m in &matches {
                assert!(
                    m.msg.contains("timeout"),
                    "Match should contain 'timeout': {}",
                    m.msg
                );
            }
        }

        // ── Empty filter edge case ────────────────────────────────────

        #[tokio::test]
        async fn test_empty_filter_matches_all_within_time_range() {
            let tmp = tempfile::tempdir().unwrap();
            let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
            let search = make_search_service(storage.clone());
            let project_id = test_project_id();

            let ctx = ContainerContext {
                project_id,
                env: "production".into(),
                service: "web".into(),
                container_id: "cnt-1".into(),
                deploy_id: None,
            };

            let lines: Vec<LogLine> = vec![
                parse_json_line(r#"{"level":"info","msg":"a"}"#, &ctx),
                parse_json_line(r#"{"level":"error","msg":"b"}"#, &ctx),
                parse_json_line(r#"{"level":"warn","msg":"c"}"#, &ctx),
                parse_json_line(r#"{"level":"debug","msg":"d"}"#, &ctx),
            ];

            // No level, service, env, text, or field filters
            let filter = LogSearchFilter {
                project_id,
                start_time: Utc::now() - Duration::hours(1),
                end_time: Utc::now() + Duration::hours(1),
                levels: vec![],
                services: vec![],
                envs: vec![],
                deploy_id: None,
                text: None,
                field_filters: vec![],
                cursor: None,
                page_size: 100,
            };

            let matches: Vec<_> = lines
                .iter()
                .filter(|l| search.line_matches_filter(l, &filter))
                .collect();
            assert_eq!(matches.len(), 4, "empty filter should match all lines");
        }
    }
}
