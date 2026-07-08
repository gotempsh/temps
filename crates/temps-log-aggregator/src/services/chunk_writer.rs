//! Chunk writer service: buffers log lines and flushes to storage as compressed NDJSON chunks
//!
//! Flush triggers:
//! - Buffer reaches 1MB uncompressed
//! - 30 seconds have elapsed since last flush
//! - Container stops (explicit flush call)

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::debug;
use uuid::Uuid;

use crate::error::LogAggregatorError;
use crate::storage::{traits::build_storage_key, LogStorage};
use crate::types::{ChunkMeta, LogLevel, LogLine};

/// Maximum uncompressed buffer size before flush (1MB)
const MAX_BUFFER_SIZE: usize = 1_024 * 1_024;

/// Maximum time between flushes (30 seconds)
const MAX_FLUSH_INTERVAL_SECS: u64 = 30;

/// Interval for recording line offsets (every 100th line)
const LINE_OFFSET_INTERVAL: usize = 100;

/// Per-container write buffer
struct ContainerBuffer {
    lines: Vec<LogLine>,
    uncompressed_bytes: usize,
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    last_flush: DateTime<Utc>,
    sequence: u64,
    has_errors: bool,
    /// Byte offsets of every 100th line in the uncompressed NDJSON
    line_offsets: Vec<i32>,
    /// Running byte count for offset tracking
    running_byte_count: usize,
}

impl ContainerBuffer {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            uncompressed_bytes: 0,
            first_timestamp: None,
            last_timestamp: None,
            last_flush: Utc::now(),
            sequence: 0,
            has_errors: false,
            line_offsets: Vec::new(),
            running_byte_count: 0,
        }
    }

    fn should_flush(&self) -> bool {
        if self.lines.is_empty() {
            return false;
        }
        self.uncompressed_bytes >= MAX_BUFFER_SIZE
            || (Utc::now() - self.last_flush).num_seconds() >= MAX_FLUSH_INTERVAL_SECS as i64
    }

    fn add_line(&mut self, line: LogLine) {
        // Track byte offsets for every 100th line
        if self.lines.len().is_multiple_of(LINE_OFFSET_INTERVAL) {
            self.line_offsets.push(self.running_byte_count as i32);
        }

        // Estimate uncompressed size of this NDJSON line
        let line_json = serde_json::to_string(&line).unwrap_or_default();
        let line_bytes = line_json.len() + 1; // +1 for newline
        self.running_byte_count += line_bytes;
        self.uncompressed_bytes += line_bytes;

        if line.level == LogLevel::Error || line.level == LogLevel::Warn {
            self.has_errors = true;
        }

        if self.first_timestamp.is_none() {
            self.first_timestamp = Some(line.ts);
        }
        self.last_timestamp = Some(line.ts);
        self.lines.push(line);
    }

    fn take_flush_data(&mut self) -> Option<FlushData> {
        if self.lines.is_empty() {
            return None;
        }

        let lines = std::mem::take(&mut self.lines);
        let offsets = std::mem::take(&mut self.line_offsets);
        let first_ts = self.first_timestamp.take()?;
        let last_ts = self.last_timestamp.take()?;
        let has_errors = self.has_errors;
        self.sequence += 1;
        let seq = self.sequence;

        // Reset buffer state
        self.uncompressed_bytes = 0;
        self.running_byte_count = 0;
        self.has_errors = false;
        self.last_flush = Utc::now();

        Some(FlushData {
            lines,
            line_offsets: offsets,
            started_at: first_ts,
            ended_at: last_ts,
            has_errors,
            sequence: seq,
        })
    }
}

struct FlushData {
    lines: Vec<LogLine>,
    line_offsets: Vec<i32>,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    has_errors: bool,
    sequence: u64,
}

/// Result of a chunk flush: contains both metadata and the original lines.
///
/// The lines are needed to create `log_events` rows in the database for indexed search.
pub struct FlushResult {
    /// Chunk metadata (IDs, timestamps, storage key, etc.)
    pub meta: ChunkMeta,
    /// The original lines that were written to this chunk.
    /// Callers can use `ChunkWriterService::extract_indexable_lines()` to filter
    /// only the ERROR/WARN lines for `log_events` insertion.
    pub lines: Vec<LogLine>,
}

