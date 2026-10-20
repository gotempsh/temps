// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const NONCE_LENGTH: usize = 12;

/// Version prefix for password-derived ciphertexts (Argon2id KDF, random salt).
pub const PASSWORD_CIPHERTEXT_V1_PREFIX: &str = "argon2id-v1:";

/// Per-ciphertext salt length for password-derived keys.
const PASSWORD_SALT_LENGTH: usize = 16;

/// Argon2id parameters for `argon2id-v1`. They are part of the format: changing
/// them requires a new prefix, otherwise existing ciphertexts become undecryptable.
/// Values follow the OWASP baseline (19 MiB, 2 passes, 1 lane).
const PASSWORD_KDF_V1_M_COST_KIB: u32 = 19 * 1024;
const PASSWORD_KDF_V1_T_COST: u32 = 2;
const PASSWORD_KDF_V1_P_COST: u32 = 1;

/// Wire-format label of an `argon2id-v1` password ciphertext, for errors.
const PASSWORD_FORMAT_V1: &str = "argon2id-v1";

/// Wire-format label of a legacy (unsalted SHA-256-keyed) password ciphertext.
const PASSWORD_FORMAT_LEGACY: &str = "legacy-sha256";

/// Errors from password-keyed encryption
/// ([`EncryptionService::encrypt_with_password`] and friends).
///
/// Decryption failures name the ciphertext format they were attempting, so an
/// operator restoring an old recovery set can tell a wrong secret from a value
/// written by an incompatible version.
#[derive(Debug, thiserror::Error)]
pub enum PasswordEncryptionError {
    #[error("Secure random generation failed while {operation}: {reason}")]
    Random {
        operation: &'static str,
        reason: String,
    },

    #[error("Invalid Argon2id parameters for password encryption: {reason}")]
    KdfParameters { reason: String },

    #[error("Argon2id key derivation failed for password encryption: {reason}")]
    KeyDerivation { reason: String },

    #[error("AES-256-GCM encryption failed for a password-keyed value: {reason}")]
    Encrypt { reason: String },

    #[error("Password-encrypted value ({format} format) is not valid base64: {reason}")]
    Encoding {
        format: &'static str,
        reason: String,
    },

    #[error(
        "Password-encrypted value ({format} format) is truncated: {length} bytes, \
         at least {minimum} required"
    )]
    Truncated {
        format: &'static str,
        length: usize,
        minimum: usize,
    },

    #[error(
        "Password decryption failed for a {format} value: wrong password or tampered ciphertext"
    )]
    Decrypt { format: &'static str },

    #[error("Decrypted password-keyed value is not valid UTF-8: {reason}")]
    Utf8 { reason: String },
}

/// Derive the AES-256 key for an `argon2id-v1` ciphertext.
fn derive_password_key_v1(
    password: &str,
    salt: &[u8],
) -> Result<[u8; 32], PasswordEncryptionError> {
    use argon2::{Algorithm, Argon2, Params, Version};

    let params = Params::new(
        PASSWORD_KDF_V1_M_COST_KIB,
        PASSWORD_KDF_V1_T_COST,
        PASSWORD_KDF_V1_P_COST,
        Some(32),
    )
    .map_err(|e| PasswordEncryptionError::KdfParameters {
        reason: e.to_string(),
    })?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| PasswordEncryptionError::KeyDerivation {
            reason: e.to_string(),
        })?;
    Ok(key)
}

/// AES-256-GCM decrypt `nonce || ciphertext+tag` under `key`, reporting
/// failures against the password ciphertext `format` being read.
fn open_password_ciphertext(
    key: [u8; 32],
    data: &[u8],
    format: &'static str,
) -> Result<Vec<u8>, PasswordEncryptionError> {
    if data.len() < NONCE_LENGTH {
        return Err(PasswordEncryptionError::Truncated {
            format,
            length: data.len(),
            minimum: NONCE_LENGTH,
        });
    }
    let (nonce_bytes, ciphertext) = data.split_at(NONCE_LENGTH);
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key));
    let nonce =
        Nonce::try_from(nonce_bytes).map_err(|_| PasswordEncryptionError::Decrypt { format })?;
    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| PasswordEncryptionError::Decrypt { format })
}

/// Service for handling encryption and decryption operations
#[derive(Debug)]
pub struct EncryptionService {
    master_key: Arc<[u8; 32]>,
}

