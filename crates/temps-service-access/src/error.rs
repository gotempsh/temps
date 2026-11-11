// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AccessError {
    #[error("Cannot load Tokenless service catalog at SDK revision {revision}: {source}")]
    Catalog {
        revision: &'static str,
        #[source]
        source: tokenless_core::CoreError,
    },
    #[error("Service {service_id} belongs to vault {vault_id}; approve a bundle session through a covering vault link")]
    VaultSessionRequired {
        service_id: String,
        vault_id: String,
    },
    #[error("Cannot {operation} access resource {resource_id}: database operation failed")]
    ResourceDatabase {
        operation: &'static str,
        resource_id: String,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Vault {vault_id} has no active link covering sandbox {sandbox_id}")]
    UncoveredSandbox { vault_id: String, sandbox_id: i32 },
    #[error("Cannot link vault {vault_id}: GitHub service {service_id} requires source project {source_project_id:?}, but consumer project is {target_project_id:?}")]
    GitLinkConflict {
        vault_id: String,
        service_id: String,
        source_project_id: Option<i32>,
        target_project_id: Option<i32>,
    },
    #[error("Cannot queue service access for sandbox {sandbox_id} in project {project}", project = project_id.map_or_else(|| "none".to_owned(), |id| id.to_string()))]
    Delivery {
        project_id: Option<i32>,
        sandbox_id: String,
        #[source]
        source: Box<crate::delivery::DeliveryError>,
    },
    #[error("Service access {kind} {id} was not found in project {project}", project = project_id.map_or_else(|| "none".to_owned(), |id| id.to_string()))]
    NotFound {
        kind: &'static str,
        id: String,
        project_id: Option<i32>,
    },
    #[error("Invalid service access {field}: {reason}")]
    Validation {
        field: &'static str,
        reason: &'static str,
    },
    #[error("Service access {id} changed; reload before saving version {version}")]
    Conflict { id: String, version: i32 },
    #[error(
        "Cannot {operation} service access in project {project}: database operation failed", project = project_id.map_or_else(|| "none".to_owned(), |id| id.to_string())
    )]
    Database {
        operation: &'static str,
        project_id: Option<i32>,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Cannot encrypt credentials for service access in project {project}", project = project_id.map_or_else(|| "none".to_owned(), |id| id.to_string()))]
    Encryption {
        project_id: Option<i32>,
        #[source]
        source: temps_core::EncryptionError,
    },
    #[error("Cannot decode policy for service access {id} in project {project}", project = project_id.map_or_else(|| "none".to_owned(), |id| id.to_string()))]
    StoredPolicy {
        id: String,
        project_id: Option<i32>,
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
    pub fn resource_database(
        operation: &'static str,
        resource_id: &str,
        source: sea_orm::DbErr,
    ) -> Self {
        Self::ResourceDatabase {
            operation,
            resource_id: resource_id.into(),
            source,
        }
    }

    pub fn database(
        operation: &'static str,
        project_id: impl Into<Option<i32>>,
        source: sea_orm::DbErr,
    ) -> Self {
        Self::Database {
            operation,
            project_id: project_id.into(),
            source,
        }
    }
}
