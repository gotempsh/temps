//! ECIES (Elliptic Curve Integrated Encryption Scheme) for edge certificate distribution.
//!
//! Uses X25519 ECDH + HKDF-SHA256 + AES-256-GCM to encrypt TLS certificate bundles
//! for specific edge nodes. Each sync response uses a fresh ephemeral key pair for
//! forward secrecy.
//!
//! # Security properties
//! - **Confidentiality**: AES-256-GCM authenticated encryption
//! - **Forward secrecy**: Fresh ephemeral key per encryption operation
//! - **Node isolation**: Each edge node has a unique X25519 key pair
//! - **Tamper detection**: GCM authentication tag

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hkdf::Hkdf;
use rand::rngs::SysRng;
use sha2::Sha256;
use thiserror::Error;

/// HKDF info string for domain separation.
const HKDF_INFO: &[u8] = b"temps-edge-cert-sync-v1";

/// Nonce size for AES-256-GCM.
const NONCE_SIZE: usize = 12;

#[derive(Error, Debug)]
pub enum EciesError {
    #[error("Invalid public key: {0}")]
    InvalidPublicKey(String),

    #[error("Invalid private key: {0}")]
    InvalidPrivateKey(String),

    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),

    #[error("Base64 decode error: {0}")]
    Base64(String),

    #[error("OS randomness failed while {operation}: {reason}")]
    RandomnessFailed { operation: String, reason: String },
}

/// Result of encrypting a payload for a specific edge node.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncryptedBundle {
    /// Base64-encoded AES-256-GCM ciphertext
    pub ciphertext: String,
    /// Base64-encoded 12-byte nonce
    pub nonce: String,
}

/// Encrypt a payload for a specific edge node's public key.
///
/// Generates a fresh ephemeral X25519 key pair, performs ECDH with the recipient's
/// public key, derives an AES-256-GCM key via HKDF, and encrypts the payload.
///
/// Returns the encrypted bundle and the ephemeral public key (base64-encoded).
pub fn encrypt_for_edge(
    recipient_public_key_b64: &str,
    plaintext: &[u8],
) -> Result<(EncryptedBundle, String), EciesError> {
    // Decode recipient's public key
    let recipient_pk_bytes = BASE64
        .decode(recipient_public_key_b64)
        .map_err(|e| EciesError::InvalidPublicKey(format!("base64 decode: {}", e)))?;
    if recipient_pk_bytes.len() != 32 {
        return Err(EciesError::InvalidPublicKey(format!(
            "expected 32 bytes, got {}",
            recipient_pk_bytes.len()
        )));
    }
    let mut pk_array = [0u8; 32];
    pk_array.copy_from_slice(&recipient_pk_bytes);
    let recipient_pk = x25519_dalek::PublicKey::from(pk_array);

    // Generate ephemeral key pair
    let ephemeral_secret = generate_x25519_static_secret()?;
    let ephemeral_public = x25519_dalek::PublicKey::from(&ephemeral_secret);

    // ECDH: derive shared secret
    let shared_secret = ephemeral_secret.diffie_hellman(&recipient_pk);

    // HKDF-SHA256: derive AES-256-GCM key
    let aes_key = derive_aes_key(shared_secret.as_bytes())?;

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    fill_secure_random_bytes("generating an ECIES nonce", &mut nonce_bytes)?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    // AES-256-GCM encrypt
    let cipher = Aes256Gcm::new_from_slice(&aes_key)
        .map_err(|e| EciesError::EncryptionFailed(format!("cipher init: {}", e)))?;
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| EciesError::EncryptionFailed(format!("aes-gcm: {}", e)))?;

    let bundle = EncryptedBundle {
        ciphertext: BASE64.encode(&ciphertext),
        nonce: BASE64.encode(nonce_bytes),
    };
    let ephemeral_pk_b64 = BASE64.encode(ephemeral_public.as_bytes());

    Ok((bundle, ephemeral_pk_b64))
}

