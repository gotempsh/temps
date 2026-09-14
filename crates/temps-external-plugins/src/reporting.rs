// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Explicitly opted-in, per-plugin installation reporting.

use std::path::Path;
use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseBackend, EntityTrait, Statement};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::catalog::REGISTRY_URL;
use temps_entities::settings;

const REPORT_URL: &str = "https://registry.temps.sh/api/installations";
const SECRET_FILE: &str = ".installation-reporting-secret";

#[derive(Debug, Error)]
pub enum ReportingError {
    #[error("Cannot access installation reporting file '{path}': {reason}")]
    File { path: String, reason: String },
    #[error("Installation reporting secret in '{path}' is invalid")]
    InvalidSecret { path: String },
    #[error("Installation reporting request for plugin '{plugin}' failed: {reason}")]
    Request { plugin: String, reason: String },
    #[error(
        "Cannot {operation} plugin installation reporting consent in settings row 1: {source}"
    )]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

#[derive(Serialize)]
struct InstallationReport<'a> {
    plugin: &'a str,
    installation_id: String,
}

pub async fn consent(db: &sea_orm::DatabaseConnection) -> Result<bool, ReportingError> {
    let row = settings::Entity::find_by_id(1)
        .one(db)
        .await
        .map_err(|source| ReportingError::Database {
            operation: "read",
            source,
        })?;
    Ok(row.is_some_and(|row| {
        temps_core::AppSettings::from_json(row.data).plugin_installation_reporting_enabled
    }))
}

pub async fn set_consent(
    db: &sea_orm::DatabaseConnection,
    enabled: bool,
) -> Result<(), ReportingError> {
    // One atomic statement updates only our key, preserving other settings
    // even when another process writes them concurrently.
    let statement = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO settings (id, data, created_at, updated_at) \
         VALUES (1, jsonb_build_object('plugin_installation_reporting_enabled', $1::boolean), NOW(), NOW()) \
         ON CONFLICT (id) DO UPDATE SET \
         data = jsonb_set(COALESCE(settings.data::jsonb, '{}'::jsonb), '{plugin_installation_reporting_enabled}', to_jsonb($1::boolean), true), \
         updated_at = NOW()",
        [enabled.into()],
    );
    db.execute(statement)
        .await
        .map_err(|source| ReportingError::Database {
            operation: "update",
            source,
        })?;
    Ok(())
}

async fn write_private(path: &Path, data: &[u8]) -> Result<(), ReportingError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    use tokio::io::AsyncWriteExt;
    let mut file = options
        .open(path)
        .await
        .map_err(|error| ReportingError::File {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    file.write_all(data)
        .await
        .map_err(|error| ReportingError::File {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    file.sync_all().await.map_err(|error| ReportingError::File {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

async fn instance_secret(plugins_dir: &Path) -> Result<[u8; 16], ReportingError> {
    let path = plugins_dir.join(SECRET_FILE);
    let generated = *uuid::Uuid::new_v4().as_bytes();
    match write_private(&path, hex::encode(generated).as_bytes()).await {
        Ok(()) => {}
        Err(ReportingError::File { .. }) if path.exists() => {}
        Err(error) => return Err(error),
    }
    let raw = tokio::fs::read_to_string(&path)
        .await
        .map_err(|error| ReportingError::File {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    let decoded = hex::decode(raw).map_err(|_| ReportingError::InvalidSecret {
        path: path.display().to_string(),
    })?;
    decoded
        .try_into()
        .map_err(|_| ReportingError::InvalidSecret {
            path: path.display().to_string(),
        })
}

fn installation_id(secret: &[u8; 16], plugin: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"temps-plugin-installation-v1\0");
    digest.update(secret);
    digest.update(b"\0");
    digest.update(plugin.as_bytes());
    hex::encode(digest.finalize())
}

pub async fn report_if_enabled(
    db: &sea_orm::DatabaseConnection,
    plugins_dir: &Path,
    registry_url: &str,
    plugin: &str,
) -> Result<(), ReportingError> {
    if registry_url != REGISTRY_URL || !consent(db).await? {
        return Ok(());
    }
    let secret = instance_secret(plugins_dir).await?;
    let report = InstallationReport {
        plugin,
        installation_id: installation_id(&secret, plugin),
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| ReportingError::Request {
            plugin: plugin.to_string(),
            reason: error.to_string(),
        })?;
    for attempt in 0..2 {
        let result = client.post(REPORT_URL).json(&report).send().await;
        match result {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) if !response.status().is_server_error() || attempt == 1 => {
                return Err(ReportingError::Request {
                    plugin: plugin.to_string(),
                    reason: format!("HTTP {}", response.status()),
                });
            }
            Err(error) if attempt == 1 => {
                return Err(ReportingError::Request {
                    plugin: plugin.to_string(),
                    reason: error.to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_settings_row_defaults_reporting_off() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<settings::Model>::new()])
            .into_connection();
        assert!(!consent(&db).await.expect("read consent"));
    }

    #[tokio::test]
    async fn disabled_by_default_and_stable_per_plugin_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let secret = instance_secret(directory.path()).await.expect("secret");
        assert_eq!(
            secret,
            instance_secret(directory.path())
                .await
                .expect("stable secret")
        );
        let one = installation_id(&secret, "example");
        assert_eq!(one.len(), 64);
        assert_eq!(one, installation_id(&secret, "example"));
        assert_ne!(one, installation_id(&secret, "another"));
    }

    #[tokio::test]
    async fn custom_registry_never_creates_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        report_if_enabled(
            &db,
            directory.path(),
            "https://example.test/api/plugins",
            "example",
        )
        .await
        .expect("custom registries do not report");
        assert!(!directory.path().join(SECRET_FILE).exists());
    }

    #[tokio::test]
    async fn corrupt_secret_is_rejected_without_replacement() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join(SECRET_FILE);
        tokio::fs::write(&path, b"not a secret")
            .await
            .expect("write malformed secret");
        let error = instance_secret(directory.path())
            .await
            .expect_err("invalid secret must be rejected");
        assert!(matches!(error, ReportingError::InvalidSecret { .. }));
        assert_eq!(
            tokio::fs::read(&path).await.expect("secret unchanged"),
            b"not a secret"
        );
    }
}
