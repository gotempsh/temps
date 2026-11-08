// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AccessError {
    #[error("Service access {kind} {id} was not found in project {project_id}")]
    NotFound {
        kind: &'static str,
        id: String,
        project_id: i32,
    },
    #[error("Invalid service access {field}: {reason}")]
    Validation {
        field: &'static str,
        reason: &'static str,
    },
    #[error("Service access {id} changed; reload before saving version {version}")]
    Conflict { id: String, version: i32 },
    #[error(
        "Cannot {operation} service access in project {project_id}: database operation failed"
    )]
    Database {
        operation: &'static str,
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Cannot encrypt credentials for service access in project {project_id}")]
    Encryption {
        project_id: i32,
        #[source]
        source: temps_core::EncryptionError,
    },
    #[error("Cannot decode policy for service access {id} in project {project_id}")]
    StoredPolicy {
        id: String,
        project_id: i32,
        #[source]
        source: serde_json::Error,
    },
    #[error("Cannot {operation} service access grant {grant_id}: the Tokenless adapter is not installed")]
    BrokerUnavailable {
        operation: &'static str,
        grant_id: String,
    },
}

impl AccessError {
    pub fn database(operation: &'static str, project_id: i32, source: sea_orm::DbErr) -> Self {
        Self::Database {
            operation,
            project_id,
            source,
        }
    }
}
