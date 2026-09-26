// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Offline-root authentication and rotation policy for registry signing keys.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const KEYSET_SIGNATURE_DOMAIN: &[u8] = b"temps-plugin-keyset-v1\0";
pub const CATALOG_SIGNATURE_DOMAIN: &[u8] = b"temps-plugin-catalog-v1\0";
pub const KEYSET_AUDIENCE: &str = "registry.temps.sh/plugins";
pub const OFFICIAL_ROOT_THRESHOLD: usize = 2;
const MAX_KEYSET_PAYLOAD_BYTES: usize = 48 * 1024;
const MAX_KEYSET_SIGNATURES: usize = 8;
const MAX_CATALOG_KEYS: usize = 16;
const MAX_KEYSET_VALIDITY: ChronoDuration = ChronoDuration::days(7);
const MAX_CATALOG_KEY_VALIDITY: ChronoDuration = ChronoDuration::days(400);

/// Public halves of the offline registry roots. Their private halves are kept
/// outside the registry host and repository. Two distinct roots must authorize
/// every online catalogue-key update.
pub const OFFICIAL_ROOT_KEYS: [(&str, [u8; 32]); 3] = [
    (
        "root-2026-1",
        [
            0xda, 0x14, 0x94, 0x97, 0x0e, 0x32, 0x41, 0xd5, 0xc2, 0xf4, 0xfc, 0x79, 0x47, 0xd5,
            0x31, 0x8f, 0xaa, 0x7b, 0x6c, 0xd8, 0x94, 0x0c, 0xc4, 0x1a, 0x0f, 0xa6, 0x20, 0xdf,
            0xcb, 0xe3, 0x45, 0x4c,
        ],
    ),
    (
        "root-2026-2",
        [
            0x3f, 0x2e, 0x40, 0xfd, 0x1b, 0x75, 0xbc, 0xe1, 0xbd, 0xd6, 0x62, 0xd6, 0x25, 0x94,
            0x8a, 0xb2, 0x0a, 0x69, 0x0f, 0x4e, 0x71, 0xae, 0x81, 0xc4, 0xcc, 0x21, 0xa5, 0xcb,
            0x84, 0x15, 0xa3, 0x6f,
        ],
    ),
    (
        "root-2026-3",
        [
            0xd6, 0xc0, 0x63, 0x11, 0xaf, 0x27, 0x37, 0xcc, 0x50, 0x96, 0x29, 0xd8, 0xa1, 0x5b,
            0x78, 0x89, 0x7b, 0xa1, 0x0d, 0x56, 0x62, 0x53, 0x9e, 0xbf, 0x82, 0xaf, 0xa7, 0xb7,
            0x7a, 0x3b, 0x85, 0x5b,
        ],
    ),
];

#[derive(Debug, Clone)]
pub struct RootTrust {
    pub keys: BTreeMap<String, [u8; 32]>,
    pub threshold: usize,
}

