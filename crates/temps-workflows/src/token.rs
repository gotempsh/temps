// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Invocation tokens and workflow credentials (ADR-103 §11).
//!
//! **Invocation token.** Temps signs every request it sends to an executor
//! (`invoke`, `manifest`) with an Ed25519 key, and the same token authorizes
//! the executor's journal writes for that run and lease only. The key is
//! derived from the instance's master key, so every control-plane process signs
//! and verifies alike without storing a key. The SDK verifies the signature
//! with the public key served at `GET /workflows/v1/invocation-key`, checks the
//! expiry and checks `body`, the SHA-256 of the request body, so a forged
//! invocation (other input, other journal) is rejected before any user code
//! runs.
//!
//! **Workflow credential.** A random `twf_` token per environment, stored as a
//! SHA-256 hash, for the app-facing subset (start, send event, read run).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temps_core::EncryptionService;
use uuid::Uuid;

use crate::error::{WorkflowError, WorkflowResult};

/// Token version prefix; also the signing context.
pub const TOKEN_VERSION: &str = "twi1";
/// Derivation label of the signing key. Changing it invalidates every token
/// in flight and every SDK's cached public key.
const SIGNING_KEY_DOMAIN: &str = "temps-workflows/invocation-signing/v1";
/// Longest token accepted, to bound parsing work.
const MAX_TOKEN_LEN: usize = 4096;

/// Prefix of a workflow credential.
pub const CREDENTIAL_PREFIX: &str = "twf_";

/// What a token authorizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenPurpose {
    /// An invocation of `run` under `lease`; also authorizes its journal.
    Invoke,
    /// A manifest read of the environment.
    Manifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationClaims {
    pub v: u8,
    pub typ: TokenPurpose,
    /// Project id.
    pub pid: i32,
    /// Environment id.
    pub eid: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<i64>,
    /// Expiry, unix seconds.
    pub exp: i64,
    /// Hex SHA-256 of the request body the token was issued for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

pub struct InvocationSigner {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl std::fmt::Debug for InvocationSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvocationSigner")
            .field("public_key", &self.public_key())
            .finish()
    }
}

impl InvocationSigner {
    pub fn new(encryption: &EncryptionService) -> Self {
        let seed = encryption.derive_subkey(SIGNING_KEY_DOMAIN);
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    /// The base64url (no padding) Ed25519 public key SDKs verify with.
    pub fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.verifying_key.to_bytes())
    }

    /// A short id of the public key, so an SDK can tell a rotated key.
    pub fn key_id(&self) -> String {
        hex::encode(&Sha256::digest(self.verifying_key.to_bytes())[..8])
    }

    pub fn sign(&self, claims: &InvocationClaims) -> WorkflowResult<String> {
        let json = serde_json::to_vec(claims).map_err(|error| WorkflowError::Validation {
            field: "invocation token",
            reason: format!("claims did not serialize: {error}"),
        })?;
        let payload = URL_SAFE_NO_PAD.encode(json);
        let signature = self
            .signing_key
            .sign(format!("{TOKEN_VERSION}.{payload}").as_bytes());
        Ok(format!(
            "{TOKEN_VERSION}.{payload}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }

    /// Check the signature and expiry; returns the claims.
    pub fn verify(&self, token: &str, now: DateTime<Utc>) -> WorkflowResult<InvocationClaims> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(WorkflowError::InvalidToken {
                reason: "token too long",
            });
        }
        let mut parts = token.split('.');
        let (Some(version), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(WorkflowError::InvalidToken {
                reason: "malformed token",
            });
        };
        if version != TOKEN_VERSION {
            return Err(WorkflowError::InvalidToken {
                reason: "unknown token version",
            });
        }
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .ok_or(WorkflowError::InvalidToken {
                reason: "malformed signature",
            })?;
        self.verifying_key
            .verify_strict(format!("{TOKEN_VERSION}.{payload}").as_bytes(), &signature)
            .map_err(|_| WorkflowError::InvalidToken {
                reason: "bad signature",
            })?;
        let json = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| WorkflowError::InvalidToken {
                reason: "malformed claims",
            })?;
        let claims: InvocationClaims =
            serde_json::from_slice(&json).map_err(|_| WorkflowError::InvalidToken {
                reason: "malformed claims",
            })?;
        if claims.v != 1 {
            return Err(WorkflowError::InvalidToken {
                reason: "unknown claims version",
            });
        }
        if claims.exp <= now.timestamp() {
            return Err(WorkflowError::InvalidToken {
                reason: "token expired",
            });
        }
        Ok(claims)
    }

    /// Verify a token authorizing `run_id`'s journal; returns its lease.
    pub fn verify_run(
        &self,
        token: &str,
        run_id: Uuid,
        now: DateTime<Utc>,
    ) -> WorkflowResult<(InvocationClaims, i64)> {
        let claims = self.verify(token, now)?;
        if claims.typ != TokenPurpose::Invoke || claims.run != Some(run_id) {
            return Err(WorkflowError::InvalidToken {
                reason: "token is for another run",
            });
        }
        let lease = claims.lease.ok_or(WorkflowError::InvalidToken {
            reason: "token carries no lease",
        })?;
        Ok((claims, lease))
    }
}

/// Hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A freshly minted workflow credential: the token (returned once), its hash
/// and its display prefix.
pub struct MintedCredential {
    pub token: String,
    pub hash: String,
    pub prefix: String,
}

impl std::fmt::Debug for MintedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedCredential")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

pub fn mint_credential() -> WorkflowResult<MintedCredential> {
    let mut secret = [0u8; 32];
    temps_core::ecies::fill_secure_random_bytes("generating a workflow credential", &mut secret)
        .map_err(|error| WorkflowError::Validation {
            field: "credential",
            reason: format!("secure random generation failed: {error}"),
        })?;
    let token = format!("{CREDENTIAL_PREFIX}{}", URL_SAFE_NO_PAD.encode(secret));
    Ok(MintedCredential {
        hash: sha256_hex(token.as_bytes()),
        prefix: token.chars().take(12).collect(),
        token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> InvocationSigner {
        InvocationSigner::new(&EncryptionService::new("12345678901234567890123456789012").unwrap())
    }

    fn claims(run: Uuid, exp: i64) -> InvocationClaims {
        InvocationClaims {
            v: 1,
            typ: TokenPurpose::Invoke,
            pid: 1,
            eid: 2,
            run: Some(run),
            lease: Some(7),
            exp,
            body: Some(sha256_hex(b"{}")),
        }
    }

    #[test]
    fn a_signed_token_verifies_for_its_run_and_lease() {
        let signer = signer();
        let run = Uuid::new_v4();
        let now = Utc::now();
        let token = signer.sign(&claims(run, now.timestamp() + 60)).unwrap();
        let (claims, lease) = signer.verify_run(&token, run, now).unwrap();
        assert_eq!(lease, 7);
        assert_eq!(claims.body.as_deref(), Some(sha256_hex(b"{}").as_str()));
    }

    #[test]
    fn tokens_for_other_runs_expired_or_tampered_are_rejected() {
        let signer = signer();
        let run = Uuid::new_v4();
        let now = Utc::now();
        let token = signer.sign(&claims(run, now.timestamp() + 60)).unwrap();
        assert!(signer.verify_run(&token, Uuid::new_v4(), now).is_err());

        let expired = signer.sign(&claims(run, now.timestamp() - 1)).unwrap();
        assert!(matches!(
            signer.verify(&expired, now),
            Err(WorkflowError::InvalidToken {
                reason: "token expired"
            })
        ));

        // Swap the claims for another run's, keeping the signature.
        let other = signer
            .sign(&claims(Uuid::new_v4(), now.timestamp() + 60))
            .unwrap();
        let forged = format!(
            "{}.{}.{}",
            TOKEN_VERSION,
            other.split('.').nth(1).unwrap(),
            token.split('.').nth(2).unwrap()
        );
        assert!(matches!(
            signer.verify(&forged, now),
            Err(WorkflowError::InvalidToken {
                reason: "bad signature"
            })
        ));
    }

    #[test]
    fn the_signing_key_is_stable_across_processes() {
        assert_eq!(signer().public_key(), signer().public_key());
        assert_eq!(signer().key_id().len(), 16);
    }

    #[test]
    fn credentials_are_random_hashed_and_never_debug_printed() {
        let first = mint_credential().unwrap();
        let second = mint_credential().unwrap();
        assert!(first.token.starts_with(CREDENTIAL_PREFIX));
        assert_ne!(first.token, second.token);
        assert_eq!(first.hash, sha256_hex(first.token.as_bytes()));
        assert!(!format!("{first:?}").contains(&first.token));
    }
}
