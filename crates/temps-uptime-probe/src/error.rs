// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/// A target or check setting that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckConfigError {
    #[error("invalid {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
}

/// A request to a user-supplied URL that could not be prepared.
#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("URL '{url}' cannot be requested: {reason}")]
    InvalidUrl { url: String, reason: String },
    #[error("could not build the HTTP client for '{url}': {source}")]
    ClientBuild {
        url: String,
        #[source]
        source: reqwest::Error,
    },
}
