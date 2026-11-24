// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

/// Failures of the coding-agent release check and its settings.
#[derive(Debug, Error)]
pub enum CodingAgentError {
    #[error("could not reach the npm registry for {package}: {reason}")]
    Registry { package: String, reason: String },

    #[error("the npm registry did not answer for {package} within {seconds}s")]
    RegistryTimeout { package: String, seconds: u64 },

    #[error("the npm registry answered HTTP {status} for {package}")]
    RegistryStatus { package: String, status: u16 },

    #[error("the npm registry answer for {package} is larger than {limit_bytes} bytes")]
    RegistryTooLarge { package: String, limit_bytes: usize },

    #[error("the npm registry answer for {package} is not a release document: {reason}")]
    RegistryParse { package: String, reason: String },

    #[error("coding-agent settings database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}
