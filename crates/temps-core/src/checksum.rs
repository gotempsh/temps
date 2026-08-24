//! SHA-256 checksum verification for downloaded artifacts.
//!
//! Used by both self-update and external plugin install flows.

use sha2::{Digest, Sha256};

/// Verify the SHA-256 checksum of `data`.
///
/// `checksum_text` must be in sha256sum format: `"<hex-hash>  <filename>"` or
/// `"<hex-hash> <filename>"`. Only the first whitespace-separated token is
/// read; the filename is ignored. Comparison is case-insensitive.
///
/// Returns `Ok(())` if the computed digest matches, or an error with both the
/// expected and actual digest if they differ.
pub fn verify_checksum(data: &[u8], checksum_text: &str) -> anyhow::Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let computed = hex::encode(hasher.finalize());

    // Checksum file format: "<hash>  <filename>" or "<hash> <filename>"
    let expected = checksum_text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid checksum file format"))?
        .to_lowercase();

    // Validate the shape before comparing. Without this, a manifest carrying a
    // truncated or non-hex digest — or an empty one, where the filename token
    // is what `split_whitespace` returns — reports a plain "mismatch", which
    // reads as "the download was tampered with" rather than "the registry
    // published a malformed entry". Both fail closed; only the diagnosis
    // differs, and that difference decides whether an operator retries or
    // escalates.
    const SHA256_HEX_LEN: usize = 64;
    if expected.len() != SHA256_HEX_LEN || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow::anyhow!(
            "Malformed SHA-256 checksum: expected {} hex characters, got {:?}",
            SHA256_HEX_LEN,
            expected
        ));
    }

    if computed != expected {
        return Err(anyhow::anyhow!(
            "Checksum mismatch!\n  Expected: {}\n  Got:      {}",
            expected,
            computed
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_checksum_matches() {
        let data = b"hello world";
        let hash = {
            let mut h = Sha256::new();
            h.update(data);
            hex::encode(h.finalize())
        };
        let checksum_text = format!("{}  hello.txt", hash);
        assert!(verify_checksum(data, &checksum_text).is_ok());
    }

    #[test]
    fn mismatched_checksum_returns_error() {
        let data = b"hello world";
        let checksum_text =
            "0000000000000000000000000000000000000000000000000000000000000000  hello.txt";
        let result = verify_checksum(data, checksum_text);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Checksum mismatch"));
    }

    #[test]
    fn empty_checksum_text_returns_error() {
        let result = verify_checksum(b"data", "");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid checksum file format"));
    }

    #[test]
    fn malformed_hash_is_reported_as_malformed_not_mismatch() {
        // Too short, and non-hex. Both must be distinguishable from a genuine
        // mismatch so an operator knows the registry is wrong, not the wire.
        for bad in ["abc123  f.txt", "zzzz  f.txt"] {
            let err = verify_checksum(b"data", bad).unwrap_err().to_string();
            assert!(
                err.contains("Malformed SHA-256 checksum"),
                "expected a malformed-checksum error for {bad:?}, got: {err}"
            );
        }
    }

    #[test]
    fn empty_hash_field_does_not_fall_through_to_the_filename() {
        // `format!("{}  {}", "", name)` leaves leading whitespace, so the
        // first token is the *filename*. It must be rejected as malformed
        // rather than silently compared against the digest.
        let err = verify_checksum(b"data", "  my-plugin-binary")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Malformed SHA-256 checksum"), "got: {err}");
    }

    #[test]
    fn uppercase_hash_matches() {
        let data = b"hello";
        let hash = {
            let mut h = Sha256::new();
            h.update(data);
            hex::encode(h.finalize()).to_uppercase()
        };
        let checksum_text = format!("{}  hello.txt", hash);
        assert!(verify_checksum(data, &checksum_text).is_ok());
    }
}