/// Encryption session for encrypting multiple bundles with the same ephemeral key.
///
/// Creates a single ECDH shared secret and derives one AES key, then encrypts
/// each payload with a unique random nonce. This is more efficient than calling
/// `encrypt_for_edge` per bundle and only requires one ephemeral public key in
/// the response.
pub struct EncryptionSession {
    cipher: Aes256Gcm,
    ephemeral_public_key_b64: String,
}

impl EncryptionSession {
    /// Create a new session for a specific edge node's public key.
    pub fn new(recipient_public_key_b64: &str) -> Result<Self, EciesError> {
        let recipient_pk_bytes = BASE64
            .decode(recipient_public_key_b64)
            .map_err(|e| EciesError::InvalidPublicKey(format!("base64 decode: {}", e)))?;
        if recipient_pk_bytes.len() != 32 {
            return Err(EciesError::InvalidPublicKey(format!(
                "expected 32 bytes, got {}",
                recipient_pk_bytes.len()
            )));
        }
        let mut pk_array = [0u8; 32];
        pk_array.copy_from_slice(&recipient_pk_bytes);
        let recipient_pk = x25519_dalek::PublicKey::from(pk_array);

        let ephemeral_secret = generate_x25519_static_secret()?;
        let ephemeral_public = x25519_dalek::PublicKey::from(&ephemeral_secret);
        let shared_secret = ephemeral_secret.diffie_hellman(&recipient_pk);
        let aes_key = derive_aes_key(shared_secret.as_bytes())?;

        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| EciesError::EncryptionFailed(format!("cipher init: {}", e)))?;

        Ok(Self {
            cipher,
            ephemeral_public_key_b64: BASE64.encode(ephemeral_public.as_bytes()),
        })
    }

    /// The ephemeral public key to include in the response.
    pub fn ephemeral_public_key(&self) -> &str {
        &self.ephemeral_public_key_b64
    }

    /// Encrypt a single payload within this session.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<EncryptedBundle, EciesError> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        fill_secure_random_bytes("generating an ECIES session nonce", &mut nonce_bytes)?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| EciesError::EncryptionFailed(format!("aes-gcm: {}", e)))?;

        Ok(EncryptedBundle {
            ciphertext: BASE64.encode(&ciphertext),
            nonce: BASE64.encode(nonce_bytes),
        })
    }
}

/// Decrypt a bundle using the edge node's private key and the sender's ephemeral public key.
pub fn decrypt_bundle(
    private_key_b64: &str,
    ephemeral_public_key_b64: &str,
    bundle: &EncryptedBundle,
) -> Result<Vec<u8>, EciesError> {
    // Decode private key
    let sk_bytes = BASE64
        .decode(private_key_b64)
        .map_err(|e| EciesError::InvalidPrivateKey(format!("base64 decode: {}", e)))?;
    if sk_bytes.len() != 32 {
        return Err(EciesError::InvalidPrivateKey(format!(
            "expected 32 bytes, got {}",
            sk_bytes.len()
        )));
    }
    let mut sk_array = [0u8; 32];
    sk_array.copy_from_slice(&sk_bytes);
    let secret = x25519_dalek::StaticSecret::from(sk_array);

    // Decode ephemeral public key
    let epk_bytes = BASE64
        .decode(ephemeral_public_key_b64)
        .map_err(|e| EciesError::InvalidPublicKey(format!("base64 decode: {}", e)))?;
    if epk_bytes.len() != 32 {
        return Err(EciesError::InvalidPublicKey(format!(
            "expected 32 bytes, got {}",
            epk_bytes.len()
        )));
    }
    let mut epk_array = [0u8; 32];
    epk_array.copy_from_slice(&epk_bytes);
    let ephemeral_pk = x25519_dalek::PublicKey::from(epk_array);

    // ECDH: derive shared secret (same as encryption side)
    let shared_secret = secret.diffie_hellman(&ephemeral_pk);

    // HKDF-SHA256: derive AES-256-GCM key
    let aes_key = derive_aes_key(shared_secret.as_bytes())?;

    // Decode nonce and ciphertext
    let nonce_bytes = BASE64
        .decode(&bundle.nonce)
        .map_err(|e| EciesError::Base64(format!("nonce: {}", e)))?;
    if nonce_bytes.len() != NONCE_SIZE {
        return Err(EciesError::DecryptionFailed(format!(
            "nonce must be {} bytes, got {}",
            NONCE_SIZE,
            nonce_bytes.len()
        )));
    }
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = BASE64
        .decode(&bundle.ciphertext)
        .map_err(|e| EciesError::Base64(format!("ciphertext: {}", e)))?;

    // AES-256-GCM decrypt
    let cipher = Aes256Gcm::new_from_slice(&aes_key)
        .map_err(|e| EciesError::DecryptionFailed(format!("cipher init: {}", e)))?;
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| EciesError::DecryptionFailed("authentication failed".to_string()))?;

    Ok(plaintext)
}

