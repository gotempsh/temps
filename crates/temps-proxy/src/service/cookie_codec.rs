//! Stateless cookie codec for visitor and session tracking.
//!
//! No database round-trips on the hot path:
//! - Visitor UUID is embedded directly in the cookie (encrypted with CookieCrypto).
//! - Session UUID + last-seen timestamp are embedded in a v2 payload
//!   (`v2|<uuid>|<unix_secs>`), so freshness is checked in-process.
//! - Legacy bare-UUID session cookies are accepted once and re-issued as v2
//!   on the next response.

use chrono::Utc;
use temps_core::CookieCrypto;
use uuid::Uuid;

/// The result of parsing a session cookie.
#[derive(Debug, Clone)]
pub struct SessionCookieDecision {
    /// UUID to use for this session.
    pub session_uuid: String,
    /// `true` if a new session was started (no valid cookie, or cookie expired).
    pub is_new_session: bool,
}

/// Parse the visitor cookie value and return the UUID to use.
///
/// - Cookie present and decrypts to a non-empty, non-v2 string → use that UUID unchanged.
/// - Cookie absent or decryption fails → generate a fresh `Uuid::new_v4()`.
pub fn parse_visitor_cookie(cookie_value: Option<&str>, crypto: &CookieCrypto) -> String {
    if let Some(value) = cookie_value {
        if let Ok(plaintext) = crypto.decrypt(value) {
            if !plaintext.is_empty() && !plaintext.starts_with("v2|") {
                return plaintext;
            }
        }
    }
    Uuid::new_v4().to_string()
}

/// Build the plaintext payload for a v2 session cookie.
///
/// The payload is `v2|<session_uuid>|<unix_secs>` where `unix_secs` is the
/// timestamp of the last observed page view. The batch writer persists this
/// timestamp asynchronously, but the freshness check uses the cookie value
/// directly — no DB round-trip needed.
pub fn make_v2_session_payload(session_uuid: &str, ts_secs: i64) -> String {
    format!("v2|{}|{}", session_uuid, ts_secs)
}

/// Parse the session cookie and decide whether the session is new or continuing.
///
/// Rules:
/// - Cookie absent or decryption fails → new session, fresh UUID.
/// - v2 payload (`v2|<uuid>|<ts>`) with `last_seen` within `session_max_age_minutes` → same session.
/// - v2 payload but stale → new session, fresh UUID.
/// - Legacy bare UUID → treat as a continuing session (unknown freshness); re-issued as v2 next response.
pub fn parse_session_cookie(
    cookie_value: Option<&str>,
    crypto: &CookieCrypto,
    session_max_age_minutes: i64,
) -> SessionCookieDecision {
    let Some(value) = cookie_value else {
        return new_session();
    };

    let plaintext = match crypto.decrypt(value) {
        Ok(p) => p,
        Err(_) => return new_session(),
    };

    if plaintext.starts_with("v2|") {
        parse_v2_session(&plaintext, session_max_age_minutes)
    } else {
        // Legacy bare-UUID cookie: accept without freshness check.
        // It will be re-issued as v2 on the next response via set_tracking_cookies.
        SessionCookieDecision {
            session_uuid: plaintext,
            is_new_session: false,
        }
    }
}

fn parse_v2_session(plaintext: &str, session_max_age_minutes: i64) -> SessionCookieDecision {
    // Format: "v2|<uuid>|<ts_secs>"
    let mut parts = plaintext.splitn(3, '|');
    let _prefix = parts.next(); // "v2"
    let session_uuid = match parts.next() {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => return new_session(),
    };
    let ts_secs: i64 = match parts.next().and_then(|s| s.parse().ok()) {
        Some(t) => t,
        None => return new_session(),
    };

    let now_secs = Utc::now().timestamp();
    let age_secs = now_secs.saturating_sub(ts_secs);
    let max_age_secs = session_max_age_minutes * 60;

    if age_secs <= max_age_secs {
        SessionCookieDecision {
            session_uuid,
            is_new_session: false,
        }
    } else {
        new_session()
    }
}