/// Service that buffers log lines per container and flushes compressed NDJSON chunks to storage.
pub struct ChunkWriterService {
    storage: Arc<dyn LogStorage>,
    buffers: Mutex<HashMap<String, ContainerBuffer>>,
}

impl ChunkWriterService {
    pub fn new(storage: Arc<dyn LogStorage>) -> Self {
        Self {
            storage,
            buffers: Mutex::new(HashMap::new()),
        }
    }

    /// Add a log line to the buffer for its container.
    ///
    /// Returns Some(FlushResult) if the buffer was flushed, None otherwise.
    pub async fn write_line(
        &self,
        line: LogLine,
    ) -> Result<Option<FlushResult>, LogAggregatorError> {
        let container_id = line.container_id.clone();
        let should_flush;

        {
            let mut buffers = self.buffers.lock().await;
            let buffer = buffers
                .entry(container_id.clone())
                .or_insert_with(ContainerBuffer::new);
            buffer.add_line(line);
            should_flush = buffer.should_flush();
        }

        if should_flush {
            return self.flush_container(&container_id).await.map(Some);
        }

        Ok(None)
    }

    /// Flush the buffer for a specific container.
    pub async fn flush_container(
        &self,
        container_id: &str,
    ) -> Result<FlushResult, LogAggregatorError> {
        let flush_data = {
            let mut buffers = self.buffers.lock().await;
            match buffers.get_mut(container_id) {
                Some(buffer) => buffer.take_flush_data(),
                None => None,
            }
        };

        match flush_data {
            Some(data) => self.write_chunk(container_id, data).await,
            None => Err(LogAggregatorError::Validation {
                message: format!("No buffered data to flush for container '{}'", container_id),
            }),
        }
    }

    /// Flush all containers that have pending data.
    pub async fn flush_all(&self) -> Vec<Result<FlushResult, LogAggregatorError>> {
        let container_ids: Vec<String> = {
            let buffers = self.buffers.lock().await;
            buffers.keys().cloned().collect()
        };

        let mut results = Vec::new();
        for container_id in container_ids {
            match self.flush_container(&container_id).await {
                Ok(meta) => results.push(Ok(meta)),
                Err(LogAggregatorError::Validation { .. }) => {
                    // Empty buffer, skip
                }
                Err(e) => results.push(Err(e)),
            }
        }
        results
    }

    /// Flush containers that have exceeded the time threshold.
    pub async fn flush_expired(&self) -> Vec<Result<FlushResult, LogAggregatorError>> {
        let container_ids: Vec<String> = {
            let buffers = self.buffers.lock().await;
            buffers
                .iter()
                .filter(|(_, buf)| buf.should_flush())
                .map(|(id, _)| id.clone())
                .collect()
        };

        let mut results = Vec::new();
        for container_id in container_ids {
            match self.flush_container(&container_id).await {
                Ok(meta) => results.push(Ok(meta)),
                Err(LogAggregatorError::Validation { .. }) => {}
                Err(e) => results.push(Err(e)),
            }
        }
        results
    }

    /// Remove a container's buffer (call when container stops).
    pub async fn remove_container(
        &self,
        container_id: &str,
    ) -> Option<Result<FlushResult, LogAggregatorError>> {
        // Flush first
        let result = match self.flush_container(container_id).await {
            Ok(meta) => Some(Ok(meta)),
            Err(LogAggregatorError::Validation { .. }) => None,
            Err(e) => Some(Err(e)),
        };

        // Remove buffer
        let mut buffers = self.buffers.lock().await;
        buffers.remove(container_id);

        result
    }

