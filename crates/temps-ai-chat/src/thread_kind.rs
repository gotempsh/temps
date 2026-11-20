// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a thread is.
//!
//! A thread is either a conversation with an AI harness or an interactive
//! terminal in the workspace sandbox. The kind is fixed when the thread is
//! created and stored in `ai_conversations.kind`.

/// An AI harness conversation. Every thread created before kinds existed.
pub const THREAD_KIND_AI: &str = "ai";
/// An interactive shell in the workspace sandbox.
pub const THREAD_KIND_TERMINAL: &str = "terminal";

/// `ai_provider` / `ai_model` stored on terminal threads. The columns are not
/// nullable; nothing may interpret these values for a terminal thread.
pub const TERMINAL_THREAD_PROVIDER: &str = "terminal";
pub const TERMINAL_THREAD_MODEL: &str = "shell";

/// Title a terminal thread starts with; the user can rename it like any other.
pub const DEFAULT_TERMINAL_THREAD_TITLE: &str = "Terminal";

/// Longest PTY tab id the sandbox terminal accepts.
const MAX_TAB_LENGTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadKind {
    Ai,
    Terminal,
}

#[derive(Debug, thiserror::Error)]
pub enum ThreadKindError {
    #[error("Unknown thread kind '{kind}'; expected 'ai' or 'terminal'")]
    Unknown { kind: String },
}

impl ThreadKind {
    /// Parse the `kind` of a create request. Omitted means an AI thread, which
    /// is what every client sent before kinds existed.
    pub fn parse(value: Option<&str>) -> Result<Self, ThreadKindError> {
        match value {
            None | Some(THREAD_KIND_AI) => Ok(Self::Ai),
            Some(THREAD_KIND_TERMINAL) => Ok(Self::Terminal),
            Some(other) => Err(ThreadKindError::Unknown {
                kind: other.chars().take(32).collect(),
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ai => THREAD_KIND_AI,
            Self::Terminal => THREAD_KIND_TERMINAL,
        }
    }
}

/// The PTY tab a terminal thread owns.
///
/// Derived from the thread's public id rather than supplied by the client, so
/// a thread always reattaches to its own shell and a client cannot open
/// arbitrary tabs through a thread it owns. Thread ids are opaque hex, so this
/// is always a valid tab (letters, digits, `-`, `_`; at most 64 characters).
pub fn terminal_thread_tab(thread_public_id: &str) -> String {
    let safe: String = thread_public_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let mut tab = format!("t-{safe}");
    tab.truncate(MAX_TAB_LENGTH);
    tab
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_omitted_kind_is_an_ai_thread() {
        assert_eq!(
            ThreadKind::parse(None).unwrap_or(ThreadKind::Terminal),
            ThreadKind::Ai
        );
        assert_eq!(ThreadKind::parse(Some("ai")).ok(), Some(ThreadKind::Ai));
    }

    #[test]
    fn terminal_is_a_known_kind() {
        let kind = ThreadKind::parse(Some("terminal")).ok();
        assert_eq!(kind, Some(ThreadKind::Terminal));
        assert_eq!(kind.map(ThreadKind::as_str), Some("terminal"));
    }

    #[test]
    fn an_unknown_kind_is_rejected_with_its_name() {
        let error = ThreadKind::parse(Some("notebook")).expect_err("unknown kind");
        assert!(error.to_string().contains("notebook"), "{error}");
        // A hostile value is bounded before it reaches a message or a log.
        let long = "x".repeat(500);
        let error = ThreadKind::parse(Some(&long)).expect_err("unknown kind");
        assert!(error.to_string().len() < 120, "{error}");
    }

    #[test]
    fn the_stored_kinds_match_the_migration_constraint() {
        // `CHECK (kind IN ('ai', 'terminal'))` in the migration.
        assert_eq!(THREAD_KIND_AI, "ai");
        assert_eq!(THREAD_KIND_TERMINAL, "terminal");
    }

    #[test]
    fn a_thread_tab_is_valid_for_the_sandbox_terminal() {
        let tab = terminal_thread_tab("0123456789abcdef0123456789abcdef");
        assert_eq!(tab, "t-0123456789abcdef0123456789abcdef");
        assert!(tab.len() <= 64);
        assert!(tab
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn a_thread_tab_is_stable_and_distinct_per_thread() {
        assert_eq!(terminal_thread_tab("aaaa"), terminal_thread_tab("aaaa"));
        assert_ne!(terminal_thread_tab("aaaa"), terminal_thread_tab("bbbb"));
    }

    #[test]
    fn a_thread_tab_never_carries_characters_the_terminal_rejects() {
        let tab = terminal_thread_tab(&format!("a b;c\n../{}", "z".repeat(200)));
        assert!(tab.len() <= 64);
        assert!(tab
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }
}
