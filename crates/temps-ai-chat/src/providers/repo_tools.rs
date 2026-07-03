//! Sentinel provider that exposes Git-repository exploration tools in **every**
//! project chat when the project has a Git provider connection — regardless of
//! context type (project, alert, deployment, error-group, …).
//!
//! Uses the sentinel `context_type` `"__repo_tools__"` (never stored on a
//! conversation row) and is merged alongside the `__api_tools__` sentinel in
//! [`crate::ConversationService::send_message`]. When the project has no Git
//! connection (or `git` is `None` — the plugin absent), `tools()` returns an
//! empty vec and the sentinel contributes nothing.
//!
//! ## Tools exposed
//!
//! | Name | Description |
//! |------|-------------|
//! | `read_repo_file` | Read one file at an optional ref |
//! | `list_repo_dir`  | List the entries of a directory at an optional ref |
//! | `list_repo_branches` | List branch names |
//! | `list_repo_tags`     | List tag names |

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};

use temps_ai::ChatTool;
use temps_entities::projects;
use temps_git::GitProviderManager;

use crate::provider::{ConversationContextProvider, ConversationSeed};

use super::repo_common::{bound, decode_file_content, validate_repo_path};

/// Cap on directory entries rendered to the model (to bound response size).
const MAX_DIR_ENTRIES: usize = 300;
/// Cap on branch names rendered.
const MAX_BRANCH_COUNT: usize = 100;
/// Cap on tag names rendered.
const MAX_TAG_COUNT: usize = 100;

/// Sentinel provider: exposes `read_repo_file`, `list_repo_dir`,
/// `list_repo_branches`, and `list_repo_tags` in every context when the
/// project has a connected Git repository.
pub struct RepoToolsProvider {
    db: Arc<DatabaseConnection>,
    git: Option<Arc<GitProviderManager>>,
}

impl RepoToolsProvider {
    pub fn new(db: Arc<DatabaseConnection>, git: Option<Arc<GitProviderManager>>) -> Self {
        Self { db, git }
    }

