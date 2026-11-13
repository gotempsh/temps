// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`PayloadBlobStore`] over the project's blob bucket (ADR-103 §5).
//!
//! The durable workflows engine offloads payloads above its inline cap here.
//! It stores ciphertext only, under its own `.temps/` prefix, so this adapter
//! never sees plaintext. Reads are bounded: an object larger than the
//! caller's limit is refused from its reported size and again while
//! streaming, so a payload never grows past what the caller can hold.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt};
use temps_core::{PayloadBlobError, PayloadBlobStore};

use super::{BlobService, ListOptions, PutOptions};
use crate::error::BlobError;

/// Keys listed per page when deleting a prefix.
const DELETE_PAGE: i32 = 500;

type ByteStream = std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// The blob operations the adapter needs, so it can be tested without S3.
#[async_trait]
trait Backend: Send + Sync {
    async fn put(&self, project_id: i32, key: &str, body: Bytes) -> Result<(), BlobError>;
    async fn open(&self, project_id: i32, key: &str) -> Result<(ByteStream, i64), BlobError>;
    async fn list(
        &self,
        project_id: i32,
        prefix: &str,
        cursor: Option<String>,
    ) -> Result<(Vec<String>, Option<String>), BlobError>;
    async fn delete(&self, project_id: i32, keys: Vec<String>) -> Result<i64, BlobError>;
}

#[async_trait]
impl Backend for BlobService {
    async fn put(&self, project_id: i32, key: &str, body: Bytes) -> Result<(), BlobError> {
        let options = PutOptions {
            content_type: Some("application/octet-stream".to_string()),
            add_random_suffix: false,
        };
        BlobService::put(self, project_id, key, body, options)
            .await
            .map(|_| ())
    }

    async fn open(&self, project_id: i32, key: &str) -> Result<(ByteStream, i64), BlobError> {
        let (stream, _content_type, size) = self.download(project_id, key).await?;
        Ok((Box::pin(stream), size))
    }

    async fn list(
        &self,
        project_id: i32,
        prefix: &str,
        cursor: Option<String>,
    ) -> Result<(Vec<String>, Option<String>), BlobError> {
        let page = BlobService::list(
            self,
            project_id,
            ListOptions {
                limit: Some(DELETE_PAGE),
                prefix: Some(prefix.to_string()),
                cursor,
            },
        )
        .await?;
        let next = if page.has_more { page.cursor } else { None };
        Ok((
            page.blobs.into_iter().map(|blob| blob.pathname).collect(),
            next,
        ))
    }

    async fn delete(&self, project_id: i32, keys: Vec<String>) -> Result<i64, BlobError> {
        self.del(project_id, keys).await
    }
}

/// The workflow payload store over [`BlobService`].
pub struct BlobPayloadStore {
    backend: Arc<dyn Backend>,
}

impl BlobPayloadStore {
    pub fn new(blob_service: Arc<BlobService>) -> Self {
        Self {
            backend: blob_service,
        }
    }

    fn map_error(
        project_id: i32,
        operation: &'static str,
        key: &str,
        error: BlobError,
    ) -> PayloadBlobError {
        match error {
            BlobError::NotFound(_) => PayloadBlobError::NotFound {
                project_id,
                key: key.to_string(),
            },
            BlobError::ConnectionFailed(reason) | BlobError::Docker(reason) => {
                PayloadBlobError::Unavailable { project_id, reason }
            }
            BlobError::ContainerNotRunning | BlobError::ServiceNotConfigured => {
                PayloadBlobError::Unavailable {
                    project_id,
                    reason: error.to_string(),
                }
            }
            BlobError::S3(_)
            | BlobError::InvalidPath(_)
            | BlobError::UploadFailed(_)
            | BlobError::ProjectNotFound(_)
            | BlobError::Internal(_) => PayloadBlobError::Operation {
                project_id,
                operation,
                key: key.to_string(),
                reason: error.to_string(),
            },
        }
    }
}

