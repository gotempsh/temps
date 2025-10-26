// Note: Deprecation warnings from generic-array 0.14.x are expected
// These will be resolved when aes-gcm upgrades to 0.11.0 (currently in RC)
// which uses generic-array 1.x
#![allow(deprecated)]

use aes_gcm::{
    aead::{Aead, KeyInit},
    AeadCore, Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const NONCE_LENGTH: usize = 12;

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

    /// Creates a new EncryptionService by deriving a key from the given password using SHA-256
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

    /// Encrypts data using AES-256-GCM
    /// Returns base64 encoded string containing nonce + ciphertext
    pub fn encrypt(&self, data: &[u8]) -> Result<String> {
        let cipher = Aes256Gcm::new(self.master_key.as_slice().into());
        let nonce = Aes256Gcm::generate_nonce(&mut aes_gcm::aead::OsRng);

        let ciphertext = cipher
            .encrypt(&nonce, data)
            .map_err(|e| anyhow::anyhow!("Encryption error: {}", e))?;

        let mut combined = nonce.to_vec();
        combined.extend(ciphertext);
        Ok(BASE64.encode(combined))
    }

    /// Decrypts base64 encoded data that was encrypted with encrypt()
    pub fn decrypt(&self, encoded_data: &str) -> Result<Vec<u8>> {
        let data = BASE64
            .decode(encoded_data)
            .map_err(|e| anyhow::anyhow!("Base64 decode error: {}", e))?;

        if data.len() < NONCE_LENGTH {
            return Err(anyhow::anyhow!("Invalid encrypted data"));
        }

        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LENGTH);
        let cipher = Aes256Gcm::new(self.master_key.as_slice().into());

        let plaintext = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|e| anyhow::anyhow!("Error decrypting {} with error: {}", encoded_data, e))?;

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

    /// Generates a random encryption key as base64 string
    pub fn generate_key() -> String {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        BASE64.encode(key)
    }

    /// Generates a random 32-byte key as hex string (for direct use with new())
    pub fn generate_raw_key() -> String {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        bytes_to_hex(&key)
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
        let key_str = EncryptionService::generate_key();
        let decoded = BASE64.decode(&key_str);
        assert!(decoded.is_ok());
        assert_eq!(decoded.unwrap().len(), 32);
    }

    #[test]
    fn test_generate_raw_key_is_64_chars() {
        let key_str = EncryptionService::generate_raw_key();
        assert_eq!(key_str.len(), 64); // 32 bytes * 2 hex chars each

        // Should be able to create service with generated key
        let service = EncryptionService::new(&key_str);
        assert!(service.is_ok());
    }

    #[test]
    fn test_generated_keys_are_different() {
        let key1 = EncryptionService::generate_key();
        let key2 = EncryptionService::generate_key();
        assert_ne!(key1, key2);

        let raw_key1 = EncryptionService::generate_raw_key();
        let raw_key2 = EncryptionService::generate_raw_key();
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
    fn test_different_passwords_produce_different_keys() {
        let service1 = EncryptionService::new_from_password("password1");
        let service2 = EncryptionService::new_from_password("password2");

        let original = "Hello, World!";
        let encrypted1 = service1.encrypt_string(original).unwrap();
        let result = service2.decrypt_string(&encrypted1);

        assert!(result.is_err());
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