/// Derive a 32-byte AES key from the ECDH shared secret using HKDF-SHA256.
fn derive_aes_key(shared_secret: &[u8]) -> Result<[u8; 32], EciesError> {
    let hk = Hkdf::<Sha256>::new(None, shared_secret);
    let mut key = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key)
        .map_err(|e| EciesError::EncryptionFailed(format!("HKDF expand: {}", e)))?;
    Ok(key)
}

/// Generate a static X25519 secret with the workspace's OS-backed CSPRNG.
///
/// Constructing from OS-generated bytes keeps this security boundary explicit
/// and lets entropy failures propagate instead of panicking.
pub fn generate_x25519_static_secret() -> Result<x25519_dalek::StaticSecret, EciesError> {
    let mut secret_bytes = [0u8; 32];
    fill_secure_random_bytes("generating an X25519 static secret", &mut secret_bytes)?;
    Ok(x25519_dalek::StaticSecret::from(secret_bytes))
}

pub(crate) fn fill_secure_random_bytes(
    operation: &str,
    bytes: &mut [u8],
) -> Result<(), EciesError> {
    fill_random_bytes_with(&mut SysRng, operation, bytes)
}

fn fill_random_bytes_with<R: rand::TryCryptoRng>(
    rng: &mut R,
    operation: &str,
    bytes: &mut [u8],
) -> Result<(), EciesError> {
    rng.try_fill_bytes(bytes)
        .map_err(|error| EciesError::RandomnessFailed {
            operation: operation.to_string(),
            reason: error.to_string(),
        })
}

