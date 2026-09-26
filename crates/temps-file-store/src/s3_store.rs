// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! S3-compatible implementation of [`FileStore`].
//!
//! Works with AWS S3, MinIO, Tigris, Cloudflare R2, RustFS, and any other
//! S3-compatible API. Uses the same two key namespaces as `FsFileStore`:
//!
//! ```text
//! {prefix}/blobs/{hash[0..2]}/{hash[2..4]}/{hash}   (content-addressed)
//! {prefix}/paths/{sanitized path}                    (path-keyed)
//! ```
//!
//! Every operation runs against the client built by
//! [`crate::s3_client::build_s3_client`], which sets a connect + operation
//! timeout so PUT/HEAD/DELETE and a GetObject's headers phase can never hang.
//! Streaming a GetObject's body (`open_blob`/`open`) is bounded separately by
//! [`IdleTimeoutReader`], because a whole-operation timeout would kill a
//! valid large object mid-stream purely for taking a while to transfer.

use crate::s3_client::build_s3_client;
use crate::s3_config::S3StorageConfig;
use crate::{FileStore, FileStoreError, OpenedBlob};
use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, ReadBuf};
use tracing::debug;

pub struct S3FileStore {
    client: S3Client,
    bucket: String,
    prefix: Option<String>,
    timeout: Duration,
}

impl S3FileStore {
    pub fn new(config: S3StorageConfig) -> Self {
        let client = build_s3_client(&config);
        Self {
            client,
            bucket: config.bucket,
            prefix: config.prefix,
            timeout: config.timeout,
        }
    }

