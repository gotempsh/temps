// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Object storage for payloads too large to keep inline in a row.
//!
//! ADR-106 §5 keeps workflow payloads up to an inline cap in Postgres and
//! offloads larger ones. `temps-blob` implements this over its project-scoped
//! bucket; callers store only ciphertext. The store is optional: a binary
//! without blob storage has none, and the caller then rejects an oversize
//! payload with a typed error instead.

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PayloadBlobError {
    #[error("Payload blob '{key}' of project {project_id} was not found")]
    NotFound { project_id: i32, key: String },
    #[error("Payload blob storage is unavailable for project {project_id}: {reason}")]
    Unavailable { project_id: i32, reason: String },
    #[error(
        "Payload blob operation '{operation}' on '{key}' of project {project_id} failed: {reason}"
    )]
    Operation {
        project_id: i32,
        operation: &'static str,
        key: String,
        reason: String,
    },
}

#[async_trait]
pub trait PayloadBlobStore: Send + Sync {
    /// Store `body` at `key` in the project's space, replacing any object.
    async fn put(&self, project_id: i32, key: &str, body: Bytes) -> Result<(), PayloadBlobError>;

    /// Read the object at `key`, refusing one larger than `max_bytes`.
    async fn get(
        &self,
        project_id: i32,
        key: &str,
        max_bytes: usize,
    ) -> Result<Bytes, PayloadBlobError>;

    /// Delete every object under `prefix`; returns how many were deleted.
    async fn delete_prefix(&self, project_id: i32, prefix: &str) -> Result<u64, PayloadBlobError>;
}
