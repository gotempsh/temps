//! Tokenization abstraction for error messages and text processing
//!
//! This module provides a trait-based abstraction for tokenization that allows
//! for different implementations (simple, BPE, etc.) to be used interchangeably.

use std::sync::Arc;

/// Error type for tokenization operations
#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    #[error("Failed to tokenize text: {0}")]
    TokenizationFailed(String),

    #[error("Failed to decode tokens: {0}")]
    DecodeFailed(String),

    #[error("Invalid token ID: {0}")]
    InvalidTokenId(u32),
}

/// Result type for tokenizer operations
pub type TokenizerResult<T> = Result<T, TokenizerError>;

/// Trait for tokenization implementations
pub trait Tokenizer: Send + Sync {
    /// Tokenize a single text string into token IDs
    fn encode(&self, text: &str) -> TokenizerResult<Vec<u32>>;

    /// Tokenize multiple text strings (batch operation for efficiency)
    fn encode_batch(&self, texts: &[&str]) -> TokenizerResult<Vec<Vec<u32>>>;

    /// Decode token IDs back to text
    fn decode(&self, token_ids: &[u32]) -> TokenizerResult<String>;

    /// Get the vocabulary size of this tokenizer
    fn vocab_size(&self) -> usize;

    /// Get the maximum sequence length supported
    fn max_length(&self) -> Option<usize> {
        None
    }
}

/// Simple whitespace-based tokenizer for testing and basic use cases
pub struct SimpleTokenizer {
    vocab: Arc<Vec<String>>,
    word_to_id: Arc<std::collections::HashMap<String, u32>>,
}

impl SimpleTokenizer {
    pub fn new() -> Self {
        Self {
            vocab: Arc::new(Vec::new()),
            word_to_id: Arc::new(std::collections::HashMap::new()),
        }
    }

    /// Create a tokenizer with a predefined vocabulary
    pub fn with_vocab(vocab: Vec<String>) -> Self {
        let word_to_id: std::collections::HashMap<String, u32> = vocab
            .iter()
            .enumerate()
            .map(|(idx, word)| (word.clone(), idx as u32))
            .collect();

        Self {
            vocab: Arc::new(vocab),
            word_to_id: Arc::new(word_to_id),
        }
    }

    fn tokenize_word(&self, word: &str) -> u32 {
        // Return existing ID or create new one
        *self
            .word_to_id
            .get(word)
            .unwrap_or(&(self.vocab.len() as u32))
    }
}

impl Default for SimpleTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer for SimpleTokenizer {
    fn encode(&self, text: &str) -> TokenizerResult<Vec<u32>> {
        // Simple whitespace tokenization
        let tokens = text
            .split_whitespace()
            .map(|word| self.tokenize_word(word))
            .collect();
        Ok(tokens)
    }

    fn encode_batch(&self, texts: &[&str]) -> TokenizerResult<Vec<Vec<u32>>> {
        texts.iter().map(|text| self.encode(text)).collect()
    }

    fn decode(&self, token_ids: &[u32]) -> TokenizerResult<String> {
        let words: Vec<String> = token_ids
            .iter()
            .filter_map(|&id| self.vocab.get(id as usize).cloned())
            .collect();
        Ok(words.join(" "))
    }

    fn vocab_size(&self) -> usize {
        self.vocab.len()
    }
}

/// Hash-based tokenizer for creating fingerprints from error messages
/// Uses a simple hashing approach to create consistent token IDs
pub struct HashTokenizer {
    vocab_size: usize,
}

impl HashTokenizer {
    pub fn new(vocab_size: usize) -> Self {
        Self { vocab_size }
    }

    fn hash_word(&self, word: &str) -> u32 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        word.hash(&mut hasher);
        (hasher.finish() % self.vocab_size as u64) as u32
    }
}

impl Default for HashTokenizer {
    fn default() -> Self {
        Self::new(10000) // Default vocab size
    }
}

impl Tokenizer for HashTokenizer {
    fn encode(&self, text: &str) -> TokenizerResult<Vec<u32>> {
        let tokens = text
            .split_whitespace()
            .map(|word| self.hash_word(word))
            .collect();
        Ok(tokens)
    }

    fn encode_batch(&self, texts: &[&str]) -> TokenizerResult<Vec<Vec<u32>>> {
        texts.iter().map(|text| self.encode(text)).collect()
    }

    fn decode(&self, _token_ids: &[u32]) -> TokenizerResult<String> {
        Err(TokenizerError::DecodeFailed(
            "HashTokenizer does not support decoding".to_string(),
        ))
    }

    fn vocab_size(&self) -> usize {
        self.vocab_size
    }
}

/// Character-level tokenizer for maximum granularity
pub struct CharTokenizer {
    max_char_code: u32,
}

impl CharTokenizer {
    pub fn new() -> Self {
        Self {
            max_char_code: 128, // ASCII by default
        }
    }

    pub fn with_unicode() -> Self {
        Self {
            max_char_code: 0x10FFFF, // All valid Unicode
        }
    }
}

impl Default for CharTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer for CharTokenizer {
    fn encode(&self, text: &str) -> TokenizerResult<Vec<u32>> {
        let tokens = text.chars().map(|c| c as u32).collect();
        Ok(tokens)
    }

    fn encode_batch(&self, texts: &[&str]) -> TokenizerResult<Vec<Vec<u32>>> {
        texts.iter().map(|text| self.encode(text)).collect()
    }

    fn decode(&self, token_ids: &[u32]) -> TokenizerResult<String> {
        let chars: Result<Vec<char>, _> = token_ids
            .iter()
            .map(|&id| char::from_u32(id).ok_or(TokenizerError::InvalidTokenId(id)))
            .collect();

        chars.map(|c| c.into_iter().collect())
    }

    fn vocab_size(&self) -> usize {
        self.max_char_code as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_tokenizer() {
        let tokenizer = SimpleTokenizer::with_vocab(vec!["hello".to_string(), "world".to_string()]);

        let tokens = tokenizer.encode("hello world").unwrap();
        assert_eq!(tokens, vec![0, 1]);

        let decoded = tokenizer.decode(&tokens).unwrap();
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn test_hash_tokenizer() {
        let tokenizer = HashTokenizer::new(1000);
        let tokens1 = tokenizer.encode("error occurred").unwrap();
        let tokens2 = tokenizer.encode("error occurred").unwrap();

        // Same input should produce same tokens
        assert_eq!(tokens1, tokens2);

        // All tokens should be within vocab size
        assert!(tokens1.iter().all(|&t| t < 1000));
    }

    #[test]
    fn test_char_tokenizer() {
        let tokenizer = CharTokenizer::new();
        let tokens = tokenizer.encode("Hi").unwrap();

        assert_eq!(tokens, vec!['H' as u32, 'i' as u32]);

        let decoded = tokenizer.decode(&tokens).unwrap();
        assert_eq!(decoded, "Hi");
    }

    #[test]
    fn test_batch_encoding() {
        let tokenizer = HashTokenizer::default();
        let texts = vec!["error one", "error two"];
        let batch_tokens = tokenizer.encode_batch(&texts).unwrap();

        assert_eq!(batch_tokens.len(), 2);
        assert_eq!(batch_tokens[0].len(), 2);
        assert_eq!(batch_tokens[1].len(), 2);
    }
}
