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
use temps_core::url_validation::{resolve_and_validate_domain, validate_external_url};
use temps_import_types::ImportCredentials;

/// HTTP client bound to one Portainer instance
pub struct PortainerClient {
    http: reqwest::Client,
    base_url: url::Url,
    username: String,
    password: String,
    skip_tls_verify: bool,
}

/// Substrings that show up in TLS handshake failures across the TLS
/// backends reqwest can be built with. Used only to turn a raw connect
/// error into an actionable hint -- never to change control flow.
fn looks_like_cert_error(e: &reqwest::Error) -> bool {
    let s = e.to_string().to_lowercase();
    e.is_connect()
        && (s.contains("certificate")
            || s.contains("self signed")
            || s.contains("self-signed")
            || s.contains("unable to get local issuer"))
}

impl PortainerClient {
    pub async fn from_credentials(
        credentials: &ImportCredentials,
    ) -> Result<Self, PortainerImportError> {
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

        // Portainer ships a self-signed cert on :9443 by default, but the
        // admin password travels over this connection, so verification is
        // ON unless the user explicitly opts out (credentials.extra
        // ["verify_tls"] = "false") after seeing the certificate error --
        // never silently by default. skip_tls_verify() lets the importer
        // surface the choice in the plan instead of hiding it.
        let skip_tls_verify = credentials
            .extra
            .get("verify_tls")
            .map(|v| v == "false")
            .unwrap_or(false);

        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(skip_tls_verify)
            // Never follow redirects: a same-origin redirect to an internal
            // address (e.g. 169.254.169.254) would bypass the DNS-pinning
            // below entirely, since resolve_to_addrs only pins the original
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
                .map_err(|e| PortainerImportError::InvalidBaseUrl {
                    url: base.to_string(),
                    reason: e.to_string(),
                })?;
            builder = builder.resolve_to_addrs(domain, &addrs);
        }

        let http = builder.build().map_err(|e| PortainerImportError::Http {
            operation: "build http client".to_string(),
            reason: e.to_string(),
        })?;

        Ok(Self {
            http,
            base_url,
            username,
            password,
            skip_tls_verify,
        })
    }

    /// Whether this client accepts Portainer's certificate without
    /// verification — false by default (verification is on), true only
    /// when the user explicitly opted out via
    /// `credentials.extra["verify_tls"] = "false"` (e.g. Portainer's
    /// out-of-the-box self-signed cert on :9443).
    pub fn skip_tls_verify(&self) -> bool {
        self.skip_tls_verify
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
            .map_err(|e| {
                let reason = if looks_like_cert_error(&e) {
                    format!(
                        "{e} — if this Portainer instance uses a self-signed certificate \
                         (Portainer's default), set credentials.extra[\"verify_tls\"] = \"false\" \
                         to accept it (not recommended over an untrusted network)"
                    )
                } else {
                    e.to_string()
                };
                PortainerImportError::Http {
                    operation: operation.to_string(),
                    reason,
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    // A literal public IP as base_url so `from_credentials` never takes the
    // DNS-resolution branch (no network call happens either way -- building
    // the client doesn't dial anything, so this is safe to run offline).
    fn credentials(verify_tls: Option<&str>) -> ImportCredentials {
        let mut extra = std::collections::HashMap::new();
        if let Some(v) = verify_tls {
            extra.insert("verify_tls".to_string(), v.to_string());
        }
        ImportCredentials {
            token: Some("admin-password".to_string()),
            team_id: None,
            base_url: Some("https://1.1.1.1:9443".to_string()),
            extra,
        }
    }

    #[tokio::test]
    async fn tls_verification_is_on_by_default() {
        let client = PortainerClient::from_credentials(&credentials(None))
            .await
            .unwrap();
        assert!(
            !client.skip_tls_verify(),
            "verification must be ON unless the user explicitly opts out"
        );
    }

    #[tokio::test]
    async fn tls_verification_is_skipped_only_with_explicit_opt_out() {
        let client = PortainerClient::from_credentials(&credentials(Some("false")))
            .await
            .unwrap();
        assert!(client.skip_tls_verify());
    }

    #[tokio::test]
    async fn any_other_verify_tls_value_keeps_verification_on() {
        // Only the literal "false" opts out -- "true", typos, or anything
        // else must NOT silently disable verification.
        for value in ["true", "no", "0", ""] {
            let client = PortainerClient::from_credentials(&credentials(Some(value)))
                .await
                .unwrap();
            assert!(
                !client.skip_tls_verify(),
                "verify_tls={value:?} must keep verification on"
            );
        }
    }
}
