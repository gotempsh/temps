// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Content-addressable file store for temps.sh
//!
//! Blobs stored by SHA-256 content hash with git-style sharding:
//!   blobs/{hash[0..2]}/{hash[2..4]}/{hash}
//!
//! URL path → content hash mapping stored in a database table
//! (`static_asset_cache`), queried by the proxy with in-memory caching.

pub mod fs_store;

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use tokio::io::AsyncRead;

#[derive(Error, Debug)]
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

    /// Check if a path key exists (for non-CAS use cases).
    async fn exists(&self, path: &str) -> Result<bool, FileStoreError>;
}
