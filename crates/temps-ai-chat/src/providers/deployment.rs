//! The deployment-failure context provider (ADR-023, consumer #1).
//!
//! Seeds a chat from a failed deployment: its state + reason, each failed step's
//! error message, and the *tail* of each failed step's log (via `LogService`).
//! `context_id` is the deployment's integer id.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use temps_ai::ChatTool;
use temps_entities::types::JobStatus;
use temps_entities::{deployment_jobs, deployments, projects};
use temps_git::GitProviderManager;
use temps_logs::LogService;

use crate::provider::{ConversationContextProvider, ConversationSeed};

/// Max bytes of a repo file fed back to the model in one `read_repo_file` call.
const MAX_REPO_FILE_BYTES: usize = 16_000;

/// Per-step log tail budget (bytes), trimmed to a line boundary. Bounds the seed
/// so a huge build log can't blow the model's context / token budget. Sized to
/// comfortably hold a framework build's error block (stack/trace + the failing
/// command), which is where the actual diagnosis lives.
const MAX_LOG_TAIL_BYTES: usize = 6_000;

/// Reject a model-supplied repo path that is absolute, escapes the repo root, or
/// uses Windows-style separators. The Git provider only ever reads files inside
/// the repo; without this a path like `../../etc/passwd` (or `..%2f` after
/// encoding) could traverse outside it. Returns a human-readable reason on
/// rejection so the model gets text it can recover from, never a panic.
fn validate_repo_path(path: &str) -> Result<(), String> {
    if path.contains('\\') {
        return Err(
            "Invalid path: use forward slashes ('/') for a repo-relative path, not backslashes."
                .to_string(),
        );
    }
    // An absolute path (`/...` already stripped by the caller, but a Windows
    // drive prefix like `C:` is still absolute) must be rejected.
    if path.starts_with('/') || path.contains(':') {
        return Err("Invalid path: provide a repo-relative path, not an absolute one.".to_string());
    }
    for segment in path.split('/') {
        if segment == ".." || segment == "." {
            return Err(
                "Invalid path: '.' and '..' segments are not allowed; provide a path inside the \
                 repository."
                    .to_string(),
            );
        }
    }
    Ok(())
}

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
    /// Read-only repo access via the configured Git provider. `None` disables
    /// the `read_repo_file` tool (e.g. git plugin absent); seeding still works.
    git: Option<Arc<GitProviderManager>>,
}

impl DeploymentChatProvider {
    pub fn new(
        db: Arc<DatabaseConnection>,
        log_service: Arc<LogService>,
        git: Option<Arc<GitProviderManager>>,
    ) -> Self {
        Self {
            db,
            log_service,
            git,
        }
    }

    /// Read one repo file for this deployment via the Git provider API. Returns a
    /// human-readable string either way — errors come back as text the model can
    /// reason about, never as a hard failure.
    async fn read_repo_file(&self, project_id: i32, deployment_id: i32, path: &str) -> String {
        let path = path.trim().trim_start_matches('/');
        if path.is_empty() {
            return "Invalid arguments: provide a non-empty repo-relative \"path\".".to_string();
        }
        if let Err(reason) = validate_repo_path(path) {
            return reason;
        }
        let Some(git) = &self.git else {
            return "Repository access is not configured on this server.".to_string();
        };

        let dep = match deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(d)) if d.project_id == project_id => d,
            _ => return format!("Deployment {deployment_id} not found."),
        };
        let project = match projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(p)) => p,
            _ => return format!("Project {project_id} not found."),
        };
        let Some(connection_id) = project.git_provider_connection_id else {
            return "This project has no connected Git repository, so repo files can't be read."
                .to_string();
        };
        // Read at the exact deployed commit when known, else the branch.
        let reference = dep.commit_sha.clone().or_else(|| dep.branch_ref.clone());

        let token = match git.get_connection_token(connection_id).await {
            Ok(t) => t,
            Err(e) => return format!("Could not authenticate with the Git provider: {e}"),
        };
        let connection = match git.get_connection(connection_id).await {
            Ok(c) => c,
            Err(e) => return format!("Git connection unavailable: {e}"),
        };
        let service = match git.get_provider_service(connection.provider_id).await {
            Ok(s) => s,
            Err(e) => return format!("Git provider unavailable: {e}"),
        };

        match service
            .get_file_content(
                &token,
                &project.repo_owner,
                &project.repo_name,
                path,
                reference.as_deref(),
            )
            .await
        {
            Ok(file) => bound(&decode_file_content(&file.content, &file.encoding), path),
            Err(e) => format!(
                "Could not read '{path}' from {}/{}: {e}",
                project.repo_owner, project.repo_name
            ),
        }
    }
}

