// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::PathBuf;

use async_trait::async_trait;
use thiserror::Error;

/// Server-owned request to deploy an already-prepared ZIP through Temps'
/// uploaded-source (Drop) pipeline. The archive path is never supplied by an
/// HTTP client; callers create it in a private temporary directory first.
#[derive(Debug, Clone)]
pub struct SourceDropRequest {
    pub project_id: i32,
    /// Explicit target environment. When omitted the service selects the
    /// project's production environment, then its oldest active environment.
    pub environment_id: Option<i32>,
    pub archive_path: PathBuf,
    pub original_filename: String,
    /// Permit a legacy manual project to become an uploaded-source project,
    /// but only after the deployment workflow has been created successfully.
    pub promote_manual_source: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDropDeployment {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub slug: String,
    pub state: String,
}

#[derive(Debug, Error)]
pub enum SourceDropError {
    #[error("project {project_id} was not found")]
    ProjectNotFound { project_id: i32 },
    #[error("environment {environment_id} was not found in project {project_id}")]
    EnvironmentNotFound {
        environment_id: i32,
        project_id: i32,
    },
    #[error("project {project_id} has no active environments")]
    NoEnvironment { project_id: i32 },
    #[error("project {project_id} does not accept uploaded source archives: {reason}")]
    SourceNotAllowed { project_id: i32, reason: String },
    #[error("source archive is invalid: {reason}")]
    InvalidArchive { reason: String },
    #[error("source archive exceeds the {max_bytes} byte limit")]
    ArchiveTooLarge { max_bytes: u64 },
    #[error("source archive storage failed: {reason}")]
    Storage { reason: String },
    #[error("source deployment database operation failed: {reason}")]
    Database { reason: String },
    #[error("source deployment workflow creation failed: {reason}")]
    Workflow { reason: String },
    #[error("source deployment queueing failed: {reason}")]
    Queue { reason: String },
    #[error("source deployment failed ({original}); rollback also failed: {cleanup}")]
    Compensation {
        original: Box<SourceDropError>,
        cleanup: String,
    },
}

#[async_trait]
pub trait SourceDropDeployer: Send + Sync {
    async fn deploy_source_drop(
        &self,
        request: SourceDropRequest,
    ) -> Result<SourceDropDeployment, SourceDropError>;
}
