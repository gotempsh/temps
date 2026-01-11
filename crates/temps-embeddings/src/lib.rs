//! # Temps Embeddings
//!
//! Pure tokenization library with no external dependencies beyond optional tokenization libraries.
//! Provides trait-based abstractions for tokenizing bytes/text into token IDs.
//!
//! ## Features
//!
//! - `huggingface` - Enable Hugging Face `tokenizers` support
//! - `openai` - Enable OpenAI `tiktoken-rs` support
//! - `all` - Enable all tokenization backends
//!
//! ## Example
//!
//! ```rust
//! use temps_embeddings::tokenizer::{HashTokenizer, Tokenizer};
//!
//! let tokenizer = HashTokenizer::new(10000);
//! let tokens = tokenizer.encode("Hello world").unwrap();
//! ```

pub mod tokenizer;

// Re-export main types
pub use tokenizer::{
    CharTokenizer, HashTokenizer, SimpleTokenizer, Tokenizer, TokenizerError, TokenizerResult,
};
