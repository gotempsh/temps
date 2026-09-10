// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated external-plugin registry documents.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use futures::StreamExt as _;
use reqwest::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, PRAGMA};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

use crate::trust::{
    signature_message, KeysetEnvelope, KeysetUse, RootTrust, TrustError, VerifiedKeyset,
    CATALOG_SIGNATURE_DOMAIN,
};

pub const REGISTRY_URL: &str = "https://registry.temps.sh/api/plugins";
pub const REGISTRY_KEYS_URL: &str = "https://registry.temps.sh/api/plugins/keys";
pub const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
pub const MAX_REGISTRY_KEYS_BYTES: u64 = 64 * 1024;
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REGISTRY_VALIDITY: ChronoDuration = ChronoDuration::days(30);

#[derive(Debug, Clone)]
pub struct RegistryConfig {
    pub url: String,
    pub(crate) keys_url: String,
    pub(crate) root_trust: RootTrust,
    pub allowed_artifact_hosts: BTreeSet<String>,
    pub(crate) allow_http: bool,
    #[cfg(test)]
    test_keyset: Option<VerifiedKeyset>,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            url: REGISTRY_URL.to_string(),
            keys_url: REGISTRY_KEYS_URL.to_string(),
            root_trust: RootTrust::default(),
            allowed_artifact_hosts: BTreeSet::from(["registry.temps.sh".to_string()]),
            allow_http: false,
            #[cfg(test)]
            test_keyset: None,
        }
    }
}

impl RegistryConfig {
    pub fn with_artifact_host(mut self, host: impl Into<String>) -> Self {
        self.allowed_artifact_hosts.insert(host.into());
        self
    }

    #[cfg(test)]
    pub(crate) fn local(url: String, key_id: &str, key: [u8; 32]) -> Self {
        let host = Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let (root_trust, test_keyset) = VerifiedKeyset::test_fixture(key_id, key);
        Self {
            keys_url: format!("{url}/keys"),
            url,
            root_trust,
            allowed_artifact_hosts: BTreeSet::from([host]),
            allow_http: true,
            test_keyset: Some(test_keyset),
        }
    }
}