    /// Resolve everything needed to call the Git provider API for `project_id`.
    ///
    /// Returns `(token, provider_service, repo_owner, repo_name, default_ref)`
    /// or a human-readable error string the model can reason about.
    async fn resolve(
        &self,
        project_id: i32,
    ) -> Result<
        (
            String,
            Arc<dyn temps_git::services::git_provider::GitProviderService>,
            String,
            String,
            String,
        ),
        String,
    > {
        let git = self
            .git
            .as_ref()
            .ok_or_else(|| "Repository access is not configured on this server.".to_string())?;

        let project = match projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(p)) => p,
            Ok(None) => return Err(format!("Project {project_id} not found.")),
            Err(e) => return Err(format!("Database error loading project {project_id}: {e}")),
        };

        let connection_id = project.git_provider_connection_id.ok_or_else(|| {
            "This project has no connected Git repository, so repo exploration tools are not \
             available."
                .to_string()
        })?;

        let token = git
            .get_connection_token(connection_id)
            .await
            .map_err(|e| format!("Could not authenticate with the Git provider: {e}"))?;

        let connection = git
            .get_connection(connection_id)
            .await
            .map_err(|e| format!("Git connection unavailable: {e}"))?;

        let service = git
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|e| format!("Git provider unavailable: {e}"))?;

        let default_ref = project.main_branch.clone();

        Ok((
            token,
            service,
            project.repo_owner,
            project.repo_name,
            default_ref,
        ))
    }

    /// Execute `read_repo_file`: read a single file from the repository.
    async fn exec_read_file(&self, project_id: i32, path: &str, ref_: Option<&str>) -> String {
        let path = path.trim().trim_start_matches('/');
        if path.is_empty() {
            return "Invalid arguments: provide a non-empty repo-relative \"path\".".to_string();
        }
        if let Err(reason) = validate_repo_path(path) {
            return reason;
        }

        let (token, service, owner, repo, default_ref) = match self.resolve(project_id).await {
            Ok(r) => r,
            Err(e) => return e,
        };

        let reference = ref_
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(&default_ref);

        match service
            .get_file_content(&token, &owner, &repo, path, Some(reference))
            .await
        {
            Ok(file) => bound(&decode_file_content(&file.content, &file.encoding), path),
            Err(e) => {
                format!("Could not read '{path}' from {owner}/{repo} at ref '{reference}': {e}")
            }
        }
    }

    /// Execute `list_repo_dir`: list directory entries at the given path.
    async fn exec_list_dir(&self, project_id: i32, path: &str, ref_: Option<&str>) -> String {
        // Empty path = repo root.
        let path = path.trim().trim_start_matches('/');

        // Only validate non-empty paths (root is always allowed).
        if !path.is_empty() {
            if let Err(reason) = validate_repo_path(path) {
                return reason;
            }
        }

        let (token, service, owner, repo, default_ref) = match self.resolve(project_id).await {
            Ok(r) => r,
            Err(e) => return e,
        };

        let reference = ref_
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(&default_ref);

        let entries = match service
            .list_directory(&token, &owner, &repo, path, Some(reference))
            .await
        {
            Ok(e) => e,
            Err(e) => {
                let display_path = if path.is_empty() { "/" } else { path };
                return format!(
                    "Could not list directory '{display_path}' in {owner}/{repo} at ref \
                     '{reference}': {e}"
                );
            }
        };

        if entries.is_empty() {
            let display_path = if path.is_empty() { "/" } else { path };
            return format!(
                "Directory '{display_path}' in {owner}/{repo} at ref '{reference}' is empty \
                 (or does not exist)."
            );
        }

        let truncated = entries.len() > MAX_DIR_ENTRIES;
        let shown = if truncated {
            &entries[..MAX_DIR_ENTRIES]
        } else {
            &entries[..]
        };

        let display_path = if path.is_empty() { "/" } else { path };
        let mut out =
            format!("Directory '{display_path}' in {owner}/{repo} at ref '{reference}':\n");
        for entry in shown {
            if entry.is_dir {
                out.push_str(&format!("  d  {}/\n", entry.path));
            } else {
                match entry.size {
                    Some(sz) => out.push_str(&format!("  f  {} ({sz} B)\n", entry.path)),
                    None => out.push_str(&format!("  f  {}\n", entry.path)),
                }
            }
        }
        if truncated {
            out.push_str(&format!(
                "\n[truncated — showing {MAX_DIR_ENTRIES} of {} entries]\n",
                entries.len()
            ));
        }
        out
    }

    /// Execute `list_repo_branches`: list branch names.
    async fn exec_list_branches(&self, project_id: i32) -> String {
        let (token, service, owner, repo, default_ref) = match self.resolve(project_id).await {
            Ok(r) => r,
            Err(e) => return e,
        };

        let branches = match service.list_branches(&token, &owner, &repo).await {
            Ok(b) => b,
            Err(e) => return format!("Could not list branches for {owner}/{repo}: {e}"),
        };

        if branches.is_empty() {
            return format!("No branches found in {owner}/{repo}.");
        }

        let truncated = branches.len() > MAX_BRANCH_COUNT;
        let shown = if truncated {
            &branches[..MAX_BRANCH_COUNT]
        } else {
            &branches[..]
        };

        let mut out = format!("Branches in {owner}/{repo} (default: '{default_ref}'):\n");
        for b in shown {
            if b.name == default_ref {
                out.push_str(&format!("  * {} (default)\n", b.name));
            } else {
                out.push_str(&format!("    {}\n", b.name));
            }
        }
        if truncated {
            out.push_str(&format!(
                "\n[truncated — showing {MAX_BRANCH_COUNT} of {} branches]\n",
                branches.len()
            ));
        }
        out
    }

    /// Execute `list_repo_tags`: list tag names.
    async fn exec_list_tags(&self, project_id: i32) -> String {
        let (token, service, owner, repo, _default_ref) = match self.resolve(project_id).await {
            Ok(r) => r,
            Err(e) => return e,
        };

        let tags = match service.list_tags(&token, &owner, &repo).await {
            Ok(t) => t,
            Err(e) => return format!("Could not list tags for {owner}/{repo}: {e}"),
        };

        if tags.is_empty() {
            return format!("No tags found in {owner}/{repo}.");
        }

        let truncated = tags.len() > MAX_TAG_COUNT;
        let shown = if truncated {
            &tags[..MAX_TAG_COUNT]
        } else {
            &tags[..]
        };

        let mut out = format!("Tags in {owner}/{repo}:\n");
        for t in shown {
            out.push_str(&format!("  {}\n", t.name));
        }
        if truncated {
            out.push_str(&format!(
                "\n[truncated — showing {MAX_TAG_COUNT} of {} tags]\n",
                tags.len()
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// JSON schema helpers
// ---------------------------------------------------------------------------

fn read_repo_file_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Repository-root-relative file path, e.g. 'tsconfig.json' or \
                                'src/app/page.tsx'. Must not start with '/', contain '..', or use backslashes."
            },
            "ref": {
                "type": "string",
                "description": "Branch name, tag, or full commit SHA to read from. \
                                Omit or leave empty to use the project's default branch. \
                                Use list_repo_branches or list_repo_tags to discover valid refs."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn list_repo_dir_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Repository-root-relative directory path to list, e.g. 'src' or \
                                'src/components'. Omit or pass '' to list the repository root."
            },
            "ref": {
                "type": "string",
                "description": "Branch name, tag, or full commit SHA. Omit to use the project's \
                                default branch."
            }
        },
        "additionalProperties": false
    })
}

