//! Minimal typed client for the Coolify REST API.
//!
//! Built per-request from [`temps_import_types::ImportCredentials`] — the
//! base URL goes through the same SSRF guard as every other importer and the
//! token is never stored.

use crate::error::CoolifyImportError;
use crate::model::{
    CoolifyApplication, CoolifyDatabase, CoolifyEnvVar, CoolifyProject, CoolifyProjectDetail,
    CoolifyServer,
};
use serde::de::DeserializeOwned;
use std::time::Duration;
use temps_core::url_validation::{resolve_and_validate_domain, validate_external_url};
use temps_import_types::ImportCredentials;

/// HTTP client bound to one Coolify instance
pub struct CoolifyClient {
    http: reqwest::Client,
    base_url: url::Url,
    token: String,
}

impl CoolifyClient {
    /// Build a client from per-request credentials.
    pub async fn from_credentials(
        credentials: &ImportCredentials,
    ) -> Result<Self, CoolifyImportError> {
        let base = credentials
            .base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .ok_or(CoolifyImportError::MissingBaseUrl)?;
        let token = credentials
            .token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or(CoolifyImportError::MissingToken)?
            .to_string();

        let base_url =
            validate_external_url(base).map_err(|e| CoolifyImportError::InvalidBaseUrl {
                url: base.to_string(),
                reason: e.to_string(),
            })?;

        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            // Never follow redirects: a same-origin redirect to an internal
            // address (e.g. 169.254.169.254) would bypass the DNS-pinning
            // above entirely, since resolve_to_addrs only pins the original
            // hostname -- mirrors temps-webhooks's webhook_client_builder.
            .redirect(reqwest::redirect::Policy::none());

        // `validate_external_url` only rejects literal IPs/localhost -- a
        // domain host could still resolve to an internal address by the time
        // reqwest actually dials it (DNS rebinding). Re-resolve here and pin
        // the client to the validated address(es), mirroring the webhook
        // service's delivery-time re-validation.
        if let Some(url::Host::Domain(domain)) = base_url.host() {
            let port = base_url.port_or_known_default().unwrap_or(443);
            let addrs = resolve_and_validate_domain(domain, port)
                .await
                .map_err(|e| CoolifyImportError::InvalidBaseUrl {
                    url: base.to_string(),
                    reason: e.to_string(),
                })?;
            builder = builder.resolve_to_addrs(domain, &addrs);
        }

        let http = builder.build().map_err(|e| CoolifyImportError::Http {
            operation: "build http client".to_string(),
            reason: e.to_string(),
        })?;

        Ok(Self {
            http,
            base_url,
            token,
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, CoolifyImportError> {
        let operation = format!("GET {}", path);
        let url = self
            .base_url
            .join(path)
            .map_err(|e| CoolifyImportError::InvalidBaseUrl {
                url: format!("{}{}", self.base_url, path),
                reason: e.to_string(),
            })?;

        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| CoolifyImportError::Http {
                operation: operation.clone(),
                reason: e.to_string(),
            })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| CoolifyImportError::Http {
                operation: operation.clone(),
                reason: format!("failed to read response body: {}", e),
            })?;

        match status.as_u16() {
            200 => {
                serde_json::from_str(&body).map_err(|e| CoolifyImportError::UnexpectedResponse {
                    operation,
                    reason: e.to_string(),
                })
            }
            401 => Err(CoolifyImportError::Unauthorized { operation }),
            403 => Err(CoolifyImportError::ApiDisabled { operation }),
            _ => Err(CoolifyImportError::Api {
                operation,
                status: status.as_u16(),
                body: body.chars().take(500).collect(),
            }),
        }
    }

    pub async fn servers(&self) -> Result<Vec<CoolifyServer>, CoolifyImportError> {
        self.get_json("/api/v1/servers").await
    }

    pub async fn projects(&self) -> Result<Vec<CoolifyProject>, CoolifyImportError> {
        self.get_json("/api/v1/projects").await
    }

    pub async fn project_detail(
        &self,
        uuid: &str,
    ) -> Result<CoolifyProjectDetail, CoolifyImportError> {
        self.get_json(&format!("/api/v1/projects/{}", uuid)).await
    }

    pub async fn applications(&self) -> Result<Vec<CoolifyApplication>, CoolifyImportError> {
        self.get_json("/api/v1/applications").await
    }

    pub async fn application_envs(
        &self,
        uuid: &str,
    ) -> Result<Vec<CoolifyEnvVar>, CoolifyImportError> {
        self.get_json(&format!("/api/v1/applications/{}/envs", uuid))
            .await
    }

    pub async fn databases(&self) -> Result<Vec<CoolifyDatabase>, CoolifyImportError> {
        self.get_json("/api/v1/databases").await
    }
}
