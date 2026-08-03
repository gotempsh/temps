//! Short-lived platform-session handoffs for sandbox previews.
//!
//! A sandbox preview can be protected by a password. Sharing such a preview
//! used to mean sharing that password — which grants indefinite access, is the
//! same secret for every recipient, and can only be withdrawn by rotating it
//! for everyone at once.
//!
//! A *grant* is the alternative: an AES-GCM authenticated payload naming one
//! sandbox and an expiry, minted by an authenticated control-plane route and
//! exchanged by the proxy for the ordinary preview cookie. It is never
//! forwarded to the sandbox, so preview application code cannot read it.
//!
//! This lives in `temps-core` rather than in the proxy because both sides need
//! it: the proxy verifies, the sandbox API mints, and neither should depend on
//! the other. `CookieCrypto` — the key material both already share — lives
//! here too.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{CookieCrypto, CryptoError};

/// Domain-separation prefix. Keeps grants from being interchangeable with any
/// other payload encrypted under the same key.
pub const PREVIEW_SESSION_GRANT_VERSION: &str = "preview-session-v2";

/// Default lifetime — long enough to cross an auto-submit bridge in the same
/// session, short enough that a leaked one is near-worthless.
pub const PREVIEW_SESSION_GRANT_TTL: Duration = Duration::from_secs(60);

/// Longest share window a minted grant may be given.
///
/// A grant is a bearer credential: whoever holds the link is inside until it
/// expires, and there is no per-grant revocation short of rotating the preview
/// password. A day is the most one link should carry.
pub const PREVIEW_SESSION_GRANT_MAX_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Error)]
pub enum PreviewGrantError {
    #[error("Preview grant subject '{subject}' contains the reserved field separator")]
    InvalidSubject { subject: String },

    #[error("Preview grant expiry overflowed the system clock for sandbox {subject}")]
    ExpiryOverflow { subject: String },

    #[error(
        "System clock is before the Unix epoch while minting a preview grant for sandbox {subject}"
    )]
    ClockBeforeUnixEpoch { subject: String },

    #[error("Failed to encrypt preview grant for sandbox {subject}: {source}")]
    Encryption {
        subject: String,
        #[source]
        source: CryptoError,
    },
}

fn password_fingerprint(password_hash: &str) -> String {
    let digest = Sha256::digest(password_hash.as_bytes());
    hex::encode(digest)
}

/// Keep post-login redirects on the preview origin.
///
/// Backslashes are rejected because WHATWG URL parsing treats them as path
/// separators for HTTP(S); `/\\evil.example` therefore becomes an external
/// redirect even though it begins with a single slash.
pub fn sanitize_preview_next(next: &str) -> String {
    if next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.chars().any(char::is_control)
    {
        next.to_string()
    } else {
        "/".to_string()
    }
}

/// Mint a grant for one sandbox.
///
/// `ttl` is clamped to [`PREVIEW_SESSION_GRANT_MAX_TTL`]. It is a parameter
/// rather than a constant because the two callers differ: a same-session
/// bridge wants [`PREVIEW_SESSION_GRANT_TTL`], while a link sent to a reviewer
/// has to outlive the click.
pub fn encode_preview_session_grant(
    crypto: &CookieCrypto,
    subject: &str,
    password_hash: &str,
    ttl: Duration,
    now: SystemTime,
) -> Result<(String, u64), PreviewGrantError> {
    // `|` is the field separator, so a subject containing one could shift the
    // expiry field and mint itself an arbitrary lifetime. Sandbox public ids
    // never contain it — refuse rather than rely on that holding.
    if subject.contains('|') {
        return Err(PreviewGrantError::InvalidSubject {
            subject: subject.to_string(),
        });
    }
    let exp = now
        .checked_add(ttl.min(PREVIEW_SESSION_GRANT_MAX_TTL))
        .ok_or_else(|| PreviewGrantError::ExpiryOverflow {
            subject: subject.to_string(),
        })?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PreviewGrantError::ClockBeforeUnixEpoch {
            subject: subject.to_string(),
        })?
        .as_secs();
    let payload = format!(
        "{}|{}|{}|{}",
        PREVIEW_SESSION_GRANT_VERSION,
        subject,
        password_fingerprint(password_hash),
        exp
    );
    let grant = crypto
        .encrypt(&payload)
        .map_err(|source| PreviewGrantError::Encryption {
            subject: subject.to_string(),
            source,
        })?;
    Ok((grant, exp))
}

/// Validate a grant against the sandbox it must name.
///
/// Returns false unless the payload decrypts under this key, carries the
/// version prefix, names exactly `subject`, has no trailing fields, and has
/// not expired.
pub fn verify_preview_session_grant(
    crypto: &CookieCrypto,
    grant: &str,
    subject: &str,
    password_hash: &str,
    now: SystemTime,
) -> bool {
    preview_session_grant_fingerprint(crypto, grant, subject, now)
        .is_some_and(|fingerprint| fingerprint == password_fingerprint(password_hash))
}

