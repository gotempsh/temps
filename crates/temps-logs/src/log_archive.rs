// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Archival backend for finished build/deploy job logs.
//!
//! Build/deploy logs are written incrementally to a local JSONL scratch file
//! while a job is running (see [`crate::structured_logs::StructuredLogService`]),
//! because S3 has no true append operation and live tailing during an
//! in-progress job needs to stay fast. Once a job reaches a terminal state,
//! [`crate::file_logs::LogService::archive_log`] uploads the finished file to
//! this backend as a single object and deletes the local copy, so local disk
//! only ever holds logs for currently-running jobs rather than a whole
//! deployment history.
//!
//! This mirrors the storage-backend pattern in `temps-log-aggregator`
//! (`LogStorage` trait + `S3Storage`), but the shape of the problem is
//! different: the aggregator writes many small compressed chunks per
//! project/service/hour and needs range reads, while a build/deploy log is a
//! single whole JSONL file per job read back in one piece. Hence a narrower
//! trait (`upload_log` / `download_log`) rather than reusing `LogStorage`
//! directly.

use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::Config;
use tokio::io::AsyncReadExt;
use tracing::debug;

const CHUNK_KEY_PREFIX: &str = "build-log-chunks";
const MAX_CHUNK_PAGE: usize = 1_000;
const MAX_CHUNK_OBJECT_BYTES: u64 = 1024 * 1024;
// Bound cold durable snapshots as well as their response size. Compacted
// logs use one Range GET instead of this fallback.
const MAX_TAIL_LIST_PAGES: usize = 32;
const TAIL_STORAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableLogChunk {
    pub line: u64,
    pub data: Vec<u8>,
}

/// Errors from the log archive storage backend.
#[derive(Debug, thiserror::Error)]
pub enum LogArchiveStorageError {
    #[error("Failed to upload archived log '{key}' to bucket '{bucket}': {reason}")]
    Upload {
        bucket: String,
        key: String,
        reason: String,
        /// Whether retrying this exact upload might succeed. `false` for
        /// permanent failures (bad credentials, a bucket that doesn't exist,
        /// malformed configuration) that will fail identically on every
        /// attempt -- `LogService::archive_log`'s retry loop checks this and
        /// stops immediately rather than spending its remaining attempts
        /// (and the delay between them) on an error that cannot resolve
        /// itself. See CLAUDE.md's "Resilience Patterns" section: retrying
        /// authentication/not-found failures is explicitly called out as
        /// something to avoid.
        retryable: bool,
    },

    #[error("Failed to download archived log '{key}' from bucket '{bucket}': {reason}")]
    Download {
        bucket: String,
        key: String,
        reason: String,
    },

    #[error("Archived log '{key}' not found in bucket '{bucket}'")]
    NotFound { bucket: String, key: String },
}

impl LogArchiveStorageError {
    /// Whether retrying the operation that produced this error stands a
    /// chance of succeeding. `Upload` carries its own classification (set at
    /// the point the underlying S3 SDK error is known); every other variant
    /// is either not part of the upload retry path (`Download`) or
    /// definitionally permanent (`NotFound`).
    pub fn is_retryable(&self) -> bool {
        match self {
            LogArchiveStorageError::Upload { retryable, .. } => *retryable,
            LogArchiveStorageError::Download { .. } => false,
            LogArchiveStorageError::NotFound { .. } => false,
        }
    }
}

/// S3 error codes that indicate a permanent failure -- retrying with the
/// same credentials/bucket/key will fail identically every time. Anything
/// not in this list (5xx service errors, throttling, or no error code at
/// all because the request never reached S3 -- a timeout or connection
/// failure) is treated as potentially transient and left retryable.
const PERMANENT_S3_ERROR_CODES: &[&str] = &[
    "AccessDenied",
    "AllAccessDisabled",
    "AuthorizationHeaderMalformed",
    "ExpiredToken",
    "InvalidAccessKeyId",
    "InvalidBucketName",
    "InvalidToken",
    "NoSuchBucket",
    "SignatureDoesNotMatch",
];

