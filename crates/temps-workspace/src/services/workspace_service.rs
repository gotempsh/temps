use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, Order, PaginatorTrait, QueryFilter, QueryOrder,
};
use std::sync::Arc;

use temps_entities::{workspace_messages, workspace_sessions};

use crate::error::WorkspaceError;
use crate::services::preview_password;

/// Request to create a new workspace session.
#[derive(Debug, Clone)]
pub struct CreateSessionRequest {
    pub project_id: i32,
    pub user_id: i32,
    pub ai_provider: String,
    /// Optional model override. `None` means "fall back to the provider's
    /// default_model from platform settings, or the CLI's own default if that
    /// is also unset". Persisted as-is on the session row so it survives
    /// provider-level model changes later.
    pub ai_model: Option<String>,
    /// Branch the workspace should check out in its sandbox. None = project default.
    /// If `base_branch_name` is also set, this is the *new* branch to be created
    /// locally off `base_branch_name` during sandbox initialization.
    pub branch_name: Option<String>,
    /// Optional base branch to fork the session's branch off. When set, the
    /// sandbox clones `base_branch_name` from the remote and then creates
    /// `branch_name` as a new local branch on top of it.
    pub base_branch_name: Option<String>,
    pub metadata: Option<serde_json::Value>,
    /// Slugs of skill definitions to inject into the sandbox at session start.
    /// Resolved from `project_skill_definitions` (falls back to global).
    pub skills: Option<Vec<String>>,
    /// Slugs of MCP server definitions to inject into the sandbox at session
    /// start. Deep-merged into `/home/temps/.claude.json` (user-level config,
    /// kept out of the bind-mounted repo to avoid leaking resolved secrets
    /// into PR diffs). Resolved from `project_mcp_definitions` (falls back
    /// to global).
    pub mcp_servers: Option<Vec<String>>,
    /// CPU limit in vCPU cores (e.g. 2.0). `None` → server default applies.
    pub cpu_limit: Option<f32>,
    /// Memory limit in MB. `None` → server default applies.
    pub memory_limit_mb: Option<i32>,
}

/// Request to send a message in a workspace session.
#[derive(Debug, Clone)]
pub struct SendMessageRequest {
    pub session_id: i32,
    pub role: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
}

/// Fields to update on a session (all optional).
#[derive(Debug, Default)]
pub struct UpdateSessionFields {
    pub status: Option<String>,
    pub sandbox_container_id: Option<String>,
    pub work_dir: Option<String>,
    pub branch_name: Option<String>,
    pub ai_model: Option<String>,
    pub tokens_input: Option<i32>,
    pub tokens_output: Option<i32>,
    pub estimated_cost_cents: Option<i32>,
    pub files_changed: Option<i32>,
    pub closed_at: Option<chrono::DateTime<Utc>>,
    /// Per-session idle timeout override. `Some(Some(n))` sets the value,
    /// `Some(None)` explicitly clears it (falls back to server default),
    /// `None` leaves it unchanged.
    pub idle_timeout_minutes: Option<Option<i32>>,
    /// Update the session's title. `Some(Some(s))` sets it, `Some(None)`
    /// clears it (falls back to "Session #{id}" in the UI), `None` leaves
    /// it unchanged.
    pub title: Option<Option<String>>,
    /// CPU core limit override (vCPUs). Same triple-state semantics.
    pub cpu_limit: Option<Option<f32>>,
    /// Memory limit override in MB. Same triple-state semantics.
    pub memory_limit_mb: Option<Option<i32>>,
    /// PID limit override. Same triple-state semantics.
    pub pids_limit: Option<Option<i32>>,
}

/// Session with its messages.
pub struct SessionWithMessages {
    pub session: workspace_sessions::Model,
    pub messages: Vec<workspace_messages::Model>,
}

/// Result of creating (or regenerating) a session's preview password.
/// The plaintext is only ever returned here — never persisted and never
/// returned by any other call.
#[derive(Debug)]
pub struct CreatedSession {
    pub session: workspace_sessions::Model,
    /// Plaintext preview password. Show to the user **once**; it's the
    /// only time it will be surfaced by the API.
    pub preview_password_plaintext: String,
}