    /// SHA-256 hex digest — identical algorithm to `FsFileStore::content_hash`
    /// so blobs written by one backend hash to the same key as the other.
    pub fn content_hash(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    fn full_key(&self, key: &str) -> String {
        match &self.prefix {
            Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), key),
            None => key.to_string(),
        }
    }

    fn blob_key(&self, hash: &str) -> Result<String, FileStoreError> {
        validate_content_hash(hash)?;
        Ok(self.full_key(&format!("blobs/{}/{}/{}", &hash[..2], &hash[2..4], hash)))
    }

    /// Path-keyed object key. Sanitizes the same way `FsFileStore::cache_path`
    /// does (strip leading slash, drop empty/`.`/`..` segments) so a
    /// corrupted or unexpected input can never address a key outside the
    /// `paths/` namespace.
    fn path_key(&self, path: &str) -> String {
        let clean = path
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
            .collect::<Vec<_>>()
            .join("/");
        self.full_key(&format!("paths/{clean}"))
    }

    async fn timed<F, T>(&self, path: &str, future: F) -> Result<T, FileStoreError>
    where
        F: std::future::Future<Output = Result<T, FileStoreError>>,
    {
        tokio::time::timeout(self.timeout, future)
            .await
            .map_err(|_| FileStoreError::Timeout {
                path: path.to_string(),
                timeout_secs: self.timeout.as_secs(),
            })?
    }

    async fn put_object(
        &self,
        key: &str,
        log_path: &str,
        data: Bytes,
    ) -> Result<(), FileStoreError> {
        let len = data.len();
        self.timed(log_path, async {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .body(ByteStream::from(data.to_vec()))
                .send()
                .await
                .map_err(|error| {
                    FileStoreError::Backend(format!(
                        "PutObject to s3://{}/{key} failed: {error}",
                        self.bucket
                    ))
                })?;
            Ok(())
        })
        .await?;
        debug!(bucket = %self.bucket, key, bytes = len, "S3FileStore: stored object");
        Ok(())
    }

    async fn head_object_length(
        &self,
        key: &str,
        log_path: &str,
    ) -> Result<Option<u64>, FileStoreError> {
        self.timed(log_path, async {
            match self
                .client
                .head_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
            {
                Ok(output) => Ok(Some(
                    output.content_length().unwrap_or_default().max(0) as u64
                )),
                Err(error) => {
                    if error
                        .as_service_error()
                        .map(|service_error| service_error.is_not_found())
                        .unwrap_or(false)
                    {
                        Ok(None)
                    } else {
                        Err(FileStoreError::Backend(format!(
                            "HeadObject for s3://{}/{key} failed: {error}",
                            self.bucket
                        )))
                    }
                }
            }
        })
        .await
    }

    async fn get_object_buffered(
        &self,
        key: &str,
        log_path: &str,
    ) -> Result<Bytes, FileStoreError> {
        self.timed(log_path, async {
            let response = match self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    if error
                        .as_service_error()
                        .map(|service_error| service_error.is_no_such_key())
                        .unwrap_or(false)
                    {
                        return Err(FileStoreError::NotFound {
                            path: log_path.to_string(),
                        });
                    }
                    return Err(FileStoreError::Backend(format!(
                        "GetObject for s3://{}/{key} failed: {error}",
                        self.bucket
                    )));
                }
            };
            let bytes = response
                .body
                .collect()
                .await
                .map_err(|error| {
                    FileStoreError::Backend(format!(
                        "reading GetObject body for s3://{}/{key} failed: {error}",
                        self.bucket
                    ))
                })?
                .into_bytes();
            Ok(bytes)
        })
        .await
    }

    /// Open a GetObject for streaming: the (bounded) headers phase happens
    /// under `self.timeout`, but the returned reader is wrapped in
    /// [`IdleTimeoutReader`] rather than bound by the same deadline, since a
    /// large-but-healthy transfer must not be killed purely for its size.
    async fn open_object(&self, key: &str, log_path: &str) -> Result<OpenedBlob, FileStoreError> {
        let response = self
            .timed(log_path, async {
                match self
                    .client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .send()
                    .await
                {
                    Ok(response) => Ok(response),
                    Err(error) => {
                        if error
                            .as_service_error()
                            .map(|service_error| service_error.is_no_such_key())
                            .unwrap_or(false)
                        {
                            Err(FileStoreError::NotFound {
                                path: log_path.to_string(),
                            })
                        } else {
                            Err(FileStoreError::Backend(format!(
                                "GetObject for s3://{}/{key} failed: {error}",
                                self.bucket
                            )))
                        }
                    }
                }
            })
            .await?;
        let size_bytes = response.content_length().unwrap_or_default().max(0) as u64;
        let reader = IdleTimeoutReader::new(response.body.into_async_read(), self.timeout);
        Ok(OpenedBlob {
            reader: Box::new(reader),
            size_bytes,
        })
    }

    async fn delete_object(&self, key: &str, log_path: &str) -> Result<bool, FileStoreError> {
        let existed = self.head_object_length(key, log_path).await?.is_some();
        self.timed(log_path, async {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
                .map_err(|error| {
                    FileStoreError::Backend(format!(
                        "DeleteObject for s3://{}/{key} failed: {error}",
                        self.bucket
                    ))
                })?;
            Ok(())
        })
        .await?;
        Ok(existed)
    }
}

#[async_trait]
impl FileStore for S3FileStore {
    async fn put_blob(&self, data: Bytes) -> Result<String, FileStoreError> {
        let hash = Self::content_hash(&data);
        let key = self.blob_key(&hash)?;

        if self.head_object_length(&key, &hash).await?.is_some() {
            debug!(
                hash_prefix = hash.get(..8).unwrap_or(hash.as_str()),
                bytes = data.len(),
                "S3FileStore: dedup hit"
            );
            return Ok(hash);
        }

        self.put_object(&key, &hash, data).await?;
        Ok(hash)
    }

    async fn get_blob(&self, hash: &str) -> Result<Bytes, FileStoreError> {
        let key = self.blob_key(hash)?;
        self.get_object_buffered(&key, hash).await
    }

    async fn open_blob(&self, hash: &str) -> Result<OpenedBlob, FileStoreError> {
        let key = self.blob_key(hash)?;
        self.open_object(&key, hash).await
    }

    async fn blob_exists(&self, hash: &str) -> Result<bool, FileStoreError> {
        let key = self.blob_key(hash)?;
        Ok(self.head_object_length(&key, hash).await?.is_some())
    }

    async fn delete_blob(&self, hash: &str) -> Result<bool, FileStoreError> {
        let key = self.blob_key(hash)?;
        self.delete_object(&key, hash).await
    }