#[async_trait]
impl PayloadBlobStore for BlobPayloadStore {
    async fn put(&self, project_id: i32, key: &str, body: Bytes) -> Result<(), PayloadBlobError> {
        self.backend
            .put(project_id, key, body)
            .await
            .map_err(|error| Self::map_error(project_id, "put", key, error))
    }

    async fn get(
        &self,
        project_id: i32,
        key: &str,
        max_bytes: usize,
    ) -> Result<Bytes, PayloadBlobError> {
        let too_large = |size: String| PayloadBlobError::Operation {
            project_id,
            operation: "get",
            key: key.to_string(),
            reason: format!("the object is {size} bytes, over the {max_bytes} byte limit"),
        };
        let (mut stream, size) = self
            .backend
            .open(project_id, key)
            .await
            .map_err(|error| Self::map_error(project_id, "get", key, error))?;
        if usize::try_from(size).is_ok_and(|size| size > max_bytes) {
            return Err(too_large(size.to_string()));
        }
        let mut body = BytesMut::with_capacity(usize::try_from(size).unwrap_or(0).min(max_bytes));
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| PayloadBlobError::Operation {
                project_id,
                operation: "get",
                key: key.to_string(),
                reason: format!("reading the object failed: {error}"),
            })?;
            if body.len() + chunk.len() > max_bytes {
                return Err(too_large(format!("more than {max_bytes}")));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    }