/// Classify an S3 SDK error as retryable or permanent from its error code.
/// `err.code()` (via [`ProvideErrorMetadata`]) is only populated for
/// responses S3 actually returned (`SdkError::ServiceError`); construction,
/// dispatch, timeout and malformed-response failures never reached S3 at
/// all and are conservatively treated as transient network conditions.
fn is_retryable_s3_error(err: &impl ProvideErrorMetadata) -> bool {
    match err.code() {
        Some(code) => !PERMANENT_S3_ERROR_CODES.contains(&code),
        None => true,
    }
}

/// Pluggable archive backend for finished build/deploy logs.
///
/// Implementations must be safe to share across threads and async tasks.
/// There is currently one production implementation, [`S3LogArchive`]; when
/// no backend is configured `LogService` holds `None` instead of a no-op
/// implementation, so archival (and the local-file deletion it triggers) can
/// never run for an install that never opted in.
#[async_trait::async_trait]
pub trait LogArchiveStorage: Send + Sync + 'static {
    /// Persist one immutable JSONL entry before its producer is acknowledged.
    async fn upload_log_chunk(
        &self,
        _log_id: &str,
        _line: u64,
        _data: Vec<u8>,
    ) -> Result<(), LogArchiveStorageError> {
        Ok(())
    }

    /// Read durable entries after `after_line`, in ascending line order.
    async fn download_log_chunks(
        &self,
        _log_id: &str,
        _after_line: u64,
        _limit: usize,
    ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
        Ok(Vec::new())
    }

    /// Read the most recent durable entries in ascending line order.
    async fn download_recent_log_chunks(
        &self,
        _log_id: &str,
        _limit: usize,
    ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
        Ok(Vec::new())
    }

    /// Read a bounded suffix of durable entries, newest first at the store
    /// and returned in ascending line order. Implementations must bound
    /// downloaded bytes as well as the number of entries.
    async fn download_recent_log_chunks_bounded(
        &self,
        log_id: &str,
        _limit: usize,
        _max_bytes: usize,
    ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
        Err(LogArchiveStorageError::Download {
            bucket: "archive".to_string(),
            key: log_id.to_string(),
            reason: "archive backend does not support bounded chunk reads".to_string(),
        })
    }

    /// Download at most `max_bytes` trailing bytes, returning whether the
    /// result starts inside the object. Never fall back to a full download.
    async fn download_log_suffix(
        &self,
        key: &str,
        _max_bytes: usize,
    ) -> Result<(Vec<u8>, bool), LogArchiveStorageError> {
        Err(LogArchiveStorageError::Download {
            bucket: "archive".to_string(),
            key: key.to_string(),
            reason: "archive backend does not support bounded suffix reads".to_string(),
        })
    }

    /// Upload the full contents of a finished log as one object.
    async fn upload_log(&self, key: &str, data: Vec<u8>) -> Result<(), LogArchiveStorageError>;

    /// Upload a finished log from a file. Production object stores override
    /// this to stream from disk with constant memory; the default keeps small
    /// test backends source-compatible.
    async fn upload_log_file(
        &self,
        key: &str,
        path: &std::path::Path,
    ) -> Result<(), LogArchiveStorageError> {
        let data = tokio::fs::read(path)
            .await
            .map_err(|error| LogArchiveStorageError::Upload {
                bucket: "local-file".to_string(),
                key: key.to_string(),
                reason: format!("failed to read '{}': {error}", path.display()),
                retryable: false,
            })?;
        self.upload_log(key, data).await
    }

    /// Download the full contents of a previously archived log.
    async fn download_log(&self, key: &str) -> Result<Vec<u8>, LogArchiveStorageError>;
}

/// S3-compatible archive backend for build/deploy logs.
///
/// Works with AWS S3, MinIO, Tigris, Cloudflare R2, RustFS, and any other
/// S3-compatible API -- the same set of backends `temps-log-aggregator`'s
/// `S3Storage` supports, since both are built from the same
/// `temps_core::LogStorageConfig::S3` variant.
pub struct S3LogArchive {
    client: S3Client,
    bucket: String,
    prefix: Option<String>,
}