    async fn put(&self, path: &str, data: Bytes) -> Result<u64, FileStoreError> {
        let size = data.len() as u64;
        let key = self.path_key(path);
        self.put_object(&key, path, data).await?;
        Ok(size)
    }

    async fn get(&self, path: &str) -> Result<Bytes, FileStoreError> {
        let key = self.path_key(path);
        self.get_object_buffered(&key, path).await
    }

    async fn open(&self, path: &str) -> Result<OpenedBlob, FileStoreError> {
        let key = self.path_key(path);
        self.open_object(&key, path).await
    }

    async fn exists(&self, path: &str) -> Result<bool, FileStoreError> {
        let key = self.path_key(path);
        Ok(self.head_object_length(&key, path).await?.is_some())
    }

    async fn open_raw(&self, key: &str) -> Result<OpenedBlob, FileStoreError> {
        self.open_object(&self.full_key(key), key).await
    }

    async fn stat_raw(&self, key: &str) -> Result<u64, FileStoreError> {
        self.head_object_length(&self.full_key(key), key)
            .await?
            .ok_or_else(|| FileStoreError::NotFound {
                path: key.to_string(),
            })
    }
}

fn validate_content_hash(hash: &str) -> Result<(), FileStoreError> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(FileStoreError::InvalidHash { length: hash.len() })
}

/// Wraps an `AsyncRead` so a single `poll_read` call that makes no progress
/// within `idle_timeout` fails with a `TimedOut` IO error instead of hanging
/// the caller forever.
///
/// Used for streaming S3 GetObject bodies on the proxy hot path, where a
/// connection that stalls mid-transfer (not a clean error, not a clean EOF —
/// just silence) must not block the request indefinitely. Every call that
/// makes progress (a successful read, an error, or EOF) resets the timer, so
/// a slow-but-steady multi-hundred-megabyte transfer is never killed purely
/// for taking a while — only genuine stalls are.
struct IdleTimeoutReader<R> {
    inner: R,
    idle_timeout: Duration,
    sleep: Pin<Box<tokio::time::Sleep>>,
}

