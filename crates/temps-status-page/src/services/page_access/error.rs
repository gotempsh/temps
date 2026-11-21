// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors of status page passwords (ADR 108 D2).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PageAccessError {
    #[error("database error in status page passwords: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("status page {status_page_id} not found")]
    StatusPageNotFound { status_page_id: i32 },
    #[error("invalid password for status page {status_page_id}: {reason}")]
    Validation { status_page_id: i32, reason: String },
    #[error(
        "cannot change the password of status page {status_page_id} now: too many status page passwords are being checked or changed at once; retry in a moment"
    )]
    Busy { status_page_id: i32 },
    #[error("could not hash the password of status page {status_page_id}: {reason}")]
    Hash { status_page_id: i32, reason: String },
}
