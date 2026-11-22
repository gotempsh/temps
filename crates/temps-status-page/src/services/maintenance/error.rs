// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors of status page maintenance windows (ADR 108 D2).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MaintenanceError {
    #[error("database error in status page maintenance windows: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("status page {status_page_id} not found")]
    StatusPageNotFound { status_page_id: i32 },
    #[error("maintenance window {maintenance_id} not found on status page {status_page_id}")]
    NotFound {
        status_page_id: i32,
        maintenance_id: i32,
    },
    #[error("invalid maintenance window on status page {status_page_id}: {reason}")]
    Validation { status_page_id: i32, reason: String },
    #[error("maintenance window {maintenance_id} cannot be changed: {reason}")]
    Conflict { maintenance_id: i32, reason: String },
    #[error(
        "status page {status_page_id} already has {limit} maintenance windows in progress or \
         scheduled; cancel one before scheduling another"
    )]
    TooManyScheduled { status_page_id: i32, limit: u64 },
}