    /// Compress NDJSON lines and write to storage.
    async fn write_chunk(
        &self,
        container_id: &str,
        data: FlushData,
    ) -> Result<FlushResult, LogAggregatorError> {
        let chunk_id = Uuid::new_v4();
        let first_line = &data.lines[0];
        let project_id = first_line.project_id;
        let external_service_id = first_line.external_service_id;
        let service = first_line.service.clone();
        let env = first_line.env.clone();
        let deploy_id = first_line.deploy_id;
        // A buffer is keyed by container_id, and one container runs on exactly
        // one node, so the node tags are uniform across the buffer — taking
        // them from the first line is correct.
        let node_id = first_line.node_id;
        let node_name = first_line.node_name.clone();
        let line_count = data.lines.len() as i32;

        // Build NDJSON
        let mut ndjson = Vec::new();
        for line in &data.lines {
            let json =
                serde_json::to_string(line).map_err(|e| LogAggregatorError::ChunkWriteFailed {
                    chunk_id,
                    project_id,
                    service: service.clone(),
                    reason: format!("Failed to serialize log line: {}", e),
                })?;
            ndjson.extend_from_slice(json.as_bytes());
            ndjson.push(b'\n');
        }

        // Compress with zstd level 3
        let compressed = zstd::encode_all(ndjson.as_slice(), 3).map_err(|e| {
            LogAggregatorError::CompressionFailed {
                chunk_id,
                reason: e.to_string(),
            }
        })?;

        // Build storage key
        let date = data.started_at.date_naive();
        let hour = data
            .started_at
            .time()
            .format("%H")
            .to_string()
            .parse::<u32>()
            .unwrap_or(0);
        let storage_key = build_storage_key(
            project_id,
            external_service_id,
            &env,
            &service,
            &date,
            hour,
            container_id,
            data.sequence,
        );

        // Write to storage
        let compressed_size = self
            .storage
            .write_chunk(&storage_key, &compressed)
            .await
            .map_err(|e| LogAggregatorError::ChunkWriteFailed {
                chunk_id,
                project_id,
                service: service.clone(),
                reason: format!("Storage write failed: {}", e),
            })?;

        debug!(
            chunk_id = %chunk_id,
            storage_key = storage_key,
            lines = line_count,
            compressed_bytes = compressed_size,
            "Flushed log chunk"
        );

        Ok(FlushResult {
            meta: ChunkMeta {
                id: chunk_id,
                project_id,
                external_service_id,
                env,
                service,
                container_id: container_id.to_string(),
                deploy_id,
                node_id,
                node_name,
                started_at: data.started_at,
                ended_at: data.ended_at,
                storage_key,
                line_count,
                compressed_size_bytes: compressed_size as i32,
                has_errors: data.has_errors,
                line_offsets: data.line_offsets,
            },
            lines: data.lines,
        })
    }

    /// Get the indexable (ERROR/WARN) lines from the most recently flushed data.
    ///
    /// This is intended to be called right after write_line returns Some(ChunkMeta),
    /// passing the lines that were just written to produce log_events rows.
    pub fn extract_indexable_lines(lines: &[LogLine]) -> Vec<&LogLine> {
        lines.iter().filter(|l| l.level.is_indexable()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FilesystemStorage;
    use crate::types::LogStream;

    fn make_line(container_id: &str, level: LogLevel) -> LogLine {
        LogLine {
            ts: Utc::now(),
            stream: LogStream::Stdout,
            level,
            msg: "test message".to_string(),
            fields: None,
            container_id: container_id.to_string(),
            service: "web".to_string(),
            env: "1".to_string(),
            project_id: 1,
            external_service_id: None,
            deploy_id: None,
            node_id: None,
            node_name: None,
        }
    }

    #[tokio::test]
    async fn test_write_line_buffers_without_flush() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage);

        let result = writer
            .write_line(make_line("cnt1", LogLevel::Info))
            .await
            .unwrap();
        assert!(result.is_none(), "should not flush after single line");
    }