impl S3LogArchive {
    /// Build a new S3 archive backend from raw connection fields (the fields
    /// of `temps_core::LogStorageConfig::S3`). Construction never fails: it
    /// only builds an SDK client config, it does not perform I/O or validate
    /// credentials against the bucket.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bucket: String,
        prefix: Option<String>,
        region: String,
        endpoint: Option<String>,
        access_key_id: String,
        secret_access_key: String,
        force_path_style: bool,
    ) -> Self {
        let creds = Credentials::new(access_key_id, secret_access_key, None, None, "temps-logs");

        let mut s3_config = Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(Region::new(region.clone()))
            .credentials_provider(creds)
            .force_path_style(force_path_style);

        if let Some(endpoint_url) = &endpoint {
            s3_config = s3_config.endpoint_url(endpoint_url);
        }

        let client = S3Client::from_conf(s3_config.build());

        debug!(bucket = %bucket, region = %region, "S3 build/deploy log archive initialized");

        Self {
            client,
            bucket,
            prefix,
        }
    }

    /// Build the full S3 key including the optional configured prefix.
    fn full_key(&self, key: &str) -> String {
        match &self.prefix {
            Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), key),
            None => key.to_string(),
        }
    }

    fn chunk_prefix(&self, log_id: &str) -> String {
        self.full_key(&format!("{CHUNK_KEY_PREFIX}/{}/", hex::encode(log_id)))
    }

    fn chunk_key(&self, log_id: &str, line: u64) -> String {
        format!("{}{:020}.jsonl", self.chunk_prefix(log_id), line)
    }

    fn line_from_chunk_key(key: &str) -> Option<u64> {
        key.rsplit('/').next()?.strip_suffix(".jsonl")?.parse().ok()
    }
    async fn recent_chunks_with_budget(
        &self,
        log_id: &str,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let bounded = max_bytes != usize::MAX;
        let prefix = self.chunk_prefix(log_id);
        let mut pages = 0;
        let mut continuation = None;
        let mut keys = std::collections::VecDeque::with_capacity(limit.min(MAX_CHUNK_PAGE));
        loop {
            if bounded && pages == MAX_TAIL_LIST_PAGES {
                return Err(LogArchiveStorageError::Download {
                    bucket: self.bucket.clone(), key: prefix.clone(),
                    reason: "durable log tail exceeded its listing budget; retry after the job compacts its log".to_string(),
                });
            }
            pages += 1;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .max_keys(MAX_CHUNK_PAGE as i32);
            if let Some(token) = continuation {
                request = request.continuation_token(token);
            }
            let response =
                request
                    .send()
                    .await
                    .map_err(|error| LogArchiveStorageError::Download {
                        bucket: self.bucket.clone(),
                        key: prefix.clone(),
                        reason: error.to_string(),
                    })?;
            for object in response.contents() {
                if let Some(key) = object.key() {
                    if keys.len() == limit {
                        keys.pop_front();
                    }
                    if limit > 0 {
                        keys.push_back(key.to_string());
                    }
                }
            }
            if !response.is_truncated().unwrap_or(false) {
                break;
            }
            continuation = response.next_continuation_token().map(str::to_string);
        }

        let mut chunks = Vec::with_capacity(keys.len());
        let mut remaining_bytes = max_bytes;
        for key in keys.into_iter().rev() {
            let Some(line) = Self::line_from_chunk_key(&key) else {
                continue;
            };
            if remaining_bytes == 0 {
                break;
            }
            let response = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|error| LogArchiveStorageError::Download {
                    bucket: self.bucket.clone(),
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
            let object_limit = remaining_bytes.min(MAX_CHUNK_OBJECT_BYTES as usize);
            if response
                .content_length()
                .is_some_and(|size| size < 0 || size as u64 > object_limit as u64)
            {
                break;
            }
            let mut body = Vec::new();
            response
                .body
                .into_async_read()
                .take(object_limit as u64 + 1)
                .read_to_end(&mut body)
                .await
                .map_err(|error| LogArchiveStorageError::Download {
                    bucket: self.bucket.clone(),
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
            if body.len() > object_limit {
                break;
            }
            remaining_bytes -= body.len();
            chunks.push(DurableLogChunk { line, data: body });
        }
        chunks.sort_by_key(|chunk| chunk.line);
        Ok(chunks)
    }
}

#[async_trait::async_trait]
impl LogArchiveStorage for S3LogArchive {
    async fn upload_log_chunk(
        &self,
        log_id: &str,
        line: u64,
        data: Vec<u8>,
    ) -> Result<(), LogArchiveStorageError> {
        let key = self.chunk_key(log_id, line);
        let expected = data.clone();
        let result = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(data))
            .content_type("application/jsonl")
            .if_none_match("*")
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if error.code() == Some("PreconditionFailed") => {
                let response = self
                    .client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(|get_error| LogArchiveStorageError::Upload {
                        bucket: self.bucket.clone(),
                        key: key.clone(),
                        reason: format!(
                            "chunk already exists but could not be verified: {get_error}"
                        ),
                        retryable: is_retryable_s3_error(&get_error),
                    })?;
                let mut reader = response
                    .body
                    .into_async_read()
                    .take(MAX_CHUNK_OBJECT_BYTES + 1);
                let mut existing =
                    Vec::with_capacity(expected.len().min(MAX_CHUNK_OBJECT_BYTES as usize));
                reader
                    .read_to_end(&mut existing)
                    .await
                    .map_err(|read_error| LogArchiveStorageError::Upload {
                        bucket: self.bucket.clone(),
                        key: key.clone(),
                        reason: format!(
                            "chunk already exists but its body could not be verified: {read_error}"
                        ),
                        retryable: true,
                    })?;
                if existing == expected {
                    Ok(())
                } else {
                    Err(LogArchiveStorageError::Upload {
                        bucket: self.bucket.clone(),
                        key,
                        reason: format!(
                            "immutable chunk key collision: existing object is {} bytes and differs from the {}-byte retry payload",
                            existing.len(), expected.len()
                        ),
                        retryable: false,
                    })
                }
            }
            Err(error) => Err(LogArchiveStorageError::Upload {
                bucket: self.bucket.clone(),
                key: key.clone(),
                reason: error.to_string(),
                retryable: is_retryable_s3_error(&error),
            }),
        }
    }

    async fn download_log_chunks(
        &self,
        log_id: &str,
        after_line: u64,
        limit: usize,
    ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
        let prefix = self.chunk_prefix(log_id);
        let start_after = self.chunk_key(log_id, after_line);
        let page_limit = limit.clamp(1, MAX_CHUNK_PAGE) as i32;
        let response = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .start_after(start_after)
            .max_keys(page_limit)
            .send()
            .await
            .map_err(|error| LogArchiveStorageError::Download {
                bucket: self.bucket.clone(),
                key: prefix.clone(),
                reason: error.to_string(),
            })?;

        let mut chunks = Vec::with_capacity(response.contents().len());
        for object in response.contents() {
            let Some(key) = object.key() else { continue };
            let Some(line) = Self::line_from_chunk_key(key) else {
                continue;
            };
            let body = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
                .map_err(|error| LogArchiveStorageError::Download {
                    bucket: self.bucket.clone(),
                    key: key.to_string(),
                    reason: error.to_string(),
                })?
                .body
                .collect()
                .await
                .map_err(|error| LogArchiveStorageError::Download {
                    bucket: self.bucket.clone(),
                    key: key.to_string(),
                    reason: error.to_string(),
                })?;
            chunks.push(DurableLogChunk {
                line,
                data: body.into_bytes().to_vec(),
            });
        }
        chunks.sort_by_key(|chunk| chunk.line);
        Ok(chunks)
    }

    async fn download_recent_log_chunks(
        &self,
        log_id: &str,
        limit: usize,
    ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
        self.download_recent_log_chunks_bounded(log_id, limit, usize::MAX)
            .await
    }

    async fn download_recent_log_chunks_bounded(
        &self,
        log_id: &str,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
        let read = self.recent_chunks_with_budget(log_id, limit, max_bytes);
        if max_bytes == usize::MAX {
            return read.await;
        }
        tokio::time::timeout(TAIL_STORAGE_TIMEOUT, read)
            .await
            .map_err(|_| LogArchiveStorageError::Download {
                bucket: self.bucket.clone(),
                key: self.chunk_prefix(log_id),
                reason: "durable log tail exceeded its 10-second storage budget".to_string(),
            })?
    }

    async fn upload_log(&self, key: &str, data: Vec<u8>) -> Result<(), LogArchiveStorageError> {
        let full_key = self.full_key(key);
        let body = ByteStream::from(data);

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .body(body)
            .content_type("application/jsonl")
            .send()
            .await
            .map_err(|e| {
                let retryable = is_retryable_s3_error(&e);
                LogArchiveStorageError::Upload {
                    bucket: self.bucket.clone(),
                    key: full_key.clone(),
                    reason: e.to_string(),
                    retryable,
                }
            })?;

        debug!(bucket = %self.bucket, key = %full_key, "Archived build/deploy log to S3");
        Ok(())
    }

    async fn upload_log_file(
        &self,
        key: &str,
        path: &std::path::Path,
    ) -> Result<(), LogArchiveStorageError> {
        let full_key = self.full_key(key);
        let body =
            ByteStream::from_path(path)
                .await
                .map_err(|error| LogArchiveStorageError::Upload {
                    bucket: self.bucket.clone(),
                    key: full_key.clone(),
                    reason: format!("failed to open '{}' for streaming: {error}", path.display()),
                    retryable: false,
                })?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .body(body)
            .content_type("application/jsonl")
            .send()
            .await
            .map_err(|error| LogArchiveStorageError::Upload {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: error.to_string(),
                retryable: is_retryable_s3_error(&error),
            })?;

        debug!(bucket = %self.bucket, key = %full_key, path = %path.display(), "Streamed build/deploy log to S3");
        Ok(())
    }

    async fn download_log_suffix(
        &self,
        key: &str,
        max_bytes: usize,
    ) -> Result<(Vec<u8>, bool), LogArchiveStorageError> {
        if max_bytes == 0 {
            return Ok((Vec::new(), false));
        }
        let full_key = self.full_key(key);
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .range(format!("bytes=-{max_bytes}"))
            .send()
            .await
            .map_err(|error| {
                if error.as_service_error().is_some_and(|e| e.is_no_such_key())
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 404)
                {
                    LogArchiveStorageError::NotFound {
                        bucket: self.bucket.clone(),
                        key: full_key.clone(),
                    }
                } else {
                    LogArchiveStorageError::Download {
                        bucket: self.bucket.clone(),
                        key: full_key.clone(),
                        reason: error.to_string(),
                    }
                }
            })?;
        let starts_inside = response
            .content_range()
            .is_some_and(|range| !range.starts_with("bytes 0-"));
        if response
            .content_length()
            .is_some_and(|size| size < 0 || size as u64 > max_bytes as u64)
        {
            return Err(LogArchiveStorageError::Download {
                bucket: self.bucket.clone(),
                key: full_key,
                reason: "archive server ignored the bounded Range request".to_string(),
            });
        }
        let mut bytes = Vec::new();
        response
            .body
            .into_async_read()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| LogArchiveStorageError::Download {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: error.to_string(),
            })?;
        if bytes.len() > max_bytes {
            return Err(LogArchiveStorageError::Download {
                bucket: self.bucket.clone(),
                key: full_key,
                reason: "archive response exceeded the tail byte limit".to_string(),
            });
        }
        Ok((bytes, starts_inside))
    }

    async fn download_log(&self, key: &str) -> Result<Vec<u8>, LogArchiveStorageError> {
        let full_key = self.full_key(key);

        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
            .map_err(|e| {
                let err_str = e.to_string();
                if err_str.contains("NoSuchKey") || err_str.contains("404") {
                    LogArchiveStorageError::NotFound {
                        bucket: self.bucket.clone(),
                        key: full_key.clone(),
                    }
                } else {
                    LogArchiveStorageError::Download {
                        bucket: self.bucket.clone(),
                        key: full_key.clone(),
                        reason: err_str,
                    }
                }
            })?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| LogArchiveStorageError::Download {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: format!("failed reading response body: {e}"),
            })?
            .into_bytes()
            .to_vec();

        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn suffix_test_server(
        response: &'static str,
    ) -> (S3LogArchive, tokio::task::JoinHandle<String>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0, "request ended before its headers");
                request.extend_from_slice(&buffer[..read]);
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        let archive = S3LogArchive::new(
            "test-bucket".into(),
            None,
            "us-east-1".into(),
            Some(format!("http://{address}")),
            "test-access-key".into(),
            "test-secret-key".into(),
            true,
        );
        (archive, task)
    }

    #[tokio::test]
    async fn archived_suffix_uses_a_range_request() {
        let (archive, server) = suffix_test_server(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 2-5/6\r\nConnection: close\r\n\r\nb\nc\n"
        ).await;
        let (bytes, starts_inside) = archive.download_log_suffix("log", 4).await.unwrap();
        assert_eq!(bytes, b"b\nc\n");
        assert!(starts_inside);
        let request = server.await.unwrap().to_ascii_lowercase();
        assert!(request.contains("range: bytes=-4"), "{request}");
    }

    #[tokio::test]
    async fn archived_suffix_rejects_a_server_that_ignores_range() {
        let (archive, server) = suffix_test_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
        )
        .await;
        let error = archive.download_log_suffix("log", 4).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("ignored the bounded Range request"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn archived_suffix_maps_plain_http_404_to_not_found() {
        let (archive, server) = suffix_test_server(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(matches!(
            archive.download_log_suffix("log", 4).await,
            Err(LogArchiveStorageError::NotFound { .. })
        ));
        server.await.unwrap();
    }

    async fn chunk_budget_test_server(
        endless_pages: bool,
    ) -> (
        S3LogArchive,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let read = socket.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let is_list = String::from_utf8_lossy(&request).contains("list-type=2");
                let body = if is_list {
                    let contents = (1..=130)
                        .map(|line| {
                            format!(
                        "<Contents><Key>build-log-chunks/6c6f67/{line:020}.jsonl</Key></Contents>"
                    )
                        })
                        .collect::<String>();
                    format!("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><IsTruncated>{endless_pages}</IsTruncated><NextContinuationToken>again</NextContinuationToken>{contents}</ListBucketResult>")
                } else {
                    "x".to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let archive = S3LogArchive::new(
            "test-bucket".into(),
            None,
            "us-east-1".into(),
            Some(format!("http://{address}")),
            "id".into(),
            "secret".into(),
            true,
        );
        (archive, requests, task)
    }

    #[tokio::test]
    async fn durable_tail_caps_object_requests_and_retains_latest_entries() {
        use std::sync::atomic::Ordering;
        let (archive, requests, server) = chunk_budget_test_server(false).await;
        let chunks = archive
            .download_recent_log_chunks_bounded("log", 128, 8 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(chunks.len(), 128);
        assert_eq!(chunks.first().unwrap().line, 3);
        assert_eq!(chunks.last().unwrap().line, 130);
        assert_eq!(requests.load(Ordering::SeqCst), 129);
        server.abort();
    }

    #[tokio::test]
    async fn durable_tail_stops_paginating_at_its_storage_budget() {
        use std::sync::atomic::Ordering;
        let (archive, requests, server) = chunk_budget_test_server(true).await;
        let error = archive
            .download_recent_log_chunks_bounded("log", 10_000, 8 * 1024 * 1024)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("listing budget"), "{error}");
        assert_eq!(requests.load(Ordering::SeqCst), MAX_TAIL_LIST_PAGES);
        server.abort();
    }

    fn test_archive(prefix: Option<&str>) -> S3LogArchive {
        S3LogArchive::new(
            "test-bucket".to_string(),
            prefix.map(str::to_string),
            "us-east-1".to_string(),
            None,
            "id".to_string(),
            "secret".to_string(),
            false,
        )
    }

    #[test]
    fn test_full_key_with_prefix() {
        let archive = test_archive(Some("logs/"));
        assert_eq!(
            archive.full_key("build-logs/deployment-1-job-build.log"),
            "logs/build-logs/deployment-1-job-build.log"
        );
    }

    #[test]
    fn test_full_key_without_prefix() {
        let archive = test_archive(None);
        assert_eq!(
            archive.full_key("build-logs/deployment-1-job-build.log"),
            "build-logs/deployment-1-job-build.log"
        );
    }

    #[test]
    fn test_full_key_trims_trailing_slash_in_prefix() {
        let archive = test_archive(Some("logs"));
        assert_eq!(
            archive.full_key("build-logs/x.log"),
            "logs/build-logs/x.log"
        );
    }
}
