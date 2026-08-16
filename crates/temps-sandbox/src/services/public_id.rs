//! Opaque public IDs for sandboxes. Format: `sbx_` + 16 hex chars (8 bytes
//! of randomness). Chosen to be short enough to type in a URL but large
//! enough that guessing is infeasible.
//!
//! The public ID is what API callers see; internally the service also keeps
//! an `i32` primary key which is what the underlying `SandboxProvider`
//! indexes by. Never expose the numeric id in responses.

use rand::Rng;

pub const PUBLIC_ID_PREFIX: &str = "sbx_";

/// Generate a new opaque public sandbox ID.
pub fn generate() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("{}{}", PUBLIC_ID_PREFIX, hex::encode(bytes))
}

/// Generate a new opaque public ID with a custom prefix (e.g. `snap_`).
pub fn generate_with_prefix(prefix: &str) -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("{}_{}", prefix, hex::encode(bytes))
}

/// Returns true iff `s` has the expected `sbx_` prefix and hex suffix.
/// Used at the HTTP boundary to reject malformed path parameters before
/// they hit the DB.
pub fn is_valid(s: &str) -> bool {
    let Some(rest) = s.strip_prefix(PUBLIC_ID_PREFIX) else {
        return false;
    };
    rest.len() == 16 && rest.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_has_prefix() {
        let id = generate();
        assert!(id.starts_with(PUBLIC_ID_PREFIX));
    }

    #[test]
    fn generate_has_16_hex_chars() {
        let id = generate();
        let rest = id.strip_prefix(PUBLIC_ID_PREFIX).unwrap();
        assert_eq!(rest.len(), 16);
        assert!(rest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_is_unique() {
        // Very low probability of collision; catches a broken RNG.
        let a = generate();
        let b = generate();
        assert_ne!(a, b);
    }

    #[test]
    fn is_valid_accepts_generated() {
        let id = generate();
        assert!(is_valid(&id), "generated id {} rejected", id);
    }

    #[test]
    fn is_valid_rejects_wrong_prefix() {
        assert!(!is_valid("run_1234567890abcdef"));
        assert!(!is_valid("1234567890abcdef"));
    }

    #[test]
    fn is_valid_rejects_wrong_length() {
        assert!(!is_valid("sbx_short"));
        assert!(!is_valid("sbx_0123456789abcdef0"));
    }

    #[test]
    fn is_valid_rejects_non_hex() {
        assert!(!is_valid("sbx_zzzzzzzzzzzzzzzz"));
    }
}
