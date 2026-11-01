// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Word-hashing tokenizer for error messages.
//!
//! Splits text on whitespace and maps each lower-cased word to a token ID in
//! `0..vocab_size` by hashing it, so no vocabulary has to be built or stored.
//! The error ingester turns these token IDs into the fixed-size vector it uses
//! to find similar error groups.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Deterministic hash tokenizer: the same word, in any case, always maps to
/// the same token ID.
pub(crate) struct HashTokenizer {
    vocab_size: usize,
}

impl HashTokenizer {
    pub(crate) fn new(vocab_size: usize) -> Self {
        Self { vocab_size }
    }

    /// Tokenize `text` into one token ID per whitespace-separated word.
    pub(crate) fn encode(&self, text: &str) -> Vec<u32> {
        text.split_whitespace()
            .map(|word| self.hash_word(word))
            .collect()
    }

    fn hash_word(&self, word: &str) -> u32 {
        let mut hasher = DefaultHasher::new();
        word.to_lowercase().hash(&mut hasher);
        (hasher.finish() % self.vocab_size as u64) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_tokenizer() {
        let tokenizer = HashTokenizer::new(1000);
        let tokens1 = tokenizer.encode("hello world");
        let tokens2 = tokenizer.encode("hello world");

        assert_eq!(tokens1, tokens2);
        assert!(tokens1.iter().all(|&t| t < 1000));
    }

    #[test]
    fn encode_yields_one_case_insensitive_token_per_word() {
        let tokenizer = HashTokenizer::new(10_000);

        let tokens = tokenizer.encode("  Connection\trefused  connection\n");

        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0], tokens[2], "case must not change the token");
        assert_eq!(tokens, tokenizer.encode("connection REFUSED Connection"));
    }

    #[test]
    fn encode_of_blank_text_is_empty() {
        let tokenizer = HashTokenizer::new(10_000);

        assert!(tokenizer.encode("").is_empty());
        assert!(tokenizer.encode(" \t\n").is_empty());
    }
}
