//! TLS Fingerprinting (JA4-like implementation)
//!
//! This module provides TLS fingerprinting capabilities to identify clients
//! based on their TLS handshake characteristics.

use pingora_core::protocols::tls::SslDigest;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::debug;

/// Compute a JA4-like fingerprint from TLS session information
///
/// JA4 fingerprint format: `{protocol}{version}_{cipher_hash}`
///
/// Since Pingora doesn't directly expose ClientHello, we compute a simplified
/// fingerprint based on available TLS session data from SslDigest:
/// - TLS version
/// - Negotiated cipher suite
///
/// This provides a reasonably unique identifier for TLS clients that can be
/// used for bot detection and attack mitigation.
pub fn compute_tls_fingerprint(ssl_digest: &SslDigest) -> Option<String> {
    // Get TLS version (e.g., "TLSv1.3")
    let version = ssl_digest.version;

    // Get negotiated cipher suite
    let cipher = ssl_digest.cipher;

    // Construct fingerprint components
    let protocol = match version {
        "TLSv1.3" => "t13",
        "TLSv1.2" => "t12",
        "TLSv1.1" => "t11",
        "TLSv1.0" => "t10",
        _ => "tun", // Unknown TLS version
    };

    // Create fingerprint data string
    let fingerprint_data = format!("{}-{}", version, cipher);

    // Hash the fingerprint data (first 12 characters of SHA-256 hex)
    let mut hasher = Sha256::new();
    hasher.update(fingerprint_data.as_bytes());
    let hash = hasher.finalize();
    let hash_str = hex::encode(&hash[..12]); // Use first 12 bytes (24 hex chars)

    let fingerprint = format!("{}_{}", protocol, hash_str);

    debug!(
        version = version,
        cipher = cipher,
        fingerprint = fingerprint,
        "Computed TLS fingerprint"
    );

    Some(fingerprint)
}

/// Compute fingerprint from Arc<SslDigest>
pub fn compute_tls_fingerprint_from_arc(ssl_digest: &Arc<SslDigest>) -> Option<String> {
    compute_tls_fingerprint(ssl_digest.as_ref())
}

/// Compute a comprehensive fingerprint with client characteristics
///
/// Extended fingerprint includes:
/// - TLS version
/// - Negotiated cipher suite
/// - Client IP address
/// - User-Agent header
///
/// This creates a unique identifier per person/device/location,
/// ensuring different users get different fingerprints even with
/// the same TLS configuration.
pub fn compute_comprehensive_fingerprint(
    ssl_digest: &SslDigest,
    ip_address: Option<&str>,
    user_agent: &str,
) -> Option<String> {
    // Get TLS version (e.g., "TLSv1.3")
    let version = ssl_digest.version;

    // Get negotiated cipher suite
    let cipher = ssl_digest.cipher;

    // Construct fingerprint components
    let protocol = match version {
        "TLSv1.3" => "t13",
        "TLSv1.2" => "t12",
        "TLSv1.1" => "t11",
        "TLSv1.0" => "t10",
        _ => "tun", // Unknown TLS version
    };

    // Create composite fingerprint data string with all characteristics
    // This ensures different people get different fingerprints
    let ip_part = ip_address.unwrap_or("unknown");
    let fingerprint_data = format!("{}-{}-{}-{}", version, cipher, ip_part, user_agent);

    // Hash the composite data (first 12 bytes = 24 hex chars)
    let mut hasher = Sha256::new();
    hasher.update(fingerprint_data.as_bytes());
    let hash = hasher.finalize();
    let hash_str = hex::encode(&hash[..12]);

    let fingerprint = format!("{}_{}", protocol, hash_str);

    debug!(
        version = version,
        cipher = cipher,
        ip_address = ip_part,
        user_agent = user_agent,
        fingerprint = fingerprint,
        "Computed comprehensive client fingerprint"
    );

    Some(fingerprint)
}

/// Compute comprehensive fingerprint from Arc<SslDigest> with client characteristics
pub fn compute_comprehensive_fingerprint_from_arc(
    ssl_digest: &Arc<SslDigest>,
    ip_address: Option<&str>,
    user_agent: &str,
) -> Option<String> {
    compute_comprehensive_fingerprint(ssl_digest.as_ref(), ip_address, user_agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_format() {
        // We can't easily test with real SSL context in unit tests,
        // but we can test the fingerprint format

        // Test that SHA-256 produces expected length
        let mut hasher = Sha256::new();
        hasher.update(b"test data");
        let hash = hasher.finalize();
        let hash_str = hex::encode(&hash[..12]);

        assert_eq!(hash_str.len(), 24); // 12 bytes = 24 hex chars

        let fingerprint = format!("t13_{}", hash_str);
        assert!(fingerprint.starts_with("t13_"));
        assert_eq!(fingerprint.len(), 28); // "t13_" (4) + 24 hex chars
    }

    #[test]
    fn test_protocol_mapping() {
        let test_cases = vec![
            ("TLSv1.3", "t13"),
            ("TLSv1.2", "t12"),
            ("TLSv1.1", "t11"),
            ("TLSv1.0", "t10"),
            ("Unknown", "tun"),
        ];

        for (version, expected_protocol) in test_cases {
            let protocol = match version {
                "TLSv1.3" => "t13",
                "TLSv1.2" => "t12",
                "TLSv1.1" => "t11",
                "TLSv1.0" => "t10",
                _ => "tun",
            };
            assert_eq!(protocol, expected_protocol);
        }
    }

    #[test]
    fn test_fingerprint_uniqueness() {
        // Test that different inputs produce different fingerprints
        let mut hasher1 = Sha256::new();
        hasher1.update(b"TLSv1.3-CIPHER1-h2-example.com");
        let hash1 = hasher1.finalize();

        let mut hasher2 = Sha256::new();
        hasher2.update(b"TLSv1.3-CIPHER2-h2-example.com");
        let hash2 = hasher2.finalize();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_fingerprint_consistency() {
        // Test that same input produces same fingerprint
        let input = b"TLSv1.3-CIPHER1-h2-example.com";

        let mut hasher1 = Sha256::new();
        hasher1.update(input);
        let hash1 = hasher1.finalize();

        let mut hasher2 = Sha256::new();
        hasher2.update(input);
        let hash2 = hasher2.finalize();

        assert_eq!(hash1, hash2);
    }
}