impl Default for RootTrust {
    fn default() -> Self {
        Self {
            keys: OFFICIAL_ROOT_KEYS
                .iter()
                .map(|(id, key)| ((*id).to_string(), *key))
                .collect(),
            threshold: OFFICIAL_ROOT_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KeysetEnvelope {
    /// Standard-base64 encoded JSON [`KeysetDocument`].
    pub payload: String,
    pub signatures: Vec<RootSignature>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RootSignature {
    pub key_id: String,
    /// Standard-base64 encoded 64-byte Ed25519 signature.
    pub signature: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KeysetDocument {
    pub schema_version: u32,
    pub audience: String,
    pub generation: u64,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub keys: Vec<CatalogKey>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogKey {
    pub key_id: String,
    pub algorithm: String,
    /// Hex-encoded 32-byte Ed25519 public key.
    pub public_key: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub status: CatalogKeyStatus,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKeyStatus {
    Active,
    VerifyOnly,
    Revoked,
}

#[derive(Debug, Clone)]
pub struct TrustedCatalogKey {
    pub public_key: [u8; 32],
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub status: CatalogKeyStatus,
}

#[derive(Debug, Clone)]
pub struct VerifiedKeyset {
    pub envelope: KeysetEnvelope,
    pub document: KeysetDocument,
    keys: BTreeMap<String, TrustedCatalogKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysetUse {
    FreshCatalog,
    HistoricalReceipt,
}

#[derive(Debug, Error)]
pub enum TrustError {
    #[error("Registry root trust is invalid: {reason}")]
    InvalidRootTrust { reason: String },
    #[error("Registry keyset envelope is invalid: {reason}")]
    InvalidEnvelope { reason: String },
    #[error("Registry keyset has only {valid} valid root signatures; {required} are required")]
    RootThresholdNotMet { valid: usize, required: usize },
    #[error("Registry keyset payload is invalid: {reason}")]
    InvalidDocument { reason: String },
    #[error("Registry catalogue signing key '{key_id}' is not trusted for {use_case}: {reason}")]
    CatalogKeyNotTrusted {
        key_id: String,
        use_case: &'static str,
        reason: String,
    },
}

impl VerifiedKeyset {
    #[cfg(test)]
    pub(crate) fn test_fixture(key_id: &str, public_key: [u8; 32]) -> (RootTrust, Self) {
        Self::test_fixture_with_keys(
            vec![(key_id.to_string(), public_key, CatalogKeyStatus::Active)],
            1,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_fixture_with_keys(
        keys: Vec<(String, [u8; 32], CatalogKeyStatus)>,
        generation: u64,
    ) -> (RootTrust, Self) {
        use ed25519_dalek::{Signer as _, SigningKey};
        use std::sync::OnceLock;

        static TEST_NOW: OnceLock<DateTime<Utc>> = OnceLock::new();
        let now = *TEST_NOW.get_or_init(Utc::now);
        let root_1 = SigningKey::from_bytes(&[41; 32]);
        let root_2 = SigningKey::from_bytes(&[42; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("test-root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("test-root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let document = KeysetDocument {
            schema_version: 1,
            audience: KEYSET_AUDIENCE.to_string(),
            generation,
            issued_at: now - ChronoDuration::minutes(1),
            expires_at: now + ChronoDuration::hours(1),
            keys: keys
                .into_iter()
                .map(|(key_id, public_key, status)| CatalogKey {
                    key_id,
                    algorithm: "ed25519".to_string(),
                    public_key: hex::encode(public_key),
                    not_before: now - ChronoDuration::days(1),
                    not_after: now + ChronoDuration::days(30),
                    status,
                })
                .collect(),
        };
        let payload = serde_json::to_vec(&document).unwrap_or_default();
        let message = signature_message(KEYSET_SIGNATURE_DOMAIN, &payload);
        let envelope = KeysetEnvelope {
            payload: base64::engine::general_purpose::STANDARD.encode(payload),
            signatures: vec![
                RootSignature {
                    key_id: "test-root-1".to_string(),
                    signature: base64::engine::general_purpose::STANDARD
                        .encode(root_1.sign(&message).to_bytes()),
                },
                RootSignature {
                    key_id: "test-root-2".to_string(),
                    signature: base64::engine::general_purpose::STANDARD
                        .encode(root_2.sign(&message).to_bytes()),
                },
            ],
        };
        let verified = Self::verify(envelope, &roots, now, KeysetUse::FreshCatalog)
            .unwrap_or_else(|error| panic!("test keyset must verify: {error}"));
        (roots, verified)
    }

    pub fn verify(
        envelope: KeysetEnvelope,
        roots: &RootTrust,
        now: DateTime<Utc>,
        use_case: KeysetUse,
    ) -> Result<Self, TrustError> {
        validate_root_trust(roots)?;
        if envelope.signatures.is_empty() || envelope.signatures.len() > MAX_KEYSET_SIGNATURES {
            return Err(TrustError::InvalidEnvelope {
                reason: format!("expected between 1 and {MAX_KEYSET_SIGNATURES} root signatures"),
            });
        }
        let payload = base64::engine::general_purpose::STANDARD
            .decode(&envelope.payload)
            .map_err(|error| TrustError::InvalidEnvelope {
                reason: format!("payload is not valid base64: {error}"),
            })?;
        if payload.len() > MAX_KEYSET_PAYLOAD_BYTES {
            return Err(TrustError::InvalidEnvelope {
                reason: format!(
                    "decoded payload exceeds the {MAX_KEYSET_PAYLOAD_BYTES}-byte limit"
                ),
            });
        }
        let message = signature_message(KEYSET_SIGNATURE_DOMAIN, &payload);
        let mut valid_roots = BTreeSet::new();
        for root_signature in &envelope.signatures {
            if valid_roots.contains(root_signature.key_id.as_str()) {
                continue;
            }
            let Some(root_key) = roots.keys.get(&root_signature.key_id) else {
                continue;
            };
            let Ok(signature_bytes) =
                base64::engine::general_purpose::STANDARD.decode(&root_signature.signature)
            else {
                continue;
            };
            let Ok(signature) = Signature::from_slice(&signature_bytes) else {
                continue;
            };
            let verifying_key = VerifyingKey::from_bytes(root_key).map_err(|error| {
                TrustError::InvalidRootTrust {
                    reason: format!(
                        "root '{}' is not a valid Ed25519 key: {error}",
                        root_signature.key_id
                    ),
                }
            })?;
            if verifying_key.verify_strict(&message, &signature).is_ok() {
                valid_roots.insert(root_signature.key_id.as_str());
            }
        }
        let valid = valid_roots.len();
        if valid < roots.threshold {
            return Err(TrustError::RootThresholdNotMet {
                valid,
                required: roots.threshold,
            });
        }

        let document: KeysetDocument =
            serde_json::from_slice(&payload).map_err(|error| TrustError::InvalidDocument {
                reason: format!("payload is not valid JSON: {error}"),
            })?;
        let keys = validate_document(&document, now, use_case)?;
        Ok(Self {
            envelope,
            document,
            keys,
        })
    }

    pub fn catalog_key(
        &self,
        key_id: &str,
        catalog_issued_at: DateTime<Utc>,
        use_case: KeysetUse,
    ) -> Result<[u8; 32], TrustError> {
        let key = self
            .keys
            .get(key_id)
            .ok_or_else(|| TrustError::CatalogKeyNotTrusted {
                key_id: key_id.to_string(),
                use_case: use_case.label(),
                reason: "key ID is absent from the accepted keyset".to_string(),
            })?;
        if catalog_issued_at < key.not_before || catalog_issued_at >= key.not_after {
            return Err(TrustError::CatalogKeyNotTrusted {
                key_id: key_id.to_string(),
                use_case: use_case.label(),
                reason: "catalogue issuance is outside the key validity window".to_string(),
            });
        }
        let status_ok = match use_case {
            KeysetUse::FreshCatalog => key.status == CatalogKeyStatus::Active,
            KeysetUse::HistoricalReceipt => matches!(
                key.status,
                CatalogKeyStatus::Active | CatalogKeyStatus::VerifyOnly
            ),
        };
        if !status_ok {
            return Err(TrustError::CatalogKeyNotTrusted {
                key_id: key_id.to_string(),
                use_case: use_case.label(),
                reason: format!("key status is {:?}", key.status),
            });
        }
        Ok(key.public_key)
    }
}

impl KeysetUse {
    fn label(self) -> &'static str {
        match self {
            Self::FreshCatalog => "a fresh catalogue",
            Self::HistoricalReceipt => "an installed receipt",
        }
    }
}

pub fn signature_message(domain: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(domain.len() + payload.len());
    message.extend_from_slice(domain);
    message.extend_from_slice(payload);
    message
}

fn validate_root_trust(roots: &RootTrust) -> Result<(), TrustError> {
    if roots.threshold == 0 || roots.threshold > roots.keys.len() {
        return Err(TrustError::InvalidRootTrust {
            reason: format!(
                "threshold {} is incompatible with {} configured roots",
                roots.threshold,
                roots.keys.len()
            ),
        });
    }
    for (key_id, key) in &roots.keys {
        if !valid_key_id(key_id) {
            return Err(TrustError::InvalidRootTrust {
                reason: format!("root key ID '{key_id}' is invalid"),
            });
        }
        VerifyingKey::from_bytes(key).map_err(|error| TrustError::InvalidRootTrust {
            reason: format!("root '{key_id}' is not a valid Ed25519 key: {error}"),
        })?;
    }
    Ok(())
}

fn validate_document(
    document: &KeysetDocument,
    now: DateTime<Utc>,
    use_case: KeysetUse,
) -> Result<BTreeMap<String, TrustedCatalogKey>, TrustError> {
    let invalid = |reason: String| TrustError::InvalidDocument { reason };
    if document.schema_version != 1 {
        return Err(invalid(format!(
            "unsupported schema version {}",
            document.schema_version
        )));
    }
    if document.audience != KEYSET_AUDIENCE {
        return Err(invalid(format!(
            "audience '{}' does not match '{KEYSET_AUDIENCE}'",
            document.audience
        )));
    }
    if document.generation == 0 {
        return Err(invalid("generation must be greater than zero".to_string()));
    }
    if document.issued_at > now + ChronoDuration::minutes(5) {
        return Err(invalid(
            "issued_at is more than five minutes in the future".to_string(),
        ));
    }
    if document.expires_at <= document.issued_at
        || document.expires_at - document.issued_at > MAX_KEYSET_VALIDITY
    {
        return Err(invalid(
            "keyset validity must be positive and no longer than seven days".to_string(),
        ));
    }
    if use_case == KeysetUse::FreshCatalog && document.expires_at <= now {
        return Err(invalid(format!(
            "keyset expired at {}",
            document.expires_at
        )));
    }
    if document.keys.is_empty() || document.keys.len() > MAX_CATALOG_KEYS {
        return Err(invalid(format!(
            "expected between 1 and {MAX_CATALOG_KEYS} catalogue keys"
        )));
    }

    let mut keys = BTreeMap::new();
    let mut active = 0usize;
    for key in &document.keys {
        if !valid_key_id(&key.key_id) {
            return Err(invalid(format!(
                "catalogue key ID '{}' is invalid",
                key.key_id
            )));
        }
        if key.algorithm != "ed25519" {
            return Err(invalid(format!(
                "catalogue key '{}' uses unsupported algorithm '{}'",
                key.key_id, key.algorithm
            )));
        }
        if key.not_after <= key.not_before
            || key.not_after - key.not_before > MAX_CATALOG_KEY_VALIDITY
        {
            return Err(invalid(format!(
                "catalogue key '{}' has an invalid validity window",
                key.key_id
            )));
        }
        let decoded = hex::decode(&key.public_key).map_err(|error| {
            invalid(format!(
                "catalogue key '{}' is not valid hexadecimal: {error}",
                key.key_id
            ))
        })?;
        let actual = decoded.len();
        let public_key: [u8; 32] = decoded.try_into().map_err(|_| {
            invalid(format!(
                "catalogue key '{}' decoded to {actual} bytes; expected 32",
                key.key_id
            ))
        })?;
        VerifyingKey::from_bytes(&public_key).map_err(|error| {
            invalid(format!(
                "catalogue key '{}' is not a valid Ed25519 key: {error}",
                key.key_id
            ))
        })?;
        if key.status == CatalogKeyStatus::Active {
            active += 1;
        }
        if keys
            .insert(
                key.key_id.clone(),
                TrustedCatalogKey {
                    public_key,
                    not_before: key.not_before,
                    not_after: key.not_after,
                    status: key.status,
                },
            )
            .is_some()
        {
            return Err(invalid(format!(
                "duplicate catalogue key ID '{}'",
                key.key_id
            )));
        }
    }
    if use_case == KeysetUse::FreshCatalog && active == 0 {
        return Err(invalid(
            "fresh keyset does not contain an active catalogue key".to_string(),
        ));
    }
    Ok(keys)
}

pub fn valid_key_id(key_id: &str) -> bool {
    !key_id.is_empty()
        && key_id.len() <= 128
        && key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    fn signed_keyset(
        roots: &[(&str, &SigningKey)],
        catalog_key: &SigningKey,
        status: CatalogKeyStatus,
        generation: u64,
    ) -> KeysetEnvelope {
        let now = Utc::now();
        let payload = serde_json::to_vec(&KeysetDocument {
            schema_version: 1,
            audience: KEYSET_AUDIENCE.to_string(),
            generation,
            issued_at: now - ChronoDuration::minutes(1),
            expires_at: now + ChronoDuration::hours(1),
            keys: vec![CatalogKey {
                key_id: "catalog-1".to_string(),
                algorithm: "ed25519".to_string(),
                public_key: hex::encode(catalog_key.verifying_key().to_bytes()),
                not_before: now - ChronoDuration::days(1),
                not_after: now + ChronoDuration::days(30),
                status,
            }],
        })
        .expect("serialize keyset");
        let message = signature_message(KEYSET_SIGNATURE_DOMAIN, &payload);
        KeysetEnvelope {
            payload: base64::engine::general_purpose::STANDARD.encode(payload),
            signatures: roots
                .iter()
                .map(|(key_id, key)| RootSignature {
                    key_id: (*key_id).to_string(),
                    signature: base64::engine::general_purpose::STANDARD
                        .encode(key.sign(&message).to_bytes()),
                })
                .collect(),
        }
    }

    #[test]
    fn threshold_keyset_accepts_two_distinct_root_signatures() {
        let root_1 = SigningKey::from_bytes(&[1; 32]);
        let root_2 = SigningKey::from_bytes(&[2; 32]);
        let catalog = SigningKey::from_bytes(&[3; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let envelope = signed_keyset(
            &[("root-1", &root_1), ("root-2", &root_2)],
            &catalog,
            CatalogKeyStatus::Active,
            1,
        );

        let verified =
            VerifiedKeyset::verify(envelope, &roots, Utc::now(), KeysetUse::FreshCatalog)
                .expect("threshold signatures must verify");

        assert_eq!(verified.document.generation, 1);
        assert_eq!(
            verified
                .catalog_key("catalog-1", Utc::now(), KeysetUse::FreshCatalog)
                .expect("active key"),
            catalog.verifying_key().to_bytes()
        );
    }

    #[test]
    fn one_signature_cannot_satisfy_two_of_two_threshold() {
        let root_1 = SigningKey::from_bytes(&[1; 32]);
        let root_2 = SigningKey::from_bytes(&[2; 32]);
        let catalog = SigningKey::from_bytes(&[3; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let envelope = signed_keyset(
            &[("root-1", &root_1)],
            &catalog,
            CatalogKeyStatus::Active,
            1,
        );

        assert!(matches!(
            VerifiedKeyset::verify(envelope, &roots, Utc::now(), KeysetUse::FreshCatalog),
            Err(TrustError::RootThresholdNotMet {
                valid: 1,
                required: 2
            })
        ));
    }

    #[test]
    fn tampered_keyset_payload_fails_root_threshold() {
        let root_1 = SigningKey::from_bytes(&[1; 32]);
        let root_2 = SigningKey::from_bytes(&[2; 32]);
        let catalog = SigningKey::from_bytes(&[3; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let mut envelope = signed_keyset(
            &[("root-1", &root_1), ("root-2", &root_2)],
            &catalog,
            CatalogKeyStatus::Active,
            1,
        );
        let mut payload = base64::engine::general_purpose::STANDARD
            .decode(&envelope.payload)
            .expect("decode fixture");
        let last = payload.last_mut().expect("non-empty fixture");
        *last ^= 1;
        envelope.payload = base64::engine::general_purpose::STANDARD.encode(payload);

        assert!(matches!(
            VerifiedKeyset::verify(envelope, &roots, Utc::now(), KeysetUse::FreshCatalog),
            Err(TrustError::RootThresholdNotMet {
                valid: 0,
                required: 2
            })
        ));
    }

    #[test]
    fn malformed_extra_signature_cannot_hide_a_valid_quorum() {
        let root_1 = SigningKey::from_bytes(&[1; 32]);
        let root_2 = SigningKey::from_bytes(&[2; 32]);
        let catalog = SigningKey::from_bytes(&[3; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let mut envelope = signed_keyset(
            &[("root-1", &root_1), ("root-2", &root_2)],
            &catalog,
            CatalogKeyStatus::Active,
            1,
        );
        envelope.signatures.insert(
            0,
            RootSignature {
                key_id: "root-1".to_string(),
                signature: "not-base64".to_string(),
            },
        );

        assert!(
            VerifiedKeyset::verify(envelope, &roots, Utc::now(), KeysetUse::FreshCatalog).is_ok()
        );
    }

    #[test]
    fn verify_only_key_accepts_receipts_but_not_new_catalogues() {
        let root_1 = SigningKey::from_bytes(&[1; 32]);
        let root_2 = SigningKey::from_bytes(&[2; 32]);
        let catalog = SigningKey::from_bytes(&[3; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let envelope = signed_keyset(
            &[("root-1", &root_1), ("root-2", &root_2)],
            &catalog,
            CatalogKeyStatus::VerifyOnly,
            2,
        );
        let verified =
            VerifiedKeyset::verify(envelope, &roots, Utc::now(), KeysetUse::HistoricalReceipt)
                .expect("historical keyset");

        assert!(verified
            .catalog_key("catalog-1", Utc::now(), KeysetUse::HistoricalReceipt)
            .is_ok());
        assert!(verified
            .catalog_key("catalog-1", Utc::now(), KeysetUse::FreshCatalog)
            .is_err());
    }

    #[test]
    fn revoked_key_rejects_historical_receipts() {
        let root_1 = SigningKey::from_bytes(&[1; 32]);
        let root_2 = SigningKey::from_bytes(&[2; 32]);
        let catalog = SigningKey::from_bytes(&[3; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let envelope = signed_keyset(
            &[("root-1", &root_1), ("root-2", &root_2)],
            &catalog,
            CatalogKeyStatus::Revoked,
            3,
        );
        let verified =
            VerifiedKeyset::verify(envelope, &roots, Utc::now(), KeysetUse::HistoricalReceipt)
                .expect("root-signed revoked keyset is structurally valid");

        assert!(matches!(
            verified.catalog_key("catalog-1", Utc::now(), KeysetUse::HistoricalReceipt),
            Err(TrustError::CatalogKeyNotTrusted { .. })
        ));
    }

    #[test]
    fn expired_keyset_supports_offline_receipts_but_not_fresh_catalogues() {
        let root_1 = SigningKey::from_bytes(&[1; 32]);
        let root_2 = SigningKey::from_bytes(&[2; 32]);
        let catalog = SigningKey::from_bytes(&[3; 32]);
        let roots = RootTrust {
            keys: BTreeMap::from([
                ("root-1".to_string(), root_1.verifying_key().to_bytes()),
                ("root-2".to_string(), root_2.verifying_key().to_bytes()),
            ]),
            threshold: 2,
        };
        let now = Utc::now();
        let payload = serde_json::to_vec(&KeysetDocument {
            schema_version: 1,
            audience: KEYSET_AUDIENCE.to_string(),
            generation: 1,
            issued_at: now - ChronoDuration::hours(2),
            expires_at: now - ChronoDuration::hours(1),
            keys: vec![CatalogKey {
                key_id: "catalog-1".to_string(),
                algorithm: "ed25519".to_string(),
                public_key: hex::encode(catalog.verifying_key().to_bytes()),
                not_before: now - ChronoDuration::days(1),
                not_after: now + ChronoDuration::days(30),
                status: CatalogKeyStatus::Active,
            }],
        })
        .expect("serialize expired keyset");
        let message = signature_message(KEYSET_SIGNATURE_DOMAIN, &payload);
        let envelope = KeysetEnvelope {
            payload: base64::engine::general_purpose::STANDARD.encode(payload),
            signatures: vec![
                RootSignature {
                    key_id: "root-1".to_string(),
                    signature: base64::engine::general_purpose::STANDARD
                        .encode(root_1.sign(&message).to_bytes()),
                },
                RootSignature {
                    key_id: "root-2".to_string(),
                    signature: base64::engine::general_purpose::STANDARD
                        .encode(root_2.sign(&message).to_bytes()),
                },
            ],
        };

        assert!(VerifiedKeyset::verify(
            envelope.clone(),
            &roots,
            now,
            KeysetUse::HistoricalReceipt
        )
        .is_ok());
        assert!(matches!(
            VerifiedKeyset::verify(envelope, &roots, now, KeysetUse::FreshCatalog),
            Err(TrustError::InvalidDocument { .. })
        ));
    }
}
