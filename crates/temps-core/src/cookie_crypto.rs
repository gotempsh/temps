// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use axum::http::StatusCode;
use base64::{engine::general_purpose, Engine as _};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("Encryption error: {0}")]
    EncryptionError(String),
    #[error("Decryption error: {0}")]
    DecryptionError(String),
    #[error("Invalid data format")]
    InvalidFormat,
    #[error("Invalid key: {0}")]
    InvalidKey(String),
}

// Convert CryptoError to Problem Details
impl From<CryptoError> for crate::problemdetails::Problem {
    fn from(error: CryptoError) -> Self {
        crate::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Encryption Error")
            .with_detail(error.to_string())
    }
}

/// Domain-separation salt for deriving the cookie AES key from the instance auth
/// secret (HKDF-SHA256 extract). Changing it invalidates every value encrypted
/// under the derived key, so bump the suffix only together with a new fallback.
const COOKIE_KEY_DERIVATION_SALT: &[u8] = b"temps-cookie-crypto-v1";

/// Derive the AES-256 key actually used for new ciphertexts.
///
/// The auth secret is a high-entropy value (auto-generated installs use 32 random
/// bytes), so a fast PRF is the right tool: HKDF-SHA256 extract keyed with a
/// domain-separation salt. This stops the raw secret bytes from doubling as an AES
/// key, and keeps this key independent of any other key derived from the same secret.
fn derive_cookie_key(secret_key: &[u8; 32]) -> [u8; 32] {
    let (prk, _) =
        hkdf::Hkdf::<sha2::Sha256>::extract(Some(COOKIE_KEY_DERIVATION_SALT), secret_key);
    let mut key = [0u8; 32];
    key.copy_from_slice(prk.as_slice());
    key
}

pub struct CookieCrypto {
    /// Cipher keyed with the HKDF-derived key. Every new ciphertext uses it.
    cipher: Aes256Gcm,
    /// Cipher keyed with the raw secret bytes, as every build before the key
    /// derivation was introduced did. Decrypt-only: it keeps long-lived values
    /// minted by an older build (session/visitor cookies, preview grants, actor
    /// tokens) readable across the upgrade. Values re-encrypted after the upgrade
    /// move to the derived key on their own.
    legacy_cipher: Aes256Gcm,
}

impl std::fmt::Debug for CookieCrypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CookieCrypto")
            .field("cipher", &"<hidden>")
            .finish()
    }
}

impl CookieCrypto {
    /// Creates a new CookieCrypto with the given key
    /// Accepts either raw 32-byte key or hex-encoded 64-character key
    pub fn new(secret_key: &str) -> Result<Self, CryptoError> {
        let key_bytes = if secret_key.len() == 32 {
            // Raw 32-byte key
            secret_key.as_bytes().to_vec()
        } else if secret_key.len() == 64 {
            // Hex-encoded 64-character key
            hex::decode(secret_key)
                .map_err(|e| CryptoError::InvalidKey(format!("Invalid hex key: {}", e)))?
        } else {
            return Err(CryptoError::InvalidKey(
                "Key must be exactly 32 bytes or 64 hex characters".to_string(),
            ));
        };

        let key: [u8; 32] = key_bytes.as_slice().try_into().map_err(|_| {
            CryptoError::InvalidKey(format!(
                "Key must be exactly 32 bytes, got {} after decoding",
                key_bytes.len()
            ))
        })?;
        Ok(Self::from_bytes(&key))
    }

    /// Creates a new CookieCrypto from a raw 32-byte array (for backward compatibility)
    pub fn from_bytes(secret_key: &[u8; 32]) -> Self {
        let derived = derive_cookie_key(secret_key);
        Self {
            cipher: Aes256Gcm::new(&Key::<Aes256Gcm>::from(derived)),
            legacy_cipher: Aes256Gcm::new(&Key::<Aes256Gcm>::from(*secret_key)),
        }
    }

