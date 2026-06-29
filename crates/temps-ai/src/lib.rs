//! The Temps AI foundation (ADR-022).
//!
//! A single, governed, provider-agnostic way for any crate to ask the configured
//! model for either free text or **typed, structured data** — plus a library of
//! reusable [`schemas`] and high-level [`diagnostics`] helpers (debugging deploy
//! / docker build failures, etc.) so consumers don't re-roll prompts or parsing.
//!
//! Layers:
//! - [`service`] — the object-safe [`AiService`] trait, registered + resolved
//!   through the plugin DI as `Arc<dyn AiService>`. The implementation lives in
//!   `temps-ai-gateway`; this crate stays dependency-light so any crate can use
//!   the trait + schemas without pulling in providers/HTTP.
//! - [`typed`] — `complete_text` / `complete_typed::<T>` ergonomics on top.
//! - [`schemas`] — reusable structured-output types (`JsonSchema + Deserialize`).
//! - [`diagnostics`] — one-call helpers that pair a prompt with a schema.
//!
//! Everything is best-effort: the trait returns [`Result`], the helpers return
//! [`Option`], and AI must never sit on a path that can block or fail a core
//! operation — callers wrap calls in a timeout.

pub mod diagnostics;
pub mod schemas;
pub mod service;
pub mod streaming;
pub mod typed;

pub use service::{AiError, AiRequest, AiResponse, AiService};
pub use streaming::{
    ChatMessage, ChatTool, ChatTurnRequest, ChatTurnResponse, TokenStream, ToolCall,
};
pub use typed::{complete_text, complete_typed, extract_json_block};
