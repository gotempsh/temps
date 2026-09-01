// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The deployment-failure context provider (ADR-023, consumer #1).
//!
//! Seeds a chat from a failed deployment: its state + reason, each failed step's
//! error message, and the *tail* of each failed step's log (via `LogService`).
//! `context_id` is the deployment's integer id.
//!
//! Repo-exploration tools (`read_repo_file`, `list_repo_dir`, etc.) are no
//! longer provided by this module — they are supplied globally by the
//! `__repo_tools__` sentinel provider (see `providers/repo_tools.rs`), which is
//! merged into every conversation by [`crate::ConversationService::send_message`].
//! The deployment seed instructs the model to pass the deployed commit SHA as
//! `ref` when calling those tools so it inspects the exact deployed source.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use temps_ai::ChatTool;
use temps_entities::types::JobStatus;
use temps_entities::{deployment_jobs, deployments};
use temps_logs::LogService;

use crate::provider::{ConversationContextProvider, ConversationSeed};

/// Per-step log tail budget (bytes), trimmed to a line boundary. Bounds the seed
/// so a huge build log can't blow the model's context / token budget. Sized to
/// comfortably hold a framework build's error block (stack/trace + the failing
/// command), which is where the actual diagnosis lives.
const MAX_LOG_TAIL_BYTES: usize = 6_000;

/// The last `MAX_LOG_TAIL_BYTES` of a log, trimmed to a line boundary. Never
/// slices on a multibyte char boundary: the start index is advanced forward to
/// the next valid `char` boundary so a UTF-8 char straddling the cut isn't split.
fn log_tail(content: &str) -> &str {
    let trimmed = content.trim_end();
    if trimmed.len() <= MAX_LOG_TAIL_BYTES {
        return trimmed;
    }
    let mut start = trimmed.len() - MAX_LOG_TAIL_BYTES;
    // Advance to a valid char boundary so slicing can't panic mid-codepoint.
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    match trimmed[start..].find('\n') {
        Some(nl) => &trimmed[start + nl + 1..],
        None => &trimmed[start..],
    }
}

const SYSTEM_PREAMBLE: &str = "You are a senior platform/DevOps engineer helping a developer debug a FAILED Temps \
deployment. Use the failure context below. Identify the most likely root cause and concrete, ordered fixes, \
grounded strictly in the evidence — do not invent file names, versions, or causes you cannot see. Ask a brief \
clarifying question only when essential. Be concise and practical.";

/// Seeds deployment-failure debugging chats.
pub struct DeploymentChatProvider {
    db: Arc<DatabaseConnection>,
    log_service: Arc<LogService>,
}

impl DeploymentChatProvider {
    pub fn new(db: Arc<DatabaseConnection>, log_service: Arc<LogService>) -> Self {
        Self { db, log_service }
    }
}

