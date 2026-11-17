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
use temps_entities::error_groups::PgVector;

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

/// Create a vector embedding from tokenized message
/// This converts token IDs to a fixed-size vector representation
pub(crate) fn create_embedding_from_tokens(tokens: &[u32], embedding_size: usize) -> PgVector {
    // Simple approach: use token frequency as features
    let mut embedding = vec![0.0; embedding_size];

    for &token in tokens {
        let idx = (token as usize) % embedding_size;
        embedding[idx] += 1.0;
    }

    // Normalize
    let sum: f32 = embedding.iter().sum();
    if sum > 0.0 {
        for val in &mut embedding {
            *val /= sum;
        }
    }

    PgVector(embedding)
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

    #[test]
    fn embedding_counts_tokens_by_slot_and_normalizes_to_one() {
        // 7 wraps onto slot 1 of a 6-slot vector, next to the real 1.
        let embedding = create_embedding_from_tokens(&[1, 7, 2, 3], 6);
        assert_eq!(embedding.0, vec![0.0, 0.5, 0.25, 0.25, 0.0, 0.0]);
    }

    #[test]
    fn embedding_of_no_tokens_is_all_zeros_and_keeps_its_size() {
        let embedding = create_embedding_from_tokens(&[], 384);
        assert_eq!(embedding.0.len(), 384);
        assert!(embedding.0.iter().all(|value| *value == 0.0));
    }
}
