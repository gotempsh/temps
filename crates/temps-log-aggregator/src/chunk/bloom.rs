// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-chunk bloom filter over message tokens and their 3-grams (ADR-046 §1).
//!
//! The filter answers one question: "could any line in this chunk contain
//! `needle` as a substring?" A miss lets the planner skip a whole chunk
//! without a block read. A hit means "maybe" — the block scan still verifies.
//!
//! ## Soundness of substring pruning
//!
//! The filter is built from two kinds of entries per message token: the
//! whole-token hash, and the hash of every contiguous 3-character run
//! ("gram") within the token. [`query_entries`] computes, for a search
//! `needle`, the entries that MUST be present in the bloom of any chunk
//! containing a line where `needle` occurs as a substring:
//!
//! - Tokenizing `needle` the same way the indexer tokenizes messages
//!   splits it into a sequence of tokens separated by non-alphanumeric
//!   characters.
//! - **Interior** needle tokens (tokens with a delimiter on both sides
//!   within the needle, i.e. not the very first token unless the needle
//!   itself starts with a delimiter, and not the very last token unless
//!   the needle itself ends with a delimiter) must, for the needle to be a
//!   substring match, appear as an **exact, complete token** of the
//!   matched message (because the same delimiter characters bound them on
//!   both sides in the message text). The message indexer inserted that
//!   token's whole-token hash, so requiring the whole-token entry is sound.
//! - **Every** needle token of length ≥ 3, interior or not, is a
//!   **substring** of some token of the matched message (edge tokens
//!   because they may be a prefix/suffix fragment of a larger message
//!   token; interior tokens because they equal a message token exactly).
//!   Any 3 consecutive characters of a substring are also 3 consecutive
//!   characters of the containing string, so every 3-gram of the needle
//!   token is also a 3-gram of the message token, and the indexer inserted
//!   all of the message token's grams.
//!
//! Because both properties hold for every possible match, `bloom.contains_all
//! (&query_entries(needle))` can never be `false` when `needle` is truly a
//! substring of some indexed message — there are no false negatives. A
//! needle with no token of length ≥ 3 and no interior token (e.g. a single
//! short word with no surrounding delimiters, or an empty string) yields no
//! required entries; [`query_entries`] returns an empty `Vec` in that case,
//! and the caller must treat that as "cannot prune" rather than "no chunks
//! match".

use std::collections::HashSet;

use xxhash_rust::xxh3::xxh3_64_with_seed;

use crate::error::LogAggregatorError;

/// Serialized format version (byte 0 of [`Bloom::to_bytes`]).
const BLOOM_VERSION: u8 = 1;

/// Fixed header size before the bit array: version(1) + k(1) + pad(2) + n_bits(8).
const HEADER_LEN: usize = 12;

/// Bits per distinct entry the builder sizes for (~1% false-positive rate at
/// `K` = 7 hash functions).
const BITS_PER_ENTRY: f64 = 9.6;

/// Minimum filter size, so tiny chunks still get a usable filter.
const MIN_BITS: u64 = 1024;

/// Number of hash functions ("k" in bloom-filter terminology).
const K: u8 = 7;

/// Seed used to hash a whole token into an entry.
const TOKEN_SEED: u64 = 0;

/// Seed used to hash a 3-gram into an entry (disjoint namespace from tokens).
const GRAM_SEED: u64 = 1;

/// A fixed-size bloom filter over `u64` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bloom {
    k: u8,
    n_bits: u64,
    bits: Vec<u8>,
}

impl Bloom {
    fn new(n_bits: u64, k: u8) -> Self {
        let byte_len = n_bits.div_ceil(8) as usize;
        Self {
            k,
            n_bits,
            bits: vec![0u8; byte_len],
        }
    }