#[async_trait]
impl ConversationContextProvider for DeploymentChatProvider {
    fn context_type(&self) -> &'static str {
        "deployment"
    }

    async fn seed(&self, project_id: i32, context_id: &str) -> Option<ConversationSeed> {
        let deployment_id: i32 = context_id.parse().ok()?;
        let dep = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await
            .ok()??;
        // Authorization belt-and-braces: the row must belong to this project.
        if dep.project_id != project_id {
            return None;
        }

        let jobs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .all(self.db.as_ref())
            .await
            .ok()?;
        let failed: Vec<&deployment_jobs::Model> = jobs
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Failure))
            .collect();

        let mut ctx = String::new();
        ctx.push_str(SYSTEM_PREAMBLE);
        ctx.push_str("\n\n");
        ctx.push_str(crate::provider::TOOL_USAGE_GUIDANCE);
        ctx.push_str("\n\n--- Failure context ---\n");
        ctx.push_str(&format!("Deployment #{} — state: {}\n", dep.id, dep.state));
        if let Some(branch) = &dep.branch_ref {
            ctx.push_str(&format!("Branch: {branch}\n"));
        }
        if let Some(commit) = &dep.commit_sha {
            let short: String = commit.chars().take(8).collect();
            ctx.push_str(&format!("Commit: {short}\n"));
            // The full SHA is surfaced separately so the model can pass it as
            // `ref` to `read_repo_file` / `list_repo_dir` (available via the
            // `__repo_tools__` sentinel) to inspect the exact deployed source.
            ctx.push_str(&format!(
                "Tip: pass ref=\"{commit}\" to read_repo_file / list_repo_dir to inspect \
                 the exact source that was deployed.\n"
            ));
        }
        if let Some(reason) = &dep.cancelled_reason {
            ctx.push_str(&format!("Failure reason: {reason}\n"));
        }

        // Pipeline overview — every step and its status, in order, so the model
        // can place the failure within the build/deploy sequence.
        if !jobs.is_empty() {
            let mut ordered: Vec<&deployment_jobs::Model> = jobs.iter().collect();
            ordered.sort_by_key(|j| j.execution_order.unwrap_or(0));
            ctx.push_str("\nPipeline steps (in order):\n");
            for j in ordered {
                ctx.push_str(&format!(
                    "- {} [{}]: {}\n",
                    j.name,
                    j.job_id,
                    j.status.as_str()
                ));
            }
        }

        if failed.is_empty() {
            ctx.push_str("No individual job failures were recorded.\n");
        } else {
            ctx.push_str("Failed steps:\n");
            for j in &failed {
                ctx.push_str(&format!(
                    "- '{}': {}\n",
                    j.job_id,
                    j.error_message.as_deref().unwrap_or("(no error message)")
                ));
            }
            // Append the tail of each failed step's log — where the real
            // diagnostic evidence lives. Best-effort: skip on read error.
            for j in &failed {
                if let Ok(content) = self.log_service.get_log_content(&j.log_id).await {
                    let tail = log_tail(&content);
                    if !tail.trim().is_empty() {
                        ctx.push_str(&format!(
                            "\n--- Log tail for failed step '{}' ---\n{}\n",
                            j.job_id, tail
                        ));
                    }
                }
            }
        }

        let metadata = serde_json::json!({
            "deployment_id": deployment_id,
            "state": dep.state,
            "failed_job_ids": failed.iter().map(|j| j.job_id.clone()).collect::<Vec<_>>(),
        });

        Some(ConversationSeed {
            system: ctx,
            first_assistant: None,
            title: Some(format!("Debug deployment #{deployment_id}")),
            metadata: Some(metadata),
        })
    }

    // Repo-exploration tools (read_repo_file, list_repo_dir, list_repo_branches,
    // list_repo_tags) are supplied by the __repo_tools__ sentinel provider and
    // merged into this context by ConversationService. This provider offers no
    // additional tools beyond what the sentinel provides.
    async fn tools(&self, _project_id: i32, _context_id: &str) -> Vec<ChatTool> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `log_tail` never panics when the cut lands inside a multibyte character.
    #[test]
    fn test_log_tail_multibyte_boundary_does_not_panic() {
        // Place 4-byte emoji at the front so the cut (len - MAX_LOG_TAIL_BYTES)
        // lands inside one — the naive `&trimmed[start..]` would panic mid-char.
        let big = "🚀".repeat(10) + &"a".repeat(MAX_LOG_TAIL_BYTES - 2);
        assert!(!big.is_char_boundary(big.len() - MAX_LOG_TAIL_BYTES));
        let tail = log_tail(&big);
        assert!(tail.len() <= MAX_LOG_TAIL_BYTES);
        // Accented + newline content straddling the cut.
        let line = "café résumé naïve\n";
        let accented = line.repeat((MAX_LOG_TAIL_BYTES / line.len()) + 50);
        let tail2 = log_tail(&accented);
        assert!(tail2.len() <= MAX_LOG_TAIL_BYTES);
    }
}
