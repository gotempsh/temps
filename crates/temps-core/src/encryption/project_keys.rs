// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Random project data keys wrapped by the instance key. Deleting the wrapped
//! key makes its ciphertext unreadable; restoring a backup containing the key
//! restores access too. Callers must apply the same retention policy to key backups.

use super::*;
use aes_gcm::aead::Payload;
use zeroize::Zeroizing;

fn key_scope(project_id: i32, key_id: &str) -> Vec<u8> {
    format!("temps/project-data-key/v1/{project_id}/{key_id}").into_bytes()
}

impl EncryptionService {
    /// Mint a random 256-bit key. Only the wrapped key is returned.
    pub fn create_project_data_key(
        &self,
        project_id: i32,
        key_id: &str,
    ) -> Result<String, EncryptionError> {
        let mut key = Zeroizing::new([0u8; 32]);
        crate::ecies::fill_secure_random_bytes("generating a project data key", key.as_mut())?;
        self.encrypt_with_aad(key.as_ref(), &key_scope(project_id, key_id))
    }

    pub fn encrypt_with_project_key(
        &self,
        project_id: i32,
        key_id: &str,
        wrapped_key: &str,
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<String, EncryptionError> {
        let key =
            Zeroizing::new(self.decrypt_with_aad(wrapped_key, &key_scope(project_id, key_id))?);
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|_| EncryptionError::InvalidKeyLength)?;
        let mut nonce = [0u8; NONCE_LENGTH];
        crate::ecies::fill_secure_random_bytes("generating a project payload nonce", &mut nonce)?;
        let ciphertext = cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| EncryptionError::Encrypt)?;
        let mut combined = nonce.to_vec();
        combined.extend(ciphertext);
        Ok(BASE64.encode(combined))
    }

    pub fn decrypt_with_project_key(
        &self,
        project_id: i32,
        key_id: &str,
        wrapped_key: &str,
        ciphertext: &str,
        aad: &[u8],
    ) -> Result<Vec<u8>, EncryptionError> {
        let key =
            Zeroizing::new(self.decrypt_with_aad(wrapped_key, &key_scope(project_id, key_id))?);
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|_| EncryptionError::InvalidKeyLength)?;
        let bytes = BASE64
            .decode(ciphertext)
            .map_err(|e| EncryptionError::InvalidBase64 {
                reason: describe_base64_error(&e),
            })?;
        if bytes.len() < NONCE_LENGTH {
            return Err(EncryptionError::Truncated {
                length: bytes.len(),
            });
        }
        let nonce =
            Nonce::try_from(&bytes[..NONCE_LENGTH]).map_err(|e| EncryptionError::InvalidNonce {
                reason: e.to_string(),
            })?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &bytes[NONCE_LENGTH..],
                    aad,
                },
            )
            .map_err(|_| EncryptionError::Decrypt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_keys_are_random_bound_to_the_project_and_cannot_be_recreated() {
        let service = EncryptionService::new("12345678901234567890123456789012").unwrap();
        let wrapped = service.create_project_data_key(1, "key-a").unwrap();
        let ciphertext = service
            .encrypt_with_project_key(
                1,
                "key-a",
                &wrapped,
                b"synthetic private payload",
                b"run:1/input",
            )
            .unwrap();
        assert_eq!(
            service
                .decrypt_with_project_key(1, "key-a", &wrapped, &ciphertext, b"run:1/input")
                .unwrap(),
            b"synthetic private payload"
        );
        assert!(service
            .decrypt_with_project_key(2, "key-a", &wrapped, &ciphertext, b"run:1/input")
            .is_err());
        assert!(service
            .decrypt_with_project_key(1, "key-b", &wrapped, &ciphertext, b"run:1/input")
            .is_err());
        assert!(service
            .decrypt_with_project_key(1, "key-a", &wrapped, &ciphertext, b"run:2/input")
            .is_err());
        let replacement = service.create_project_data_key(1, "key-a").unwrap();
        assert!(service
            .decrypt_with_project_key(1, "key-a", &replacement, &ciphertext, b"run:1/input")
            .is_err());
        assert!(service
            .decrypt_with_aad(&ciphertext, b"run:1/input")
            .is_err());
    }
}
