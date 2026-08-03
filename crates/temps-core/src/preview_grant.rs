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

use crate::CookieCrypto;

/// Domain-separation prefix. Keeps grants from being interchangeable with any
/// other payload encrypted under the same key.
pub const PREVIEW_SESSION_GRANT_VERSION: &str = "preview-session-v1";

/// Default lifetime — long enough to cross an auto-submit bridge in the same
/// session, short enough that a leaked one is near-worthless.
pub const PREVIEW_SESSION_GRANT_TTL: Duration = Duration::from_secs(60);

/// Longest share window a minted grant may be given.
///
/// A grant is a bearer credential: whoever holds the link is inside until it
/// expires, and there is no per-grant revocation short of rotating the preview
/// password. A day is the most one link should carry.
pub const PREVIEW_SESSION_GRANT_MAX_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Mint a grant for one sandbox.
///
/// `ttl` is clamped to [`PREVIEW_SESSION_GRANT_MAX_TTL`]. It is a parameter
/// rather than a constant because the two callers differ: a same-session
/// bridge wants [`PREVIEW_SESSION_GRANT_TTL`], while a link sent to a reviewer
/// has to outlive the click.
pub fn encode_preview_session_grant(
    crypto: &CookieCrypto,
    subject: &str,
    ttl: Duration,
    now: SystemTime,
) -> Option<String> {
    // `|` is the field separator, so a subject containing one could shift the
    // expiry field and mint itself an arbitrary lifetime. Sandbox public ids
    // never contain it — refuse rather than rely on that holding.
    if subject.contains('|') {
        return None;
    }
    let exp = now
        .checked_add(ttl.min(PREVIEW_SESSION_GRANT_MAX_TTL))?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let payload = format!("{}|{}|{}", PREVIEW_SESSION_GRANT_VERSION, subject, exp);
    crypto.encrypt(&payload).ok()
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
    now: SystemTime,
) -> bool {
    let Ok(plain) = crypto.decrypt(grant) else {
        return false;
    };
    let mut parts = plain.split('|');
    if parts.next() != Some(PREVIEW_SESSION_GRANT_VERSION) || parts.next() != Some(subject) {
        return false;
    }
    let Some(exp) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let Ok(now_secs) = now.duration_since(UNIX_EPOCH) else {
        return false;
    };
    now_secs.as_secs() <= exp
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
        let grant = encode_preview_session_grant(
            &crypto,
            "sbx_7702c56bfb804b49",
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .unwrap();

        assert!(verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            now
        ));
        // A grant for one sandbox must not open another.
        assert!(!verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_c5d8e38f791dbc40",
            now
        ));
        assert!(!verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            now + PREVIEW_SESSION_GRANT_TTL + Duration::from_secs(1)
        ));
    }

    #[test]
    fn ttl_is_clamped_not_honoured_or_rejected() {
        let crypto = crypto();
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let grant = encode_preview_session_grant(
            &crypto,
            "sbx_7702c56bfb804b49",
            PREVIEW_SESSION_GRANT_MAX_TTL * 30,
            now,
        )
        .unwrap();

        assert!(verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
            now + PREVIEW_SESSION_GRANT_MAX_TTL
        ));
        assert!(!verify_preview_session_grant(
            &crypto,
            &grant,
            "sbx_7702c56bfb804b49",
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
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .is_none());
    }

    #[test]
    fn rejects_a_payload_encrypted_under_another_key() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let other = CookieCrypto::new("another-32-byte-key-for-testing!").unwrap();
        let grant = encode_preview_session_grant(
            &other,
            "sbx_7702c56bfb804b49",
            PREVIEW_SESSION_GRANT_TTL,
            now,
        )
        .unwrap();

        assert!(!verify_preview_session_grant(
            &crypto(),
            &grant,
            "sbx_7702c56bfb804b49",
            now
        ));
    }
}