    fn bit_indices(&self, entry: u64) -> impl Iterator<Item = u64> + '_ {
        let bytes = entry.to_le_bytes();
        (0..self.k).map(move |i| xxh3_64_with_seed(&bytes, i as u64) % self.n_bits)
    }

    fn set(&mut self, idx: u64) {
        let idx = idx as usize;
        self.bits[idx / 8] |= 1 << (idx % 8);
    }

    fn is_set(&self, idx: u64) -> bool {
        let idx = idx as usize;
        (self.bits[idx / 8] >> (idx % 8)) & 1 == 1
    }

    fn insert(&mut self, entry: u64) {
        let indices: Vec<u64> = self.bit_indices(entry).collect();
        for idx in indices {
            self.set(idx);
        }
    }

    /// True when every entry may be present (no false negatives; may be a
    /// false positive). An empty `entries` slice is vacuously `true` — the
    /// caller is responsible for treating "no required entries" as "cannot
    /// prune" (see [`query_entries`]).
    pub fn contains_all(&self, entries: &[u64]) -> bool {
        entries
            .iter()
            .all(|&e| self.bit_indices(e).all(|idx| self.is_set(idx)))
    }

    /// Serialized length in bytes (header + bit array). This is what the
    /// footer records as `bloom_len`.
    pub fn byte_len(&self) -> usize {
        HEADER_LEN + self.bits.len()
    }

    /// `[u8 version=1][u8 k][u16 zero][u64 n_bits LE][bits…]`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.byte_len());
        buf.push(BLOOM_VERSION);
        buf.push(self.k);
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&self.n_bits.to_le_bytes());
        buf.extend_from_slice(&self.bits);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LogAggregatorError> {
        if bytes.len() < HEADER_LEN {
            return Err(LogAggregatorError::ChunkFormat {
                reason: format!(
                    "bloom buffer too short: {} bytes, need at least {HEADER_LEN}",
                    bytes.len()
                ),
            });
        }
        let version = bytes[0];
        if version != BLOOM_VERSION {
            return Err(LogAggregatorError::ChunkFormat {
                reason: format!("unsupported bloom version {version}"),
            });
        }
        let k = bytes[1];
        let n_bits_bytes: [u8; 8] =
            bytes[4..12]
                .try_into()
                .map_err(|_| LogAggregatorError::ChunkFormat {
                    reason: "bloom n_bits field truncated".to_string(),
                })?;
        let n_bits = u64::from_le_bytes(n_bits_bytes);
        let expected_bit_bytes = n_bits.div_ceil(8) as usize;
        let bits = bytes[HEADER_LEN..].to_vec();
        if bits.len() != expected_bit_bytes {
            return Err(LogAggregatorError::ChunkFormat {
                reason: format!(
                    "bloom bit array length mismatch: got {} bytes, expected {expected_bit_bytes} for n_bits={n_bits}",
                    bits.len()
                ),
            });
        }
        Ok(Self { k, n_bits, bits })
    }
}

/// Accumulates the distinct entries for one chunk's bloom filter.
#[derive(Debug, Default)]
pub struct BloomBuilder {
    entries: HashSet<u64>,
}

impl BloomBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tokenizes `msg` and inserts, for every token, the whole-token hash
    /// and (for tokens of at least 3 characters) the hash of every 3-gram.
    pub fn insert_message(&mut self, msg: &str) {
        for token in tokens(msg) {
            self.entries.insert(hash_token(&token));
            for gram in char_3grams(&token) {
                self.entries.insert(hash_gram(&gram));
            }
        }
    }

    /// Number of distinct entries accumulated so far.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Builds the filter, sized at `ceil(9.6 bits * entries)` rounded up to
    /// a multiple of 8, with a floor of 1024 bits and `k` = 7.
    pub fn build(&self) -> Bloom {
        let n_bits = size_for(self.entries.len());
        let mut bloom = Bloom::new(n_bits, K);
        for &entry in &self.entries {
            bloom.insert(entry);
        }
        bloom
    }
}

fn size_for(n_entries: usize) -> u64 {
    let raw_bits = (BITS_PER_ENTRY * n_entries as f64).ceil() as u64;
    let rounded = raw_bits.div_ceil(8) * 8;
    rounded.max(MIN_BITS)
}

fn hash_token(token: &str) -> u64 {
    xxh3_64_with_seed(token.as_bytes(), TOKEN_SEED)
}

fn hash_gram(gram: &str) -> u64 {
    xxh3_64_with_seed(gram.as_bytes(), GRAM_SEED)
}

/// Every contiguous 3-character run of `token`, over `char`s (not bytes), or
/// an empty `Vec` when `token` has fewer than 3 characters.
fn char_3grams(token: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < 3 {
        return Vec::new();
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Lowercases `text` and splits on any character that is not
/// [`char::is_alphanumeric`], dropping tokens shorter than 2 or longer than
/// 64 characters.
pub fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| (2..=64).contains(&t.chars().count()))
        .map(|t| t.to_string())
        .collect()
}

