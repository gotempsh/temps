//! Service for AI pending-action lifecycle management (propose → confirm/reject).
//!
//! The AI NEVER executes a mutation. When the model proposes a write, this
//! service inserts a `proposed` row in `ai_pending_actions`. A human later
//! calls the confirm endpoint, which is the ONLY place execution runs. The
//! confirming user's `AuthContext` is used — never anything the model supplied.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};

use temps_ai_api_tools::{ApiCallScope, WriteApiToolsHandle};
use temps_auth::context::AuthContext;
use temps_auth::permissions::Permission;
use temps_entities::ai_pending_actions;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from the pending-action service.
#[derive(Debug, thiserror::Error)]
pub enum PendingActionError {
    #[error("pending action '{public_id}' not found")]
    NotFound { public_id: String },

    #[error("pending action '{public_id}' has status '{status}', expected 'proposed'")]
    InvalidState { public_id: String, status: String },

    #[error(
        "step {step_index} of this plan cannot run yet — {pending} earlier step(s) \
         are not completed; confirm them in order first"
    )]
    StepBlocked { step_index: i32, pending: usize },

    #[error("permission '{permission}' is required to confirm this action")]
    PermissionDenied { permission: String },

    #[error("AI write actions are disabled for project {project_id}")]
    Disabled { project_id: i32 },

    #[error("write action feature is not available (write caller not yet wired)")]
    Unavailable,

    #[error("execution of '{operation_id}' failed: {reason}")]
    Execution {
        operation_id: String,
        reason: String,
    },

    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Manages the lifecycle of AI-proposed write actions (propose → confirm/reject).
pub struct PendingActionService {
    db: Arc<DatabaseConnection>,
    write_handle: Arc<WriteApiToolsHandle>,
}

impl PendingActionService {
    pub fn new(db: Arc<DatabaseConnection>, write_handle: Arc<WriteApiToolsHandle>) -> Self {
        Self { db, write_handle }
    }

