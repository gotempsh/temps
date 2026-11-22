// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Confirmation and unsubscribe tokens.
//!
//! Both are 256-bit values written as 64 lowercase hex characters, and only
//! their SHA-256 is stored. A confirmation token is random and exists in
//! plaintext only in the confirmation mail. An unsubscribe token is derived
//! from the server key and a random per-subscriber nonce
//! ([`temps_core::EncryptionService::derive_subkey`], HMAC-SHA256), so every
//! notice can carry the link without the token being stored anywhere.

use rand::TryCryptoRng;
use sha2::{Digest, Sha256};
use temps_core::EncryptionService;

use super::error::SubscriptionError;

/// Bytes in a token or nonce.
const TOKEN_BYTES: usize = 32;
/// Length of a token as it appears in a URL.
pub const TOKEN_LEN: usize = TOKEN_BYTES * 2;

/// Whether `candidate` has the shape of a token this module issues. Lets a
/// public handler refuse garbage without touching the database.
pub fn is_well_formed_token(candidate: &str) -> bool {
    candidate.len() == TOKEN_LEN
        && candidate
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn random_hex_with<R: TryCryptoRng>(rng: &mut R) -> Result<String, SubscriptionError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    rng.try_fill_bytes(&mut bytes)
        .map_err(|e| SubscriptionError::TokenGeneration {
            reason: e.to_string(),
        })?;
    Ok(hex::encode(bytes))
}

/// 256 random bits from the operating system's CSPRNG, as hex.
pub fn random_hex() -> Result<String, SubscriptionError> {
    random_hex_with(&mut rand::rngs::SysRng)
}

/// Hex SHA-256 of a token: the only form that is stored.
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The unsubscribe token of the subscriber whose nonce is `nonce`.
pub fn unsubscribe_token(encryption: &EncryptionService, nonce: &str) -> String {
    hex::encode(encryption.derive_subkey(&format!("status-page-unsubscribe:{nonce}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_64_lowercase_hex_and_distinct() {
        let first = random_hex().expect("CSPRNG");
        let second = random_hex().expect("CSPRNG");
        assert!(is_well_formed_token(&first));
        assert_ne!(first, second);
    }

    #[test]
    fn shape_check_rejects_anything_else() {
        for bad in [
            "",
            "abc",
            &"A".repeat(TOKEN_LEN),
            &"g".repeat(TOKEN_LEN),
            &"a".repeat(TOKEN_LEN + 1),
            &format!("{}/", "a".repeat(TOKEN_LEN - 1)),
        ] {
            assert!(!is_well_formed_token(bad), "{bad}");
        }
    }

    #[test]
    fn unsubscribe_tokens_are_stable_per_nonce_and_keyed() {
        let service = EncryptionService::new_from_password("subscriber-tokens-test");
        let token = unsubscribe_token(&service, "n1");
        assert!(is_well_formed_token(&token));
        assert_eq!(token, unsubscribe_token(&service, "n1"));
        assert_ne!(token, unsubscribe_token(&service, "n2"));

        let other = EncryptionService::new_from_password("another-server");
        assert_ne!(
            token,
            unsubscribe_token(&other, "n1"),
            "depends on the server key"
        );
        assert_ne!(token_hash(&token), token, "only the hash is stored");
        assert_eq!(token_hash(&token).len(), 64);
    }
}