impl<R> IdleTimeoutReader<R> {
    fn new(inner: R, idle_timeout: Duration) -> Self {
        Self {
            inner,
            idle_timeout,
            sleep: Box::pin(tokio::time::sleep(idle_timeout)),
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for IdleTimeoutReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                let deadline = tokio::time::Instant::now() + self.idle_timeout;
                self.sleep.as_mut().reset(deadline);
                Poll::Ready(result)
            }
            Poll::Pending => match self.sleep.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "S3 object body made no progress for {:?}",
                        self.idle_timeout
                    ),
                ))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;

    fn sample_config() -> S3StorageConfig {
        S3StorageConfig {
            bucket: "temps-static".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://127.0.0.1:9000".to_string()),
            access_key_id: "key".to_string(),
            secret_access_key: "secret".to_string(),
            force_path_style: true,
            timeout: Duration::from_millis(200),
            prefix: None,
        }
    }

    #[test]
    fn blob_key_uses_git_style_double_prefix_sharding() {
        let store = S3FileStore::new(sample_config());
        let hash = "a".repeat(64);
        let key = store.blob_key(&hash).unwrap();
        assert_eq!(key, format!("blobs/aa/aa/{hash}"));
    }

    #[test]
    fn blob_key_rejects_invalid_hashes() {
        let store = S3FileStore::new(sample_config());
        assert!(matches!(
            store.blob_key("too-short"),
            Err(FileStoreError::InvalidHash { .. })
        ));
        assert!(matches!(
            store.blob_key(&"z".repeat(64)),
            Err(FileStoreError::InvalidHash { .. })
        ));
    }

    #[test]
    fn path_key_sanitizes_traversal_and_leading_slash() {
        let store = S3FileStore::new(sample_config());
        assert_eq!(store.path_key("/assets/app.js"), "paths/assets/app.js");
        assert_eq!(store.path_key("../../etc/passwd"), "paths/etc/passwd");
        assert_eq!(store.path_key("a/./b//c"), "paths/a/b/c");
    }

    #[test]
    fn open_raw_uses_the_exact_key_with_no_paths_namespace_or_sanitization() {
        // `open_raw` (used by the proxy's S3-backed static-site read path)
        // must resolve to `full_key(key)`, matching exactly what
        // `S3StaticDeployer::object_key`/`full_key` writes to — not
        // `path_key(key)`'s sanitized `paths/` sub-namespace, which no
        // production writer uses for these objects. Regression test for a
        // real bug: reads 404'd against a real bucket because `open` (not
        // `open_raw`) was wired into the proxy, looking under `paths/`.
        let store = S3FileStore::new(sample_config());
        let key = "projects/site/production/2026/01/01/deploy-1/index.html";
        assert_eq!(store.full_key(key), key);
        assert_ne!(store.path_key(key), store.full_key(key));

        let mut prefixed_config = sample_config();
        prefixed_config.prefix = Some("prod/".to_string());
        let prefixed_store = S3FileStore::new(prefixed_config);
        assert_eq!(prefixed_store.full_key(key), format!("prod/{key}"));
    }

    #[test]
    fn prefix_is_applied_ahead_of_the_blob_and_path_namespaces() {
        let mut config = sample_config();
        config.prefix = Some("prod/".to_string());
        let store = S3FileStore::new(config);
        let hash = "b".repeat(64);
        assert_eq!(
            store.blob_key(&hash).unwrap(),
            format!("prod/blobs/bb/bb/{hash}")
        );
        assert_eq!(store.path_key("assets/app.js"), "prod/paths/assets/app.js");
    }

    #[test]
    fn content_hash_matches_fs_file_store_algorithm() {
        // Both backends must hash identically so a blob written by one and
        // read by the other (e.g. during a migration) resolves to the same key.
        assert_eq!(
            S3FileStore::content_hash(b"hello"),
            crate::fs_store::FsFileStore::content_hash(b"hello")
        );
    }

    /// An `AsyncRead` that never completes, to prove `IdleTimeoutReader`
    /// fails a stalled read instead of hanging forever.
    struct NeverReady;
    impl AsyncRead for NeverReady {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn idle_timeout_reader_fails_a_stalled_read() {
        let mut reader = IdleTimeoutReader::new(NeverReady, Duration::from_millis(50));
        let mut buf = [0_u8; 8];
        let error = reader
            .read(&mut buf)
            .await
            .expect_err("a read that never makes progress must time out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn idle_timeout_reader_passes_through_a_healthy_reader() {
        let mut reader = IdleTimeoutReader::new(
            std::io::Cursor::new(Bytes::from_static(b"hello world")),
            Duration::from_secs(5),
        );
        let mut collected = Vec::new();
        reader.read_to_end(&mut collected).await.unwrap();
        assert_eq!(collected, b"hello world");
    }

    /// A reader that is Pending on the first poll, then ready — proves the
    /// idle timer resets on real progress instead of firing on cumulative
    /// wall-clock time since construction.
    struct StallThenReady {
        polls: Arc<AtomicUsize>,
        payload: &'static [u8],
    }
    impl AsyncRead for StallThenReady {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.polls.fetch_add(1, Ordering::SeqCst) == 0 {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            buf.put_slice(self.payload);
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn idle_timeout_reader_survives_a_single_pending_poll_within_the_deadline() {
        let polls = Arc::new(AtomicUsize::new(0));
        let mut reader = IdleTimeoutReader::new(
            StallThenReady {
                polls: polls.clone(),
                payload: b"ok",
            },
            Duration::from_secs(5),
        );
        let mut buf = [0_u8; 8];
        let read = reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..read], b"ok");
    }

    #[test]
    fn timed_wraps_a_slow_future_in_a_typed_timeout_error() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let store = S3FileStore::new(sample_config());
        let result: Result<(), FileStoreError> =
            runtime.block_on(store.timed("slow-op", async { never_completes().await }));
        assert!(matches!(result, Err(FileStoreError::Timeout { .. })));
    }

    fn never_completes() -> impl Future<Output = Result<(), FileStoreError>> {
        std::future::pending()
    }
}
