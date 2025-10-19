use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
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

pub struct CookieCrypto {
    cipher: Aes256Gcm,
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
                "Key must be exactly 32 bytes or 64 hex characters".to_string()
            ));
        };

        if key_bytes.len() != 32 {
            return Err(CryptoError::InvalidKey(
                "Key must be exactly 32 bytes".to_string()
            ));
        }

        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);
        Ok(Self { cipher })
    }

    /// Creates a new CookieCrypto from a raw 32-byte array (for backward compatibility)
    pub fn from_bytes(secret_key: &[u8; 32]) -> Self {
        let key = Key::<Aes256Gcm>::from_slice(secret_key);
        let cipher = Aes256Gcm::new(key);
        Self { cipher }
    }

    /// Encrypt a string value for use in cookies
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| CryptoError::EncryptionError(e.to_string()))?;

        // Combine nonce + ciphertext and encode as base64
        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ciphertext);
        
        Ok(general_purpose::URL_SAFE_NO_PAD.encode(&combined))
    }

    /// Decrypt a cookie value back to the original string
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
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| CryptoError::DecryptionError(e.to_string()))?;

        String::from_utf8(plaintext)
            .map_err(|_| CryptoError::InvalidFormat)
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
        assert!(result.unwrap_err().to_string().contains("Key must be exactly 32 bytes"));
    }

    #[test]
    fn test_new_with_invalid_hex() {
        let invalid_hex = "invalid_hex_key_with_64_chars_but_not_valid_hex_encoding_at_allz";  // Exactly 64 chars
        let result = CookieCrypto::new(invalid_hex);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid hex key"));
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