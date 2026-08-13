//! ADR-037: subscription-backed agent CLI providers for the Temps AI foundation.
//!
//! This crate introduces two types:
//!
//! - [`AgentCliAiService`]: implements [`temps_ai::AiService`] by delegating
//!   to an [`temps_agents::ai_cli::AiCliProvider`] (Claude Code, Codex,
//!   OpenCode). Subscription credentials are ambient on the host — no API key
//!   is read or forwarded.
//!
//! - [`DispatchingAiService`]: routes `is_available`/`complete`/`chat_stream`
//!   to whichever provider is active (BYOK gateway or a subscription agent
//!   CLI), read through the [`ActiveProviderReader`] seam so this crate stays
//!   free of a DB dependency. Multi-turn tool-calling is exposed to agent CLIs
//!   through a per-turn, authenticated loopback MCP bridge.

pub mod dispatch;
pub mod service;

pub use dispatch::{ActiveProviderReader, AiProviderRegistry, DispatchingAiService};
pub use service::{AgentCliAiService, ScopedMcpBridge};
