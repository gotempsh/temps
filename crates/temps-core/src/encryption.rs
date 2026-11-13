// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, DecodeError, Engine};
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

/// Errors from [`EncryptionService`] operations under its master key.
///
/// No variant carries key material or plaintext. A rejected hex or base64
/// character is reported by position only, because the value being parsed can be
/// a key, or a plaintext passed to `decrypt` by mistake.
#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("Master key must be exactly 32 bytes or 64 hex characters")]
    InvalidKeyLength,

    #[error("Invalid hex key: invalid character at position {position}")]
    InvalidHexKey { position: usize },

    #[error(transparent)]
    Random(#[from] crate::ecies::EciesError),

    #[error("Encryption error: aead::Error")]
    Encrypt,

    #[error("Base64 decode error: {reason}")]
    InvalidBase64 { reason: String },

    #[error("Invalid encrypted data")]
    Truncated { length: usize },

    #[error("Invalid encryption nonce: {reason}")]
    InvalidNonce { reason: String },

    #[error("Decryption failed: aead::Error")]
    Decrypt,

    #[error("UTF-8 decode failed: {reason}")]
    InvalidUtf8 { reason: std::str::Utf8Error },
}

/// Describe a base64 failure by position only. `DecodeError`'s own `Display`
/// prints the rejected byte, which would put part of the input into the message.
pub(crate) fn describe_base64_error(error: &DecodeError) -> String {
    match error {
        DecodeError::InvalidByte(offset, _) => format!("Invalid symbol at offset {offset}."),
        DecodeError::InvalidLength(length) => format!("Invalid input length: {length}"),
        DecodeError::InvalidLastSymbol { offset, .. } => {
            format!("Invalid last symbol at offset {offset}.")
        }
        DecodeError::InvalidPadding => "Invalid padding".to_string(),
    }
}

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
pub struct EncryptionService {
    master_key: Arc<[u8; 32]>,
}

impl std::fmt::Debug for EncryptionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionService")
            .field("master_key", &"***")
            .finish()
    }
}

