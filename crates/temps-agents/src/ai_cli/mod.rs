pub mod catalog;
pub mod claude;
pub mod codex;
pub mod opencode;

pub use catalog::{
    find_provider, AuthFlavor, CredentialFormat, ProviderCatalogEntry, PROVIDER_CATALOG,
};

use async_trait::async_trait;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::error::AgentError;

/// Callback invoked for each line of AI CLI output (for real-time streaming)
pub type OnEventCallback =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

pub struct AiRunConfig {
    pub work_dir: PathBuf,
    pub prompt: String,
    pub api_key: String,
    pub max_turns: i32,
    pub timeout: Duration,
    /// Optional preferred model name (e.g. "sonnet", "gpt-5-codex").
    /// `None` lets the CLI pick its default.
    pub model: Option<String>,
    /// Optional callback for streaming each line of output in real-time
    pub on_event: Option<OnEventCallback>,
}

pub struct AiRunResult {
    pub output: String,
    pub exit_code: i32,
    pub tokens_input: Option<i32>,
    pub tokens_output: Option<i32>,
    pub model: Option<String>,
    /// If the provider knows which files it changed, list them here.
    /// If `None`, the executor will detect changes via `git diff`.
    pub changed_files: Option<Vec<String>>,
    /// Claude CLI session ID (UUID) extracted from the `system/init` event.
    /// Used to resume the conversation in a workspace via `--resume`.
    pub session_id: Option<String>,
    /// True when the CLI hit the max turns limit without completing.
    pub is_max_turns_error: bool,
}

/// Status of the AI CLI tool on this server.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiCliStatus {
    pub provider: String,
    pub installed: bool,
    pub version: Option<String>,
    pub authenticated: bool,
    pub auth_method: Option<String>,
    pub email: Option<String>,
    pub subscription_type: Option<String>,
    /// Instructions for the user if not installed or not authenticated.
    pub setup_hint: Option<String>,
}

#[async_trait]
pub trait AiCliProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn check_installed(&self) -> bool;
    async fn get_status(&self) -> AiCliStatus;
    async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError>;
    /// Continue an existing conversation in the same work directory.
    /// Uses `--continue` to resume the most recent session.
    async fn continue_conversation(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError>;
}

/// Parse raw CLI output into the common parsed shape for any provider.
/// Codex/OpenCode don't report session ids or max-turn errors, so those
/// fields come back as `None`/`false` for them.
pub fn parse_output(provider: &str, output: &str) -> claude::ParsedClaudeOutput {
    match provider {
        "codex_cli" => {
            let (tokens_input, tokens_output, model) = codex::parse_codex_output(output);
            claude::ParsedClaudeOutput {
                tokens_input,
                tokens_output,
                model,
                session_id: None,
                is_max_turns_error: false,
            }
        }
        "opencode" => {
            let (tokens_input, tokens_output, model) = opencode::parse_opencode_output(output);
            claude::ParsedClaudeOutput {
                tokens_input,
                tokens_output,
                model,
                session_id: None,
                is_max_turns_error: false,
            }
        }
        _ => claude::parse_claude_output(output),
    }
}

/// Create an AI CLI provider by name
pub fn create_provider(name: &str) -> Option<Box<dyn AiCliProvider>> {
    match name {
        "claude_cli" => Some(Box::new(claude::ClaudeCliProvider)),
        "codex_cli" => Some(Box::new(codex::CodexCliProvider)),
        "opencode" => Some(Box::new(opencode::OpenCodeCliProvider)),
        _ => None,
    }
}

/// All supported provider names.
pub const PROVIDER_NAMES: &[(&str, &str)] = &[
    ("claude_cli", "Claude Code"),
    ("opencode", "OpenCode"),
    ("codex_cli", "Codex"),
];
