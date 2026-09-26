// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared configuration for where Temps stores log data.
//!
//! Temps writes two kinds of logs to durable storage: aggregated container
//! logs (`temps-log-aggregator`, compressed NDJSON chunks) and build/deploy
//! job logs (`temps-logs`, JSONL files written incrementally while a job
//! runs). Both are operationally the same question for a self-hosted
//! operator -- "where does Temps put log data, local disk or an
//! S3-compatible bucket" -- so they share one configuration type and one set
//! of `TEMPS_LOG_STORAGE_BACKEND` / `TEMPS_LOG_S3_*` environment variables,
//! resolved once in `temps-cli`'s serve command and handed to both plugins.
//!
//! This type lives in `temps-core` rather than in `temps-log-aggregator`
//! (where it originated) because `temps-logs` needed it too: `temps-logs` is
//! a lightweight leaf crate (only `temps-core` as a dependency) used broadly
//! across the deployment pipeline, while `temps-log-aggregator` pulls in
//! `temps-auth`, `temps-database`, `temps-entities` and `sea-orm` for its
//! chunk-indexing responsibilities. Depending on `temps-log-aggregator` from
//! `temps-logs` would have dragged that entire dependency graph into a crate
//! that otherwise needs none of it. `temps-core` is the one crate both
//! already depend on, so the shared *data* (this enum) lives here while each
//! crate keeps its own storage-backend *implementation* (chunked
//! read/write/list for the aggregator, whole-object upload/download for
//! build logs) -- the credentials and backend selection are shared, the I/O
//! code is not.

/// Storage backend selection for log data.
///
/// `Filesystem` is the default for every existing self-hosted install and
/// keeps logs on local disk with no retention. `S3` stores logs in an
/// S3-compatible bucket (AWS S3, MinIO, Tigris, Cloudflare R2, or any other
/// S3 API-compatible service).
#[derive(Debug, Clone)]
pub enum LogStorageConfig {
    Filesystem {
        base_path: std::path::PathBuf,
    },
    S3 {
        bucket: String,
        prefix: Option<String>,
        region: String,
        endpoint: Option<String>,
        access_key_id: String,
        secret_access_key: String,
        force_path_style: bool,
    },
}

impl LogStorageConfig {
    /// Whether this configuration selects the S3 backend.
    pub fn is_s3(&self) -> bool {
        matches!(self, LogStorageConfig::S3 { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_s3() {
        assert!(!LogStorageConfig::Filesystem {
            base_path: "/tmp".into()
        }
        .is_s3());

        assert!(LogStorageConfig::S3 {
            bucket: "b".into(),
            prefix: None,
            region: "us-east-1".into(),
            endpoint: None,
            access_key_id: "id".into(),
            secret_access_key: "secret".into(),
            force_path_style: false,
        }
        .is_s3());
    }
}