/// Computes the bloom entries that MUST be present for `needle` to occur as
/// a substring of some indexed message. Returns an empty `Vec` when the
/// needle has no token that can be proven present (e.g. empty string, or a
/// single short word with no surrounding delimiters) — callers must treat
/// that as "cannot prune", never as "no match possible". See the module
/// docs for the soundness argument.
pub fn query_entries(needle: &str) -> Vec<u64> {
    let toks = tokens(needle);
    if toks.is_empty() {
        return Vec::new();
    }

    let starts_with_delim = needle
        .chars()
        .next()
        .map(|c| !c.is_alphanumeric())
        .unwrap_or(false);
    let ends_with_delim = needle
        .chars()
        .last()
        .map(|c| !c.is_alphanumeric())
        .unwrap_or(false);

    let last_idx = toks.len() - 1;
    let mut entries = Vec::new();
    for (idx, token) in toks.iter().enumerate() {
        for gram in char_3grams(token) {
            entries.push(hash_gram(&gram));
        }

        let is_first = idx == 0;
        let is_last = idx == last_idx;
        let interior = (!is_first || starts_with_delim) && (!is_last || ends_with_delim);
        if interior {
            entries.push(hash_token(token));
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift64* PRNG so tests are reproducible without
    /// pulling in a `rand` dependency for this crate.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn range(&mut self, max: usize) -> usize {
            (self.next_u64() as usize) % max
        }
    }

    fn random_message(rng: &mut Rng) -> String {
        const WORDS: &[&str] = &[
            "connection",
            "refused",
            "GET",
            "/api/v1/users",
            "500",
            "error",
            "timeout",
            "request_id=abc-123",
            "user@example.com",
            "panicked",
            "at",
            "src/main.rs:42",
            "retrying",
            "in",
            "5s",
            "database",
            "pool",
            "exhausted",
        ];
        let n_words = 3 + rng.range(6);
        (0..n_words)
            .map(|_| WORDS[rng.range(WORDS.len())])
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn random_substring(rng: &mut Rng, msg: &str) -> String {
        let chars: Vec<char> = msg.chars().collect();
        if chars.is_empty() {
            return String::new();
        }
        let start = rng.range(chars.len());
        let remaining = chars.len() - start;
        let len = 1 + rng.range(remaining.max(1));
        chars[start..(start + len).min(chars.len())]
            .iter()
            .collect()
    }

    #[test]
    fn no_false_negatives_over_random_messages_and_substrings() {
        let mut rng = Rng::new(42);
        let messages: Vec<String> = (0..300).map(|_| random_message(&mut rng)).collect();

        let mut builder = BloomBuilder::new();
        for msg in &messages {
            builder.insert_message(msg);
        }
        let bloom = builder.build();

        for msg in &messages {
            for _ in 0..3 {
                let needle = random_substring(&mut rng, msg);
                if needle.is_empty() {
                    continue;
                }
                let entries = query_entries(&needle);
                assert!(
                    bloom.contains_all(&entries),
                    "false negative: needle {needle:?} is a substring of {msg:?} but bloom missed entries {entries:?}"
                );
            }
        }
    }

    #[test]
    fn single_token_and_punctuated_needles_have_no_false_negatives() {
        let messages = [
            "connection refused: timeout after 5s",
            "user@example.com logged in from 10.0.0.1",
            "request_id=abc-123 status=500",
            "panicked at src/main.rs:42",
        ];
        let mut builder = BloomBuilder::new();
        for msg in &messages {
            builder.insert_message(msg);
        }
        let bloom = builder.build();

        let needles = [
            "connection",
            "refused",
            "timeout after",
            "example.com",
            "abc-123",
            "status=500",
            "main.rs:42",
            "onnection refus",
            ":42",
            "42",
        ];
        for needle in needles {
            let found = messages.iter().any(|m| m.contains(needle));
            if !found {
                continue;
            }
            let entries = query_entries(needle);
            assert!(
                bloom.contains_all(&entries),
                "false negative for needle {needle:?}"
            );
        }
    }

    #[test]
    fn empty_and_single_char_needles_return_no_entries() {
        assert!(query_entries("").is_empty());
        assert!(query_entries("a").is_empty());
        assert!(query_entries(" ").is_empty());
    }

    #[test]
    fn round_trip_bytes() {
        let mut builder = BloomBuilder::new();
        builder.insert_message("hello world this is a test message");
        let bloom = builder.build();
        let bytes = bloom.to_bytes();
        assert_eq!(bytes.len(), bloom.byte_len());
        let decoded = Bloom::from_bytes(&bytes).expect("round trip should decode");
        assert_eq!(decoded, bloom);
    }

    #[test]
    fn from_bytes_rejects_bad_version_and_truncated_input() {
        let mut builder = BloomBuilder::new();
        builder.insert_message("some message");
        let bloom = builder.build();
        let mut bytes = bloom.to_bytes();
        bytes[0] = 99;
        assert!(Bloom::from_bytes(&bytes).is_err());

        let bytes = bloom.to_bytes();
        assert!(Bloom::from_bytes(&bytes[..5]).is_err());

        let mut truncated_bits = bloom.to_bytes();
        truncated_bits.pop();
        assert!(Bloom::from_bytes(&truncated_bits).is_err());
    }

    #[test]
    fn false_positive_rate_is_bounded() {
        let mut rng = Rng::new(7);
        let mut builder = BloomBuilder::new();
        let mut members = HashSet::new();
        // The word list is tiny, so entries would plateau far below 5000;
        // give every message a unique token (as real logs do: ids, uuids).
        let mut uid = 0u32;
        while builder.len() < 5000 {
            let msg = format!("{} uid{uid}x", random_message(&mut rng));
            uid += 1;
            for tok in tokens(&msg) {
                members.insert(tok);
            }
            builder.insert_message(&msg);
        }
        let bloom = builder.build();

        let mut false_positives = 0u32;
        let trials = 10_000u32;
        for i in 0..trials {
            let needle = format!("zzz_nonexistent_token_{i}_qqq");
            debug_assert!(!members.contains(&needle));
            let entries = query_entries(&needle);
            if bloom.contains_all(&entries) {
                false_positives += 1;
            }
        }
        let rate = f64::from(false_positives) / f64::from(trials);
        assert!(rate < 0.03, "false-positive rate too high: {rate}");
    }

    #[test]
    fn tokens_lowercases_and_filters_length() {
        assert_eq!(
            tokens("Hello, World! a bb ccc"),
            vec!["hello", "world", "bb", "ccc"]
        );
        let long = "a".repeat(65);
        assert!(tokens(&long).is_empty());
    }
}