    /// Insert a new `proposed` pending action row.
    ///
    /// `public_id` is generated the same way as [`ai_conversations`]: a UUID
    /// v4 formatted as a 32-hex-char simple (no-hyphen) string.
    pub async fn create(
        &self,
        conversation_id: i64,
        project_id: i32,
        prepared: &temps_ai_api_tools::PreparedWrite,
        required_permission: Option<String>,
        created_by: Option<i32>,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        let public_id = uuid::Uuid::new_v4().simple().to_string();
        let now = Utc::now();
        let model = ai_pending_actions::ActiveModel {
            public_id: Set(public_id),
            conversation_id: Set(conversation_id),
            message_id: Set(None),
            project_id: Set(project_id),
            operation_id: Set(prepared.operation_id.clone()),
            method: Set(prepared.method.clone()),
            summary: Set(prepared.summary.clone()),
            params: Set(prepared.params.clone()),
            required_permission: Set(required_permission),
            status: Set("proposed".to_string()),
            result: Set(None),
            error: Set(None),
            created_by: Set(created_by),
            confirmed_by: Set(None),
            created_at: Set(now),
            confirmed_at: Set(None),
            executed_at: Set(None),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(model)
    }

    /// Insert a multi-step *plan*: an ordered chain of proposed mutations that
    /// share a `plan_public_id` and are confirmed one at a time in `step_index`
    /// order. `steps` is `(prepared, required_permission)` per step, already in
    /// execution order. Returns the created rows (in order).
    ///
    /// A single-element `steps` still produces a grouped plan — callers that want
    /// a standalone action use [`create`] instead (`plan_public_id = NULL`).
    pub async fn create_plan(
        &self,
        conversation_id: i64,
        project_id: i32,
        steps: &[(temps_ai_api_tools::PreparedWrite, Option<String>)],
        created_by: Option<i32>,
    ) -> Result<Vec<ai_pending_actions::Model>, PendingActionError> {
        let plan_public_id = uuid::Uuid::new_v4().simple().to_string();
        let now = Utc::now();
        let mut created = Vec::with_capacity(steps.len());
        for (idx, (prepared, required_permission)) in steps.iter().enumerate() {
            let public_id = uuid::Uuid::new_v4().simple().to_string();
            let model = ai_pending_actions::ActiveModel {
                public_id: Set(public_id),
                conversation_id: Set(conversation_id),
                message_id: Set(None),
                project_id: Set(project_id),
                plan_public_id: Set(Some(plan_public_id.clone())),
                step_index: Set(idx as i32),
                operation_id: Set(prepared.operation_id.clone()),
                method: Set(prepared.method.clone()),
                summary: Set(prepared.summary.clone()),
                params: Set(prepared.params.clone()),
                required_permission: Set(required_permission.clone()),
                status: Set("proposed".to_string()),
                result: Set(None),
                error: Set(None),
                created_by: Set(created_by),
                confirmed_by: Set(None),
                created_at: Set(now),
                confirmed_at: Set(None),
                executed_at: Set(None),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await?;
            created.push(model);
        }
        Ok(created)
    }

    /// Best-effort: set `message_id` on a batch of pending-action rows.
    ///
    /// This links actions created during a turn to the persisted assistant
    /// message. Failure is logged but never propagated — the turn is already
    /// committed.
    pub async fn link_message(
        &self,
        action_ids: &[i64],
        message_id: i64,
    ) -> Result<(), PendingActionError> {
        if action_ids.is_empty() {
            return Ok(());
        }
        ai_pending_actions::Entity::update_many()
            .col_expr(
                ai_pending_actions::Column::MessageId,
                sea_orm::sea_query::Expr::value(message_id),
            )
            .filter(ai_pending_actions::Column::Id.is_in(action_ids.to_vec()))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Get a single pending action by project-scoped `public_id`.
    ///
    /// 404 when not found OR when the `project_id` does not match (scoping guard).
    pub async fn get(
        &self,
        project_id: i32,
        public_id: &str,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::PublicId.eq(public_id))
            .filter(ai_pending_actions::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| PendingActionError::NotFound {
                public_id: public_id.to_string(),
            })
    }

    /// All pending actions for a conversation, most-recently-created first.
    ///
    /// `project_id` is required as an additional scope guard: the conversation id
    /// alone is not sufficient because a caller could supply a conversation id from
    /// a different project if they constructed the request manually.
    pub async fn list_for_conversation(
        &self,
        project_id: i32,
        conversation_id: i64,
    ) -> Result<Vec<ai_pending_actions::Model>, PendingActionError> {
        Ok(ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::ProjectId.eq(project_id))
            .filter(ai_pending_actions::Column::ConversationId.eq(conversation_id))
            .order_by_desc(ai_pending_actions::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?)
    }

    /// Confirm a proposed action: validate, claim atomically, execute, persist
    /// outcome (success or failure). Returns the updated model in all cases —
    /// a failed execution is surfaced as `status = "failed"` on the model, not
    /// as an Err, so the caller can surface status to the UI.
    pub async fn confirm(
        &self,
        project_id: i32,
        public_id: &str,
        auth: &AuthContext,
        confirmed_by: Option<i32>,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        // 1. Load + project-scope check.
        let action = self.get(project_id, public_id).await?;

        // 2. Defense-in-depth: re-check the write-actions toggle at confirm time
        //    (the toggle may have been turned off after the action was proposed).
        self.check_write_actions_enabled(project_id).await?;

        // 3. Status pre-check.
        if action.status != "proposed" {
            return Err(PendingActionError::InvalidState {
                public_id: public_id.to_string(),
                status: action.status.clone(),
            });
        }

        // 3b. Plan ordering: a chained step can only run once every earlier step
        //     has executed. Blocks before the atomic claim so a premature confirm
        //     never leaves a stuck "executing" row.
        self.ensure_plan_step_ready(&action).await?;

        // 4. Advisory permission check using the caller's auth.
        //    This MUST run before the atomic claim so a denied user never leaves
        //    a stuck "executing" row.
        if let Some(ref perm_str) = action.required_permission {
            if let Some(perm) = Permission::from_str(perm_str) {
                if !auth.has_permission(&perm) {
                    return Err(PendingActionError::PermissionDenied {
                        permission: perm_str.clone(),
                    });
                }
            }
            // Unknown permission string → pass (router's permission_guard! is the
            // real boundary at execute time).
        }

        // 5. Atomic claim: flip to "executing" only if still "proposed".
        let now = Utc::now();
        let rows_affected = ai_pending_actions::Entity::update_many()
            .col_expr(
                ai_pending_actions::Column::Status,
                sea_orm::sea_query::Expr::value("executing"),
            )
            .filter(ai_pending_actions::Column::Id.eq(action.id))
            .filter(ai_pending_actions::Column::Status.eq("proposed"))
            .exec(self.db.as_ref())
            .await?
            .rows_affected;

        if rows_affected == 0 {
            // Lost a race or already handled — treat as invalid state.
            return Err(PendingActionError::InvalidState {
                public_id: public_id.to_string(),
                status: "already handled (concurrent confirm/reject)".to_string(),
            });
        }

        // 6. Get write caller.
        let caller = match self.write_handle.get() {
            Some(c) => c,
            None => {
                // Mark failed — we already claimed the row.
                let _ = self
                    .set_failed(&action, "Write caller not available (startup incomplete).")
                    .await;
                return Err(PendingActionError::Unavailable);
            }
        };

        // 7. Execute with the CONFIRMING user's auth, scoped to the action's project.
        let scope = ApiCallScope {
            auth: auth.clone(),
            project_ids: vec![project_id],
        };
        let exec_result = caller
            .execute_write(&action.operation_id, action.params.clone(), &scope)
            .await;

        // 8. Persist outcome regardless of success/failure.
        let updated = match exec_result {
            Ok(resp) => {
                ai_pending_actions::ActiveModel {
                    id: Set(action.id),
                    status: Set("executed".to_string()),
                    result: Set(Some(resp.body)),
                    confirmed_by: Set(confirmed_by),
                    confirmed_at: Set(Some(now)),
                    executed_at: Set(Some(Utc::now())),
                    ..Default::default()
                }
                .update(self.db.as_ref())
                .await?
            }
            Err(e) => {
                let failed = ai_pending_actions::ActiveModel {
                    id: Set(action.id),
                    status: Set("failed".to_string()),
                    error: Set(Some(e.to_string())),
                    confirmed_by: Set(confirmed_by),
                    confirmed_at: Set(Some(now)),
                    ..Default::default()
                }
                .update(self.db.as_ref())
                .await?;
                // Stop-and-report: a failed step halts the plan — the later steps
                // must not run against a failed prerequisite.
                self.skip_remaining_steps(&action).await;
                failed
            }
        };

        // Audit is emitted by the handler with full RequestMetadata (ip/user_agent).
        Ok(updated)
    }

    /// Reject a proposed action (no execution — simply mark rejected).
    ///
    /// Uses the same atomic claim pattern as `confirm` to prevent a
    /// confirmed/executed action from being flipped to "rejected" via a race.
    pub async fn reject(
        &self,
        project_id: i32,
        public_id: &str,
        _auth: &AuthContext,
        rejected_by: Option<i32>,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        // Load first so we have the id and can return the updated row.
        let action = self.get(project_id, public_id).await?;

        // Defense-in-depth: re-check the write-actions toggle (same as confirm).
        self.check_write_actions_enabled(project_id).await?;

        let now = Utc::now();

        // Atomic claim: flip to "rejected" only if still "proposed".
        // This prevents a race where a confirmed/executed row gets flipped.
        let rows_affected = ai_pending_actions::Entity::update_many()
            .col_expr(
                ai_pending_actions::Column::Status,
                sea_orm::sea_query::Expr::value("rejected"),
            )
            .col_expr(
                ai_pending_actions::Column::ConfirmedBy,
                sea_orm::sea_query::Expr::value(rejected_by),
            )
            .col_expr(
                ai_pending_actions::Column::ConfirmedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(ai_pending_actions::Column::Id.eq(action.id))
            .filter(ai_pending_actions::Column::Status.eq("proposed"))
            .exec(self.db.as_ref())
            .await?
            .rows_affected;

        if rows_affected == 0 {
            // Lost a race or already handled — treat as invalid state.
            return Err(PendingActionError::InvalidState {
                public_id: public_id.to_string(),
                status: "already handled (concurrent confirm/reject)".to_string(),
            });
        }

        // Reload the row to return its current state.
        let updated = self.get(project_id, public_id).await?;

        // Stop-and-report: rejecting a step halts the rest of the plan.
        self.skip_remaining_steps(&updated).await;

        // Audit is emitted by the handler with full RequestMetadata (ip/user_agent).
        Ok(updated)
    }

    /// Check that `ai_write_actions_enabled` is true for `project_id`.
    ///
    /// Returns `PendingActionError::Disabled` when the toggle is off or the
    /// project cannot be loaded. Called at the start of both `confirm` and
    /// `reject` so the toggle is enforced even after a row was proposed while
    /// the feature was on.
    async fn check_write_actions_enabled(&self, project_id: i32) -> Result<(), PendingActionError> {
        let project = temps_entities::projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?;
        match project {
            Some(p) if p.ai_write_actions_enabled => Ok(()),
            _ => Err(PendingActionError::Disabled { project_id }),
        }
    }

    /// Enforce step ordering for a plan: a step can only run once every earlier
    /// step (lower `step_index`, same `plan_public_id`) has `executed`. Standalone
    /// actions (`plan_public_id = None`) and the first step are always ready.
    ///
    /// Returns [`PendingActionError::StepBlocked`] listing how many earlier steps
    /// are still incomplete, so the UI/caller can explain why nothing ran.
    async fn ensure_plan_step_ready(
        &self,
        action: &ai_pending_actions::Model,
    ) -> Result<(), PendingActionError> {
        let (Some(plan_id), true) = (action.plan_public_id.as_ref(), action.step_index > 0) else {
            return Ok(());
        };
        let earlier = ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::PlanPublicId.eq(plan_id))
            .filter(ai_pending_actions::Column::StepIndex.lt(action.step_index))
            .all(self.db.as_ref())
            .await?;
        let pending = earlier.iter().filter(|s| s.status != "executed").count();
        if pending > 0 {
            return Err(PendingActionError::StepBlocked {
                step_index: action.step_index,
                pending,
            });
        }
        Ok(())
    }

    /// Halt a plan after a step failed or was rejected: mark every later step
    /// (`step_index` greater than `after`, same plan, still `proposed`) as
    /// `skipped` so the UI stops offering them and the chain doesn't proceed.
    /// No-op for standalone actions. Best-effort — errors are swallowed since the
    /// triggering step's outcome is already persisted.
    async fn skip_remaining_steps(&self, action: &ai_pending_actions::Model) {
        let Some(plan_id) = action.plan_public_id.as_ref() else {
            return;
        };
        let res = ai_pending_actions::Entity::update_many()
            .col_expr(
                ai_pending_actions::Column::Status,
                sea_orm::sea_query::Expr::value("skipped"),
            )
            .filter(ai_pending_actions::Column::PlanPublicId.eq(plan_id))
            .filter(ai_pending_actions::Column::StepIndex.gt(action.step_index))
            .filter(ai_pending_actions::Column::Status.eq("proposed"))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = res {
            tracing::warn!("Failed to skip remaining steps of plan {}: {}", plan_id, e);
        }
    }

    /// Helper: mark a row as failed (best-effort, used in the confirm error path).
    async fn set_failed(
        &self,
        action: &ai_pending_actions::Model,
        reason: &str,
    ) -> Result<(), PendingActionError> {
        ai_pending_actions::ActiveModel {
            id: Set(action.id),
            status: Set("failed".to_string()),
            error: Set(Some(reason.to_string())),
            confirmed_at: Set(Some(Utc::now())),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_ai_api_tools::PreparedWrite;

    fn make_prepared() -> PreparedWrite {
        PreparedWrite {
            operation_id: "redeploy_deployment".to_string(),
            method: "POST".to_string(),
            path: "/projects/{project_id}/deployments/redeploy".to_string(),
            summary: "POST /projects/{project_id}/deployments/redeploy — redeploy_deployment"
                .to_string(),
            params: serde_json::json!({"deployment_id": 42}),
            required_permission: Some("deployments:create".to_string()),
        }
    }

    fn make_proposed(id: i64, public_id: &str, project_id: i32) -> ai_pending_actions::Model {
        let now = Utc::now();
        ai_pending_actions::Model {
            id,
            public_id: public_id.to_string(),
            conversation_id: 1,
            message_id: None,
            project_id,
            plan_public_id: None,
            step_index: 0,
            operation_id: "redeploy_deployment".to_string(),
            method: "POST".to_string(),
            summary: "POST ... — redeploy_deployment".to_string(),
            params: serde_json::json!({}),
            required_permission: None,
            status: "proposed".to_string(),
            result: None,
            error: None,
            created_by: Some(1),
            confirmed_by: None,
            created_at: now,
            confirmed_at: None,
            executed_at: None,
        }
    }

    fn make_project(id: i32, write_enabled: bool) -> temps_entities::projects::Model {
        let now = Utc::now();
        temps_entities::projects::Model {
            id,
            name: "test-project".to_string(),
            repo_name: "repo".to_string(),
            repo_owner: "owner".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Static,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "test-project".to_string(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: Some(true),
            ai_write_actions_enabled: write_enabled,
            error_source_context_enabled: false,
            error_source_root: None,
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

    fn make_auth() -> AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "t".to_string(),
            email: "t@t.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        AuthContext::new_session(user, temps_auth::permissions::Role::Admin)
    }

    fn noop_write_handle() -> Arc<WriteApiToolsHandle> {
        Arc::new(WriteApiToolsHandle::new())
    }

    fn make_svc(db: sea_orm::DatabaseConnection) -> PendingActionService {
        PendingActionService::new(Arc::new(db), noop_write_handle())
    }

    // create inserts a proposed row.
    #[tokio::test]
    async fn test_create_inserts_proposed_row() {
        let row = make_proposed(1, "abc", 7);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row.clone()]])
            .into_connection();
        let svc = make_svc(db);
        let prepared = make_prepared();
        let result = svc.create(1, 7, &prepared, None, Some(1)).await;
        assert!(result.is_ok(), "create should succeed: {:?}", result.err());
        let model = result.unwrap();
        assert_eq!(model.status, "proposed");
        assert_eq!(model.project_id, 7);
    }

    // get: not-found returns NotFound.
    #[tokio::test]
    async fn test_get_not_found_returns_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_pending_actions::Model>::new()])
            .into_connection();
        let svc = make_svc(db);
        let err = svc
            .get(7, "nonexistent")
            .await
            .expect_err("should be not found");
        assert!(
            matches!(err, PendingActionError::NotFound { ref public_id } if public_id == "nonexistent"),
            "unexpected error: {err:?}"
        );
    }

