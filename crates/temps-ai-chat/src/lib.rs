//! Persistent, resumable AI debugging conversations (ADR-023).
//!
//! A generic conversation store keyed by a polymorphic `(context_type,
//! context_id)` — one resumable chat per interaction. [`ConversationService`]
//! owns create/find/history + streaming `send_message`; each context type
//! supplies a [`ConversationContextProvider`] that seeds the chat (the
//! deployment provider seeds from a failure diagnosis). Built on the `temps-ai`
//! foundation; the AI is injected as `Arc<dyn AiService>`.

pub mod audit;
pub mod handlers;
pub mod pending_actions;
pub mod plugin;
pub mod provider;
pub mod providers;
pub mod service;

pub use pending_actions::{PendingActionError, PendingActionService};
pub use plugin::AiChatPlugin;
pub use provider::{ConversationContextProvider, ConversationSeed};
pub use providers::alert::AlertChatProvider;
pub use providers::api_tools::ApiToolsProvider;
pub use providers::deployment::DeploymentChatProvider;
pub use providers::project::ProjectChatProvider;
pub use service::{ChatStreamEvent, ConversationService};

/// Errors from the conversation layer. All map cleanly to HTTP at the handler.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("conversation '{0}' not found")]
    NotFound(String),
    #[error("no AI context provider registered for type '{0}'")]
    NoProvider(String),
    #[error("AI is not configured for this project")]
    AiUnavailable,
    #[error("context not found or not accessible")]
    ContextUnavailable,
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
    #[error("AI provider error: {0}")]
    Ai(String),
}
