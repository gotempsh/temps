//! Tokenization abstraction
//!
//! This module provides a trait-based interface for tokenizing text into token IDs.
//! Multiple implementations are provided for different use cases.

use std::collections::HashMap;

/// Error type for tokenization operations
#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    #[error("Failed to tokenize text: {0}")]
    TokenizationFailed(String),

    #[error("Failed to decode tokens: {0}")]
    DecodeFailed(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
}

/// Result type for tokenizer operations
pub type TokenizerResult<T> = Result<T, TokenizerError>;

/// Trait for tokenization implementations
///
/// Implementors should provide efficient tokenization of text into numeric token IDs.
pub trait Tokenizer: Send + Sync {
    /// Tokenize a single text string into token IDs
    fn encode(&self, text: &str) -> TokenizerResult<Vec<u32>>;

    /// Tokenize multiple text strings (batch operation for efficiency)
    fn encode_batch(&self, texts: &[&str]) -> TokenizerResult<Vec<Vec<u32>>> {
        texts.iter().map(|text| self.encode(text)).collect()
    }

    /// Decode token IDs back to text (if supported)
    fn decode(&self, token_ids: &[u32]) -> TokenizerResult<String>;

    /// Get the vocabulary size of this tokenizer
    fn vocab_size(&self) -> usize;

    /// Get a human-readable name for this tokenizer
    fn name(&self) -> &str;
}

/// Hash-based tokenizer for creating deterministic token IDs
///
/// Uses hashing to map words to token IDs, providing fast and consistent tokenization
/// without requiring a pre-built vocabulary.
pub struct HashTokenizer {
    vocab_size: usize,
    name: String,
}

impl HashTokenizer {
    pub fn new(vocab_size: usize) -> Self {
        Self {
            vocab_size,
            name: format!("HashTokenizer(vocab_size={})", vocab_size),
        }
    }

    fn hash_word(&self, word: &str) -> u32 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        word.to_lowercase().hash(&mut hasher);
        (hasher.finish() % self.vocab_size as u64) as u32
    }
}

impl Default for HashTokenizer {
    fn default() -> Self {
        Self::new(10000)
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

    fn decode(&self, _token_ids: &[u32]) -> TokenizerResult<String> {
        Err(TokenizerError::DecodeFailed(
            "HashTokenizer does not support decoding".to_string(),
        ))
    }

    fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Simple whitespace-based tokenizer with vocabulary
///
/// Maintains a vocabulary of known words and their token IDs.
pub struct SimpleTokenizer {
    vocab: Vec<String>,
    word_to_id: HashMap<String, u32>,
    name: String,
}

impl SimpleTokenizer {
    pub fn new() -> Self {
        Self {
            vocab: Vec::new(),
            word_to_id: HashMap::new(),
            name: "SimpleTokenizer(empty)".to_string(),
        }
    }

    pub fn with_vocab(vocab: Vec<String>) -> Self {
        let word_to_id: HashMap<String, u32> = vocab
            .iter()
            .enumerate()
            .map(|(idx, word)| (word.clone(), idx as u32))
            .collect();

        Self {
            vocab: vocab.clone(),
            word_to_id,
            name: format!("SimpleTokenizer(vocab_size={})", vocab.len()),
        }
    }

    fn get_token_id(&self, word: &str) -> u32 {
        *self.word_to_id.get(word).unwrap_or(&(self.vocab.len() as u32))
    }
}

impl Default for SimpleTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer for SimpleTokenizer {
    fn encode(&self, text: &str) -> TokenizerResult<Vec<u32>> {
        let tokens = text
            .split_whitespace()
            .map(|word| self.get_token_id(word))
            .collect();
        Ok(tokens)
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

    fn name(&self) -> &str {
        &self.name
    }
}

/// Character-level tokenizer
///
/// Tokenizes text at the character level for maximum granularity.
pub struct CharTokenizer {
    max_char_code: u32,
    name: String,
}

impl CharTokenizer {
    pub fn new() -> Self {
        Self {
            max_char_code: 128,
            name: "CharTokenizer(ASCII)".to_string(),
        }
    }

    pub fn with_unicode() -> Self {
        Self {
            max_char_code: 0x10FFFF,
            name: "CharTokenizer(Unicode)".to_string(),
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

    fn decode(&self, token_ids: &[u32]) -> TokenizerResult<String> {
        let chars: Result<Vec<char>, _> = token_ids
            .iter()
            .map(|&id| {
                char::from_u32(id).ok_or_else(|| {
                    TokenizerError::DecodeFailed(format!("Invalid char code: {}", id))
                })
            })
            .collect();

        chars.map(|c| c.into_iter().collect())
    }

    fn vocab_size(&self) -> usize {
        self.max_char_code as usize
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_tokenizer() {
        let tokenizer = HashTokenizer::new(1000);
        let tokens1 = tokenizer.encode("hello world").unwrap();
        let tokens2 = tokenizer.encode("hello world").unwrap();

        assert_eq!(tokens1, tokens2);
        assert!(tokens1.iter().all(|&t| t < 1000));
    }

    #[test]
    fn test_simple_tokenizer() {
        let tokenizer = SimpleTokenizer::with_vocab(vec![
            "hello".to_string(),
            "world".to_string(),
        ]);

        let tokens = tokenizer.encode("hello world").unwrap();
        assert_eq!(tokens, vec![0, 1]);

        let decoded = tokenizer.decode(&tokens).unwrap();
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn test_char_tokenizer() {
        let tokenizer = CharTokenizer::new();
        let tokens = tokenizer.encode("Hi").unwrap();
        assert_eq!(tokens, vec!['H' as u32, 'i' as u32]);

        let decoded = tokenizer.decode(&tokens).unwrap();
        assert_eq!(decoded, "Hi");
    }
}
