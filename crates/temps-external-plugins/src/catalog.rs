// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated external-plugin registry documents.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

pub const REGISTRY_URL: &str = "https://registry.temps.sh/api/plugins";
pub const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REGISTRY_VALIDITY: ChronoDuration = ChronoDuration::days(30);

#[derive(Debug, Clone)]
pub struct RegistryConfig {
    pub url: String,
    pub trust_anchors: BTreeMap<String, [u8; 32]>,
    pub allowed_artifact_hosts: BTreeSet<String>,
    pub allow_http: bool,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            url: REGISTRY_URL.to_string(),
            // Deliberately empty until the registry owner publishes the
            // production Ed25519 public key. An absent key is an error, never
            // an invitation to accept an unsigned document.
            trust_anchors: BTreeMap::new(),
            allowed_artifact_hosts: BTreeSet::from(["registry.temps.sh".to_string()]),
            allow_http: false,
        }
    }
}

impl RegistryConfig {
    pub fn with_trust_anchor(mut self, key_id: impl Into<String>, key: [u8; 32]) -> Self {
        self.trust_anchors.insert(key_id.into(), key);
        self
    }

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
        Self {
            url,
            trust_anchors: BTreeMap::from([(key_id.to_string(), key)]),
            allowed_artifact_hosts: BTreeSet::from([host]),
            allow_http: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum RegistryTrustConfigError {
    #[error("External-plugin registry trust configuration is incomplete: {provided} is set but {missing} is missing")]
    Incomplete {
        provided: &'static str,
        missing: &'static str,
    },
    #[error("External-plugin registry key ID must not be empty and may contain only ASCII letters, digits, '.', '_', or '-'")]
    InvalidKeyId,
    #[error("External-plugin registry public key for key ID '{key_id}' is not valid hexadecimal: {reason}")]
    InvalidPublicKeyHex { key_id: String, reason: String },
    #[error("External-plugin registry public key for key ID '{key_id}' decoded to {actual} bytes; Ed25519 public keys must be exactly 32 bytes")]
    InvalidPublicKeyLength { key_id: String, actual: usize },
}

/// Build the production registry configuration from the paired bootstrap
/// values accepted by `temps serve`. Neither value alone grants any trust;
/// absent values keep the catalogue visible but fail closed on fetch/startup.
pub fn registry_config_from_anchor(
    key_id: Option<&str>,
    public_key_hex: Option<&str>,
) -> Result<RegistryConfig, RegistryTrustConfigError> {
    let (key_id, public_key_hex) = match (key_id, public_key_hex) {
        (None, None) => return Ok(RegistryConfig::default()),
        (Some(_), None) => {
            return Err(RegistryTrustConfigError::Incomplete {
                provided: "registry key ID",
                missing: "registry public key",
            });
        }
        (None, Some(_)) => {
            return Err(RegistryTrustConfigError::Incomplete {
                provided: "registry public key",
                missing: "registry key ID",
            });
        }
        (Some(key_id), Some(public_key_hex)) => (key_id.trim(), public_key_hex.trim()),
    };
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RegistryTrustConfigError::InvalidKeyId);
    }
    let decoded = hex::decode(public_key_hex).map_err(|error| {
        RegistryTrustConfigError::InvalidPublicKeyHex {
            key_id: key_id.to_string(),
            reason: error.to_string(),
        }
    })?;
    let actual = decoded.len();
    let key: [u8; 32] =
        decoded
            .try_into()
            .map_err(|_| RegistryTrustConfigError::InvalidPublicKeyLength {
                key_id: key_id.to_string(),
                actual,
            })?;
    Ok(RegistryConfig::default().with_trust_anchor(key_id, key))
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
    pub envelope: RegistryEnvelope,
    pub document: RegistryDocument,
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error(
        "Plugin registry trust is not configured: no Ed25519 public keys are trusted for {url}"
    )]
    TrustNotConfigured { url: String },
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
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REGISTRY_TIMEOUT)
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

    pub async fn fetch(&self) -> Result<VerifiedRegistry, CatalogError> {
        if self.config.trust_anchors.is_empty() {
            return Err(CatalogError::TrustNotConfigured {
                url: self.config.url.clone(),
            });
        }
        let response = self
            .client
            .get(&self.config.url)
            .header("User-Agent", "temps-plugin-installer")
            .send()
            .await
            .map_err(|error| CatalogError::Fetch {
                url: self.config.url.clone(),
                reason: error.to_string(),
            })?;
        if response.status().is_redirection() {
            return Err(CatalogError::Status {
                url: self.config.url.clone(),
                status: response.status().as_u16(),
            });
        }
        if !response.status().is_success() {
            return Err(CatalogError::Status {
                url: self.config.url.clone(),
                status: response.status().as_u16(),
            });
        }
        let body = read_body_capped(response, &self.config.url, MAX_REGISTRY_BYTES).await?;
        let envelope: RegistryEnvelope =
            serde_json::from_slice(&body).map_err(|error| CatalogError::Parse {
                url: self.config.url.clone(),
                reason: error.to_string(),
            })?;
        let verified = verify_envelope(envelope, &self.config.trust_anchors, &self.config.url)?;
        validate_freshness(&verified.document, &self.config.url, Utc::now())?;
        Ok(verified)
    }
}

