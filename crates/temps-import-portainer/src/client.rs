//! Minimal typed client for the Portainer REST API.
//!
//! Portainer exchanges username+password for a JWT (`POST /api/auth`) which
//! is then sent as `Authorization: Bearer`. Container data comes from its
//! Docker proxy (`/api/endpoints/{id}/docker/...`).
//!
//! Portainer defaults to a self-signed certificate on :9443, so TLS
//! verification is relaxed for this client only — the alternative is that
//! nobody can import from a default install. The credentials still travel
//! over TLS, and the base URL goes through the standard SSRF guard.

use crate::error::PortainerImportError;
use crate::model::{
    DockerContainer, DockerContainerDetail, PortainerAuth, PortainerEndpoint, PortainerStack,
    PortainerStackFile,
};
use serde::de::DeserializeOwned;
use std::time::Duration;
use temps_core::url_validation::validate_external_url;
use temps_import_types::ImportCredentials;

/// HTTP client bound to one Portainer instance
pub struct PortainerClient {
    http: reqwest::Client,
    base_url: url::Url,
    username: String,
    password: String,
}

impl PortainerClient {
    pub fn from_credentials(credentials: &ImportCredentials) -> Result<Self, PortainerImportError> {
        let base = credentials
            .base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .ok_or(PortainerImportError::MissingBaseUrl)?;
        let password = credentials
            .token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or(PortainerImportError::MissingPassword)?
            .to_string();
        let username = credentials
            .extra
            .get("username")
            .filter(|u| !u.is_empty())
            .cloned()
            .unwrap_or_else(|| "admin".to_string());

        let base_url =
            validate_external_url(base).map_err(|e| PortainerImportError::InvalidBaseUrl {
                url: base.to_string(),
                reason: e.to_string(),
            })?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            // Portainer ships a self-signed cert on :9443 by default.
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| PortainerImportError::Http {
                operation: "build http client".to_string(),
                reason: e.to_string(),
            })?;

        Ok(Self {
            http,
            base_url,
            username,
            password,
        })
    }

    fn url(&self, path: &str) -> Result<url::Url, PortainerImportError> {
        self.base_url
            .join(path)
            .map_err(|e| PortainerImportError::InvalidBaseUrl {
                url: format!("{}{}", self.base_url, path),
                reason: e.to_string(),
            })
    }

    /// Exchange username+password for a session JWT.
    pub async fn login(&self) -> Result<String, PortainerImportError> {
        let operation = "POST /api/auth";
        let response = self
            .http
            .post(self.url("/api/auth")?)
            .json(&serde_json::json!({
                "username": self.username,
                "password": self.password,
            }))
            .send()
            .await
            .map_err(|e| PortainerImportError::Http {
                operation: operation.to_string(),
                reason: e.to_string(),
            })?;

        if !response.status().is_success() {
            return Err(PortainerImportError::LoginFailed {
                username: self.username.clone(),
            });
        }
        let auth: PortainerAuth =
            response
                .json()
                .await
                .map_err(|e| PortainerImportError::UnexpectedResponse {
                    operation: operation.to_string(),
                    reason: e.to_string(),
                })?;
        Ok(auth.jwt)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        jwt: &str,
        path: &str,
    ) -> Result<T, PortainerImportError> {
        let operation = format!("GET {}", path);
        let response = self
            .http
            .get(self.url(path)?)
            .bearer_auth(jwt)
            .send()
            .await
            .map_err(|e| PortainerImportError::Http {
                operation: operation.clone(),
                reason: e.to_string(),
            })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| PortainerImportError::Http {
                operation: operation.clone(),
                reason: format!("failed to read response body: {}", e),
            })?;

        if !status.is_success() {
            return Err(PortainerImportError::Api {
                operation,
                status: status.as_u16(),
                body: body.chars().take(500).collect(),
            });
        }
        serde_json::from_str(&body).map_err(|e| PortainerImportError::UnexpectedResponse {
            operation,
            reason: e.to_string(),
        })
    }

    /// Host of the Portainer instance — the machine the containers run on,
    /// used to build externally reachable database URLs.
    pub fn host(&self) -> Option<String> {
        self.base_url.host_str().map(|h| h.to_string())
    }

    pub async fn endpoints(
        &self,
        jwt: &str,
    ) -> Result<Vec<PortainerEndpoint>, PortainerImportError> {
        self.get_json(jwt, "/api/endpoints").await
    }

    pub async fn stacks(&self, jwt: &str) -> Result<Vec<PortainerStack>, PortainerImportError> {
        self.get_json(jwt, "/api/stacks").await
    }

    pub async fn stack_file(
        &self,
        jwt: &str,
        stack_id: i64,
    ) -> Result<PortainerStackFile, PortainerImportError> {
        self.get_json(jwt, &format!("/api/stacks/{}/file", stack_id))
            .await
    }

    pub async fn containers(
        &self,
        jwt: &str,
        endpoint_id: i64,
    ) -> Result<Vec<DockerContainer>, PortainerImportError> {
        self.get_json(
            jwt,
            &format!(
                "/api/endpoints/{}/docker/containers/json?all=true",
                endpoint_id
            ),
        )
        .await
    }

    pub async fn container_detail(
        &self,
        jwt: &str,
        endpoint_id: i64,
        container_id: &str,
    ) -> Result<DockerContainerDetail, PortainerImportError> {
        self.get_json(
            jwt,
            &format!(
                "/api/endpoints/{}/docker/containers/{}/json",
                endpoint_id, container_id
            ),
        )
        .await
    }
}