/// Compute SHA-256 fingerprint of certificate DER bytes (hex-encoded).
pub fn cert_fingerprint(cert_pem: &str) -> String {
    use sha2::Digest;
    let mut hasher = Sha256::new();
    hasher.update(cert_pem.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingCryptoRng;

    impl rand::TryRng for FailingCryptoRng {
        type Error = std::io::Error;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            Err(std::io::Error::other("entropy source unavailable"))
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            Err(std::io::Error::other("entropy source unavailable"))
        }

        fn try_fill_bytes(&mut self, _dst: &mut [u8]) -> Result<(), Self::Error> {
            Err(std::io::Error::other("entropy source unavailable"))
        }
    }

    impl rand::TryCryptoRng for FailingCryptoRng {}

    fn generate_test_keypair() -> (String, String) {
        let secret = generate_x25519_static_secret().unwrap();
        let public = x25519_dalek::PublicKey::from(&secret);
        (
            BASE64.encode(secret.as_bytes()),
            BASE64.encode(public.as_bytes()),
        )
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let (private_key, public_key) = generate_test_keypair();
        let plaintext = b"-----BEGIN CERTIFICATE-----\nMIIBxTCCAW...\n-----END CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nMIIEvQIBAD...\n-----END PRIVATE KEY-----";

        let (bundle, ephemeral_pk) = encrypt_for_edge(&public_key, plaintext).unwrap();

        let decrypted = decrypt_bundle(&private_key, &ephemeral_pk, &bundle).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encryption_session_roundtrip() {
        let (private_key, public_key) = generate_test_keypair();
        let plaintext = b"certificate bundle encrypted in a session";

        let session = EncryptionSession::new(&public_key).unwrap();
        let bundle = session.encrypt(plaintext).unwrap();
        let decrypted =
            decrypt_bundle(&private_key, session.ephemeral_public_key(), &bundle).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_randomness_failure_preserves_operation_context() {
        let mut bytes = [0u8; 32];
        let error =
            fill_random_bytes_with(&mut FailingCryptoRng, "testing entropy failure", &mut bytes)
                .unwrap_err();

        assert!(matches!(
            error,
            EciesError::RandomnessFailed { operation, reason }
                if operation == "testing entropy failure"
                    && reason.contains("entropy source unavailable")
        ));
    }

    #[test]
    fn test_different_ephemeral_keys_each_time() {
        let (_private_key, public_key) = generate_test_keypair();
        let plaintext = b"test data";

        let (_, epk1) = encrypt_for_edge(&public_key, plaintext).unwrap();
        let (_, epk2) = encrypt_for_edge(&public_key, plaintext).unwrap();

        // Each encryption uses a fresh ephemeral key (forward secrecy)
        assert_ne!(epk1, epk2);
    }

    #[test]
    fn test_wrong_private_key_fails() {
        let (_private_key, public_key) = generate_test_keypair();
        let (wrong_private, _) = generate_test_keypair();
        let plaintext = b"secret cert data";

        let (bundle, ephemeral_pk) = encrypt_for_edge(&public_key, plaintext).unwrap();

        let result = decrypt_bundle(&wrong_private, &ephemeral_pk, &bundle);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EciesError::DecryptionFailed(_)
        ));
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let (private_key, public_key) = generate_test_keypair();
        let plaintext = b"secret cert data";

        let (mut bundle, ephemeral_pk) = encrypt_for_edge(&public_key, plaintext).unwrap();

        // Tamper with ciphertext
        let mut ct_bytes = BASE64.decode(&bundle.ciphertext).unwrap();
        ct_bytes[0] ^= 0xFF;
        bundle.ciphertext = BASE64.encode(&ct_bytes);

        let result = decrypt_bundle(&private_key, &ephemeral_pk, &bundle);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_public_key_length() {
        let result = encrypt_for_edge("dG9vc2hvcnQ=", b"data"); // "tooshort" base64
        assert!(matches!(
            result.unwrap_err(),
            EciesError::InvalidPublicKey(_)
        ));
    }

    #[test]
    fn test_empty_plaintext() {
        let (private_key, public_key) = generate_test_keypair();

        let (bundle, ephemeral_pk) = encrypt_for_edge(&public_key, b"").unwrap();
        let decrypted = decrypt_bundle(&private_key, &ephemeral_pk, &bundle).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn test_large_payload() {
        let (private_key, public_key) = generate_test_keypair();
        let plaintext = vec![0xAB; 100_000]; // 100KB payload

        let (bundle, ephemeral_pk) = encrypt_for_edge(&public_key, &plaintext).unwrap();
        let decrypted = decrypt_bundle(&private_key, &ephemeral_pk, &bundle).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_cert_fingerprint_deterministic() {
        let cert = "-----BEGIN CERTIFICATE-----\nABCDEF\n-----END CERTIFICATE-----";
        let fp1 = cert_fingerprint(cert);
        let fp2 = cert_fingerprint(cert);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_cert_fingerprint_changes_on_different_input() {
        let fp1 = cert_fingerprint("cert-v1");
        let fp2 = cert_fingerprint("cert-v2");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_node_isolation() {
        let (private_a, public_a) = generate_test_keypair();
        let (_private_b, _public_b) = generate_test_keypair();
        let plaintext = b"secret for node A only";

        // Encrypt for node A
        let (bundle, ephemeral_pk) = encrypt_for_edge(&public_a, plaintext).unwrap();

        // Node A can decrypt
        let decrypted = decrypt_bundle(&private_a, &ephemeral_pk, &bundle).unwrap();
        assert_eq!(decrypted, plaintext);

        // Node B cannot decrypt
        let result = decrypt_bundle(&_private_b, &ephemeral_pk, &bundle);
        assert!(result.is_err());
    }
}