/// Authenticate the self-contained parts of a grant before callers perform
/// any database lookup needed for revocation checking.
///
/// This deliberately does not accept the grant: callers must still compare it
/// with the current password hash via [`verify_preview_session_grant`]. It only
/// prevents random public input from amplifying into an uncached database read.
pub fn validate_preview_session_grant_envelope(
    crypto: &CookieCrypto,
    grant: &str,
    subject: &str,
    now: SystemTime,
) -> bool {
    preview_session_grant_fingerprint(crypto, grant, subject, now).is_some()
}

fn preview_session_grant_fingerprint(
    crypto: &CookieCrypto,
    grant: &str,
    subject: &str,
    now: SystemTime,
) -> Option<String> {
    let Ok(plain) = crypto.decrypt(grant) else {
        return None;
    };
    let mut parts = plain.split('|');
    if parts.next() != Some(PREVIEW_SESSION_GRANT_VERSION) || parts.next() != Some(subject) {
        return None;
    }
    let fingerprint = parts.next()?;
    if fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let exp = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let now_secs = now.duration_since(UNIX_EPOCH).ok()?;
    (now_secs.as_secs() <= exp).then(|| fingerprint.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> CookieCrypto {
        CookieCrypto::new("default-32-byte-key-for-testing!").unwrap()
    }

    #[test]
    fn round_trips_and_is_scoped_to_one_sandbox() {
        let crypto = crypto();
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let password_hash = "argon2-current";
        let (grant, expires_at) = encode_preview_session_grant(
            &crypto,
            "sbx_7702c56bfb804b49",
            password_hash,
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .unwrap();
        assert_eq!(expires_at, 1_000 + PREVIEW_SESSION_GRANT_TTL.as_secs());

        assert!(verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            password_hash,
            now
        ));
        // A grant for one sandbox must not open another.
        assert!(!verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_c5d8e38f791dbc40",
            password_hash,
            now
        ));
        assert!(!verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            password_hash,
            now + PREVIEW_SESSION_GRANT_TTL + Duration::from_secs(1)
        ));
    }

    #[test]
    fn ttl_is_clamped_not_honoured_or_rejected() {
        let crypto = crypto();
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (grant, expires_at) = encode_preview_session_grant(
            &crypto,
            "sbx_7702c56bfb804b49",
            "argon2-current",
            PREVIEW_SESSION_GRANT_MAX_TTL * 30,
            now,
        )
        .unwrap();
        assert_eq!(expires_at, 1_000 + PREVIEW_SESSION_GRANT_MAX_TTL.as_secs());

        assert!(verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            "argon2-current",
            now + PREVIEW_SESSION_GRANT_MAX_TTL
        ));
        assert!(!verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            "argon2-current",
            now + PREVIEW_SESSION_GRANT_MAX_TTL + Duration::from_secs(1)
        ));
    }

    #[test]
    fn refuses_a_subject_carrying_the_field_separator() {
        let crypto = crypto();
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(encode_preview_session_grant(
            &crypto,
            "sbx_7702c56bfb804b49|9999999999",
            "argon2-current",
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .is_err());
    }

    #[test]
    fn rejects_a_payload_encrypted_under_another_key() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let other = CookieCrypto::new("another-32-byte-key-for-testing!").unwrap();
        let (grant, _) = encode_preview_session_grant(
            &other,
            "sbx_7702c56bfb804b49",
            "argon2-current",
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .unwrap();

        assert!(!verify_preview_session_grant(
            &crypto(),
            &grant,
            "sbx_7702c56bfb804b49",
            "argon2-current",
            now
        ));
    }

    #[test]
    fn password_rotation_revokes_an_unredeemed_grant() {
        let crypto = crypto();
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (grant, _) = encode_preview_session_grant(
            &crypto,
            "sbx_7702c56bfb804b49",
            "argon2-before-rotation",
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .unwrap();

        assert!(!verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            "argon2-after-rotation",
            now,
        ));
    }

    #[test]
    fn envelope_validation_rejects_random_wrong_subject_and_expired_input() {
        let crypto = crypto();
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (grant, _) = encode_preview_session_grant(
            &crypto,
            "sbx_7702c56bfb804b49",
            "argon2-current",
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .unwrap();

        assert!(validate_preview_session_grant_envelope(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            now,
        ));
        assert!(!validate_preview_session_grant_envelope(
            &crypto,
            "random-public-input",
            "sbx_7702c56bfb804b49",
            now,
        ));
        assert!(!validate_preview_session_grant_envelope(
            &crypto,
            &grant,
            "sbx_c5d8e38f791dbc40",
            now,
        ));
        assert!(!validate_preview_session_grant_envelope(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            now + PREVIEW_SESSION_GRANT_TTL + Duration::from_secs(1),
        ));
    }

    #[test]
    fn sanitize_next_rejects_cross_origin_and_header_injection_shapes() {
        assert_eq!(
            sanitize_preview_next("/pricing?plan=pro"),
            "/pricing?plan=pro"
        );
        assert_eq!(sanitize_preview_next("//evil.example"), "/");
        assert_eq!(sanitize_preview_next("/\\evil.example"), "/");
        assert_eq!(
            sanitize_preview_next("/safe\r\nLocation: //evil.example"),
            "/"
        );
        assert_eq!(sanitize_preview_next("https://evil.example"), "/");
    }
}
