// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Content-addressable file store for temps.sh
//!
//! Blobs stored by SHA-256 content hash with git-style sharding:
//!   blobs/{hash[0..2]}/{hash[2..4]}/{hash}
//!
//! URL path → content hash mapping stored in a database table
//! (`static_asset_cache`), queried by the proxy with in-memory caching.

pub mod cache;
pub mod fs_store;
pub mod s3_client;
pub mod s3_config;
pub mod s3_store;

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use tokio::io::AsyncRead;

#[derive(Error, Debug, Clone)]
pub enum FileStoreError {
    #[error(
        "Invalid CAS content hash ({length} bytes): expected exactly 64 ASCII hexadecimal characters"
    )]
    InvalidHash { length: usize },

    #[error("File not found: {path}")]
    NotFound { path: String },

    #[error("IO error for file {path}: {reason}")]
    Io { path: String, reason: String },

    #[error("Backend error: {0}")]
    Backend(String),

    /// A remote-backend operation (S3-compatible PUT/GET/HEAD/DELETE, or an
    /// idle body-read stall) did not complete within its configured timeout.
    /// Never surfaced by the filesystem backend, which has no network I/O.
    #[error("Operation on '{path}' timed out after {timeout_secs}s")]
    Timeout { path: String, timeout_secs: u64 },
}

/// An opened CAS blob whose body can be consumed with bounded async reads.
pub struct OpenedBlob {
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    pub size_bytes: u64,
}

/// Content-addressable blob store.
///
/// Stores and retrieves blobs by content hash.
/// URL path → hash mapping is handled by the database (`static_asset_cache` table).
#[async_trait]
pub trait FileStore: Send + Sync {
    /// Store a blob and return its content hash. Skips write if already exists.
    async fn put_blob(&self, data: Bytes) -> Result<String, FileStoreError>;

    /// Retrieve a blob by its content hash.
    async fn get_blob(&self, hash: &str) -> Result<Bytes, FileStoreError>;

    /// Open a blob without buffering its body and return metadata from the
    /// opened file/stream itself.
    async fn open_blob(&self, hash: &str) -> Result<OpenedBlob, FileStoreError>;

    /// Check if a blob exists.
    async fn blob_exists(&self, hash: &str) -> Result<bool, FileStoreError>;

    /// Delete a blob by hash. Returns true if it existed.
    async fn delete_blob(&self, hash: &str) -> Result<bool, FileStoreError>;

    /// Store data by path key (for non-CAS use cases like edge caching).
    async fn put(&self, path: &str, data: Bytes) -> Result<u64, FileStoreError>;

    /// Retrieve data by path key (for non-CAS use cases like edge caching).
    async fn get(&self, path: &str) -> Result<Bytes, FileStoreError>;

    /// Open data by path key without buffering its body, returning metadata
    /// from the opened file/stream itself.
    ///
    /// Unlike [`FileStore::get`], this is safe to use for arbitrarily large
    /// path-keyed content (e.g. static-site build output up to hundreds of
    /// megabytes per file): callers stream the body in fixed-size chunks
    /// instead of buffering it whole. Mirrors [`FileStore::open_blob`] for
    /// the path-keyed namespace.
    async fn open(&self, path: &str) -> Result<OpenedBlob, FileStoreError>;

    /// Check if a path key exists (for non-CAS use cases).
    async fn exists(&self, path: &str) -> Result<bool, FileStoreError>;

    /// Open an object at exactly `key`, with none of [`FileStore::open`]'s
    /// `path`-keyed namespacing or defensive traversal-sanitization applied
    /// (only the backend's own storage root/prefix, e.g. an S3 bucket
    /// prefix, is still applied).
    ///
    /// This is a distinct namespace from [`FileStore::open`]/[`FileStore::get`]:
    /// those are for callers that hand this store an arbitrary path and want
    /// it sanitized into a private "cache"/"paths" sub-namespace they don't
    /// otherwise control the layout of. `open_raw` is for callers that
    /// already fully own and validate the key themselves and need it opened
    /// exactly as given, because something outside this trait wrote the
    /// object at that exact key.
    ///
    /// The motivating caller is the proxy's static-site read path: an S3
    /// static-site deployment is written key-for-key by
    /// `temps_deployer::s3_static_deployer::S3StaticDeployer` (its own
    /// `aws_sdk_s3::Client`, not this trait), so reading it back must use
    /// the identical key — not a `path/`-namespaced derivative of it.
    async fn open_raw(&self, key: &str) -> Result<OpenedBlob, FileStoreError>;

    /// Return the size in bytes of the object at exactly `key` (same
    /// namespace as [`FileStore::open_raw`]) without opening or reading its
    /// body.
    ///
    /// Exists so a `HEAD` request — which only needs `Content-Length` and an
    /// `ETag`, never the body — doesn't have to pay for a full
    /// open-and-buffer through a caching decorator: [`crate::cache::CachingFileStore`]
    /// answers a cached key from memory with no backend call at all, and an
    /// uncached key with a cheap metadata-only backend request (e.g. S3
    /// `HeadObject`) instead of a `GetObject` that a decorator would then
    /// buffer in full.
    async fn stat_raw(&self, key: &str) -> Result<u64, FileStoreError>;
}