fn new_session() -> SessionCookieDecision {
    SessionCookieDecision {
        session_uuid: Uuid::new_v4().to_string(),
        is_new_session: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_crypto() -> CookieCrypto {
        CookieCrypto::new("default-32-byte-key-for-testing!")
            .expect("Failed to create cookie crypto")
    }

    // ── visitor cookie ───────────────────────────────────────────────────────

    #[test]
    fn test_parse_visitor_cookie_absent_generates_new_uuid() {
        let crypto = make_crypto();
        let uuid = parse_visitor_cookie(None, &crypto);
        assert!(!uuid.is_empty());
        assert_eq!(uuid.len(), 36, "expected UUID format");
    }

    #[test]
    fn test_parse_visitor_cookie_valid_returns_original() {
        let crypto = make_crypto();
        let original = Uuid::new_v4().to_string();
        let encrypted = crypto.encrypt(&original).unwrap();
        let result = parse_visitor_cookie(Some(&encrypted), &crypto);
        assert_eq!(result, original);
    }

    #[test]
    fn test_parse_visitor_cookie_tampered_generates_new_uuid() {
        let crypto = make_crypto();
        let result = parse_visitor_cookie(Some("tampered-garbage"), &crypto);
        assert_eq!(
            result.len(),
            36,
            "tampered cookie should produce fresh UUID"
        );
    }

    #[test]
    fn test_parse_visitor_cookie_empty_string_generates_new_uuid() {
        let crypto = make_crypto();
        let result = parse_visitor_cookie(Some(""), &crypto);
        assert_eq!(result.len(), 36);
    }

    // ── v2 session payload ───────────────────────────────────────────────────

    #[test]
    fn test_make_v2_session_payload_format() {
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let ts = 1234567890i64;
        let payload = make_v2_session_payload(uuid, ts);
        assert_eq!(
            payload,
            "v2|550e8400-e29b-41d4-a716-446655440000|1234567890"
        );
    }

    // ── session cookie ───────────────────────────────────────────────────────

    #[test]
    fn test_parse_session_cookie_absent_new_session() {
        let crypto = make_crypto();
        let d = parse_session_cookie(None, &crypto, 30);
        assert!(d.is_new_session);
        assert_eq!(d.session_uuid.len(), 36);
    }

    #[test]
    fn test_parse_session_cookie_tampered_new_session() {
        let crypto = make_crypto();
        let d = parse_session_cookie(Some("tampered"), &crypto, 30);
        assert!(d.is_new_session);
    }

    #[test]
    fn test_parse_session_cookie_v2_fresh_reuses_session() {
        let crypto = make_crypto();
        let session_uuid = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let payload = make_v2_session_payload(&session_uuid, now);
        let encrypted = crypto.encrypt(&payload).unwrap();

        let d = parse_session_cookie(Some(&encrypted), &crypto, 30);
        assert!(!d.is_new_session);
        assert_eq!(d.session_uuid, session_uuid);
    }

    #[test]
    fn test_parse_session_cookie_v2_expired_new_session() {
        let crypto = make_crypto();
        let session_uuid = Uuid::new_v4().to_string();
        // 31 minutes ago
        let old_ts = Utc::now().timestamp() - 31 * 60;
        let payload = make_v2_session_payload(&session_uuid, old_ts);
        let encrypted = crypto.encrypt(&payload).unwrap();

        let d = parse_session_cookie(Some(&encrypted), &crypto, 30);
        assert!(d.is_new_session);
        assert_ne!(
            d.session_uuid, session_uuid,
            "expired session should get new UUID"
        );
    }

    #[test]
    fn test_parse_session_cookie_v2_at_boundary_not_expired() {
        let crypto = make_crypto();
        let session_uuid = Uuid::new_v4().to_string();
        // Exactly 30 minutes ago (boundary is inclusive: age_secs <= max_age_secs)
        let boundary_ts = Utc::now().timestamp() - 30 * 60;
        let payload = make_v2_session_payload(&session_uuid, boundary_ts);
        let encrypted = crypto.encrypt(&payload).unwrap();

        let d = parse_session_cookie(Some(&encrypted), &crypto, 30);
        assert!(
            !d.is_new_session,
            "session at boundary should not be expired"
        );
        assert_eq!(d.session_uuid, session_uuid);
    }

    #[test]
    fn test_parse_session_cookie_legacy_uuid_accepted() {
        let crypto = make_crypto();
        let session_uuid = Uuid::new_v4().to_string();
        // Legacy format: bare UUID encrypted (no v2| prefix)
        let encrypted = crypto.encrypt(&session_uuid).unwrap();

        let d = parse_session_cookie(Some(&encrypted), &crypto, 30);
        assert!(!d.is_new_session, "legacy UUID cookie should be accepted");
        assert_eq!(d.session_uuid, session_uuid);
    }

    #[test]
    fn test_parse_session_cookie_v2_malformed_new_session() {
        let crypto = make_crypto();
        // Malformed v2 (missing ts segment)
        let payload = "v2|some-uuid";
        let encrypted = crypto.encrypt(payload).unwrap();
        let d = parse_session_cookie(Some(&encrypted), &crypto, 30);
        assert!(d.is_new_session, "malformed v2 should produce new session");
    }
}