/// The outer envelope signs the decoded bytes in `payload`. Encoding the
/// payload instead of reserializing a JSON object avoids ambiguous map order,
/// whitespace, and number representations.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct RegistryEnvelope {
    pub key_id: String,
    /// Standard-base64 encoded JSON [`RegistryDocument`].
    pub payload: String,
    /// Standard-base64 encoded 64-byte Ed25519 signature over payload bytes.
    pub signature: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryDocument {
    pub schema_version: u32,
    /// Monotonic publisher revision. Persisted rollback protection can build
    /// on this value without changing the signed wire format.
    pub revision: u64,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub plugins: Vec<RegistryPlugin>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct RegistryPlugin {
    pub name: String,
    pub title: String,
    pub summary: String,
    pub description: String,
    pub author: String,
    pub category: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub docs_url: Option<String>,
    pub version: String,
    pub platforms: BTreeMap<String, PlatformRelease>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct PlatformRelease {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct VerifiedRegistry {
    pub keyset: VerifiedKeyset,
    pub envelope: RegistryEnvelope,
    pub document: RegistryDocument,
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error("Plugin registry document from {url} uses untrusted key id '{key_id}'")]
    UntrustedKey { url: String, key_id: String },
    #[error("Plugin registry document from {url} contains invalid base64 in {field}: {reason}")]
    InvalidEncoding {
        url: String,
        field: &'static str,
        reason: String,
    },
    #[error("Plugin registry signature from {url} using key '{key_id}' is invalid")]
    InvalidSignature { url: String, key_id: String },
    #[error("Plugin registry document from {url} has unsupported schema version {version}")]
    UnsupportedSchema { url: String, version: u32 },
    #[error("Plugin registry document from {url} has invalid validity metadata: {reason}")]
    InvalidValidity { url: String, reason: String },
    #[error("Failed to parse signed plugin registry payload from {url}: {reason}")]
    Parse { url: String, reason: String },
    #[error("Refusing plugin registry URL with unsafe transport or host: {url}")]
    UnsafeUrl { url: String },
    #[error("Failed to create HTTP client for plugin registry {url}: {reason}")]
    Client { url: String, reason: String },
    #[error("Failed to fetch plugin registry from {url}: {reason}")]
    Fetch { url: String, reason: String },
    #[error("Plugin registry {url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("Plugin registry response from {url} exceeded the {limit}-byte limit")]
    TooLarge { url: String, limit: u64 },
}

#[derive(Clone)]
pub struct RegistryClient {
    config: RegistryConfig,
    client: reqwest::Client,
}

impl RegistryClient {
    pub fn new(config: RegistryConfig) -> Result<Self, CatalogError> {
        validate_url(&config.url, &config, true)?;
        validate_url(&config.keys_url, &config, true)?;
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REGISTRY_TIMEOUT)
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            .build()
            .map_err(|error| CatalogError::Client {
                url: config.url.clone(),
                reason: error.to_string(),
            })?;
        Ok(Self { config, client })
    }

    pub fn config(&self) -> &RegistryConfig {
        &self.config
    }

    pub async fn fetch_keyset(&self) -> Result<VerifiedKeyset, CatalogError> {
        #[cfg(test)]
        if let Some(keyset) = &self.config.test_keyset {
            return Ok(keyset.clone());
        }
        let response = self
            .client
            .get(&self.config.keys_url)
            .header("User-Agent", "temps-plugin-installer")
            .header(ACCEPT, "application/json")
            .header(CACHE_CONTROL, "no-cache, no-store, max-age=0")
            .header(PRAGMA, "no-cache")
            .send()
            .await
            .map_err(|error| CatalogError::Fetch {
                url: self.config.keys_url.clone(),
                reason: error.to_string(),
            })?;
        require_json_success(&response, &self.config.keys_url)?;
        let body =
            read_body_capped(response, &self.config.keys_url, MAX_REGISTRY_KEYS_BYTES).await?;
        let envelope: KeysetEnvelope =
            serde_json::from_slice(&body).map_err(|error| CatalogError::Parse {
                url: self.config.keys_url.clone(),
                reason: error.to_string(),
            })?;
        Ok(VerifiedKeyset::verify(
            envelope,
            &self.config.root_trust,
            Utc::now(),
            KeysetUse::FreshCatalog,
        )?)
    }

    pub async fn fetch(&self) -> Result<VerifiedRegistry, CatalogError> {
        let keyset = self.fetch_keyset().await?;
        let response = self
            .client
            .get(&self.config.url)
            .header("User-Agent", "temps-plugin-installer")
            .header(ACCEPT, "application/json")
            .header(CACHE_CONTROL, "no-cache, no-store, max-age=0")
            .header(PRAGMA, "no-cache")
            .send()
            .await
            .map_err(|error| CatalogError::Fetch {
                url: self.config.url.clone(),
                reason: error.to_string(),
            })?;
        require_json_success(&response, &self.config.url)?;
        let body = read_body_capped(response, &self.config.url, MAX_REGISTRY_BYTES).await?;
        let envelope: RegistryEnvelope =
            serde_json::from_slice(&body).map_err(|error| CatalogError::Parse {
                url: self.config.url.clone(),
                reason: error.to_string(),
            })?;
        let verified =
            verify_envelope(envelope, keyset, &self.config.url, KeysetUse::FreshCatalog)?;
        validate_freshness(&verified.document, &self.config.url, Utc::now())?;
        Ok(verified)
    }
}

pub fn verify_envelope(
    envelope: RegistryEnvelope,
    keyset: VerifiedKeyset,
    source: &str,
    use_case: KeysetUse,
) -> Result<VerifiedRegistry, CatalogError> {
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&envelope.payload)
        .map_err(|error| CatalogError::InvalidEncoding {
            url: source.to_string(),
            field: "payload",
            reason: error.to_string(),
        })?;
    let document: RegistryDocument =
        serde_json::from_slice(&payload).map_err(|error| CatalogError::Parse {
            url: source.to_string(),
            reason: error.to_string(),
        })?;
    let key_bytes = keyset.catalog_key(&envelope.key_id, document.issued_at, use_case)?;
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(&envelope.signature)
        .map_err(|error| CatalogError::InvalidEncoding {
            url: source.to_string(),
            field: "signature",
            reason: error.to_string(),
        })?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|error| CatalogError::InvalidEncoding {
            url: source.to_string(),
            field: "signature",
            reason: error.to_string(),
        })?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|error| CatalogError::Parse {
        url: source.to_string(),
        reason: format!("invalid catalogue key '{}': {error}", envelope.key_id),
    })?;
    let message = signature_message(CATALOG_SIGNATURE_DOMAIN, &payload);
    key.verify_strict(&message, &signature)
        .map_err(|_| CatalogError::InvalidSignature {
            url: source.to_string(),
            key_id: envelope.key_id.clone(),
        })?;
    if document.schema_version != 1 {
        return Err(CatalogError::UnsupportedSchema {
            url: source.to_string(),
            version: document.schema_version,
        });
    }
    if document.revision == 0 {
        return Err(CatalogError::InvalidValidity {
            url: source.to_string(),
            reason: "revision must be greater than zero".to_string(),
        });
    }
    if document.expires_at <= document.issued_at {
        return Err(CatalogError::InvalidValidity {
            url: source.to_string(),
            reason: "expires_at must be later than issued_at".to_string(),
        });
    }
    if document.expires_at - document.issued_at > MAX_REGISTRY_VALIDITY {
        return Err(CatalogError::InvalidValidity {
            url: source.to_string(),
            reason: "catalogue validity may not exceed 30 days".to_string(),
        });
    }
    Ok(VerifiedRegistry {
        keyset,
        envelope,
        document,
    })
}

