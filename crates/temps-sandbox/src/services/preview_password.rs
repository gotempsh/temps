// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional per-sandbox preview password.
//!
//! Sandboxes default to "URL-only" protection: the 16-hex public_id is the
//! only thing standing between the caller and the container. Users who need
//! a stronger gate can set a password via `PUT /v1/sandbox/{id}/preview-password`.
//! When set, the proxy's `check_preview_auth` path requires a cookie minted
//! against the stored argon2 hash before forwarding traffic.
//!
//! Unlike workspace sessions, sandbox passwords are **user-supplied**, not
//! generated. The user picks the password; we never see it again after
//! hashing. Min-length is enforced at the HTTP boundary.
//!
//! Persistence lives in `SandboxService`. This module is pure crypto.

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

/// Minimum plaintext length we'll accept. Short enough to not annoy users
/// who just want a shared team password, long enough that online brute force
/// against the Pingora rate limiter isn't practical. Offline attackers face
/// argon2id.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Maximum plaintext length. argon2 itself has no meaningful upper bound,
/// but capping here prevents a malicious client from forcing the server to
/// hash gigabyte-long "passwords".
pub const MAX_PASSWORD_LEN: usize = 256;

/// Outcome of hashing a user-supplied password.
pub struct HashedPassword {
    /// Argon2 PHC string. Persist in `preview_password_hash`.
    pub hash: String,
    /// Last 4 characters of the plaintext. Safe to display in the UI.
    pub hint: String,
}

/// Validate a user-supplied plaintext against the length rules. Returns a
/// descriptive error string on rejection so the handler can surface it
/// directly in a Problem Details `detail`.
pub fn validate(plaintext: &str) -> Result<(), String> {
    let len = plaintext.chars().count();
    if len < MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {} characters",
            MIN_PASSWORD_LEN
        ));
    }
    if len > MAX_PASSWORD_LEN {
        return Err(format!(
            "password must be at most {} characters",
            MAX_PASSWORD_LEN
        ));
    }
    Ok(())
}

/// Hash a validated plaintext. Caller is responsible for calling
/// [`validate`] first — this function trusts its input.
pub fn hash_password(plaintext: &str) -> Result<HashedPassword, String> {
    let hash = Argon2::default()
        .hash_password(plaintext.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| format!("argon2 hash failed: {}", e))?;
    Ok(HashedPassword {
        hash,
        hint: last_four(plaintext),
    })
}

/// Verify a plaintext attempt against a stored argon2 PHC hash.
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch, `Err` only if
/// the stored hash string is malformed.
pub fn verify_password(attempt: &str, stored_hash: &str) -> Result<bool, String> {
    let parsed =
        PasswordHash::new(stored_hash).map_err(|e| format!("stored hash is invalid: {}", e))?;
    Ok(Argon2::default()
        .verify_password(attempt.as_bytes(), &parsed)
        .is_ok())
}

fn last_four(s: &str) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(4)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_too_short() {
        assert!(validate("short").is_err());
        assert!(validate("1234567").is_err());
    }

    #[test]
    fn validate_accepts_min_length() {
        assert!(validate("12345678").is_ok());
    }

    #[test]
    fn validate_rejects_too_long() {
        let too_long: String = "a".repeat(MAX_PASSWORD_LEN + 1);
        assert!(validate(&too_long).is_err());
    }

    #[test]
    fn hash_round_trip() {
        let hp = hash_password("correct-horse-battery-staple").expect("hash");
        assert!(hp.hash.starts_with("$argon2"));
        assert_eq!(hp.hint.chars().count(), 4);
        assert!(verify_password("correct-horse-battery-staple", &hp.hash).unwrap());
        assert!(!verify_password("wrong-password", &hp.hash).unwrap());
    }

    #[test]
    fn hash_is_salted() {
        // Same plaintext → different hashes (different salts).
        let a = hash_password("same-input-12345").unwrap();
        let b = hash_password("same-input-12345").unwrap();
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn hint_is_last_four() {
        let hp = hash_password("abcdefghij").unwrap();
        assert_eq!(hp.hint, "ghij");
    }

    #[test]
    fn malformed_stored_hash_errors() {
        assert!(verify_password("anything", "not-a-phc-string").is_err());
    }
}
