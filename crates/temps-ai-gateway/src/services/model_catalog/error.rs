// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors of the runtime model catalog (ADR 112).

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ModelCatalogError {
    #[error("Model catalog source URL '{url}' is not allowed: {reason}")]
    InvalidSourceUrl { url: String, reason: String },

    #[error("Model catalog setting '{field}' is invalid: {reason}")]
    InvalidSettings { field: &'static str, reason: String },

    #[error("Could not fetch the model catalog from '{url}': {reason}")]
    Fetch { url: String, reason: String },

    #[error("Fetching the model catalog from '{url}' timed out after {seconds}s")]
    Timeout { url: String, seconds: u64 },

    #[error("The model catalog at '{url}' answered HTTP {status}")]
    HttpStatus { url: String, status: u16 },

    #[error("The model catalog at '{url}' is larger than the {limit_bytes}-byte limit")]
    TooLarge { url: String, limit_bytes: usize },

    #[error("The model catalog at '{url}' is not a valid catalog: {reason}")]
    Parse { url: String, reason: String },

    #[error(
        "The model catalog at '{url}' lists no valid model for a supported provider \
         ({skipped} entries were rejected)"
    )]
    Empty { url: String, skipped: usize },

    #[error("The model catalog at '{url}' lists more than {limit} models")]
    TooManyEntries { url: String, limit: usize },

    #[error("A model catalog refresh is already running; wait for it to finish")]
    RefreshInProgress,

    #[error("Model catalog database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl ModelCatalogError {
    /// Whether the failure is the source's (network, HTTP, content) rather
    /// than this instance's, so the attempt is recorded instead of surfaced
    /// as an internal error.
    pub fn is_source_failure(&self) -> bool {
        matches!(
            self,
            Self::Fetch { .. }
                | Self::Timeout { .. }
                | Self::HttpStatus { .. }
                | Self::TooLarge { .. }
                | Self::Parse { .. }
                | Self::Empty { .. }
                | Self::TooManyEntries { .. }
        )
    }
}
