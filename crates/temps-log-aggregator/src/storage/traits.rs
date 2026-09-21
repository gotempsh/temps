// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// Build the storage key for a **v1** (legacy, single-frame `.ndjson.zst`)
/// log chunk.
///
/// Deployment/application layout:
///   `logs/{project_id}/{env}/{service}/{YYYY-MM-DD}/{HH}/{container_id}-{sequence}.ndjson.zst`
/// Imported/managed external-service layout (when `external_service_id` is set):
///   `logs/external-services/{service_id}/{env}/{service}/{YYYY-MM-DD}/{HH}/{container_id}-{sequence}.ndjson.zst`
///
/// Nothing writes v1 chunks anymore (see [`build_storage_key_v2`], ADR-046
/// §1) — this is retained so the legacy key layout stays documented and
/// tested for existing production objects the v1 read path in
/// [`crate::store::chunk_store::ChunkStore`] still decodes.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
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

/// Build the storage key for a v2 (ADR-046) log chunk object.
///
/// Layout: `logs/{scope}/{env}/{service}/{YYYY-MM-DD}/{HH}/{container_id[..12]}-{first_ts_nanos}[-{suffix}].zst`,
/// where `scope` follows the same rule as [`build_storage_key`]. The key is
/// deterministic from `(container_id, first_ts)` (plus `suffix` for
/// compacted chunks), so a crash-and-replay seal re-PUTs the same key and the
/// manifest's `ON CONFLICT (storage_key) DO NOTHING` absorbs the duplicate
/// (ADR-046 §8a.2). The writer passes `suffix: None`; the compactor passes
/// `Some("c")` so a merged chunk never collides with one of its inputs.
pub fn build_storage_key_v2(
    project_id: i32,
    external_service_id: Option<i32>,
    env: &str,
    service: &str,
    first_ts: chrono::DateTime<chrono::Utc>,
    container_id: &str,
    suffix: Option<&str>,
) -> String {
    let scope = match external_service_id {
        Some(id) => format!("external-services/{id}"),
        None => project_id.to_string(),
    };
    let date = first_ts.date_naive();
    let hour = first_ts.format("%H").to_string();
    let first_ts_nanos = first_ts
        .timestamp_nanos_opt()
        .unwrap_or_else(|| first_ts.timestamp_millis().saturating_mul(1_000_000));
    let short_container = &container_id[..std::cmp::min(12, container_id.len())];
    let suffix_part = suffix.map(|s| format!("-{s}")).unwrap_or_default();
    format!(
        "logs/{scope}/{env}/{service}/{date}/{hour}/{short_container}-{first_ts_nanos}{suffix_part}.{ext}",
        date = date.format("%Y-%m-%d"),
        ext = crate::chunk::V2_EXTENSION,
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

    #[test]
    fn test_build_storage_key_v2_deterministic_from_content() {
        let ts: chrono::DateTime<chrono::Utc> = "2026-02-25T14:05:00.123456789Z".parse().unwrap();
        let a = build_storage_key_v2(2, None, "3", "web", ts, "abc123def456xyz", None);
        let b = build_storage_key_v2(2, None, "3", "web", ts, "abc123def456xyz", None);
        assert_eq!(a, b, "same content must yield the same key");
        assert!(a.starts_with("logs/2/3/web/2026-02-25/14/abc123def456-"));
        assert!(a.ends_with(".zst"));
        assert!(!a.ends_with(crate::chunk::V1_SUFFIX));
    }

    #[test]
    fn test_build_storage_key_v2_suffix_disambiguates_compacted_chunks() {
        let ts: chrono::DateTime<chrono::Utc> = "2026-02-25T14:05:00Z".parse().unwrap();
        let plain = build_storage_key_v2(2, None, "3", "web", ts, "abc123def456", None);
        let compacted = build_storage_key_v2(2, None, "3", "web", ts, "abc123def456", Some("c"));
        assert_ne!(plain, compacted);
        assert!(compacted.ends_with("-c.zst"));
    }

    #[test]
    fn test_build_storage_key_v2_external_service() {
        let ts: chrono::DateTime<chrono::Utc> = "2026-02-25T14:05:00Z".parse().unwrap();
        let key = build_storage_key_v2(0, Some(7), "default", "postgres", ts, "abc123def456", None);
        assert!(key.starts_with("logs/external-services/7/default/postgres/2026-02-25/14/"));
    }
}