    #[tokio::test]
    async fn test_flush_container_writes_chunk() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage.clone());

        // Write some lines
        for _ in 0..5 {
            writer
                .write_line(make_line("cnt1", LogLevel::Info))
                .await
                .unwrap();
        }

        // Explicit flush
        let result = writer.flush_container("cnt1").await.unwrap();
        assert_eq!(result.meta.line_count, 5);
        assert!(!result.meta.has_errors);
        assert!(result.meta.compressed_size_bytes > 0);
        assert_eq!(result.lines.len(), 5);

        // Verify the chunk exists in storage
        assert!(storage
            .chunk_exists(&result.meta.storage_key)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_flush_detects_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage);

        writer
            .write_line(make_line("cnt1", LogLevel::Info))
            .await
            .unwrap();
        writer
            .write_line(make_line("cnt1", LogLevel::Error))
            .await
            .unwrap();

        let result = writer.flush_container("cnt1").await.unwrap();
        assert!(result.meta.has_errors);
    }

    #[tokio::test]
    async fn test_remove_container_flushes_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage);

        writer
            .write_line(make_line("cnt1", LogLevel::Info))
            .await
            .unwrap();

        let result = writer.remove_container("cnt1").await;
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());

        // Verify buffer is removed
        let result = writer.flush_container("cnt1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_flush_empty_container_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage);

        let result = writer.flush_container("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_line_offsets_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage);

        // Write 250 lines
        let project_id = 100;
        for _ in 0..250 {
            let mut line = make_line("cnt1", LogLevel::Info);
            line.project_id = project_id;
            writer.write_line(line).await.unwrap();
        }

        let result = writer.flush_container("cnt1").await.unwrap();
        // Should have offsets at lines 0, 100, 200
        assert_eq!(result.meta.line_offsets.len(), 3);
        assert_eq!(result.meta.line_offsets[0], 0);
        assert!(result.meta.line_offsets[1] > 0);
    }

    #[tokio::test]
    async fn test_flush_result_lines_match_written_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage);

        let project_id = 101;
        let mut written_messages = Vec::new();

        for i in 0..10 {
            let mut line = make_line("cnt-fr", LogLevel::Info);
            line.project_id = project_id;
            line.msg = format!("Message number {}", i);
            written_messages.push(line.msg.clone());
            writer.write_line(line).await.unwrap();
        }

        let result = writer.flush_container("cnt-fr").await.unwrap();

        // FlushResult.lines must contain exactly the lines we wrote
        assert_eq!(result.lines.len(), 10);
        let flushed_messages: Vec<String> = result.lines.iter().map(|l| l.msg.clone()).collect();
        assert_eq!(
            flushed_messages, written_messages,
            "FlushResult.lines must preserve order and content"
        );

        // Metadata must match
        assert_eq!(result.meta.line_count, 10);
        assert_eq!(result.meta.project_id, project_id);
    }

    #[tokio::test]
    async fn test_multiple_containers_flush_independently() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage.clone());

        let project_id = 102;

        // Write 3 lines to container A
        for i in 0..3 {
            let mut line = make_line("cnt-a", LogLevel::Info);
            line.project_id = project_id;
            line.msg = format!("A-{}", i);
            writer.write_line(line).await.unwrap();
        }

        // Write 5 lines to container B
        for i in 0..5 {
            let mut line = make_line("cnt-b", LogLevel::Error);
            line.project_id = project_id;
            line.msg = format!("B-{}", i);
            writer.write_line(line).await.unwrap();
        }

        // Flush container A
        let result_a = writer.flush_container("cnt-a").await.unwrap();
        assert_eq!(result_a.meta.line_count, 3);
        assert!(!result_a.meta.has_errors);
        assert!(result_a.lines.iter().all(|l| l.msg.starts_with("A-")));

        // Container B should still be buffered (A's flush shouldn't affect it)
        let result_b = writer.flush_container("cnt-b").await.unwrap();
        assert_eq!(result_b.meta.line_count, 5);
        assert!(result_b.meta.has_errors);
        assert!(result_b.lines.iter().all(|l| l.msg.starts_with("B-")));

        // Both chunks should exist in storage
        assert!(storage
            .chunk_exists(&result_a.meta.storage_key)
            .await
            .unwrap());
        assert!(storage
            .chunk_exists(&result_b.meta.storage_key)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_large_batch_auto_flush_at_buffer_size() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage.clone());

        let project_id = 103;
        let mut auto_flushed = Vec::new();

        // Write large messages until auto-flush triggers (MAX_BUFFER_SIZE = 1MB)
        // Each line is ~200 bytes JSON, so ~5000 lines ≈ 1MB
        let big_msg = "X".repeat(180);
        for i in 0..6000 {
            let mut line = make_line("cnt-big", LogLevel::Info);
            line.project_id = project_id;
            line.msg = format!("{}-{}", big_msg, i);
            if let Some(result) = writer.write_line(line).await.unwrap() {
                auto_flushed.push(result);
            }
        }

        // At least one auto-flush should have triggered
        assert!(
            !auto_flushed.is_empty(),
            "Expected auto-flush to trigger for > 1MB of data"
        );

        // Every auto-flushed result should have non-empty lines
        for result in &auto_flushed {
            assert!(!result.lines.is_empty());
            assert_eq!(result.meta.line_count, result.lines.len() as i32);
        }

        // Manually flush remaining
        let remaining = writer.flush_container("cnt-big").await;
        let total_lines: usize = auto_flushed.iter().map(|r| r.lines.len()).sum::<usize>()
            + remaining.map(|r| r.lines.len()).unwrap_or(0);
        assert_eq!(total_lines, 6000);
    }

    #[tokio::test]
    async fn test_large_batch_compression_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let writer = ChunkWriterService::new(storage.clone());

        let project_id = 104;

        // Write 1000 diverse lines
        for i in 0..1000 {
            let mut line = make_line(
                "cnt-roundtrip",
                if i % 10 == 0 {
                    LogLevel::Error
                } else {
                    LogLevel::Info
                },
            );
            line.project_id = project_id;
            line.msg = format!(
                "Log event #{}: status={} latency={}ms path=/api/v1/resource/{}",
                i,
                200 + (i % 5),
                i * 2,
                i
            );
            writer.write_line(line).await.unwrap();
        }

        let result = writer.flush_container("cnt-roundtrip").await.unwrap();
        assert_eq!(result.meta.line_count, 1000);
        assert!(result.meta.compressed_size_bytes > 0);

        // Read back from storage and decompress
        let compressed = storage.read_chunk(&result.meta.storage_key).await.unwrap();
        let decompressed = zstd::decode_all(compressed.as_slice()).unwrap();
        let content = String::from_utf8_lossy(&decompressed);
        let roundtripped: Vec<LogLine> = content
            .lines()
            .filter_map(|raw| serde_json::from_str(raw).ok())
            .collect();

        assert_eq!(
            roundtripped.len(),
            1000,
            "All 1000 lines must survive roundtrip"
        );

        // Verify ordering preserved
        for (i, line) in roundtripped.iter().enumerate() {
            assert!(
                line.msg.contains(&format!("Log event #{}", i)),
                "Line {} should contain 'Log event #{}', got: {}",
                i,
                i,
                line.msg
            );
        }

        // Compression ratio: should be significantly smaller than raw
        let raw_size: usize = content.lines().map(|l| l.len() + 1).sum();
        let ratio = result.meta.compressed_size_bytes as f64 / raw_size as f64;
        assert!(
            ratio < 0.5,
            "Expected compression ratio < 50%, got {:.1}% ({} compressed / {} raw)",
            ratio * 100.0,
            result.meta.compressed_size_bytes,
            raw_size
        );
    }

    #[test]
    fn test_extract_indexable_lines() {
        let lines = vec![
            make_line("cnt1", LogLevel::Info),
            make_line("cnt1", LogLevel::Error),
            make_line("cnt1", LogLevel::Warn),
            make_line("cnt1", LogLevel::Debug),
        ];

        let indexable = ChunkWriterService::extract_indexable_lines(&lines);
        assert_eq!(indexable.len(), 2);
        assert_eq!(indexable[0].level, LogLevel::Error);
        assert_eq!(indexable[1].level, LogLevel::Warn);
    }
}