    async fn delete_prefix(&self, project_id: i32, prefix: &str) -> Result<u64, PayloadBlobError> {
        let mut deleted = 0u64;
        let mut cursor = None;
        loop {
            let (keys, next) = self
                .backend
                .list(project_id, prefix, cursor)
                .await
                .map_err(|error| Self::map_error(project_id, "list", prefix, error))?;
            if !keys.is_empty() {
                let count = self
                    .backend
                    .delete(project_id, keys)
                    .await
                    .map_err(|error| Self::map_error(project_id, "delete", prefix, error))?;
                deleted += u64::try_from(count).unwrap_or(0);
            }
            match next {
                Some(token) => cursor = Some(token),
                None => return Ok(deleted),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Objects keyed by (project, key); lists in pages of `page`.
    struct MemoryBackend {
        objects: Mutex<BTreeMap<(i32, String), Bytes>>,
        page: usize,
        /// Lie about the size to exercise the streaming bound.
        report_size: Option<i64>,
        unavailable: bool,
    }

    impl MemoryBackend {
        fn new(page: usize) -> Self {
            Self {
                objects: Mutex::new(BTreeMap::new()),
                page,
                report_size: None,
                unavailable: false,
            }
        }
    }

    #[async_trait]
    impl Backend for MemoryBackend {
        async fn put(&self, project_id: i32, key: &str, body: Bytes) -> Result<(), BlobError> {
            if self.unavailable {
                return Err(BlobError::ConnectionFailed("connection refused".into()));
            }
            self.objects
                .lock()
                .unwrap()
                .insert((project_id, key.to_string()), body);
            Ok(())
        }

        async fn open(&self, project_id: i32, key: &str) -> Result<(ByteStream, i64), BlobError> {
            let body = self
                .objects
                .lock()
                .unwrap()
                .get(&(project_id, key.to_string()))
                .cloned()
                .ok_or_else(|| BlobError::NotFound(key.to_string()))?;
            let size = self.report_size.unwrap_or(body.len() as i64);
            let chunks: Vec<Result<Bytes, std::io::Error>> = body
                .chunks(4)
                .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                .collect();
            Ok((Box::pin(futures::stream::iter(chunks)), size))
        }

        async fn list(
            &self,
            project_id: i32,
            prefix: &str,
            cursor: Option<String>,
        ) -> Result<(Vec<String>, Option<String>), BlobError> {
            let objects = self.objects.lock().unwrap();
            let keys: Vec<String> = objects
                .keys()
                .filter(|(project, key)| *project == project_id && key.starts_with(prefix))
                .filter(|(_, key)| cursor.as_ref().is_none_or(|after| key > after))
                .take(self.page + 1)
                .map(|(_, key)| key.clone())
                .collect();
            if keys.len() > self.page {
                let page = keys[..self.page].to_vec();
                let next = page.last().cloned();
                Ok((page, next))
            } else {
                Ok((keys, None))
            }
        }

        async fn delete(&self, project_id: i32, keys: Vec<String>) -> Result<i64, BlobError> {
            let mut objects = self.objects.lock().unwrap();
            let removed = keys
                .into_iter()
                .filter(|key| objects.remove(&(project_id, key.clone())).is_some())
                .count();
            Ok(removed as i64)
        }
    }

    fn payload_store(backend: MemoryBackend) -> (BlobPayloadStore, Arc<MemoryBackend>) {
        let backend = Arc::new(backend);
        (
            BlobPayloadStore {
                backend: backend.clone(),
            },
            backend,
        )
    }

    #[tokio::test]
    async fn put_then_get_round_trips_within_the_limit() {
        let (store, _) = payload_store(MemoryBackend::new(10));
        let body = Bytes::from_static(b"ciphertext-bytes");
        store
            .put(3, ".temps/workflows/1/r/input-1", body.clone())
            .await
            .unwrap();
        let read = store
            .get(3, ".temps/workflows/1/r/input-1", 1024)
            .await
            .unwrap();
        assert_eq!(read, body);
    }

    #[tokio::test]
    async fn objects_are_scoped_to_their_project() {
        let (store, _) = payload_store(MemoryBackend::new(10));
        store.put(3, "k", Bytes::from_static(b"x")).await.unwrap();
        let error = store.get(4, "k", 1024).await.unwrap_err();
        assert!(matches!(
            error,
            PayloadBlobError::NotFound { project_id: 4, ref key } if key == "k"
        ));
    }

    #[tokio::test]
    async fn an_object_over_the_limit_is_refused_by_size_and_while_streaming() {
        let (store, _) = payload_store(MemoryBackend::new(10));
        store
            .put(3, "big", Bytes::from(vec![7u8; 64]))
            .await
            .unwrap();
        let error = store.get(3, "big", 16).await.unwrap_err().to_string();
        assert!(
            error.contains("64 bytes, over the 16 byte limit"),
            "{error}"
        );

        // A backend that under-reports the size is still cut off.
        let mut lying = MemoryBackend::new(10);
        lying.report_size = Some(1);
        let (store, _) = payload_store(lying);
        store
            .put(3, "big", Bytes::from(vec![7u8; 64]))
            .await
            .unwrap();
        let error = store.get(3, "big", 16).await.unwrap_err().to_string();
        assert!(error.contains("more than 16"), "{error}");
    }

    #[tokio::test]
    async fn delete_prefix_walks_every_page_and_spares_other_keys() {
        let (store, backend) = payload_store(MemoryBackend::new(2));
        for index in 0..5 {
            store
                .put(
                    3,
                    &format!(".temps/workflows/1/run-a/f-{index}"),
                    Bytes::from_static(b"x"),
                )
                .await
                .unwrap();
        }
        store
            .put(3, ".temps/workflows/1/run-b/f-0", Bytes::from_static(b"x"))
            .await
            .unwrap();
        store
            .put(9, ".temps/workflows/1/run-a/f-0", Bytes::from_static(b"x"))
            .await
            .unwrap();

        let deleted = store
            .delete_prefix(3, ".temps/workflows/1/run-a/")
            .await
            .unwrap();
        assert_eq!(deleted, 5);
        let left: Vec<_> = backend.objects.lock().unwrap().keys().cloned().collect();
        assert_eq!(
            left,
            vec![
                (3, ".temps/workflows/1/run-b/f-0".to_string()),
                (9, ".temps/workflows/1/run-a/f-0".to_string()),
            ]
        );
        assert_eq!(
            store
                .delete_prefix(3, ".temps/workflows/1/run-a/")
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_connection_failure_is_reported_as_unavailable() {
        let mut backend = MemoryBackend::new(10);
        backend.unavailable = true;
        let (store, _) = payload_store(backend);
        let error = store
            .put(3, "k", Bytes::from_static(b"x"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            PayloadBlobError::Unavailable { project_id: 3, ref reason } if reason == "connection refused"
        ));
    }
}
