//! Minimal typed client for the Dokploy REST API.
//!
//! Built per-request from [`temps_import_types::ImportCredentials`] — the
//! base URL goes through the same SSRF guard as every other importer and the
//! API key (sent as `x-api-key`) is never stored.

use crate::error::DokployImportError;
use crate::model::{DokployApplication, DokployDatabase, DokployProject};
use serde::de::DeserializeOwned;
use std::time::Duration;
use temps_core::url_validation::validate_external_url;
use temps_import_types::ImportCredentials;

/// Database kinds Dokploy models as separate routers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DokployDbKind {
    Postgres,
    Mysql,
    Mariadb,
    Mongo,
    Redis,
}

impl DokployDbKind {
    pub const ALL: [DokployDbKind; 5] = [
        DokployDbKind::Postgres,
        DokployDbKind::Mysql,
        DokployDbKind::Mariadb,
        DokployDbKind::Mongo,
        DokployDbKind::Redis,
    ];

    /// Router name in API paths (`postgres.one`) and id query parameter
    pub fn router(&self) -> &'static str {
        match self {
            DokployDbKind::Postgres => "postgres",
            DokployDbKind::Mysql => "mysql",
            DokployDbKind::Mariadb => "mariadb",
            DokployDbKind::Mongo => "mongo",
            DokployDbKind::Redis => "redis",
        }
    }

    pub fn id_param(&self) -> &'static str {
        match self {
            DokployDbKind::Postgres => "postgresId",
            DokployDbKind::Mysql => "mysqlId",
            DokployDbKind::Mariadb => "mariadbId",
            DokployDbKind::Mongo => "mongoId",
            DokployDbKind::Redis => "redisId",
        }
    }
}

/// HTTP client bound to one Dokploy instance
pub struct DokployClient {
    http: reqwest::Client,
    base_url: url::Url,
    api_key: String,
}

impl DokployClient {
    /// Build a client from per-request credentials.
    pub fn from_credentials(credentials: &ImportCredentials) -> Result<Self, DokployImportError> {
        let base = credentials
            .base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .ok_or(DokployImportError::MissingBaseUrl)?;
        let api_key = credentials
            .token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or(DokployImportError::MissingToken)?
            .to_string();

        let base_url =
            validate_external_url(base).map_err(|e| DokployImportError::InvalidBaseUrl {
                url: base.to_string(),
                reason: e.to_string(),
            })?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| DokployImportError::Http {
                operation: "build http client".to_string(),
                reason: e.to_string(),
            })?;

        Ok(Self {
            http,
            base_url,
            api_key,
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, DokployImportError> {
        let operation = format!("GET {}", path);
        let url = self
            .base_url
            .join(path)
            .map_err(|e| DokployImportError::InvalidBaseUrl {
                url: format!("{}{}", self.base_url, path),
                reason: e.to_string(),
            })?;

        let response = self
            .http
            .get(url)
            .header("x-api-key", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| DokployImportError::Http {
                operation: operation.clone(),
                reason: e.to_string(),
            })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| DokployImportError::Http {
                operation: operation.clone(),
                reason: format!("failed to read response body: {}", e),
            })?;

        match status.as_u16() {
            200 => {
                serde_json::from_str(&body).map_err(|e| DokployImportError::UnexpectedResponse {
                    operation,
                    reason: e.to_string(),
                })
            }
            401 => Err(DokployImportError::Unauthorized { operation }),
            _ => Err(DokployImportError::Api {
                operation,
                status: status.as_u16(),
                body: body.chars().take(500).collect(),
            }),
        }
    }

    /// The instance host (for constructing externally-reachable DB URLs)
    pub fn host(&self) -> Option<String> {
        self.base_url.host_str().map(|h| h.to_string())
    }

    pub async fn projects(&self) -> Result<Vec<DokployProject>, DokployImportError> {
        self.get_json("/api/project.all").await
    }

    pub async fn application(
        &self,
        application_id: &str,
    ) -> Result<DokployApplication, DokployImportError> {
        self.get_json(&format!(
            "/api/application.one?applicationId={}",
            application_id
        ))
        .await
    }

    pub async fn database(
        &self,
        kind: DokployDbKind,
        id: &str,
    ) -> Result<DokployDatabase, DokployImportError> {
        self.get_json(&format!(
            "/api/{}.one?{}={}",
            kind.router(),
            kind.id_param(),
            id
        ))
        .await
    }
}
