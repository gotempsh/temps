// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! # Temps Embeddings
//!
//! Pure tokenization library with no external tokenizer dependencies.
//! Provides trait-based abstractions for tokenizing bytes/text into token IDs.
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
