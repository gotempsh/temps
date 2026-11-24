// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bypass tokens (ADR 066, D7 and I2): what monitors, CI and API clients
//! send in [`HEADER`] so Shield never challenges them.
//!
//! A token reads `tst_<id>_<secret>`. Only its SHA-256 is stored, in the
//! database and in what workers receive, like node tokens; the id in the
//! clear lets the console record when a token was last used without
//! hashing anything twice.

use std::collections::HashSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

/// The request header that carries a bypass token.
pub const HEADER: &str = "x-temps-shield-token";

const PREFIX: &str = "tst_";
const SECRET_BYTES: usize = 32;

/// A new token for the row `id`, and the hash to store for it.
pub fn generate(id: i64) -> (String, [u8; 32]) {
    let secret: [u8; SECRET_BYTES] = rand::random();
    let token = format!("{PREFIX}{id}_{}", URL_SAFE_NO_PAD.encode(secret));
    let hash = hash(&token);
    (token, hash)
}

/// What is stored for `token`.
pub fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The row id a well-formed token names. Says nothing about whether the
/// token is valid.
pub fn token_id(token: &str) -> Option<i64> {
    let rest = token.strip_prefix(PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    let decoded = URL_SAFE_NO_PAD.decode(secret).ok()?;
    (decoded.len() == SECRET_BYTES).then_some(())?;
    id.parse().ok().filter(|id: &i64| *id > 0)
}

/// The hashes of the tokens valid for one environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenSet {
    hashes: HashSet<[u8; 32]>,
}

impl TokenSet {
    pub fn new(hashes: impl IntoIterator<Item = [u8; 32]>) -> Self {
        Self {
            hashes: hashes.into_iter().collect(),
        }
    }

    /// The hashes from their hex form, as the database stores them; a
    /// value that is not 64 hex digits is skipped.
    pub fn from_hex<S: AsRef<str>>(hexes: &[S]) -> Self {
        Self::new(hexes.iter().filter_map(|hex| parse_hex(hex.as_ref())))
    }

    /// The id of `token` when it is one of these.
    pub fn check(&self, token: &str) -> Option<i64> {
        if self.hashes.is_empty() {
            return None;
        }
        let id = token_id(token)?;
        self.hashes.contains(&hash(token)).then_some(id)
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    pub fn hashes(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.hashes.iter()
    }
}

/// `hash` as the lowercase hex the database stores.
pub fn to_hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_checks_against_its_hash_only() {
        let (token, stored) = generate(42);
        assert!(token.starts_with("tst_42_"));
        assert_eq!(token_id(&token), Some(42));
        let set = TokenSet::from_hex(&[to_hex(&stored)]);
        assert_eq!(set.check(&token), Some(42));
        let (other, _) = generate(42);
        assert_eq!(set.check(&other), None);
        assert_eq!(TokenSet::default().check(&token), None);
    }

    #[test]
    fn malformed_tokens_name_no_row() {
        for token in [
            "",
            "tst_",
            "tst_1_",
            "tst_x_aaaa",
            "tst_0_short",
            "tok_1_aaaa",
            "tst_-1_aaaa",
        ] {
            assert_eq!(token_id(token), None, "{token}");
        }
        assert!(TokenSet::from_hex(&["zz", "00"]).is_empty());
    }
}
