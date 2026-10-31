// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request size limits shared by the chat routes.

use super::*;

/// Upper bounds on client-supplied chat inputs, enforced before any DB or AI
/// call so oversized payloads can't bloat storage or run up AI token cost.
pub(super) const MAX_CONTEXT_TYPE_LEN: usize = 64;
pub(super) const MAX_CONTEXT_ID_LEN: usize = 128;
pub(super) const MAX_MESSAGE_CONTENT_LEN: usize = 32_000;
pub(super) const MAX_TURN_ID_LEN: usize = 128;
/// Cap on the advisory `page_context` (well under a message; it's framing).
pub(super) const MAX_PAGE_CONTEXT_LEN: usize = 4_000;
/// Cap on a user-supplied conversation title (a short label, not prose).
pub(super) const MAX_TITLE_LEN: usize = 200;
/// Conversation history is paged so a long-running thread never produces an
/// unbounded response or forces the browser to render its entire transcript.
pub(super) const DEFAULT_MESSAGE_PAGE_LIMIT: u64 = 50;
pub(super) const MAX_MESSAGE_PAGE_LIMIT: u64 = 100;
pub(super) const MAX_CHAT_ATTACHMENTS: usize = 8;
pub(crate) const MAX_CHAT_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
pub(super) const MAX_CHAT_ATTACHMENT_UPLOAD_BYTES: usize = MAX_CHAT_ATTACHMENT_BYTES + 64 * 1024;

/// Body-size limit for `resolve_permission` POST payloads (finding 3).
///
/// The payload is a `PermissionDecision` — at most a few short strings.  8 KB
/// is ample for any real answer; anything larger is either a client bug or an
/// attempt to exhaust memory / blow the stdin pipe to the subprocess.
pub(super) const RESOLVE_PERMISSION_BODY_LIMIT: usize = 8 * 1024;

/// Maximum byte length for free-text fields inside a `PermissionDecision`
/// (`DenyTool.reason`, `RejectPlan.feedback`, serialized `AnswerQuestion.answers`).
///
/// These strings are written verbatim to the subprocess's stdin.  A hard cap
/// prevents a large payload from hanging the stdin write or exhausting buffers.
pub(super) const MAX_DECISION_STRING_LEN: usize = 4 * 1024;

/// 400 for an over-length input field.
pub(super) fn too_long(field: &str, max: usize) -> Problem {
    problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
        .with_title("Input Too Long")
        .with_detail(format!(
            "'{field}' exceeds the maximum length of {max} characters."
        ))
}