pub fn verify_envelope(
    envelope: RegistryEnvelope,
    anchors: &BTreeMap<String, [u8; 32]>,
    source: &str,
) -> Result<VerifiedRegistry, CatalogError> {
    if anchors.is_empty() {
        return Err(CatalogError::TrustNotConfigured {
            url: source.to_string(),
        });
    }
    let key_bytes = anchors
        .get(&envelope.key_id)
        .ok_or_else(|| CatalogError::UntrustedKey {
            url: source.to_string(),
            key_id: envelope.key_id.clone(),
        })?;
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&envelope.payload)
        .map_err(|error| CatalogError::InvalidEncoding {
            url: source.to_string(),
            field: "payload",
            reason: error.to_string(),
        })?;
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
    let key = VerifyingKey::from_bytes(key_bytes).map_err(|error| CatalogError::Parse {
        url: source.to_string(),
        reason: format!("invalid trust anchor '{}': {error}", envelope.key_id),
    })?;
    key.verify(&payload, &signature)
        .map_err(|_| CatalogError::InvalidSignature {
            url: source.to_string(),
            key_id: envelope.key_id.clone(),
        })?;
    let document: RegistryDocument =
        serde_json::from_slice(&payload).map_err(|error| CatalogError::Parse {
            url: source.to_string(),
            reason: error.to_string(),
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
    Ok(VerifiedRegistry { envelope, document })
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
    fn test_registry_config_from_anchor_without_values_returns_unconfigured_defaults() {
        // Arrange / Act
        let config = registry_config_from_anchor(None, None).expect("empty configuration is valid");

        // Assert
        assert_eq!(config.url, REGISTRY_URL);
        assert!(config.trust_anchors.is_empty());
        assert!(!config.allow_http);
        assert_eq!(
            config.allowed_artifact_hosts,
            BTreeSet::from(["registry.temps.sh".to_string()])
        );
    }

    #[test]
    fn test_registry_config_from_anchor_with_pair_configures_trimmed_anchor() {
        // Arrange
        let expected_key = [0xabu8; 32];
        let encoded_key = hex::encode(expected_key);

        // Act
        let config = registry_config_from_anchor(
            Some("  production-key_1  "),
            Some(&format!("  {encoded_key}  ")),
        )
        .expect("a complete valid trust anchor must be accepted");

        // Assert
        assert_eq!(
            config.trust_anchors.get("production-key_1"),
            Some(&expected_key)
        );
        assert_eq!(config.trust_anchors.len(), 1);
        assert_eq!(config.url, REGISTRY_URL);
        assert!(!config.allow_http);
    }

    #[test]
    fn test_registry_config_from_anchor_with_half_pair_returns_incomplete_error() {
        // Arrange / Act / Assert
        assert!(matches!(
            registry_config_from_anchor(Some("key-1"), None),
            Err(RegistryTrustConfigError::Incomplete {
                provided: "registry key ID",
                missing: "registry public key"
            })
        ));
        assert!(matches!(
            registry_config_from_anchor(None, Some(&hex::encode([7u8; 32]))),
            Err(RegistryTrustConfigError::Incomplete {
                provided: "registry public key",
                missing: "registry key ID"
            })
        ));
    }

    #[test]
    fn test_registry_config_from_anchor_with_malformed_key_returns_precise_error() {
        // Arrange / Act / Assert
        assert!(matches!(
            registry_config_from_anchor(Some("key-1"), Some("not-hex")),
            Err(RegistryTrustConfigError::InvalidPublicKeyHex { ref key_id, .. })
                if key_id == "key-1"
        ));
        assert!(matches!(
            registry_config_from_anchor(Some("key-1"), Some("abcd")),
            Err(RegistryTrustConfigError::InvalidPublicKeyLength {
                ref key_id,
                actual: 2
            }) if key_id == "key-1"
        ));
    }

    #[test]
    fn test_registry_config_from_anchor_with_invalid_key_id_is_rejected() {
        // Arrange
        let key = hex::encode([7u8; 32]);
        let too_long = "a".repeat(129);

        // Act / Assert
        for key_id in ["", "contains space", "path/key", "keyé", &too_long] {
            assert!(
                matches!(
                    registry_config_from_anchor(Some(key_id), Some(&key)),
                    Err(RegistryTrustConfigError::InvalidKeyId)
                ),
                "invalid key ID was accepted: {key_id:?}"
            );
        }
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
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
            signature: base64::engine::general_purpose::STANDARD
                .encode(signing_key.sign(&payload).to_bytes()),
        }
    }

    #[test]
    fn accepts_valid_signature() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let anchors =
            BTreeMap::from([("test-key".to_string(), signing.verifying_key().to_bytes())]);
        let verified = verify_envelope(signed_envelope("test-key", &signing), &anchors, "test")
            .expect("signature should verify");
        assert_eq!(verified.document.schema_version, 1);
    }

    #[test]
    fn node_crypto_cross_ecosystem_test_vector_verifies() {
        // Generated from a 32-byte test seed containing 0x07 using Node's
        // built-in crypto Ed25519 implementation. This literal vector keeps
        // the registry signer and Rust verifier aligned on standard base64
        // and on signing the decoded JSON payload bytes.
        let public_key: [u8; 32] =
            hex::decode("ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c")
                .expect("test public key hex")
                .try_into()
                .expect("32-byte test public key");
        let envelope = RegistryEnvelope {
            key_id: "node-test".to_string(),
            payload: "eyJzY2hlbWFfdmVyc2lvbiI6MSwicmV2aXNpb24iOjEsImlzc3VlZF9hdCI6IjIwMjYtMDktMDFUMDA6MDA6MDBaIiwiZXhwaXJlc19hdCI6IjIwMjYtMDktMzBUMDA6MDA6MDBaIiwicGx1Z2lucyI6W119".to_string(),
            signature: "wS4olkCwC6M9ytN05byXhfWi1MHKtEqryxOufhHTykeei5XpWfwiRCNc222xnkSXjNj7Db9qmdWMlvL3AD5hCQ==".to_string(),
        };
        let verified = verify_envelope(
            envelope,
            &BTreeMap::from([("node-test".to_string(), public_key)]),
            "node-test-vector",
        )
        .expect("Node signature must verify in Rust");
        assert_eq!(verified.document.schema_version, 1);
        assert!(verified.document.plugins.is_empty());
    }

    #[test]
    fn rejects_invalid_signature() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let anchors = BTreeMap::from([("test-key".to_string(), other.verifying_key().to_bytes())]);
        assert!(matches!(
            verify_envelope(signed_envelope("test-key", &signing), &anchors, "test"),
            Err(CatalogError::InvalidSignature { .. })
        ));
    }

    #[test]
    fn rejects_untrusted_key() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let anchors = BTreeMap::from([("other".to_string(), signing.verifying_key().to_bytes())]);
        assert!(matches!(
            verify_envelope(signed_envelope("test-key", &signing), &anchors, "test"),
            Err(CatalogError::UntrustedKey { .. })
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