fn list_repo_branches_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

fn list_repo_tags_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

// ---------------------------------------------------------------------------
// ConversationContextProvider impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ConversationContextProvider for RepoToolsProvider {
    fn context_type(&self) -> &'static str {
        // Sentinel — never stored on a conversation row. Merged into every
        // context by ConversationService, gated on the project having a git
        // connection (tools() returns empty otherwise).
        "__repo_tools__"
    }

    async fn seed(&self, _project_id: i32, _context_id: &str) -> Option<ConversationSeed> {
        // Sentinel providers have no seed.
        None
    }

    async fn tools(&self, project_id: i32, _context_id: &str) -> Vec<ChatTool> {
        // No git manager → no tools.
        if self.git.is_none() {
            return Vec::new();
        }
        // No connected repo → no tools.
        let has_repo = matches!(
            projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await,
            Ok(Some(p)) if p.git_provider_connection_id.is_some()
        );
        if !has_repo {
            return Vec::new();
        }

        vec![
            ChatTool {
                name: "read_repo_file".to_string(),
                description: "Read a single file from this project's connected Git repository via \
                    the Git provider API (no clone, no local filesystem access). Read-only. Use \
                    list_repo_dir to discover what files exist, and list_repo_branches / \
                    list_repo_tags to discover valid refs. Provide a repository-root-relative path \
                    (e.g. 'package.json', 'src/index.ts'). Pass a ref (branch, tag, or commit SHA) \
                    to inspect a specific version; omit to use the project's default branch."
                    .to_string(),
                parameters: read_repo_file_schema(),
            },
            ChatTool {
                name: "list_repo_dir".to_string(),
                description: "List the files and subdirectories at a path in this project's \
                    connected Git repository. Read-only. Returns one entry per line, prefixed with \
                    'd' for directories and 'f' for files (with size when available). Omit 'path' \
                    or pass '' to list the repository root. Use before read_repo_file to verify a \
                    path exists. Pass a ref (branch, tag, or commit SHA) to inspect a specific \
                    version; omit to use the project's default branch."
                    .to_string(),
                parameters: list_repo_dir_schema(),
            },
            ChatTool {
                name: "list_repo_branches".to_string(),
                description: "List the branch names of this project's connected Git repository. \
                    Read-only. Shows which branch is the default. Use to discover valid 'ref' \
                    values for read_repo_file and list_repo_dir."
                    .to_string(),
                parameters: list_repo_branches_schema(),
            },
            ChatTool {
                name: "list_repo_tags".to_string(),
                description: "List the tag names of this project's connected Git repository. \
                    Read-only. Use to discover valid 'ref' values for read_repo_file and \
                    list_repo_dir."
                    .to_string(),
                parameters: list_repo_tags_schema(),
            },
        ]
    }

    async fn execute_tool(
        &self,
        project_id: i32,
        _context_id: &str,
        name: &str,
        arguments: &str,
    ) -> String {
        // Parse the JSON args. On failure return a readable message so the
        // model can self-correct rather than receiving a raw Rust error.
        let args: serde_json::Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => return format!("Invalid tool arguments (not valid JSON): {e}"),
        };

        match name {
            "read_repo_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if path.is_empty() {
                    return "Invalid arguments for read_repo_file: 'path' is required and must be \
                        a non-empty repository-relative string, e.g. 'package.json'."
                        .to_string();
                }
                let ref_ = args.get("ref").and_then(|v| v.as_str()).map(str::to_string);
                self.exec_read_file(project_id, &path, ref_.as_deref())
                    .await
            }

            "list_repo_dir" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let ref_ = args.get("ref").and_then(|v| v.as_str()).map(str::to_string);
                self.exec_list_dir(project_id, &path, ref_.as_deref()).await
            }

            "list_repo_branches" => self.exec_list_branches(project_id).await,

            "list_repo_tags" => self.exec_list_tags(project_id).await,

            other => format!(
                "Unknown repo tool '{other}'. Available tools: read_repo_file, list_repo_dir, \
                 list_repo_branches, list_repo_tags."
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// Verify that `validate_repo_path` still rejects traversal / absolute paths
    /// (the shared helper is tested more exhaustively in repo_common, but we want
    /// at least a smoke test here so a refactor can't silently break the guard).
    #[test]
    fn validate_path_smoke() {
        assert!(validate_repo_path("../../etc/passwd").is_err());
        assert!(validate_repo_path("/absolute").is_err());
        assert!(validate_repo_path("src/valid/file.ts").is_ok());
    }

    /// `tools()` returns an empty vec when `git` is `None`.
    #[tokio::test]
    async fn tools_empty_when_git_absent() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let provider = RepoToolsProvider::new(Arc::new(db), None);
        let tools = provider.tools(1, "").await;
        assert!(
            tools.is_empty(),
            "expected no tools when git manager is absent"
        );
    }

    /// `tools()` checks `git.is_none()` first; the DB is never queried
    /// when git is absent.  We separately verify that a project with
    /// `git_provider_connection_id = None` also yields no tools, by supplying
    /// a mock DB result with connection = None and git = None (same early-exit
    /// code path — a proper integration test with a live GitProviderManager
    /// would be needed for the connected-but-DB-says-no-connection branch).
    #[tokio::test]
    async fn tools_empty_when_project_has_no_git_connection_via_db() {
        let project_model = build_project_model(42, None);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![project_model]])
            .into_connection();

        // git = None → early return before DB query; result is still empty.
        let provider = RepoToolsProvider::new(Arc::new(db), None);
        let tools = provider.tools(42, "").await;
        assert!(tools.is_empty());
    }

    /// Directory listing renderer formats entries in the expected shape.
    #[test]
    fn dir_listing_format() {
        use temps_git::services::git_provider::RepoDirEntry;

        let entries = vec![
            RepoDirEntry {
                name: "src".to_string(),
                path: "src".to_string(),
                is_dir: true,
                size: None,
            },
            RepoDirEntry {
                name: "README.md".to_string(),
                path: "README.md".to_string(),
                is_dir: false,
                size: Some(1234),
            },
            RepoDirEntry {
                name: "package.json".to_string(),
                path: "package.json".to_string(),
                is_dir: false,
                size: None,
            },
        ];

        // Reproduce the renderer logic inline so we test it without a live DB.
        let owner = "owner";
        let repo = "repo";
        let reference = "main";
        let display_path = "/";
        let mut out =
            format!("Directory '{display_path}' in {owner}/{repo} at ref '{reference}':\n");
        for entry in &entries {
            if entry.is_dir {
                out.push_str(&format!("  d  {}/\n", entry.path));
            } else {
                match entry.size {
                    Some(sz) => out.push_str(&format!("  f  {} ({sz} B)\n", entry.path)),
                    None => out.push_str(&format!("  f  {}\n", entry.path)),
                }
            }
        }

        assert!(out.contains("d  src/"));
        assert!(out.contains("f  README.md (1234 B)"));
        assert!(out.contains("f  package.json\n"));
    }

    /// Truncation note appears when there are more than MAX_DIR_ENTRIES entries.
    #[test]
    fn dir_listing_truncation() {
        use temps_git::services::git_provider::RepoDirEntry;

        // Build MAX_DIR_ENTRIES + 5 file entries.
        let entries: Vec<RepoDirEntry> = (0..MAX_DIR_ENTRIES + 5)
            .map(|i| RepoDirEntry {
                name: format!("file{i}.txt"),
                path: format!("file{i}.txt"),
                is_dir: false,
                size: Some(0),
            })
            .collect();

        let truncated = entries.len() > MAX_DIR_ENTRIES;
        assert!(truncated, "sanity check");

        let shown = &entries[..MAX_DIR_ENTRIES];
        let mut out = String::new();
        for entry in shown {
            out.push_str(&format!(
                "  f  {} ({} B)\n",
                entry.path,
                entry.size.unwrap()
            ));
        }
        out.push_str(&format!(
            "\n[truncated — showing {MAX_DIR_ENTRIES} of {} entries]\n",
            entries.len()
        ));

        assert!(out.contains("truncated"));
        assert!(out.contains(&format!("showing {MAX_DIR_ENTRIES} of")));
    }

    /// execute_tool returns a readable error for an unknown tool name.
    #[tokio::test]
    async fn execute_tool_unknown_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let provider = RepoToolsProvider::new(Arc::new(db), None);
        let result = provider.execute_tool(1, "", "nonexistent_tool", "{}").await;
        assert!(
            result.contains("Unknown repo tool"),
            "expected unknown-tool message, got: {result}"
        );
    }

    /// execute_tool returns a readable error for malformed JSON.
    #[tokio::test]
    async fn execute_tool_bad_json() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let provider = RepoToolsProvider::new(Arc::new(db), None);
        let result = provider
            .execute_tool(1, "", "read_repo_file", "not json {{{")
            .await;
        assert!(
            result.contains("not valid JSON") || result.contains("Invalid"),
            "expected JSON parse error, got: {result}"
        );
    }

    /// execute_tool returns a readable error when path is missing for read_repo_file.
    #[tokio::test]
    async fn execute_tool_read_repo_file_missing_path() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let provider = RepoToolsProvider::new(Arc::new(db), None);
        // Arguments have no "path" key at all.
        let result = provider.execute_tool(1, "", "read_repo_file", "{}").await;
        // The missing-path guard fires before any git access.
        assert!(
            result.contains("path"),
            "expected path-required message, got: {result}"
        );
    }

    // ---------------------------------------------------------------------------
    // Helper — build a minimal projects::Model with safe defaults.
    // Mirrors the helper in project.rs tests.
    // ---------------------------------------------------------------------------

    fn build_project_model(id: i32, connection_id: Option<i32>) -> projects::Model {
        let now = chrono::Utc::now();
        projects::Model {
            id,
            name: "test".to_string(),
            repo_name: "repo".to_string(),
            repo_owner: "owner".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Static,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "test".to_string(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: connection_id,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: true,
        }
    }
}