impl EncryptionService {
    /// Creates a new EncryptionService with the given master key
    /// Accepts either raw 32-byte key or hex-encoded 64-character key
    pub fn new(master_key: &str) -> Result<Self> {
        let key_bytes = if master_key.len() == 32 {
            // Raw 32-byte key
            master_key.as_bytes().to_vec()
        } else if master_key.len() == 64 {
            // Hex-encoded 64-character key
            hex::decode(master_key).map_err(|e| anyhow!("Invalid hex key: {}", e))?
        } else {
            return Err(anyhow!(
                "Master key must be exactly 32 bytes or 64 hex characters"
            ));
        };

        if key_bytes.len() != 32 {
            return Err(anyhow!("Master key must be exactly 32 bytes"));
        }

        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);

        Ok(Self {
            master_key: Arc::new(key),
        })
    }

    /// Creates a new EncryptionService by deriving a key from the given password using a
    /// single unsalted SHA-256 pass.
    ///
    /// **Legacy derivation.** This is *not* a password KDF: it has no salt and no work
    /// factor. It is kept only (a) so ciphertexts written before password-based
    /// encryption moved to Argon2id stay decryptable (see
    /// [`EncryptionService::decrypt_with_password`], which falls back to it), and
    /// (b) as a cheap deterministic key for tests. New at-rest data that is keyed by a
    /// password/secret must use [`EncryptionService::encrypt_with_password`].
    pub fn new_from_password(password: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        let key_bytes = hasher.finalize();

        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);

        Self {
            master_key: Arc::new(key),
        }
    }

    /// Encrypts `data` under a key derived from `password` with Argon2id and a fresh
    /// random per-ciphertext salt.
    ///
    /// Output format (versioned so the KDF can evolve without ambiguity):
    /// `argon2id-v1:` + base64(salt[16] || nonce[12] || AES-256-GCM ciphertext+tag).
    /// The `:` can never occur in the legacy format (plain standard base64), so
    /// [`EncryptionService::decrypt_with_password`] can tell the two apart.
    pub fn encrypt_with_password(
        password: &str,
        data: &[u8],
    ) -> Result<String, PasswordEncryptionError> {
        const SALT_OPERATION: &str = "generating a password-encryption salt";
        let mut salt = [0u8; PASSWORD_SALT_LENGTH];
        crate::ecies::fill_secure_random_bytes(SALT_OPERATION, &mut salt).map_err(|error| {
            PasswordEncryptionError::Random {
                operation: SALT_OPERATION,
                reason: error.to_string(),
            }
        })?;
        let key = derive_password_key_v1(password, &salt)?;
        let service = Self {
            master_key: Arc::new(key),
        };
        let inner = service
            .seal(data)
            .map_err(|error| PasswordEncryptionError::Encrypt {
                reason: error.to_string(),
            })?;

        let mut combined = Vec::with_capacity(PASSWORD_SALT_LENGTH + inner.len());
        combined.extend_from_slice(&salt);
        combined.extend(inner);
        Ok(format!(
            "{}{}",
            PASSWORD_CIPHERTEXT_V1_PREFIX,
            BASE64.encode(combined)
        ))
    }

    /// String convenience wrapper around [`EncryptionService::encrypt_with_password`].
    pub fn encrypt_string_with_password(
        password: &str,
        data: &str,
    ) -> Result<String, PasswordEncryptionError> {
        Self::encrypt_with_password(password, data.as_bytes())
    }

    /// Decrypts a value produced by [`EncryptionService::encrypt_with_password`].
    ///
    /// Values without the versioned prefix are treated as legacy ciphertexts produced by
    /// [`EncryptionService::new_from_password`] (unsalted SHA-256 key) and are decrypted
    /// that way, so data written before the Argon2id switch remains readable.
    pub fn decrypt_with_password(
        password: &str,
        encoded_data: &str,
    ) -> Result<Vec<u8>, PasswordEncryptionError> {
        let Some(body) = encoded_data.strip_prefix(PASSWORD_CIPHERTEXT_V1_PREFIX) else {
            let data =
                BASE64
                    .decode(encoded_data)
                    .map_err(|e| PasswordEncryptionError::Encoding {
                        format: PASSWORD_FORMAT_LEGACY,
                        reason: e.to_string(),
                    })?;
            let legacy = Self::new_from_password(password);
            return open_password_ciphertext(*legacy.master_key, &data, PASSWORD_FORMAT_LEGACY);
        };
        let data = BASE64
            .decode(body)
            .map_err(|e| PasswordEncryptionError::Encoding {
                format: PASSWORD_FORMAT_V1,
                reason: e.to_string(),
            })?;
        let minimum = PASSWORD_SALT_LENGTH + NONCE_LENGTH;
        if data.len() < minimum {
            return Err(PasswordEncryptionError::Truncated {
                format: PASSWORD_FORMAT_V1,
                length: data.len(),
                minimum,
            });
        }
        let (salt, rest) = data.split_at(PASSWORD_SALT_LENGTH);
        let key = derive_password_key_v1(password, salt)?;
        open_password_ciphertext(key, rest, PASSWORD_FORMAT_V1)
    }

    /// String convenience wrapper around [`EncryptionService::decrypt_with_password`].
    pub fn decrypt_string_with_password(
        password: &str,
        encoded_data: &str,
    ) -> Result<String, PasswordEncryptionError> {
        let decrypted = Self::decrypt_with_password(password, encoded_data)?;
        String::from_utf8(decrypted).map_err(|e| PasswordEncryptionError::Utf8 {
            reason: e.to_string(),
        })
    }

    /// Encrypts data using AES-256-GCM
    /// Returns base64 encoded string containing nonce + ciphertext
    pub fn encrypt(&self, data: &[u8]) -> Result<String> {
        Ok(BASE64.encode(self.seal(data)?))
    }

    /// AES-256-GCM encrypt under the master key; returns `nonce || ciphertext+tag`.
    fn seal(&self, data: &[u8]) -> Result<Vec<u8>> {
        let key = Key::<Aes256Gcm>::from(*self.master_key.as_ref());
        let cipher = Aes256Gcm::new(&key);
        let mut nonce_bytes = [0u8; NONCE_LENGTH];
        crate::ecies::fill_secure_random_bytes("generating an encryption nonce", &mut nonce_bytes)
            .map_err(|error| anyhow!(error))?;
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = cipher
            .encrypt(&nonce, data)
            .map_err(|e| anyhow::anyhow!("Encryption error: {}", e))?;

        let mut combined = nonce.to_vec();
        combined.extend(ciphertext);
        Ok(combined)
    }

    /// Decrypts base64 encoded data that was encrypted with encrypt()
    pub fn decrypt(&self, encoded_data: &str) -> Result<Vec<u8>> {
        let data = BASE64
            .decode(encoded_data)
            .map_err(|e| anyhow::anyhow!("Base64 decode error: {}", e))?;
        self.open(&data)
    }

    /// Inverse of [`EncryptionService::seal`]: `data` is `nonce || ciphertext+tag`.
    fn open(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < NONCE_LENGTH {
            return Err(anyhow::anyhow!("Invalid encrypted data"));
        }

        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LENGTH);
        let key = Key::<Aes256Gcm>::from(*self.master_key.as_ref());
        let cipher = Aes256Gcm::new(&key);
        let nonce = Nonce::try_from(nonce_bytes)
            .map_err(|error| anyhow!("Invalid encryption nonce: {error}"))?;

        let plaintext = cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

        Ok(plaintext)
    }

    /// Encrypts a string and returns base64 encoded encrypted data
    pub fn encrypt_string(&self, data: &str) -> Result<String> {
        self.encrypt(data.as_bytes())
    }

    /// Decrypts base64 encoded data and returns it as a UTF-8 string
    pub fn decrypt_string(&self, encoded_data: &str) -> Result<String> {
        let decrypted = self.decrypt(encoded_data)?;
        String::from_utf8(decrypted).map_err(|e| anyhow!("UTF-8 decode failed: {}", e))
    }

    /// Derive a domain-specific subkey from the master key using HMAC-SHA256.
    ///
    /// This produces a deterministic 32-byte key for a given domain string,
    /// cryptographically independent from the master key and from other derived keys.
    /// Use this when you need a separate key for a specific purpose (e.g., HMAC signing).
    pub fn derive_subkey(&self, domain: &str) -> [u8; 32] {
        use hmac::{Hmac, KeyInit, Mac};
        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(self.master_key.as_slice())
            .expect("HMAC can take key of any size");
        mac.update(domain.as_bytes());
        let result = mac.finalize().into_bytes();

        let mut key = [0u8; 32];
        key.copy_from_slice(&result);
        key
    }

    /// Generates a random encryption key as base64 string
    pub fn generate_key() -> Result<String> {
        let mut key = [0u8; 32];
        crate::ecies::fill_secure_random_bytes("generating a base64 encryption key", &mut key)
            .map_err(|error| anyhow!(error))?;
        Ok(BASE64.encode(key))
    }

    /// Generates a random 32-byte key as hex string (for direct use with new())
    pub fn generate_raw_key() -> Result<String> {
        let mut key = [0u8; 32];
        crate::ecies::fill_secure_random_bytes("generating a hexadecimal encryption key", &mut key)
            .map_err(|error| anyhow!(error))?;
        Ok(bytes_to_hex(&key))
    }
}

