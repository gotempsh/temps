//! Minimal typed client for the CapRover REST API.
//!
//! CapRover authenticates by exchanging the admin password for a session
//! token (`POST /api/v2/login`), which is then sent as `x-captain-auth`.
//! Every request also needs `x-namespace: captain`. Responses are wrapped in
//! `{status, description, data}` with status 100 meaning OK.

use crate::error::CaproverImportError;
use crate::model::{
    CaproverAppList, CaproverEnvelope, CaproverLogin, CaproverSystemInfo, CAPROVER_STATUS_OK,
};
use serde::de::DeserializeOwned;
use std::time::Duration;
use temps_core::url_validation::{resolve_and_validate_domain, validate_external_url};
use temps_import_types::ImportCredentials;

/// HTTP client bound to one CapRover instance (token acquired lazily)
pub struct CaproverClient {
    http: reqwest::Client,
    base_url: url::Url,
    password: String,
}

impl CaproverClient {
    /// Build a client from per-request credentials.
    pub async fn from_credentials(
        credentials: &ImportCredentials,
    ) -> Result<Self, CaproverImportError> {
        let base = credentials
            .base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .ok_or(CaproverImportError::MissingBaseUrl)?;
        let password = credentials
            .token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or(CaproverImportError::MissingPassword)?
            .to_string();

        let base_url =
            validate_external_url(base).map_err(|e| CaproverImportError::InvalidBaseUrl {
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
                .map_err(|e| CaproverImportError::InvalidBaseUrl {
                    url: base.to_string(),
                    reason: e.to_string(),
                })?;
            builder = builder.resolve_to_addrs(domain, &addrs);
        }

        let http = builder.build().map_err(|e| CaproverImportError::Http {
            operation: "build http client".to_string(),
            reason: e.to_string(),
        })?;

        Ok(Self {
            http,
            base_url,
            password,
        })
    }

    fn url(&self, path: &str) -> Result<url::Url, CaproverImportError> {
        self.base_url
            .join(path)
            .map_err(|e| CaproverImportError::InvalidBaseUrl {
                url: format!("{}{}", self.base_url, path),
                reason: e.to_string(),
            })
    }

    fn unwrap_envelope<T>(
        operation: &str,
        envelope: CaproverEnvelope<T>,
    ) -> Result<T, CaproverImportError> {
        if envelope.status != CAPROVER_STATUS_OK {
            return Err(CaproverImportError::Api {
                operation: operation.to_string(),
                status: envelope.status,
                description: envelope.description.unwrap_or_default(),
            });
        }
        envelope
            .data
            .ok_or_else(|| CaproverImportError::UnexpectedResponse {
                operation: operation.to_string(),
                reason: "status OK but no data in response".to_string(),
            })
    }

    /// Exchange the password for a session token.
    pub async fn login(&self) -> Result<String, CaproverImportError> {
        let operation = "POST /api/v2/login";
        let response = self
            .http
            .post(self.url("/api/v2/login")?)
            .header("x-namespace", "captain")
            .json(&serde_json::json!({ "password": self.password }))
            .send()
            .await
            .map_err(|e| CaproverImportError::Http {
                operation: operation.to_string(),
                reason: e.to_string(),
            })?;
        let envelope: CaproverEnvelope<CaproverLogin> =
            response
                .json()
                .await
                .map_err(|e| CaproverImportError::UnexpectedResponse {
                    operation: operation.to_string(),
                    reason: e.to_string(),
                })?;
        if envelope.status != CAPROVER_STATUS_OK {
            return Err(CaproverImportError::LoginFailed);
        }
        Ok(Self::unwrap_envelope(operation, envelope)?.token)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        token: &str,
        path: &str,
    ) -> Result<T, CaproverImportError> {
        let operation = format!("GET {}", path);
        let response = self
            .http
            .get(self.url(path)?)
            .header("x-captain-auth", token)
            .header("x-namespace", "captain")
            .send()
            .await
            .map_err(|e| CaproverImportError::Http {
                operation: operation.clone(),
                reason: e.to_string(),
            })?;
        let envelope: CaproverEnvelope<T> =
            response
                .json()
                .await
                .map_err(|e| CaproverImportError::UnexpectedResponse {
                    operation: operation.clone(),
                    reason: e.to_string(),
                })?;
        Self::unwrap_envelope(&operation, envelope)
    }

    /// The instance host (for constructing externally-reachable DB URLs)
    pub fn host(&self) -> Option<String> {
        self.base_url.host_str().map(|h| h.to_string())
    }

    pub async fn system_info(
        &self,
        token: &str,
    ) -> Result<CaproverSystemInfo, CaproverImportError> {
        self.get_json(token, "/api/v2/user/system/info").await
    }

    pub async fn apps(&self, token: &str) -> Result<CaproverAppList, CaproverImportError> {
        self.get_json(token, "/api/v2/user/apps/appDefinitions")
            .await
    }
}