/// Decode a provider `FileContent`. GitHub returns base64 (with embedded
/// newlines); GitLab/raw returns utf-8. Fall back to the raw string if base64
/// decoding fails so the model still sees *something*.
fn decode_file_content(content: &str, encoding: &str) -> String {
    if encoding.eq_ignore_ascii_case("base64") {
        let stripped: String = content.split_whitespace().collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(stripped) {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    content.to_string()
}

/// Bound a file body so a large file can't blow the model's context. Never
/// slices on a multibyte char boundary: the cut index is retreated to the
/// previous valid `char` boundary so a UTF-8 char straddling it isn't split.
fn bound(content: &str, path: &str) -> String {
    if content.len() <= MAX_REPO_FILE_BYTES {
        return content.to_string();
    }
    let mut end = MAX_REPO_FILE_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let head = &content[..end];
    format!(
        "{head}\n\n[truncated — '{path}' is {} bytes; showing the first {}]",
        content.len(),
        MAX_REPO_FILE_BYTES
    )
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

    async fn tools(&self, project_id: i32, _context_id: &str) -> Vec<ChatTool> {
        // No repo access configured, or the project has no connected repo →
        // offer no tools, so the chat stays plain-streaming.
        if self.git.is_none() {
            return Vec::new();
        }
        let has_repo = matches!(
            projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await,
            Ok(Some(p)) if p.git_provider_connection_id.is_some()
        );
        if !has_repo {
            return Vec::new();
        }

        vec![ChatTool {
            name: "read_repo_file".to_string(),
            description: "Read a file from this project's Git repository at the deployed commit, \
via the configured Git provider API (no clone, no filesystem). Use it to confirm the real cause \
of the failure: read the file named in the error or stack trace, plus relevant config such as \
tsconfig.json, package.json, next.config.js, Dockerfile, or lockfiles. Provide a \
repository-root-relative path."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Repository-root-relative file path, e.g. 'tsconfig.json' or 'src/app/page.tsx'."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }]
    }

    async fn execute_tool(
        &self,
        project_id: i32,
        context_id: &str,
        name: &str,
        arguments: &str,
    ) -> String {
        if name != "read_repo_file" {
            return format!("Unknown tool '{name}'.");
        }
        let deployment_id: i32 = match context_id.parse() {
            Ok(id) => id,
            Err(_) => return "Invalid deployment reference.".to_string(),
        };
        let path = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string));
        match path {
            Some(p) => self.read_repo_file(project_id, deployment_id, &p).await,
            None => "Invalid arguments: expected {\"path\": \"<repo-relative path>\"}.".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_file_content_base64() {
        // GitHub returns base64 with embedded newlines.
        let raw = base64::engine::general_purpose::STANDARD.encode("hello\nworld");
        let with_newlines = format!("{}\n{}", &raw[..4], &raw[4..]);
        assert_eq!(
            decode_file_content(&with_newlines, "base64"),
            "hello\nworld"
        );
    }

    #[test]
    fn test_decode_file_content_utf8_passthrough() {
        assert_eq!(decode_file_content("{ \"a\": 1 }", "utf-8"), "{ \"a\": 1 }");
    }

    #[test]
    fn test_decode_file_content_bad_base64_falls_back() {
        // Not valid base64 → return the raw string rather than losing it.
        assert_eq!(
            decode_file_content("!!!not base64!!!", "base64"),
            "!!!not base64!!!"
        );
    }

    #[test]
    fn test_bound_truncates() {
        let big = "x".repeat(MAX_REPO_FILE_BYTES + 100);
        let out = bound(&big, "big.txt");
        assert!(out.len() < big.len() + 100);
        assert!(out.contains("truncated"));
        let small = "small";
        assert_eq!(bound(small, "s.txt"), "small");
    }

    #[test]
    fn test_bound_multibyte_boundary_does_not_panic() {
        // Build content whose byte length exceeds the cap and whose cut index
        // (MAX_REPO_FILE_BYTES) lands in the middle of a multibyte char. Each
        // emoji is 4 bytes, so a string of emoji guarantees the byte cut is not
        // on a char boundary for most cap values. This must not panic.
        // Place a 4-byte emoji so the fixed cut (MAX_REPO_FILE_BYTES) lands on
        // its 3rd byte — the naive `&content[..MAX]` would panic mid-codepoint.
        let big = "a".repeat(MAX_REPO_FILE_BYTES - 2) + &"😀".repeat(10);
        assert!(!big.is_char_boundary(MAX_REPO_FILE_BYTES));
        let out = bound(&big, "emoji.txt");
        assert!(out.contains("truncated"));
        // Accented content over the cap, too.
        let accented = "café—".repeat(MAX_REPO_FILE_BYTES); // multibyte é and em dash
        let out2 = bound(&accented, "accent.txt");
        assert!(out2.contains("truncated"));
    }

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

    #[test]
    fn test_validate_repo_path_rejects_traversal() {
        assert!(validate_repo_path("../../etc/passwd").is_err());
        assert!(validate_repo_path("src/../../../secret").is_err());
        assert!(validate_repo_path("./hidden").is_err());
        assert!(validate_repo_path("src/./x").is_err());
        assert!(validate_repo_path("..").is_err());
        assert!(validate_repo_path("src\\windows").is_err());
        assert!(validate_repo_path("/etc/passwd").is_err());
        assert!(validate_repo_path("C:/Windows").is_err());
    }

    #[test]
    fn test_validate_repo_path_accepts_normal() {
        assert!(validate_repo_path("tsconfig.json").is_ok());
        assert!(validate_repo_path("src/app/page.tsx").is_ok());
        assert!(validate_repo_path("a/b/c/d.txt").is_ok());
        // A dot inside a filename (not a whole segment) is fine.
        assert!(validate_repo_path("src/next.config.js").is_ok());
        assert!(validate_repo_path(".gitignore").is_ok());
    }
}
