//! HMAC generation and verification for click tracking URLs
//!
//! Prevents open redirects by cryptographically binding each redirect URL
//! to the specific email that contained it.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Generate a tracking HMAC for a given email_id and URL.
///
/// Returns a hex-encoded HMAC-SHA256 truncated to 32 chars.
pub fn generate_tracking_hmac(key: &[u8], email_id: &str, url: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(email_id.as_bytes());
    mac.update(b"|");
    mac.update(url.as_bytes());
    let result = mac.finalize();
    let code_bytes = result.into_bytes();
    // Truncate to 16 bytes (32 hex chars) for shorter URLs
    hex::encode(&code_bytes[..16])
}

/// Verify a tracking HMAC matches the expected value for a given email_id and URL.
pub fn verify_tracking_hmac(key: &[u8], email_id: &str, url: &str, expected: &str) -> bool {
    let computed = generate_tracking_hmac(key, email_id, url);
    // Constant-time comparison to prevent timing attacks
    constant_time_eq(computed.as_bytes(), expected.as_bytes())
}

/// Constant-time byte comparison
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &[u8] = b"test-hmac-key-for-email-tracking";

    #[test]
    fn test_generate_hmac_is_deterministic() {
        let hmac1 = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com");
        let hmac2 = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com");
        assert_eq!(hmac1, hmac2);
    }

    #[test]
    fn test_generate_hmac_is_32_hex_chars() {
        let hmac = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com");
        assert_eq!(hmac.len(), 32);
        assert!(hmac.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_different_emails_produce_different_hmacs() {
        let hmac1 = generate_tracking_hmac(TEST_KEY, "email-1", "https://example.com");
        let hmac2 = generate_tracking_hmac(TEST_KEY, "email-2", "https://example.com");
        assert_ne!(hmac1, hmac2);
    }

    #[test]
    fn test_different_urls_produce_different_hmacs() {
        let hmac1 = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com/a");
        let hmac2 = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com/b");
        assert_ne!(hmac1, hmac2);
    }

    #[test]
    fn test_different_keys_produce_different_hmacs() {
        let hmac1 = generate_tracking_hmac(
            b"key1-padded-to-be-long-enough-xx",
            "abc-123",
            "https://example.com",
        );
        let hmac2 = generate_tracking_hmac(
            b"key2-padded-to-be-long-enough-xx",
            "abc-123",
            "https://example.com",
        );
        assert_ne!(hmac1, hmac2);
    }

    #[test]
    fn test_verify_hmac_success() {
        let hmac = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com");
        assert!(verify_tracking_hmac(
            TEST_KEY,
            "abc-123",
            "https://example.com",
            &hmac
        ));
    }

    #[test]
    fn test_verify_hmac_wrong_url() {
        let hmac = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com");
        assert!(!verify_tracking_hmac(
            TEST_KEY,
            "abc-123",
            "https://evil.com",
            &hmac
        ));
    }

    #[test]
    fn test_verify_hmac_wrong_email() {
        let hmac = generate_tracking_hmac(TEST_KEY, "abc-123", "https://example.com");
        assert!(!verify_tracking_hmac(
            TEST_KEY,
            "wrong-id",
            "https://example.com",
            &hmac
        ));
    }

    #[test]
    fn test_verify_hmac_tampered() {
        assert!(!verify_tracking_hmac(
            TEST_KEY,
            "abc-123",
            "https://example.com",
            "0000000000000000000000000000000"
        ));
    }

    #[test]
    fn test_verify_hmac_wrong_length() {
        assert!(!verify_tracking_hmac(
            TEST_KEY,
            "abc-123",
            "https://example.com",
            "too-short"
        ));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
    }
}