    // confirm: write-actions toggle is off → Disabled before anything else.
    #[tokio::test]
    async fn test_confirm_disabled_returns_disabled_error() {
        let action = make_proposed(1, "abc", 7);
        let project = make_project(7, false); // toggle OFF
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action
            .append_query_results(vec![vec![action]])
            // check_write_actions_enabled → project query
            .append_query_results(vec![vec![project]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .confirm(7, "abc", &auth, Some(1))
            .await
            .expect_err("should fail with Disabled");
        assert!(
            matches!(err, PendingActionError::Disabled { project_id: 7 }),
            "unexpected: {err:?}"
        );
    }

    // reject: write-actions toggle is off → Disabled.
    #[tokio::test]
    async fn test_reject_disabled_returns_disabled_error() {
        let action = make_proposed(1, "abc", 7);
        let project = make_project(7, false); // toggle OFF
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action
            .append_query_results(vec![vec![action]])
            // check_write_actions_enabled → project query
            .append_query_results(vec![vec![project]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .reject(7, "abc", &auth, Some(1))
            .await
            .expect_err("should fail with Disabled");
        assert!(
            matches!(err, PendingActionError::Disabled { project_id: 7 }),
            "unexpected: {err:?}"
        );
    }

    // confirm on non-proposed → InvalidState (after the enabled check).
    #[tokio::test]
    async fn test_confirm_non_proposed_returns_invalid_state() {
        let mut row = make_proposed(1, "abc", 7);
        row.status = "executed".to_string();
        let project = make_project(7, true); // toggle ON
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action (executed status)
            .append_query_results(vec![vec![row]])
            // check_write_actions_enabled → project query
            .append_query_results(vec![vec![project]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .confirm(7, "abc", &auth, Some(1))
            .await
            .expect_err("should fail");
        assert!(
            matches!(err, PendingActionError::InvalidState { .. }),
            "unexpected: {err:?}"
        );
    }

    // confirm: a plan step whose earlier step hasn't executed → StepBlocked
    // (before any atomic claim). Query order: get(action) → enabled(project) →
    // status-proposed ok → ensure_plan_step_ready loads earlier steps.
    #[tokio::test]
    async fn test_confirm_plan_step_blocked_when_prior_step_incomplete() {
        // step 2 of a plan (step_index = 1)
        let mut step2 = make_proposed(2, "step2", 7);
        step2.plan_public_id = Some("plan-abc".to_string());
        step2.step_index = 1;
        // its predecessor, still "proposed" (not executed)
        let mut step1 = make_proposed(1, "step1", 7);
        step1.plan_public_id = Some("plan-abc".to_string());
        step1.step_index = 0;

        let project = make_project(7, true); // toggle ON
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![step2]]) // get()
            .append_query_results(vec![vec![project]]) // check enabled
            .append_query_results(vec![vec![step1]]) // ensure_plan_step_ready: earlier steps
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .confirm(7, "step2", &auth, Some(1))
            .await
            .expect_err("step 2 must be blocked until step 1 executes");
        assert!(
            matches!(
                err,
                PendingActionError::StepBlocked {
                    step_index: 1,
                    pending: 1
                }
            ),
            "unexpected: {err:?}"
        );
    }

    // A standalone action (no plan) is never blocked by the ordering guard.
    #[tokio::test]
    async fn test_ensure_plan_step_ready_ignores_standalone() {
        let standalone = make_proposed(1, "solo", 7); // plan_public_id None, step_index 0
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = make_svc(db);
        // No earlier-steps query is issued for a standalone action.
        assert!(svc.ensure_plan_step_ready(&standalone).await.is_ok());
    }

    // reject: proposed → rejected (atomic path: exec_result 1 row, then get).
    #[tokio::test]
    async fn test_reject_transitions_to_rejected() {
        let proposed = make_proposed(1, "abc", 7);
        let rejected = {
            let mut m = proposed.clone();
            m.status = "rejected".to_string();
            m
        };
        let project = make_project(7, true); // toggle ON
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action
            .append_query_results(vec![vec![proposed]])
            // check_write_actions_enabled → project query
            .append_query_results(vec![vec![project]])
            // atomic update_many (exec result)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // reload get() after successful claim
            .append_query_results(vec![vec![rejected]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let result = svc.reject(7, "abc", &auth, Some(1)).await;
        assert!(result.is_ok(), "reject should succeed: {:?}", result.err());
        let m = result.unwrap();
        assert_eq!(m.status, "rejected");
    }

    // reject on non-proposed → atomic update returns 0 rows → InvalidState.
    #[tokio::test]
    async fn test_reject_non_proposed_returns_invalid_state() {
        let mut row = make_proposed(1, "abc", 7);
        row.status = "rejected".to_string();
        let project = make_project(7, true); // toggle ON
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action (already rejected)
            .append_query_results(vec![vec![row]])
            // check_write_actions_enabled → project query
            .append_query_results(vec![vec![project]])
            // atomic update_many returns 0 rows (nothing to flip)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .reject(7, "abc", &auth, Some(1))
            .await
            .expect_err("should fail");
        assert!(
            matches!(err, PendingActionError::InvalidState { .. }),
            "unexpected: {err:?}"
        );
    }

    // list_for_conversation: returns rows ordered by created_at DESC (project-scoped).
    #[tokio::test]
    async fn test_list_for_conversation_returns_rows() {
        let r1 = make_proposed(1, "p1", 7);
        let r2 = make_proposed(2, "p2", 7);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![r1, r2]])
            .into_connection();
        let svc = make_svc(db);
        let rows = svc
            .list_for_conversation(7, 1)
            .await
            .expect("should succeed");
        assert_eq!(rows.len(), 2);
    }

    // link_message: no-op on empty slice.
    #[tokio::test]
    async fn test_link_message_empty_slice_is_noop() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = make_svc(db);
        let result = svc.link_message(&[], 42).await;
        assert!(result.is_ok());
    }

    // link_message: updates rows.
    #[tokio::test]
    async fn test_link_message_updates_rows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .into_connection();
        let svc = make_svc(db);
        let result = svc.link_message(&[1, 2], 99).await;
        assert!(
            result.is_ok(),
            "link_message should succeed: {:?}",
            result.err()
        );
    }

    // confirm when write handle is empty → Unavailable (after claiming "executing").
    // Sequence: get action → check_write_actions_enabled (project query) → status
    // pre-check passes (proposed) → permission check passes (None) → atomic claim
    // (1 row affected) → handle.get() returns None → set_failed → Unavailable.
    #[tokio::test]
    async fn test_confirm_unavailable_write_handle() {
        let proposed = make_proposed(1, "abc", 7);
        let failed_model = {
            let mut m = proposed.clone();
            m.status = "failed".to_string();
            m
        };
        let project = make_project(7, true); // toggle ON
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action
            .append_query_results(vec![vec![proposed]])
            // check_write_actions_enabled → project query
            .append_query_results(vec![vec![project]])
            // atomic claim update_many
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // set_failed update
            .append_query_results(vec![vec![failed_model]])
            .into_connection();
        // Empty write handle (not wired)
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .confirm(7, "abc", &auth, Some(1))
            .await
            .expect_err("should fail with Unavailable");
        assert!(
            matches!(err, PendingActionError::Unavailable),
            "unexpected: {err:?}"
        );
    }
}
