// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors of the funnel definition methods (create, read, list, update,
//! delete).

use sea_orm::DbErr;

/// A funnel definition operation that failed.
#[derive(Debug, thiserror::Error)]
pub enum FunnelError {
    /// No active funnel with this id belongs to the project.
    #[error("Funnel {funnel_id} not found in project {project_id}")]
    NotFound { project_id: i32, funnel_id: i32 },

    /// The funnels or funnel steps tables could not be read or written.
    #[error("Funnel query failed: {0}")]
    Database(#[from] DbErr),
}