/// Convert bytes to hex string
fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_valid_32_byte_key() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key);
        assert!(service.is_ok());
    }

    #[test]
    fn test_new_with_invalid_key_length() {
        let short_key = "short";
        let result = EncryptionService::new(short_key);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Master key must be exactly 32 bytes"));
    }

    #[test]
    fn test_new_from_password() {
        let service = EncryptionService::new_from_password("test_password");
        let original = "Hello, World!";
        let encrypted = service.encrypt_string(original).unwrap();
        let decrypted = service.decrypt_string(&encrypted).unwrap();
        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_encryption_decryption() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = "Hello, World!";
        let encrypted = service.encrypt_string(original).unwrap();
        let decrypted = service.decrypt_string(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_encryption_decryption_bytes() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = b"Binary data \x00\x01\x02\xFF";
        let encrypted = service.encrypt(original).unwrap();
        let decrypted = service.decrypt(&encrypted).unwrap();

        assert_eq!(original.to_vec(), decrypted);
    }

    #[test]
    fn test_decrypts_pre_upgrade_aes_gcm_payload() {
        // AES-256-GCM vector encoded in Temps' existing nonce || ciphertext || tag format.
        // Keeping this stable ensures values written by aes-gcm 0.10 remain readable.
        let service = EncryptionService::new(&"00".repeat(32)).unwrap();
        let encoded = "AAAAAAAAAAAAAAAAzqdAPU1ga24HTsXTuvOdGNDRyKeZmWvwJluYtdSKuRk=";

        let decrypted = service.decrypt(encoded).unwrap();

        assert_eq!(decrypted, vec![0u8; 16]);
    }

    #[test]
    fn test_encryption_different_each_time() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = "Hello, World!";
        let encrypted1 = service.encrypt_string(original).unwrap();
        let encrypted2 = service.encrypt_string(original).unwrap();

        // Each encryption should produce different output due to random nonce
        assert_ne!(encrypted1, encrypted2);

        // But both should decrypt to the same value
        let decrypted1 = service.decrypt_string(&encrypted1).unwrap();
        let decrypted2 = service.decrypt_string(&encrypted2).unwrap();
        assert_eq!(decrypted1, decrypted2);
        assert_eq!(original, decrypted1);
    }

    #[test]
    fn test_decrypt_invalid_base64() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let result = service.decrypt_string("invalid-base64");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Base64 decode error"));
    }

    #[test]
    fn test_decrypt_too_short_data() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        // Create valid base64 but too short (less than nonce length)
        let short_data = BASE64.encode(b"short");
        let result = service.decrypt_string(&short_data);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid encrypted data"));
    }

    #[test]
    fn test_decrypt_corrupted_data() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = "Hello, World!";
        let mut encrypted = service.encrypt_string(original).unwrap();

        // Corrupt the encrypted data by changing a character
        encrypted.pop();
        encrypted.push('X');

        let result = service.decrypt_string(&encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_with_wrong_key() {
        let key1 = "12345678901234567890123456789012";
        let key2 = "09876543210987654321098765432109";

        let service1 = EncryptionService::new(key1).unwrap();
        let service2 = EncryptionService::new(key2).unwrap();

        let original = "Hello, World!";
        let encrypted = service1.encrypt_string(original).unwrap();

        // Try to decrypt with different key
        let result = service2.decrypt_string(&encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_string_encryption() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = "";
        let encrypted = service.encrypt_string(original).unwrap();
        let decrypted = service.decrypt_string(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_large_data_encryption() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = "A".repeat(10000);
        let encrypted = service.encrypt_string(&original).unwrap();
        let decrypted = service.decrypt_string(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_unicode_string_encryption() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = "Hello 世界! 🦀 Rust";
        let encrypted = service.encrypt_string(original).unwrap();
        let decrypted = service.decrypt_string(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_generate_key_is_valid_base64() {
        let key_str = EncryptionService::generate_key().unwrap();
        let decoded = BASE64.decode(&key_str);
        assert!(decoded.is_ok());
        assert_eq!(decoded.unwrap().len(), 32);
    }

    #[test]
    fn test_generate_raw_key_is_64_chars() {
        let key_str = EncryptionService::generate_raw_key().unwrap();
        assert_eq!(key_str.len(), 64); // 32 bytes * 2 hex chars each

        // Should be able to create service with generated key
        let service = EncryptionService::new(&key_str);
        assert!(service.is_ok());
    }

    #[test]
    fn test_generated_keys_are_different() {
        let key1 = EncryptionService::generate_key().unwrap();
        let key2 = EncryptionService::generate_key().unwrap();
        assert_ne!(key1, key2);

        let raw_key1 = EncryptionService::generate_raw_key().unwrap();
        let raw_key2 = EncryptionService::generate_raw_key().unwrap();
        assert_ne!(raw_key1, raw_key2);
        assert_eq!(raw_key1.len(), 64); // Verify hex encoding produces 64 chars
        assert_eq!(raw_key2.len(), 64);
    }

    #[test]
    fn test_same_password_produces_same_key() {
        let service1 = EncryptionService::new_from_password("test_password");
        let service2 = EncryptionService::new_from_password("test_password");

        let original = "Hello, World!";
        let encrypted1 = service1.encrypt_string(original).unwrap();
        let decrypted2 = service2.decrypt_string(&encrypted1).unwrap();

        assert_eq!(original, decrypted2);
    }

    #[test]
    fn test_password_encryption_round_trip_uses_versioned_salted_format() {
        let password = "s3-secret-for-password-encryption-test";
        let original = "encryption_key: abc\nauth_secret: def\n";

        let first = EncryptionService::encrypt_string_with_password(password, original).unwrap();
        let second = EncryptionService::encrypt_string_with_password(password, original).unwrap();

        assert!(first.starts_with(PASSWORD_CIPHERTEXT_V1_PREFIX));
        // Fresh salt (and nonce) per ciphertext: same input never yields the same output,
        // and the salt bytes differ.
        assert_ne!(first, second);
        let salt_of = |value: &str| {
            BASE64
                .decode(value.strip_prefix(PASSWORD_CIPHERTEXT_V1_PREFIX).unwrap())
                .unwrap()[..PASSWORD_SALT_LENGTH]
                .to_vec()
        };
        assert_ne!(salt_of(&first), salt_of(&second));

        assert_eq!(
            EncryptionService::decrypt_string_with_password(password, &first).unwrap(),
            original
        );
        assert_eq!(
            EncryptionService::decrypt_string_with_password(password, &second).unwrap(),
            original
        );
    }

    #[test]
    fn test_password_encryption_key_is_not_the_legacy_sha256_key() {
        let password = "kdf-separation-test";
        let encrypted = EncryptionService::encrypt_with_password(password, b"payload").unwrap();
        let body = encrypted
            .strip_prefix(PASSWORD_CIPHERTEXT_V1_PREFIX)
            .unwrap();
        let rest = &BASE64.decode(body).unwrap()[PASSWORD_SALT_LENGTH..];

        // The inner AES-GCM blob must not open under SHA-256(password).
        assert!(EncryptionService::new_from_password(password)
            .decrypt(&BASE64.encode(rest))
            .is_err());
    }

    #[test]
    fn test_password_decryption_accepts_legacy_sha256_ciphertext() {
        let password = "legacy-manifest-secret";
        let legacy = EncryptionService::new_from_password(password)
            .encrypt_string("legacy payload")
            .unwrap();
        assert!(!legacy.starts_with(PASSWORD_CIPHERTEXT_V1_PREFIX));

        assert_eq!(
            EncryptionService::decrypt_string_with_password(password, &legacy).unwrap(),
            "legacy payload"
        );
        assert!(EncryptionService::decrypt_string_with_password("wrong", &legacy).is_err());
    }

    #[test]
    fn test_password_decryption_rejects_wrong_password_and_tampering() {
        let encrypted =
            EncryptionService::encrypt_string_with_password("right-password", "secret").unwrap();
        assert!(
            EncryptionService::decrypt_string_with_password("wrong-password", &encrypted).is_err()
        );

        let mut tampered = encrypted.clone();
        tampered.pop();
        tampered.push(if encrypted.ends_with('A') { 'B' } else { 'A' });
        assert!(
            EncryptionService::decrypt_string_with_password("right-password", &tampered).is_err()
        );

        let truncated = format!(
            "{}{}",
            PASSWORD_CIPHERTEXT_V1_PREFIX,
            BASE64.encode([0u8; 20])
        );
        assert!(matches!(
            EncryptionService::decrypt_with_password("right-password", &truncated),
            Err(PasswordEncryptionError::Truncated {
                format: PASSWORD_FORMAT_V1,
                length: 20,
                ..
            })
        ));
    }

    #[test]
    fn test_password_decryption_errors_are_typed_and_name_the_format() {
        let encrypted =
            EncryptionService::encrypt_string_with_password("right-password", "secret").unwrap();
        assert!(matches!(
            EncryptionService::decrypt_string_with_password("wrong-password", &encrypted),
            Err(PasswordEncryptionError::Decrypt {
                format: PASSWORD_FORMAT_V1
            })
        ));

        let legacy = EncryptionService::new_from_password("right-password")
            .encrypt_string("secret")
            .unwrap();
        assert!(matches!(
            EncryptionService::decrypt_string_with_password("wrong-password", &legacy),
            Err(PasswordEncryptionError::Decrypt {
                format: PASSWORD_FORMAT_LEGACY
            })
        ));

        assert!(matches!(
            EncryptionService::decrypt_with_password("pw", "not base64!"),
            Err(PasswordEncryptionError::Encoding {
                format: PASSWORD_FORMAT_LEGACY,
                ..
            })
        ));

        let not_utf8 = EncryptionService::encrypt_with_password("pw", &[0xff, 0xfe]).unwrap();
        assert!(matches!(
            EncryptionService::decrypt_string_with_password("pw", &not_utf8),
            Err(PasswordEncryptionError::Utf8 { .. })
        ));
    }

    #[test]
    fn test_different_passwords_produce_different_keys() {
        let service1 = EncryptionService::new_from_password("password1");
        let service2 = EncryptionService::new_from_password("password2");

        let original = "Hello, World!";
        let encrypted1 = service1.encrypt_string(original).unwrap();
        let result = service2.decrypt_string(&encrypted1);

        assert!(result.is_err());
    }

    #[test]
    fn test_decryption_error_does_not_leak_ciphertext() {
        let key1 = "12345678901234567890123456789012";
        let key2 = "09876543210987654321098765432109";

        let service1 = EncryptionService::new(key1).unwrap();
        let service2 = EncryptionService::new(key2).unwrap();

        let secret_data = "super_secret_api_key_12345";
        let encrypted = service1.encrypt_string(secret_data).unwrap();

        // Attempt decryption with wrong key -- should fail
        let result = service2.decrypt_string(&encrypted);
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();

        // The error message must NOT contain the ciphertext
        assert!(
            !error_msg.contains(&encrypted),
            "Decryption error message must not contain ciphertext. Got: {}",
            error_msg
        );

        // The error message must NOT contain the original plaintext
        assert!(
            !error_msg.contains(secret_data),
            "Decryption error message must not contain plaintext. Got: {}",
            error_msg
        );

        // The error message should be generic
        assert!(
            error_msg.contains("Decryption failed"),
            "Expected generic decryption error message. Got: {}",
            error_msg
        );
    }

    #[test]
    fn test_corrupted_data_error_does_not_leak_ciphertext() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        let original = "sensitive_password_123";
        let encrypted = service.encrypt_string(original).unwrap();

        // Corrupt the encrypted data
        let mut corrupted = encrypted.clone();
        corrupted.pop();
        corrupted.push('X');

        let result = service.decrypt_string(&corrupted);
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(
                !error_msg.contains(&corrupted),
                "Corrupted data decryption error must not contain the ciphertext. Got: {}",
                error_msg
            );
            assert!(
                !error_msg.contains(original),
                "Error must not contain plaintext. Got: {}",
                error_msg
            );
        }
    }

    #[test]
    fn test_invalid_utf8_decryption_error() {
        let key = "12345678901234567890123456789012";
        let service = EncryptionService::new(key).unwrap();

        // Encrypt invalid UTF-8 bytes
        let invalid_utf8 = vec![0xFF, 0xFE, 0xFD];
        let encrypted = service.encrypt(&invalid_utf8).unwrap();

        // decrypt() should work
        let decrypted_bytes = service.decrypt(&encrypted).unwrap();
        assert_eq!(invalid_utf8, decrypted_bytes);

        // But decrypt_string() should fail
        let result = service.decrypt_string(&encrypted);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("UTF-8 decode failed"));
    }
}
