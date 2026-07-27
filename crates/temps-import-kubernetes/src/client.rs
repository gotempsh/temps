//! Minimal Kubernetes API client
//!
//! A thin, read-only REST client over `reqwest` — enough to LIST/GET the
//! handful of resource kinds the importer needs. TLS uses rustls with the
//! cluster CA from the kubeconfig; auth is bearer token, client certificate,
//! or basic auth.
//!
//! # SSRF
//!
//! The API server URL is user-supplied, so it goes through the same
//! `validate_external_url` guard the other importers apply to `base_url`
//! (blocks loopback, RFC 1918, link-local, and cloud-metadata addresses).

use serde::de::DeserializeOwned;
use std::time::Duration;
use tracing::{debug, warn};

use crate::error::KubernetesImportError;
use crate::kubeconfig::{KubeAuth, ResolvedKubeconfig};
use crate::model::List;

/// Maximum pages to follow per LIST call — bounds memory on huge clusters.
const MAX_LIST_PAGES: usize = 10;
/// Page size for LIST calls.
const LIST_PAGE_LIMIT: usize = 500;

/// Read-only Kubernetes API client
pub struct KubeClient {
    http: reqwest::Client,
    base: String,
    auth: KubeAuth,
    skip_tls_verify: bool,
}

impl KubeClient {
    /// Build a client from a resolved kubeconfig.
    ///
    /// Validates the API server URL against the importer SSRF policy before
    /// anything else.
    pub async fn new(config: &ResolvedKubeconfig) -> Result<Self, KubernetesImportError> {
        let parsed =
            temps_core::url_validation::validate_external_url(&config.server).map_err(|e| {
                KubernetesImportError::ServerUrlRejected {
                    url: config.server.clone(),
                    reason: e.to_string(),
                }
            })?;

        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
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
        if let Some(url::Host::Domain(domain)) = parsed.host() {
            let port = parsed.port_or_known_default().unwrap_or(443);
            let addrs = temps_core::url_validation::resolve_and_validate_domain(domain, port)
                .await
                .map_err(|e| KubernetesImportError::ServerUrlRejected {
                    url: config.server.clone(),
                    reason: e.to_string(),
                })?;
            builder = builder.resolve_to_addrs(domain, &addrs);
        }

        if let Some(ca_pem) = &config.ca_pem {
            let cert = reqwest::Certificate::from_pem(ca_pem).map_err(|e| {
                KubernetesImportError::ClientBuild {
                    server: config.server.clone(),
                    reason: format!("invalid certificate-authority-data: {}", e),
                }
            })?;
            builder = builder.add_root_certificate(cert);
        }

        if config.skip_tls_verify {
            warn!(
                "Kubeconfig for {} sets insecure-skip-tls-verify — TLS verification disabled",
                config.server
            );
            builder = builder.danger_accept_invalid_certs(true);
        }

        if let KubeAuth::ClientCert { cert_pem, key_pem } = &config.auth {
            let mut identity_pem = Vec::with_capacity(cert_pem.len() + key_pem.len() + 1);
            identity_pem.extend_from_slice(cert_pem);
            identity_pem.push(b'\n');
            identity_pem.extend_from_slice(key_pem);
            let identity = reqwest::Identity::from_pem(&identity_pem).map_err(|e| {
                KubernetesImportError::ClientBuild {
                    server: config.server.clone(),
                    reason: format!("invalid client certificate/key data: {}", e),
                }
            })?;
            builder = builder.identity(identity);
        }

        let http = builder
            .build()
            .map_err(|e| KubernetesImportError::ClientBuild {
                server: config.server.clone(),
                reason: e.to_string(),
            })?;

        Ok(Self {
            http,
            base: config.server.clone(),
            auth: config.auth.clone(),
            skip_tls_verify: config.skip_tls_verify,
        })
    }

    /// Whether this client was built with `insecure-skip-tls-verify` set —
    /// callers surface this to the user rather than leaving it as a
    /// server-side-only log line.
    pub fn skip_tls_verify(&self) -> bool {
        self.skip_tls_verify
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.get(format!("{}{}", self.base, path));
        match &self.auth {
            KubeAuth::Token(token) => req = req.bearer_auth(token),
            KubeAuth::Basic { username, password } => {
                req = req.basic_auth(username, Some(password))
            }
            KubeAuth::ClientCert { .. } => {} // handled at TLS layer
        }
        req
    }

    /// GET a single object and deserialize it. `Ok(None)` on HTTP 404.
    pub async fn get_opt<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Option<T>, KubernetesImportError> {
        debug!("Kubernetes API GET {}", path);
        let response =
            self.request(path)
                .send()
                .await
                .map_err(|e| KubernetesImportError::ApiRequest {
                    path: path.to_string(),
                    reason: e.to_string(),
                })?;

        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(KubernetesImportError::ApiStatus {
                path: path.to_string(),
                status: status.as_u16(),
                body: truncate(&body, 300),
            });
        }
        let value = response
            .json::<T>()
            .await
            .map_err(|e| KubernetesImportError::ApiDecode {
                path: path.to_string(),
                reason: e.to_string(),
            })?;
        Ok(Some(value))
    }

    /// GET a single object; HTTP 404 is an error (callers that expect the
    /// object to exist).
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, KubernetesImportError> {
        self.get_opt(path)
            .await?
            .ok_or_else(|| KubernetesImportError::ApiStatus {
                path: path.to_string(),
                status: 404,
                body: "not found".to_string(),
            })
    }

    /// LIST a collection, following pagination up to [`MAX_LIST_PAGES`].
    ///
    /// `path` may already contain a query string (e.g. a fieldSelector).
    pub async fn list<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Vec<T>, KubernetesImportError> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let mut items = Vec::new();
        let mut continue_token: Option<String> = None;

        for page in 0..MAX_LIST_PAGES {
            let url = match &continue_token {
                Some(token) => format!(
                    "{}{}limit={}&continue={}",
                    path, separator, LIST_PAGE_LIMIT, token
                ),
                None => format!("{}{}limit={}", path, separator, LIST_PAGE_LIMIT),
            };
            let list: List<T> = self.get(&url).await?;
            items.extend(list.items);

            match list.metadata.continue_token {
                Some(token) if !token.is_empty() => {
                    continue_token = Some(token);
                    if page == MAX_LIST_PAGES - 1 {
                        warn!(
                            "LIST {} truncated at {} items ({} pages) — very large cluster",
                            path,
                            items.len(),
                            MAX_LIST_PAGES
                        );
                    }
                }
                _ => break,
            }
        }
        Ok(items)
    }

    /// Probe `/version` — returns the cluster's gitVersion string.
    pub async fn server_version(&self) -> Result<String, KubernetesImportError> {
        #[derive(serde::Deserialize)]
        struct VersionInfo {
            #[serde(rename = "gitVersion")]
            git_version: Option<String>,
        }
        let info: VersionInfo = self.get("/version").await?;
        Ok(info.git_version.unwrap_or_else(|| "unknown".to_string()))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