/// Workspace service handles CRUD for sessions and messages.
pub struct WorkspaceService {
    db: Arc<DatabaseConnection>,
}

impl WorkspaceService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Create a new workspace session. Also generates the session's
    /// one-time preview password — the plaintext is returned in
    /// `CreatedSession.preview_password_plaintext` and only there.
    pub async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<CreatedSession, WorkspaceError> {
        if request.ai_provider.is_empty() {
            return Err(WorkspaceError::Validation {
                message: "ai_provider cannot be empty".to_string(),
            });
        }

        // Allowlist `ai_provider` against the catalog. Without this, the
        // session manager's `build_chat_cmd` fallback would execute the raw
        // string as a binary inside the sandbox container.
        if temps_agents::ai_cli::catalog::find_provider(&request.ai_provider).is_none() {
            return Err(WorkspaceError::Validation {
                message: format!(
                    "ai_provider {:?} is not a known CLI provider",
                    request.ai_provider
                ),
            });
        }

        // Cap `ai_model` length and reject control characters. The model
        // string is spliced into a `bash -lc` command (single-quoted, so
        // metacharacter injection is blocked) for the OpenCode provider, and
        // passed as a direct argv slot for `claude` and `codex`. Bound the
        // length so a runaway value can't bloat persisted rows or argv.
        if let Some(m) = &request.ai_model {
            if m.len() > 200 {
                return Err(WorkspaceError::Validation {
                    message: "ai_model must be 200 characters or less".to_string(),
                });
            }
            if m.chars().any(|c| c.is_control()) {
                return Err(WorkspaceError::Validation {
                    message: "ai_model must not contain control characters".to_string(),
                });
            }
        }

        // If a base branch is specified, the new branch_name must also be
        // provided — otherwise we'd have nothing to fork into.
        if request.base_branch_name.is_some() && request.branch_name.is_none() {
            return Err(WorkspaceError::Validation {
                message: "branch_name is required when base_branch_name is set".to_string(),
            });
        }

        if let Some(cpu) = request.cpu_limit {
            if !cpu.is_finite() || cpu <= 0.0 {
                return Err(WorkspaceError::Validation {
                    message: "cpu_limit must be a positive number".to_string(),
                });
            }
        }
        if let Some(mem) = request.memory_limit_mb {
            if mem <= 0 {
                return Err(WorkspaceError::Validation {
                    message: "memory_limit_mb must be a positive integer".to_string(),
                });
            }
        }

        // Generate the preview password up front so we can store the hash
        // and return the plaintext in a single round trip to the caller.
        // If hashing fails, we fail the whole create — a session without
        // a preview password would be silently unreachable, which is
        // worse than refusing to create it.
        let gp =
            preview_password::generate().map_err(|reason| WorkspaceError::PasswordHashFailed {
                session_id: 0,
                reason,
            })?;

        let now = Utc::now();
        let session = workspace_sessions::ActiveModel {
            public_id: Set(crate::services::public_id::generate()),
            project_id: Set(request.project_id),
            user_id: Set(request.user_id),
            status: Set("active".to_string()),
            ai_provider: Set(request.ai_provider),
            ai_model: Set(request.ai_model),
            branch_name: Set(request.branch_name),
            base_branch_name: Set(request.base_branch_name),
            metadata: Set(request.metadata),
            skills_config: Set(request.skills.filter(|v| !v.is_empty()).map(|v| {
                serde_json::Value::Array(v.into_iter().map(serde_json::Value::String).collect())
            })),
            mcp_servers_config: Set(request.mcp_servers.filter(|v| !v.is_empty()).map(|v| {
                serde_json::Value::Array(v.into_iter().map(serde_json::Value::String).collect())
            })),
            cpu_milli: Set(request.cpu_limit.map(|v| (v * 1000.0).round() as i32)),
            memory_limit_mb: Set(request.memory_limit_mb),
            last_activity_at: Set(now),
            started_at: Set(now),
            created_at: Set(now),
            tokens_input: Set(0),
            tokens_output: Set(0),
            estimated_cost_cents: Set(0),
            files_changed: Set(0),
            preview_password_hash: Set(Some(gp.hash)),
            preview_password_hint: Set(Some(gp.hint)),
            ..Default::default()
        };

        let model = session.insert(self.db.as_ref()).await?;
        Ok(CreatedSession {
            session: model,
            preview_password_plaintext: gp.plaintext,
        })
    }

    /// Regenerate the preview password for an existing session. Invalidates
    /// the old password. Returns the new plaintext once.
    pub async fn regenerate_preview_password(
        &self,
        session_id: i32,
    ) -> Result<String, WorkspaceError> {
        let session = self.get_session(session_id).await?;
        let gp = preview_password::generate()
            .map_err(|reason| WorkspaceError::PasswordHashFailed { session_id, reason })?;
        let mut active: workspace_sessions::ActiveModel = session.into();
        active.preview_password_hash = Set(Some(gp.hash));
        active.preview_password_hint = Set(Some(gp.hint));
        active.update(self.db.as_ref()).await?;
        Ok(gp.plaintext)
    }

    /// Get a session by ID.
    pub async fn get_session(
        &self,
        session_id: i32,
    ) -> Result<workspace_sessions::Model, WorkspaceError> {
        workspace_sessions::Entity::find_by_id(session_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(WorkspaceError::SessionNotFound { session_id })
    }

    /// Get a session with all its messages.
    pub async fn get_session_with_messages(
        &self,
        session_id: i32,
    ) -> Result<SessionWithMessages, WorkspaceError> {
        let session = self.get_session(session_id).await?;
        let messages = workspace_messages::Entity::find()
            .filter(workspace_messages::Column::SessionId.eq(session_id))
            .order_by_asc(workspace_messages::Column::Id)
            .all(self.db.as_ref())
            .await?;

        Ok(SessionWithMessages { session, messages })
    }

    /// List sessions for a project with pagination.
    pub async fn list_sessions(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<workspace_sessions::Model>, u64), WorkspaceError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let paginator = workspace_sessions::Entity::find()
            .filter(workspace_sessions::Column::ProjectId.eq(project_id))
            .order_by(workspace_sessions::Column::CreatedAt, Order::Desc)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;

        Ok((items, total))
    }

    /// Update session fields.
    pub async fn update_session(
        &self,
        session_id: i32,
        fields: UpdateSessionFields,
    ) -> Result<workspace_sessions::Model, WorkspaceError> {
        let session = self.get_session(session_id).await?;
        let mut active: workspace_sessions::ActiveModel = session.into();

        if let Some(status) = fields.status {
            active.status = Set(status);
        }
        if let Some(container_id) = fields.sandbox_container_id {
            active.sandbox_container_id = Set(Some(container_id));
        }
        if let Some(work_dir) = fields.work_dir {
            active.work_dir = Set(Some(work_dir));
        }
        if let Some(branch_name) = fields.branch_name {
            active.branch_name = Set(Some(branch_name));
        }
        if let Some(ai_model) = fields.ai_model {
            active.ai_model = Set(Some(ai_model));
        }
        if let Some(tokens_input) = fields.tokens_input {
            active.tokens_input = Set(tokens_input);
        }
        if let Some(tokens_output) = fields.tokens_output {
            active.tokens_output = Set(tokens_output);
        }
        if let Some(cost) = fields.estimated_cost_cents {
            active.estimated_cost_cents = Set(cost);
        }
        if let Some(files) = fields.files_changed {
            active.files_changed = Set(files);
        }
        if let Some(closed_at) = fields.closed_at {
            active.closed_at = Set(Some(closed_at));
        }
        if let Some(idle) = fields.idle_timeout_minutes {
            active.idle_timeout_minutes = Set(idle);
        }
        if let Some(title) = fields.title {
            active.title = Set(title);
        }
        if let Some(cpu) = fields.cpu_limit {
            active.cpu_milli = Set(cpu.map(|v| (v * 1000.0).round() as i32));
        }
        if let Some(mem) = fields.memory_limit_mb {
            active.memory_limit_mb = Set(mem);
        }
        if let Some(pids) = fields.pids_limit {
            active.pids_limit = Set(pids);
        }

        active.last_activity_at = Set(Utc::now());

        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Close a workspace session.
    pub async fn close_session(
        &self,
        session_id: i32,
    ) -> Result<workspace_sessions::Model, WorkspaceError> {
        let session = self.get_session(session_id).await?;

        if session.status == "closed" {
            return Err(WorkspaceError::SessionNotActive {
                session_id,
                status: "closed".to_string(),
            });
        }

        self.update_session(
            session_id,
            UpdateSessionFields {
                status: Some("closed".to_string()),
                closed_at: Some(Utc::now()),
                ..Default::default()
            },
        )
        .await
    }

    /// Hard-delete a workspace session. Cascades to `workspace_messages`
    /// via the FK `on_delete = Cascade`. The caller is responsible for
    /// releasing the sandbox container and cancelling any in-flight run
    /// before invoking this.
    pub async fn delete_session(&self, session_id: i32) -> Result<(), WorkspaceError> {
        // Ensure it exists so we return a typed NotFound rather than a
        // silent no-op delete.
        let _ = self.get_session(session_id).await?;

        workspace_sessions::Entity::delete_by_id(session_id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    /// Reopen a previously closed workspace session.
    ///
    /// Flips status back to `active` and clears `closed_at`. The sandbox
    /// container is recreated lazily on the next message via
    /// `MessageExecutor::initialize_sandbox`.
    pub async fn reopen_session(
        &self,
        session_id: i32,
    ) -> Result<workspace_sessions::Model, WorkspaceError> {
        let session = self.get_session(session_id).await?;

        if session.status != "closed" {
            return Err(WorkspaceError::SessionNotActive {
                session_id,
                status: session.status,
            });
        }

        let mut active: workspace_sessions::ActiveModel = session.into();
        active.status = Set("active".to_string());
        active.closed_at = Set(None);
        active.last_activity_at = Set(Utc::now());
        // Clear the stale sandbox id; a new container will be created.
        active.sandbox_container_id = Set(None);

        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Append a message to a session.
    pub async fn append_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<workspace_messages::Model, WorkspaceError> {
        // Validate the session exists and is active
        let session = self.get_session(request.session_id).await?;
        if session.status == "closed" {
            return Err(WorkspaceError::SessionNotActive {
                session_id: request.session_id,
                status: "closed".to_string(),
            });
        }

        let message = workspace_messages::ActiveModel {
            session_id: Set(request.session_id),
            role: Set(request.role),
            content: Set(request.content),
            metadata: Set(request.metadata),
            created_at: Set(Utc::now()),
            ..Default::default()
        };

        let model = message.insert(self.db.as_ref()).await?;

        // Update session last_activity_at
        let _ = self
            .update_session(request.session_id, UpdateSessionFields::default())
            .await;

        Ok(model)
    }

    /// Get messages after a given ID (for SSE polling).
    pub async fn get_messages_after(
        &self,
        session_id: i32,
        after_id: i64,
    ) -> Result<Vec<workspace_messages::Model>, WorkspaceError> {
        let messages = workspace_messages::Entity::find()
            .filter(workspace_messages::Column::SessionId.eq(session_id))
            .filter(workspace_messages::Column::Id.gt(after_id))
            .order_by_asc(workspace_messages::Column::Id)
            .all(self.db.as_ref())
            .await?;

        Ok(messages)
    }

    /// Highest user-message id on this session that has already been answered —
    /// i.e., there is some non-user message with a greater id. A fresh drain
    /// loop seeds its watermark from this so it only concatenates *unanswered*
    /// pending user messages rather than replaying the whole transcript.
    pub async fn last_answered_user_message_id(
        &self,
        session_id: i32,
    ) -> Result<i64, WorkspaceError> {
        let last_non_user = workspace_messages::Entity::find()
            .filter(workspace_messages::Column::SessionId.eq(session_id))
            .filter(workspace_messages::Column::Role.ne("user"))
            .order_by_desc(workspace_messages::Column::Id)
            .one(self.db.as_ref())
            .await?;
        let Some(last) = last_non_user else {
            return Ok(0);
        };
        let prior_user = workspace_messages::Entity::find()
            .filter(workspace_messages::Column::SessionId.eq(session_id))
            .filter(workspace_messages::Column::Role.eq("user"))
            .filter(workspace_messages::Column::Id.lt(last.id))
            .order_by_desc(workspace_messages::Column::Id)
            .one(self.db.as_ref())
            .await?;
        Ok(prior_user.map(|m| m.id).unwrap_or(0))
    }

    /// Count active sessions for a project (for concurrency limits).
    pub async fn count_active_sessions(&self, project_id: i32) -> Result<u64, WorkspaceError> {
        let count = workspace_sessions::Entity::find()
            .filter(workspace_sessions::Column::ProjectId.eq(project_id))
            .filter(workspace_sessions::Column::Status.eq("active"))
            .count(self.db.as_ref())
            .await?;

        Ok(count)
    }

    /// Bump `last_activity_at` to NOW() for a single session. Cheap,
    /// single-row UPDATE — used by the terminal WS to keep long-running
    /// background agent sessions from being reaped while they're actively
    /// producing PTY output, even if no user keystrokes are coming in.
    pub async fn touch_activity(&self, session_id: i32) -> Result<(), WorkspaceError> {
        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE workspace_sessions SET last_activity_at = NOW() WHERE id = $1",
            [session_id.into()],
        );
        self.db.as_ref().execute(stmt).await?;
        Ok(())
    }

    /// Mark stale sessions as closed using each session's own
    /// `idle_timeout_minutes` override, falling back to `default_minutes`
    /// when null. Called at startup and periodically by the sweeper.
    /// Returns the list of session IDs that were closed so the caller can
    /// tear down their sandbox handles.
    pub async fn recover_stale_sessions(
        &self,
        default_minutes: i64,
    ) -> Result<Vec<i32>, WorkspaceError> {
        let db = self.db.as_ref();

        // Postgres lets us do the per-row comparison in a single UPDATE
        // by using COALESCE + interval arithmetic. Each row is evaluated
        // against its own idle_timeout_minutes (or the default when null).
        let result = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            // `idle_timeout_minutes = 0` means "never time out" — skip
            // those rows entirely. Null falls back to the server default.
            "UPDATE workspace_sessions \
             SET status = 'closed', closed_at = NOW() \
             WHERE status = 'active' \
               AND COALESCE(idle_timeout_minutes, $1) > 0 \
               AND last_activity_at < NOW() - (COALESCE(idle_timeout_minutes, $1) || ' minutes')::interval \
             RETURNING id",
            [(default_minutes as i32).into()],
        );

        let rows = db.query_all(result).await?;
        let closed_ids: Vec<i32> = rows
            .iter()
            .filter_map(|r| r.try_get::<i32>("", "id").ok())
            .collect();

        if !closed_ids.is_empty() {
            tracing::info!(
                "Recovered {} stale workspace sessions (default {}min, per-session overrides honored): {:?}",
                closed_ids.len(),
                default_minutes,
                closed_ids
            );
        }

        Ok(closed_ids)
    }

    /// Find every active session whose newest message is a `user` row — i.e.
    /// an executor was running for that user message when the previous
    /// `temps serve` process exited (crash, restart, OOM kill, deploy). Insert
    /// a synthetic assistant error message so the UI's "Thinking…" indicator
    /// clears as soon as the user reopens the session.
    ///
    /// Without this, every server restart leaves dangling spinners forever
    /// because the in-memory tokio task that would have written the terminal
    /// turn died with the previous process and nothing on disk knows it.
    /// List (session_id, project_id) pairs for every currently active
    /// workspace session. Used at server startup to adopt existing sandbox
    /// containers back into the in-memory session manager.
    pub async fn list_active_sessions_with_project(
        &self,
    ) -> Result<Vec<(i32, i32)>, WorkspaceError> {
        use sea_orm::{ColumnTrait, QueryFilter};
        let rows = workspace_sessions::Entity::find()
            .filter(workspace_sessions::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;
        Ok(rows.into_iter().map(|s| (s.id, s.project_id)).collect())
    }

    pub async fn reconcile_orphaned_runs(&self) -> Result<Vec<i32>, WorkspaceError> {
        // Pick the latest message per active session in a single query.
        let stmt = sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT DISTINCT ON (m.session_id) m.session_id, m.role \
             FROM workspace_messages m \
             JOIN workspace_sessions s ON s.id = m.session_id \
             WHERE s.status = 'active' \
             ORDER BY m.session_id, m.id DESC"
                .to_string(),
        );

        let rows = self.db.query_all(stmt).await?;
        let mut orphaned: Vec<i32> = Vec::new();
        for row in rows {
            let session_id: i32 = row.try_get("", "session_id").unwrap_or(0);
            let role: String = row.try_get("", "role").unwrap_or_default();
            // `user` means we owed an assistant turn. `ai_event` means a run
            // was streaming output and got cut mid-flight — also orphaned.
            if role == "user" || role == "ai_event" {
                orphaned.push(session_id);
            }
        }

        let count = orphaned.len() as u64;
        let reconciled_ids = orphaned.clone();
        let now = Utc::now();
        for session_id in orphaned {
            let error_text = "Error: previous run was interrupted (server restarted before the assistant finished). Send a new message to continue.".to_string();

            let _ = workspace_messages::ActiveModel {
                session_id: Set(session_id),
                role: Set("system".to_string()),
                content: Set(error_text.clone()),
                metadata: Set(None),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await;

            let _ = workspace_messages::ActiveModel {
                session_id: Set(session_id),
                role: Set("assistant".to_string()),
                content: Set(error_text),
                metadata: Set(Some(serde_json::json!({
                    "error": true,
                    "error_kind": "interrupted",
                }))),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await;
        }

        if count > 0 {
            tracing::info!("Reconciled {} orphaned workspace runs on startup", count);
        }
        Ok(reconciled_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn mock_session(id: i32, project_id: i32, status: &str) -> workspace_sessions::Model {
        let now = Utc::now();
        workspace_sessions::Model {
            id,
            public_id: format!("wss_{:016x}", id as u64),
            project_id,
            user_id: 1,
            title: None,
            status: status.to_string(),
            sandbox_container_id: None,
            work_dir: None,
            branch_name: None,
            base_branch_name: None,
            ai_provider: "claude_cli".to_string(),
            ai_model: None,
            tokens_input: 0,
            tokens_output: 0,
            estimated_cost_cents: 0,
            files_changed: 0,
            metadata: None,
            preview_password_hash: None,
            preview_password_hint: None,
            idle_timeout_minutes: None,
            cpu_milli: None,
            memory_limit_mb: None,
            pids_limit: None,
            mcp_servers_config: None,
            skills_config: None,
            last_activity_at: now,
            started_at: now,
            closed_at: None,
            created_at: now,
        }
    }

    fn mock_message(
        id: i64,
        session_id: i32,
        role: &str,
        content: &str,
    ) -> workspace_messages::Model {
        workspace_messages::Model {
            id,
            session_id,
            role: role.to_string(),
            content: content.to_string(),
            metadata: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_create_session_success() {
        let session = mock_session(1, 10, "active");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session.clone()]])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service
            .create_session(CreateSessionRequest {
                project_id: 10,
                user_id: 1,
                ai_provider: "claude_cli".to_string(),
                ai_model: None,
                branch_name: None,
                base_branch_name: None,
                metadata: None,
                skills: None,
                mcp_servers: None,
                cpu_limit: None,
                memory_limit_mb: None,
            })
            .await;

        assert!(result.is_ok());
        let s = result.unwrap();
        assert_eq!(s.session.project_id, 10);
        assert_eq!(s.session.status, "active");
    }

    #[tokio::test]
    async fn test_create_session_empty_provider_fails() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = WorkspaceService::new(Arc::new(db));

        let result = service
            .create_session(CreateSessionRequest {
                project_id: 10,
                user_id: 1,
                ai_provider: "".to_string(),
                ai_model: None,
                branch_name: None,
                base_branch_name: None,
                metadata: None,
                skills: None,
                mcp_servers: None,
                cpu_limit: None,
                memory_limit_mb: None,
            })
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WorkspaceError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_get_session_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<workspace_sessions::Model>::new()])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service.get_session(999).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WorkspaceError::SessionNotFound { session_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_get_session_with_messages() {
        let session = mock_session(1, 10, "active");
        let messages = vec![
            mock_message(1, 1, "user", "Hello"),
            mock_message(2, 1, "assistant", "Hi there"),
        ];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session.clone()]])
            .append_query_results(vec![messages.clone()])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service.get_session_with_messages(1).await;

        assert!(result.is_ok());
        let swm = result.unwrap();
        assert_eq!(swm.session.id, 1);
        assert_eq!(swm.messages.len(), 2);
        assert_eq!(swm.messages[0].role, "user");
        assert_eq!(swm.messages[1].role, "assistant");
    }

    #[tokio::test]
    async fn test_close_session_success() {
        let session = mock_session(1, 10, "active");
        let mut closed_session = session.clone();
        closed_session.status = "closed".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get_session (inside close_session)
            .append_query_results(vec![vec![session.clone()]])
            // get_session (inside update_session)
            .append_query_results(vec![vec![session.clone()]])
            // update result
            .append_query_results(vec![vec![closed_session.clone()]])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service.close_session(1).await;

        assert!(result.is_ok());
        let s = result.unwrap();
        assert_eq!(s.status, "closed");
    }

    #[tokio::test]
    async fn test_close_already_closed_session_fails() {
        let session = mock_session(1, 10, "closed");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service.close_session(1).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WorkspaceError::SessionNotActive { session_id: 1, .. }
        ));
    }

    #[tokio::test]
    async fn test_append_message_to_active_session() {
        let session = mock_session(1, 10, "active");
        let message = mock_message(1, 1, "user", "Hello");
        // For the update_session call inside append_message
        let session_for_update = mock_session(1, 10, "active");
        let updated_session = mock_session(1, 10, "active");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get_session in append_message
            .append_query_results(vec![vec![session]])
            // insert message
            .append_query_results(vec![vec![message.clone()]])
            // get_session inside update_session
            .append_query_results(vec![vec![session_for_update]])
            // update session
            .append_query_results(vec![vec![updated_session]])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service
            .append_message(SendMessageRequest {
                session_id: 1,
                role: "user".to_string(),
                content: "Hello".to_string(),
                metadata: None,
            })
            .await;

        assert!(result.is_ok());
        let m = result.unwrap();
        assert_eq!(m.role, "user");
        assert_eq!(m.content, "Hello");
    }

    #[tokio::test]
    async fn test_append_message_to_closed_session_fails() {
        let session = mock_session(1, 10, "closed");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service
            .append_message(SendMessageRequest {
                session_id: 1,
                role: "user".to_string(),
                content: "Hello".to_string(),
                metadata: None,
            })
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WorkspaceError::SessionNotActive { session_id: 1, .. }
        ));
    }

    #[tokio::test]
    async fn test_get_messages_after() {
        let messages = vec![
            mock_message(5, 1, "assistant", "Response 1"),
            mock_message(6, 1, "user", "Follow-up"),
        ];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![messages.clone()])
            .into_connection();

        let service = WorkspaceService::new(Arc::new(db));
        let result = service.get_messages_after(1, 4).await;

        assert!(result.is_ok());
        let msgs = result.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, 5);
    }

    // Note: count_active_sessions and list_sessions pagination tests require
    // a real database connection because MockDatabase doesn't support count/paginate
    // queries well. These are covered by integration tests.

    // recover_stale_sessions now uses a per-row interval query with
    // RETURNING id, which MockDatabase can't model exactly. Covered by
    // integration tests against a real Postgres.
}