impl EncryptionService {
    /// Creates a new EncryptionService with the given master key
    /// Accepts either raw 32-byte key or hex-encoded 64-character key
    pub fn new(master_key: &str) -> Result<Self, EncryptionError> {
        let key_bytes = if master_key.len() == 32 {
            // Raw 32-byte key
            master_key.as_bytes().to_vec()
        } else if master_key.len() == 64 {
            // Hex-encoded 64-character key
            hex::decode(master_key).map_err(|error| match error {
                hex::FromHexError::InvalidHexCharacter { index, .. } => {
                    EncryptionError::InvalidHexKey { position: index }
                }
                hex::FromHexError::OddLength | hex::FromHexError::InvalidStringLength => {
                    EncryptionError::InvalidKeyLength
                }
            })?
        } else {
            return Err(EncryptionError::InvalidKeyLength);
        };

        let key: [u8; 32] = key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| EncryptionError::InvalidKeyLength)?;

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
                        reason: describe_base64_error(&e),
                    })?;
            let legacy = Self::new_from_password(password);
            return open_password_ciphertext(*legacy.master_key, &data, PASSWORD_FORMAT_LEGACY);
        };
        let data = BASE64
            .decode(body)
            .map_err(|e| PasswordEncryptionError::Encoding {
                format: PASSWORD_FORMAT_V1,
                reason: describe_base64_error(&e),
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
    pub fn encrypt(&self, data: &[u8]) -> Result<String, EncryptionError> {
        Ok(BASE64.encode(self.seal(data)?))
    }

    /// AES-256-GCM encrypt under the master key; returns `nonce || ciphertext+tag`.
    fn seal(&self, data: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        let key = Key::<Aes256Gcm>::from(*self.master_key.as_ref());
        let cipher = Aes256Gcm::new(&key);
        let mut nonce_bytes = [0u8; NONCE_LENGTH];
        crate::ecies::fill_secure_random_bytes("generating an encryption nonce", &mut nonce_bytes)?;
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = cipher
            .encrypt(&nonce, data)
            .map_err(|_| EncryptionError::Encrypt)?;

        let mut combined = nonce.to_vec();
        combined.extend(ciphertext);
        Ok(combined)
    }

    /// Decrypts base64 encoded data that was encrypted with encrypt()
    pub fn decrypt(&self, encoded_data: &str) -> Result<Vec<u8>, EncryptionError> {
        let data = BASE64
            .decode(encoded_data)
            .map_err(|error| EncryptionError::InvalidBase64 {
                reason: describe_base64_error(&error),
            })?;
        self.open(&data)
    }

    /// Inverse of [`EncryptionService::seal`]: `data` is `nonce || ciphertext+tag`.
    fn open(&self, data: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        if data.len() < NONCE_LENGTH {
            return Err(EncryptionError::Truncated { length: data.len() });
        }

        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LENGTH);
        let key = Key::<Aes256Gcm>::from(*self.master_key.as_ref());
        let cipher = Aes256Gcm::new(&key);
        let nonce =
            Nonce::try_from(nonce_bytes).map_err(|error| EncryptionError::InvalidNonce {
                reason: error.to_string(),
            })?;

        let plaintext = cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| EncryptionError::Decrypt)?;

        Ok(plaintext)
    }

    /// AES-256-GCM encrypt `data` with `associated_data` authenticated but not
    /// encrypted; returns base64 of `nonce || ciphertext+tag`, the same layout
    /// as [`EncryptionService::encrypt`].
    ///
    /// The associated data binds the ciphertext to where it is stored (for
    /// example a run, a step and a field). Decrypting it under any other
    /// associated data fails, so a ciphertext copied into another row, project
    /// or field cannot be read there. The associated data itself is not secret
    /// and is not stored in the output: the caller rebuilds it from the row.
    pub fn encrypt_with_aad(
        &self,
        data: &[u8],
        associated_data: &[u8],
    ) -> Result<String, EncryptionError> {
        use aes_gcm::aead::Payload;

        let key = Key::<Aes256Gcm>::from(*self.master_key.as_ref());
        let cipher = Aes256Gcm::new(&key);
        let mut nonce_bytes = [0u8; NONCE_LENGTH];
        crate::ecies::fill_secure_random_bytes("generating an encryption nonce", &mut nonce_bytes)?;
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: data,
                    aad: associated_data,
                },
            )
            .map_err(|_| EncryptionError::Encrypt)?;
        let mut combined = nonce.to_vec();
        combined.extend(ciphertext);
        Ok(BASE64.encode(combined))
    }

    /// Inverse of [`EncryptionService::encrypt_with_aad`]. Fails with
    /// [`EncryptionError::Decrypt`] when the associated data differs from the
    /// one the value was encrypted under, or the ciphertext was altered.
    pub fn decrypt_with_aad(
        &self,
        encoded_data: &str,
        associated_data: &[u8],
    ) -> Result<Vec<u8>, EncryptionError> {
        use aes_gcm::aead::Payload;

        let data = BASE64
            .decode(encoded_data)
            .map_err(|error| EncryptionError::InvalidBase64 {
                reason: describe_base64_error(&error),
            })?;
        if data.len() < NONCE_LENGTH {
            return Err(EncryptionError::Truncated { length: data.len() });
        }
        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LENGTH);
        let key = Key::<Aes256Gcm>::from(*self.master_key.as_ref());
        let cipher = Aes256Gcm::new(&key);
        let nonce =
            Nonce::try_from(nonce_bytes).map_err(|error| EncryptionError::InvalidNonce {
                reason: error.to_string(),
            })?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: associated_data,
                },
            )
            .map_err(|_| EncryptionError::Decrypt)
    }

    /// Encrypts a string and returns base64 encoded encrypted data
    pub fn encrypt_string(&self, data: &str) -> Result<String, EncryptionError> {
        self.encrypt(data.as_bytes())
    }

    /// Decrypts base64 encoded data and returns it as a UTF-8 string
    pub fn decrypt_string(&self, encoded_data: &str) -> Result<String, EncryptionError> {
        let decrypted = self.decrypt(encoded_data)?;
        // `FromUtf8Error` owns the decrypted bytes and prints them in its `Debug`,
        // so only the position-only `Utf8Error` may stay in the error.
        String::from_utf8(decrypted).map_err(|error| EncryptionError::InvalidUtf8 {
            reason: error.utf8_error(),
        })
    }

    /// Derive a domain-specific subkey from the master key using HMAC-SHA256.
    ///
    /// This produces a deterministic 32-byte key for a given domain string,
    /// cryptographically independent from the master key and from other derived keys.
    /// Use this when you need a separate key for a specific purpose (e.g., HMAC signing).
    pub fn derive_subkey(&self, domain: &str) -> [u8; 32] {
        use hmac::{Hmac, KeyInit, Mac};
        type HmacSha256 = Hmac<Sha256>;

        #[allow(
            clippy::expect_used,
            reason = "HMAC accepts keys of any length (long keys are hashed, short ones padded), \
                      so new_from_slice never returns InvalidLength"
        )]
        let mut mac = HmacSha256::new_from_slice(self.master_key.as_slice())
            .expect("HMAC can take key of any size");
        mac.update(domain.as_bytes());
        let result = mac.finalize().into_bytes();

        let mut key = [0u8; 32];
        key.copy_from_slice(&result);
        key
    }

    /// Generates a random encryption key as base64 string
    pub fn generate_key() -> Result<String, EncryptionError> {
        let mut key = [0u8; 32];
        crate::ecies::fill_secure_random_bytes("generating a base64 encryption key", &mut key)?;
        Ok(BASE64.encode(key))
    }

    /// Generates a random 32-byte key as hex string (for direct use with new())
    pub fn generate_raw_key() -> Result<String, EncryptionError> {
        let mut key = [0u8; 32];
        crate::ecies::fill_secure_random_bytes(
            "generating a hexadecimal encryption key",
            &mut key,
        )?;
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

    const TEST_KEY: &str = "12345678901234567890123456789012";

    #[test]
    fn aad_round_trips_and_binds_the_ciphertext_to_its_associated_data() {
        let service = EncryptionService::new(TEST_KEY).unwrap();
        let sealed = service
            .encrypt_with_aad(b"payload", b"run:1/step:2/result")
            .unwrap();
        assert_eq!(
            service
                .decrypt_with_aad(&sealed, b"run:1/step:2/result")
                .unwrap(),
            b"payload"
        );
        // Moved to another step, the value no longer opens.
        assert!(matches!(
            service.decrypt_with_aad(&sealed, b"run:1/step:3/result"),
            Err(EncryptionError::Decrypt)
        ));
        // Nor does the AAD-free decrypt path read it.
        assert!(matches!(
            service.decrypt(&sealed),
            Err(EncryptionError::Decrypt)
        ));
    }

    #[test]
    fn aad_decrypt_reports_truncated_input() {
        let service = EncryptionService::new(TEST_KEY).unwrap();
        assert!(matches!(
            service.decrypt_with_aad("AAAA", b"x"),
            Err(EncryptionError::Truncated { length: 3 })
        ));
    }

    fn service() -> EncryptionService {
        EncryptionService::new(TEST_KEY).unwrap()
    }

    /// The master key as a `Debug`-printed byte array.
    fn debug_key_bytes(key: &str) -> String {
        format!("{:?}", key.as_bytes())
            .trim_matches(['[', ']'])
            .to_string()
    }

    #[test]
    fn debug_output_redacts_the_master_key() {
        let debug = format!("{:?}", service());

        assert_eq!(debug, r#"EncryptionService { master_key: "***" }"#);
        assert!(!debug.contains(TEST_KEY));
        assert!(!debug.contains(&debug_key_bytes(TEST_KEY)));
    }

    #[test]
    fn encryption_error_messages_are_pinned() {
        let cases: Vec<(EncryptionError, &str)> = vec![
            (
                EncryptionError::InvalidKeyLength,
                "Master key must be exactly 32 bytes or 64 hex characters",
            ),
            (
                EncryptionError::InvalidHexKey { position: 7 },
                "Invalid hex key: invalid character at position 7",
            ),
            (
                EncryptionError::Random(crate::ecies::EciesError::RandomnessFailed {
                    operation: "generating an encryption nonce".to_string(),
                    reason: "entropy source unavailable".to_string(),
                }),
                "OS randomness failed while generating an encryption nonce: \
                 entropy source unavailable",
            ),
            (EncryptionError::Encrypt, "Encryption error: aead::Error"),
            (
                EncryptionError::InvalidBase64 {
                    reason: "Invalid padding".to_string(),
                },
                "Base64 decode error: Invalid padding",
            ),
            (
                EncryptionError::Truncated { length: 3 },
                "Invalid encrypted data",
            ),
            (
                EncryptionError::InvalidNonce {
                    reason: "wrong length".to_string(),
                },
                "Invalid encryption nonce: wrong length",
            ),
            (EncryptionError::Decrypt, "Decryption failed: aead::Error"),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn invalid_hex_key_reports_the_position_but_not_the_character() {
        // A 64-byte key whose character at position 3 is not hex.
        let key = format!("abcZ{}", "a".repeat(60));
        let error = EncryptionService::new(&key).unwrap_err();

        assert!(matches!(
            error,
            EncryptionError::InvalidHexKey { position: 3 }
        ));
        assert_eq!(
            error.to_string(),
            "Invalid hex key: invalid character at position 3"
        );
        assert!(!error.to_string().contains('Z'));
        assert!(!format!("{error:?}").contains('Z'));
    }

    #[test]
    fn invalid_key_length_error_does_not_echo_the_key() {
        let error = EncryptionService::new("short-secret-key").unwrap_err();

        assert!(matches!(error, EncryptionError::InvalidKeyLength));
        assert!(!error.to_string().contains("short-secret-key"));
        assert!(!format!("{error:?}").contains("short-secret-key"));
    }

    #[test]
    fn base64_errors_never_name_the_rejected_symbol() {
        let cases = [
            (
                DecodeError::InvalidByte(6, b'!'),
                "Invalid symbol at offset 6.",
            ),
            (DecodeError::InvalidLength(5), "Invalid input length: 5"),
            (
                DecodeError::InvalidLastSymbol {
                    offset: 2,
                    symbol: b'R',
                    symbol_value: 17,
                },
                "Invalid last symbol at offset 2.",
            ),
            (DecodeError::InvalidPadding, "Invalid padding"),
        ];
        for (error, expected) in cases {
            assert_eq!(describe_base64_error(&error), expected);
        }
    }

    #[test]
    fn decrypting_a_plaintext_by_mistake_does_not_leak_its_bytes() {
        // `!` is byte 33. The upstream message would be "Invalid symbol 33, offset 6."
        let error = service().decrypt("secret!value").unwrap_err();

        assert_eq!(
            error.to_string(),
            "Base64 decode error: Invalid symbol at offset 6."
        );
        let debug = format!("{error:?}");
        assert!(!error.to_string().contains("33"), "{error}");
        assert!(!debug.contains("33"), "{debug}");
        assert!(!debug.contains("secret"), "{debug}");
    }

    #[test]
    fn decrypt_with_the_wrong_key_leaks_neither_key_nor_plaintext() {
        let plaintext = "hunter2-plaintext";
        let encrypted = service().encrypt_string(plaintext).unwrap();
        let other_key = "abcdefghijklmnopqrstuvwxyz012345";
        let error = EncryptionService::new(other_key)
            .unwrap()
            .decrypt_string(&encrypted)
            .unwrap_err();

        assert!(matches!(error, EncryptionError::Decrypt));
        assert_eq!(error.to_string(), "Decryption failed: aead::Error");
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains(plaintext), "{text}");
            assert!(!text.contains(other_key), "{text}");
            assert!(!text.contains(TEST_KEY), "{text}");
            assert!(!text.contains(&debug_key_bytes(other_key)), "{text}");
            assert!(!text.contains(&encrypted), "{text}");
        }
    }

    #[test]
    fn truncated_ciphertext_reports_its_length() {
        let error = service().decrypt(&BASE64.encode([1u8, 2, 3])).unwrap_err();

        assert!(matches!(error, EncryptionError::Truncated { length: 3 }));
        assert_eq!(error.to_string(), "Invalid encrypted data");
    }

    #[test]
    fn non_utf8_plaintext_error_does_not_hold_the_decrypted_bytes() {
        let plaintext = b"hunter2-plaintext\xff";
        let encrypted = service().encrypt(plaintext).unwrap();
        let error = service().decrypt_string(&encrypted).unwrap_err();

        assert_eq!(
            error.to_string(),
            "UTF-8 decode failed: invalid utf-8 sequence of 1 bytes from index 17"
        );
        let debug = format!("{error:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        // `FromUtf8Error` would print the bytes as a number list.
        assert!(!debug.contains("104, 117, 110"), "{debug}");
    }

    #[test]
    fn password_decrypt_encoding_errors_never_name_the_rejected_symbol() {
        let legacy =
            EncryptionService::decrypt_with_password("password", "secret!value").unwrap_err();
        assert_eq!(
            legacy.to_string(),
            "Password-encrypted value (legacy-sha256 format) is not valid base64: \
             Invalid symbol at offset 6."
        );

        let versioned = EncryptionService::decrypt_with_password(
            "password",
            &format!("{PASSWORD_CIPHERTEXT_V1_PREFIX}secret!value"),
        )
        .unwrap_err();
        assert_eq!(
            versioned.to_string(),
            "Password-encrypted value (argon2id-v1 format) is not valid base64: \
             Invalid symbol at offset 6."
        );
    }

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
