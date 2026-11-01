// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Small cryptographic helpers shared across crates.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Compare two byte strings for equality in time independent of their contents.
///
/// Use this for every comparison of a secret, a token, a MAC or a token hash
/// against a value the caller controls, so a match cannot be recovered a byte
/// at a time from response timing.
///
/// Inputs of different lengths compare unequal immediately: the length of a
/// value is not treated as secret (every caller compares fixed-length hashes,
/// MACs or tokens whose length is public). For equal-length inputs every byte
/// is examined. Two empty inputs are equal.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// SHA-256 of `input`'s UTF-8 bytes as 64 lowercase hex characters.
///
/// This is the stored form of node, deployment and ingest tokens: the
/// database keeps this digest and a presented token is hashed and compared
/// against it. The output must never change, or every stored token stops
/// matching; `sha256_hex_is_byte_identical_to_the_stored_token_hashes` pins it.
pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, sha256_hex};

    #[test]
    fn sha256_hex_is_byte_identical_to_the_stored_token_hashes() {
        // FIPS 180-2 test vectors, plus a token-shaped input.
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let hash = sha256_hex("tk_node_0123456789abcdef");
        assert_eq!(hash.len(), 64);
        assert!(hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    }

    #[test]
    fn sha256_hex_hashes_the_utf8_bytes() {
        use sha2::{Digest, Sha256};
        // "é" is the two UTF-8 bytes 0xC3 0xA9.
        assert_eq!(
            sha256_hex("é"),
            hex::encode(Sha256::digest([0xC3_u8, 0xA9]))
        );
    }

    #[test]
    fn equal_inputs_match() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"whsec_abc123", b"whsec_abc123"));
    }

    #[test]
    fn unequal_inputs_of_the_same_length_do_not_match() {
        // Last byte, first byte, every byte, and bytes that differ only in
        // value (so a sum or count of bytes would not tell them apart).
        assert!(!constant_time_eq(b"whsec_abc123", b"whsec_abc124"));
        assert!(!constant_time_eq(b"whsec_abc123", b"whsec_xyz123"));
        assert!(!constant_time_eq(b"xbc", b"abc"));
        assert!(!constant_time_eq(b"aaaaaaaaaaaa", b"bbbbbbbbbbbb"));
        assert!(!constant_time_eq(b"\0\0\0\0", b"\x01\x01\x01\x01"));
        assert!(!constant_time_eq(b"ab", b"ba"));
    }

    #[test]
    fn inputs_of_different_lengths_do_not_match() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        // A prefix of the expected value must not match.
        assert!(!constant_time_eq(b"short", b"short_and_longer"));
    }

    #[test]
    fn empty_inputs_match_each_other() {
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn empty_input_does_not_match_a_non_empty_one() {
        assert!(!constant_time_eq(b"", b"nonempty"));
        assert!(!constant_time_eq(b"nonempty", b""));
        assert!(!constant_time_eq(b"", b"\0"));
    }
}
