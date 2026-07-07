//! Storage trait definition for log chunk backends

use async_trait::async_trait;

use crate::error::LogAggregatorError;

/// Pluggable storage backend for compressed log chunks.
///
/// Implementations must be safe to share across threads and async tasks.
/// Both filesystem and S3 backends implement this trait identically.
#[async_trait]
pub trait LogStorage: Send + Sync + 'static {
    /// Write a compressed chunk to storage at the given key.
    ///
    /// The `data` is already zstd-compressed NDJSON. The implementation stores it as-is.
    /// Returns the number of bytes written (compressed size).
    async fn write_chunk(&self, key: &str, data: &[u8]) -> Result<u64, LogAggregatorError>;

    /// Read a compressed chunk from storage by key.
    ///
    /// Returns the raw zstd-compressed bytes. The caller is responsible for decompression.
    async fn read_chunk(&self, key: &str) -> Result<Vec<u8>, LogAggregatorError>;

    /// Read a byte range from a chunk for partial retrieval.
    ///
    /// Used with line offset indices to decompress only the needed portion.
    /// If `end` is None, reads from `start` to the end of the chunk.
    async fn read_chunk_range(
        &self,
        key: &str,
        start: u64,
        end: Option<u64>,
    ) -> Result<Vec<u8>, LogAggregatorError>;

    /// List all chunk keys under a prefix.
    ///
    /// The prefix follows the storage layout: `logs/{project_id}/{service}/{YYYY-MM-DD}/{HH}/`
    async fn list_chunks(&self, prefix: &str) -> Result<Vec<String>, LogAggregatorError>;

    /// Delete a chunk from storage by key.
    ///
    /// Returns Ok(()) even if the key does not exist (idempotent deletion).
    async fn delete_chunk(&self, key: &str) -> Result<(), LogAggregatorError>;

    /// Check if a chunk exists at the given key.
    async fn chunk_exists(&self, key: &str) -> Result<bool, LogAggregatorError>;
}

/// Build the storage key for a log chunk.
///
/// Deployment/application layout:
///   `logs/{project_id}/{env}/{service}/{YYYY-MM-DD}/{HH}/{container_id}-{sequence}.ndjson.zst`
/// Imported/managed external-service layout (when `external_service_id` is set):
///   `logs/external-services/{service_id}/{env}/{service}/{YYYY-MM-DD}/{HH}/{container_id}-{sequence}.ndjson.zst`
#[allow(clippy::too_many_arguments)]
pub fn build_storage_key(
    project_id: i32,
    external_service_id: Option<i32>,
    env: &str,
    service: &str,
    date: &chrono::NaiveDate,
    hour: u32,
    container_id: &str,
    sequence: u64,
) -> String {
    let scope = match external_service_id {
        Some(id) => format!("external-services/{id}"),
        None => project_id.to_string(),
    };
    format!(
        "logs/{scope}/{env}/{service}/{date}/{hour:02}/{container_id}-{sequence:06}.ndjson.zst",
        scope = scope,
        env = env,
        service = service,
        date = date.format("%Y-%m-%d"),
        hour = hour,
        container_id = &container_id[..std::cmp::min(12, container_id.len())],
        sequence = sequence,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn test_build_storage_key() {
        let date = NaiveDate::from_ymd_opt(2026, 2, 25).unwrap();
        let key = build_storage_key(2, None, "3", "web", &date, 14, "abc123def456", 1);
        assert_eq!(
            key,
            "logs/2/3/web/2026-02-25/14/abc123def456-000001.ndjson.zst"
        );
    }

    #[test]
    fn test_build_storage_key_truncates_long_container_id() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let key = build_storage_key(5, None, "1", "api", &date, 0, "abcdef123456789extra", 42);
        assert!(key.contains("abcdef123456-000042.ndjson.zst"));
    }

    #[test]
    fn test_build_storage_key_external_service() {
        let date = NaiveDate::from_ymd_opt(2026, 2, 25).unwrap();
        // External-service chunks (project_id sentinel = 0) key under
        // logs/external-services/{id}/… instead of logs/{project_id}/….
        let key = build_storage_key(
            0,
            Some(7),
            "default",
            "postgres",
            &date,
            14,
            "abc123def456",
            1,
        );
        assert_eq!(
            key,
            "logs/external-services/7/default/postgres/2026-02-25/14/abc123def456-000001.ndjson.zst"
        );
    }
}