fn require_json_success(response: &reqwest::Response, url: &str) -> Result<(), CatalogError> {
    if response.status().is_redirection() || !response.status().is_success() {
        return Err(CatalogError::Status {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(CatalogError::Parse {
            url: url.to_string(),
            reason: "response Content-Type must be application/json".to_string(),
        });
    }
    Ok(())
}

fn validate_freshness(
    document: &RegistryDocument,
    source: &str,
    now: DateTime<Utc>,
) -> Result<(), CatalogError> {
    if document.issued_at > now + ChronoDuration::minutes(5) {
        return Err(CatalogError::InvalidValidity {
            url: source.to_string(),
            reason: "issued_at is more than five minutes in the future".to_string(),
        });
    }
    if document.expires_at <= now {
        return Err(CatalogError::InvalidValidity {
            url: source.to_string(),
            reason: format!("catalogue expired at {}", document.expires_at),
        });
    }
    Ok(())
}

pub(crate) fn validate_url(
    value: &str,
    config: &RegistryConfig,
    registry: bool,
) -> Result<Url, CatalogError> {
    let parsed = Url::parse(value).map_err(|_| CatalogError::UnsafeUrl {
        url: value.to_string(),
    })?;
    let scheme_ok = parsed.scheme() == "https" || (config.allow_http && parsed.scheme() == "http");
    let host = parsed.host_str();
    let host_ok = if registry {
        Url::parse(&config.url)
            .ok()
            .and_then(|url| url.host_str().map(ToOwned::to_owned))
            .as_deref()
            == host
    } else {
        host.is_some_and(|host| config.allowed_artifact_hosts.contains(host))
    };
    if !scheme_ok || !host_ok || parsed.username() != "" || parsed.password().is_some() {
        return Err(CatalogError::UnsafeUrl {
            url: value.to_string(),
        });
    }
    Ok(parsed)
}

async fn read_body_capped(
    response: reqwest::Response,
    url: &str,
    limit: u64,
) -> Result<Vec<u8>, CatalogError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(CatalogError::TooLarge {
            url: url.to_string(),
            limit,
        });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| CatalogError::Fetch {
            url: url.to_string(),
            reason: error.to_string(),
        })?;
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(CatalogError::TooLarge {
                url: url.to_string(),
                limit,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[test]
    fn production_config_uses_embedded_offline_root_quorum() {
        let config = RegistryConfig::default();

        assert_eq!(config.url, REGISTRY_URL);
        assert_eq!(config.keys_url, REGISTRY_KEYS_URL);
        assert_eq!(config.root_trust.threshold, 2);
        assert_eq!(config.root_trust.keys.len(), 3);
        assert!(!config.allow_http);
        assert_eq!(
            config.allowed_artifact_hosts,
            BTreeSet::from(["registry.temps.sh".to_string()])
        );
    }

    async fn oversized_registry_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind registry test server");
        let address = listener.local_addr().expect("registry test address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept registry request");
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await.expect("read request");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_REGISTRY_BYTES + 1
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });
        format!("http://{address}/api/plugins")
    }

    fn signed_envelope(key_id: &str, signing_key: &SigningKey) -> RegistryEnvelope {
        let payload = serde_json::to_vec(&RegistryDocument {
            schema_version: 1,
            revision: 1,
            issued_at: Utc::now() - ChronoDuration::minutes(1),
            expires_at: Utc::now() + ChronoDuration::hours(1),
            plugins: Vec::new(),
        })
        .expect("serialize registry");
        RegistryEnvelope {
            key_id: key_id.to_string(),
            payload: base64::engine::general_purpose::STANDARD.encode(&payload),
            signature: base64::engine::general_purpose::STANDARD.encode(
                signing_key
                    .sign(&signature_message(CATALOG_SIGNATURE_DOMAIN, &payload))
                    .to_bytes(),
            ),
        }
    }

    fn test_keyset(key_id: &str, key: [u8; 32]) -> VerifiedKeyset {
        VerifiedKeyset::test_fixture(key_id, key).1
    }

    #[test]
    fn accepts_valid_signature() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let verified = verify_envelope(
            signed_envelope("test-key", &signing),
            test_keyset("test-key", signing.verifying_key().to_bytes()),
            "test",
            KeysetUse::FreshCatalog,
        )
        .expect("signature should verify");
        assert_eq!(verified.document.schema_version, 1);
    }

    #[test]
    fn rejects_invalid_signature() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        assert!(matches!(
            verify_envelope(
                signed_envelope("test-key", &signing),
                test_keyset("test-key", other.verifying_key().to_bytes()),
                "test",
                KeysetUse::FreshCatalog,
            ),
            Err(CatalogError::InvalidSignature { .. })
        ));
    }

    #[test]
    fn rejects_signature_without_catalogue_domain_separator() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let mut envelope = signed_envelope("test-key", &signing);
        let payload = base64::engine::general_purpose::STANDARD
            .decode(&envelope.payload)
            .expect("fixture payload");
        envelope.signature =
            base64::engine::general_purpose::STANDARD.encode(signing.sign(&payload).to_bytes());

        assert!(matches!(
            verify_envelope(
                envelope,
                test_keyset("test-key", signing.verifying_key().to_bytes()),
                "test",
                KeysetUse::FreshCatalog,
            ),
            Err(CatalogError::InvalidSignature { .. })
        ));
    }

    #[test]
    fn rejects_untrusted_key() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        assert!(matches!(
            verify_envelope(
                signed_envelope("test-key", &signing),
                test_keyset("other", signing.verifying_key().to_bytes()),
                "test",
                KeysetUse::FreshCatalog,
            ),
            Err(CatalogError::Trust(TrustError::CatalogKeyNotTrusted { .. }))
        ));
    }

    #[test]
    fn production_policy_rejects_http_and_foreign_hosts() {
        let config = RegistryConfig::default();
        assert!(validate_url("http://registry.temps.sh/api/plugins", &config, true).is_err());
        assert!(validate_url("https://example.com/plugin", &config, false).is_err());
    }

    #[test]
    fn expired_live_catalogue_is_rejected() {
        let now = Utc::now();
        let document = RegistryDocument {
            schema_version: 1,
            revision: 2,
            issued_at: now - ChronoDuration::hours(2),
            expires_at: now - ChronoDuration::hours(1),
            plugins: Vec::new(),
        };
        assert!(matches!(
            validate_freshness(&document, "test", now),
            Err(CatalogError::InvalidValidity { .. })
        ));
    }

    #[tokio::test]
    async fn registry_body_size_is_bounded() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let url = oversized_registry_url().await;
        let config = RegistryConfig::local(url, "test-key", signing.verifying_key().to_bytes());
        let client = RegistryClient::new(config).expect("registry client");
        assert!(matches!(
            client.fetch().await,
            Err(CatalogError::TooLarge { .. })
        ));
    }
}
