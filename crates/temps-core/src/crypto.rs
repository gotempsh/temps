// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Small cryptographic helpers shared across crates.

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

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

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