    /// Encrypt a string value for use in cookies
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        let mut nonce_bytes = [0u8; 12];
        crate::ecies::fill_secure_random_bytes("generating a cookie nonce", &mut nonce_bytes)
            .map_err(|error| CryptoError::EncryptionError(error.to_string()))?;
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| CryptoError::EncryptionError(e.to_string()))?;

        // Combine nonce + ciphertext and encode as base64
        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ciphertext);

        Ok(general_purpose::URL_SAFE_NO_PAD.encode(&combined))
    }

    /// Decrypt a cookie value back to the original string.
    ///
    /// Tries the derived key first, then the legacy raw key so values minted before
    /// the key-derivation change keep working.
    pub fn decrypt(&self, encrypted: &str) -> Result<String, CryptoError> {
        // Decode from base64
        let combined = general_purpose::URL_SAFE_NO_PAD
            .decode(encrypted)
            .map_err(|_| CryptoError::InvalidFormat)?;

        if combined.len() < 12 {
            return Err(CryptoError::InvalidFormat);
        }

        // Split nonce and ciphertext
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::try_from(nonce_bytes).map_err(|_| CryptoError::InvalidFormat)?;

        let plaintext = match self.cipher.decrypt(&nonce, ciphertext) {
            Ok(plaintext) => plaintext,
            Err(_) => self
                .legacy_cipher
                .decrypt(&nonce, ciphertext)
                .map_err(|e| CryptoError::DecryptionError(e.to_string()))?,
        };

        String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidFormat)
    }

    /// Encrypt a numeric ID (i32) for use in cookies
    pub fn encrypt_id(&self, id: i32) -> Result<String, CryptoError> {
        self.encrypt(&id.to_string())
    }

    /// Decrypt a cookie value back to a numeric ID (i32)
    pub fn decrypt_id(&self, encrypted: &str) -> Result<i32, CryptoError> {
        let decrypted = self.decrypt(encrypted)?;
        decrypted
            .parse::<i32>()
            .map_err(|_| CryptoError::InvalidFormat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_test_crypto() -> CookieCrypto {
        let test_key = "test_key_32_bytes_long_for_tests"; // 32 bytes
        CookieCrypto::new(test_key).unwrap()
    }

    fn get_test_crypto_hex() -> CookieCrypto {
        let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"; // 64 hex chars
        CookieCrypto::new(test_key).unwrap()
    }

    #[test]
    fn test_encrypt_decrypt_string() {
        let crypto = get_test_crypto();
        let original = "test_value_123";

        let encrypted = crypto.encrypt(original).unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_encrypt_decrypt_id() {
        let crypto = get_test_crypto();
        let original_id = 12345;

        let encrypted = crypto.encrypt_id(original_id).unwrap();
        let decrypted_id = crypto.decrypt_id(&encrypted).unwrap();

        assert_eq!(original_id, decrypted_id);
    }

    #[test]
    fn test_decrypt_invalid_data() {
        let crypto = get_test_crypto();

        assert!(crypto.decrypt("invalid_data").is_err());
        assert!(crypto.decrypt_id("invalid_data").is_err());
    }

    #[test]
    fn test_new_with_hex_key() {
        let crypto = get_test_crypto_hex();
        let original = "test_value_123";

        let encrypted = crypto.encrypt(original).unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_new_with_invalid_key_length() {
        let short_key = "short";
        let result = CookieCrypto::new(short_key);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Key must be exactly 32 bytes"));
    }

    #[test]
    fn test_new_with_invalid_hex() {
        let invalid_hex = "invalid_hex_key_with_64_chars_but_not_valid_hex_encoding_at_allz"; // Exactly 64 chars
        let result = CookieCrypto::new(invalid_hex);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid hex key"));
    }

    /// Encrypt the way every build before the key derivation did: AES-256-GCM
    /// keyed directly with the raw secret bytes.
    fn legacy_encrypt(raw_key: &[u8; 32], plaintext: &str) -> String {
        let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*raw_key));
        let nonce = Nonce::from([7u8; 12]);
        let ciphertext = cipher.encrypt(&nonce, plaintext.as_bytes()).unwrap();
        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ciphertext);
        general_purpose::URL_SAFE_NO_PAD.encode(&combined)
    }

    fn open_with_raw_key(raw_key: &[u8; 32], encrypted: &str) -> Result<Vec<u8>, ()> {
        let combined = general_purpose::URL_SAFE_NO_PAD.decode(encrypted).unwrap();
        let (nonce, ciphertext) = combined.split_at(12);
        let nonce = Nonce::try_from(nonce).unwrap();
        Aes256Gcm::new(&Key::<Aes256Gcm>::from(*raw_key))
            .decrypt(&nonce, ciphertext)
            .map_err(|_| ())
    }

    #[test]
    fn test_new_ciphertexts_do_not_use_the_raw_secret_as_key() {
        let raw = *b"test_key_32_bytes_long_for_tests";
        let crypto = CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap();

        let encrypted = crypto.encrypt("session-token").unwrap();

        assert!(
            open_with_raw_key(&raw, &encrypted).is_err(),
            "new values must be keyed by the derived key, not the raw secret"
        );
        assert_eq!(crypto.decrypt(&encrypted).unwrap(), "session-token");
    }

    #[test]
    fn test_decrypts_values_minted_under_the_legacy_raw_key() {
        // Raw 32-byte form.
        let raw = *b"test_key_32_bytes_long_for_tests";
        let crypto = CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap();
        assert_eq!(
            crypto
                .decrypt(&legacy_encrypt(&raw, "legacy-session"))
                .unwrap(),
            "legacy-session"
        );

        // Hex form (the auto-generated installation secret format).
        let hex_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let decoded: [u8; 32] = hex::decode(hex_key).unwrap().try_into().unwrap();
        let crypto = CookieCrypto::new(hex_key).unwrap();
        assert_eq!(
            crypto.decrypt_id(&legacy_encrypt(&decoded, "42")).unwrap(),
            42
        );
    }

    #[test]
    fn test_legacy_fallback_does_not_accept_other_keys() {
        let crypto = CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap();
        let other_raw = *b"another_key_32_bytes_long_test!!";
        let other = CookieCrypto::from_bytes(&other_raw);

        assert!(crypto.decrypt(&legacy_encrypt(&other_raw, "x")).is_err());
        assert!(crypto.decrypt(&other.encrypt("x").unwrap()).is_err());
    }

    #[test]
    fn test_from_bytes_backward_compatibility() {
        let test_key = b"test_key_32_bytes_long_for_tests"; // 32 bytes
        let crypto = CookieCrypto::from_bytes(test_key);
        let original = "test_value_123";

        let encrypted = crypto.encrypt(original).unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }
}
