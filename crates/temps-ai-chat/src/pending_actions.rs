// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable records for approval-gated AI platform actions.
//!
//! The model can prepare a mutation but cannot authorize it. The chat runtime
//! stores an encrypted `awaiting_inline_approval` row, pauses the tool call on an inline human
//! approval, then calls this service with the resolving user's current
//! `AuthContext`. The actual success, rejection, or sanitized failure is
//! returned to the same model turn. Legacy confirm/reject endpoints remain for
//! actions created before the inline lifecycle was introduced.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};

use temps_ai_api_tools::{ApiCallScope, ProjectSelectorScope, WriteApiToolsHandle};
use temps_auth::context::AuthContext;
use temps_auth::permissions::Permission;
use temps_core::ai_tool_call::AiToolOrigin;
use temps_entities::ai_pending_actions;

use crate::sensitive::{decrypt_value, encrypt_value, redact_text, redact_value};

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

    /// A legacy (detached) confirm/reject was aimed at an action that is
    /// waiting on a live inline approval. Resolving it here would race the
    /// chat turn that is paused on it, so the only valid path is the
    /// approval prompt inside the conversation.
    #[error(
        "pending action '{public_id}' is waiting for inline approval in the chat conversation. \
         Approve or reject it there; this endpoint only resolves actions proposed before inline \
         approvals existed"
    )]
    InlineApprovalRequired { public_id: String },

    #[error(
        "step {step_index} of this plan cannot run yet — {pending} earlier step(s) \
         are not completed; confirm them in order first"
    )]
    StepBlocked { step_index: i32, pending: usize },

    #[error("permission '{permission}' is required to {decision} pending action '{public_id}'")]
    PermissionDenied {
        public_id: String,
        permission: String,
        decision: &'static str,
    },

    #[error("write action feature is not available (write caller not yet wired)")]
    Unavailable,

    #[error("execution of '{operation_id}' failed: {reason}")]
    Execution {
        operation_id: String,
        reason: String,
    },

    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("failed to {operation} protected pending-action parameters: {reason}")]
    Encryption {
        operation: &'static str,
        reason: String,
    },
}

/// Status of an action paused on a live inline approval in a chat turn.
const INLINE_APPROVAL_STATUS: &str = "awaiting_inline_approval";

/// Check that `action` is in the lifecycle state the caller resolves, naming
/// the actual reason when it is not.
///
/// The legacy detached endpoints resolve `proposed` rows, the inline flow
/// resolves `awaiting_inline_approval` rows. Aiming a detached endpoint at an
/// inline row used to surface as "already handled (concurrent confirm/reject)",
/// which sends the user hunting for a race that never happened.
fn ensure_expected_status(
    action: &ai_pending_actions::Model,
    public_id: &str,
    expected_status: &str,
) -> Result<(), PendingActionError> {
    if action.status == expected_status {
        return Ok(());
    }
    if action.status == INLINE_APPROVAL_STATUS && expected_status != INLINE_APPROVAL_STATUS {
        return Err(PendingActionError::InlineApprovalRequired {
            public_id: public_id.to_string(),
        });
    }
    Err(PendingActionError::InvalidState {
        public_id: public_id.to_string(),
        status: action.status.clone(),
    })
}

/// Whether a reject must re-check the caller's authority over the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RejectAuthority {
    Check,
    AlreadyChecked,
}

/// Rejecting a proposal decides its fate just as confirming does, so it takes
/// the same authority: the permission the operation itself requires. Without
/// this, a credential that could never approve an action, such as a read-only
/// API key, could still discard one its owner meant to approve.
///
/// Unlike confirm, a reject executes nothing, so there is no router guard
/// behind it to fall back on. An unknown permission therefore fails closed, as
/// the inline approval prompt does, and an action that declares no permission
/// requires `projects:write`, the gate the project-scoped reject route applies.
fn ensure_reject_authority(
    action: &ai_pending_actions::Model,
    auth: &AuthContext,
) -> Result<(), PendingActionError> {
    let (required, name) = match action.required_permission.as_deref() {
        Some(name) => (Permission::from_str(name), name.to_string()),
        None => (
            Some(Permission::ProjectsWrite),
            Permission::ProjectsWrite.to_string(),
        ),
    };
    if required.is_some_and(|permission| auth.has_permission(&permission)) {
        return Ok(());
    }
    Err(PendingActionError::PermissionDenied {
        public_id: action.public_id.clone(),
        permission: name,
        decision: "reject",
    })
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Manages both legacy detached proposals and live inline write approvals.
#[derive(Clone)]
pub struct PendingActionService {
    db: Arc<DatabaseConnection>,
    write_handle: Arc<WriteApiToolsHandle>,
    encryption: Arc<temps_core::EncryptionService>,
    /// Where `secret_ref:` handles in an approved action's params are resolved
    /// from. `None` means the action runs without secrets, and a handle in its
    /// params is refused rather than sent as text.
    secret_slots: Option<crate::secret_slots::SecretSlotService>,
}

impl PendingActionService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        write_handle: Arc<WriteApiToolsHandle>,
        encryption: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            write_handle,
            encryption,
            secret_slots: None,
        }
    }

    /// Let approved actions use the secrets the user provided in this chat.
    pub fn with_secret_slots(mut self, slots: crate::secret_slots::SecretSlotService) -> Self {
        self.secret_slots = Some(slots);
        self
    }

    /// Insert a new `proposed` pending action row.
    ///
    /// `public_id` is generated the same way as [`ai_conversations`]: a UUID
    /// v4 formatted as a 32-hex-char simple (no-hyphen) string.
    pub async fn create(
        &self,
        conversation_id: i64,
        project_id: Option<i32>,
        prepared: &temps_ai_api_tools::PreparedWrite,
        required_permission: Option<String>,
        created_by: i32,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        self.create_with_status(
            conversation_id,
            project_id,
            prepared,
            required_permission,
            created_by,
            "proposed",
        )
        .await
    }

    /// Store an action whose only valid resolution path is the live permission
    /// waiter that created it. Legacy pending-action HTTP endpoints deliberately
    /// accept only `proposed`, so an abandoned inline action cannot be executed
    /// later through a detached confirmation surface.
    pub async fn create_inline(
        &self,
        conversation_id: i64,
        project_id: Option<i32>,
        prepared: &temps_ai_api_tools::PreparedWrite,
        required_permission: Option<String>,
        created_by: i32,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        self.create_with_status(
            conversation_id,
            project_id,
            prepared,
            required_permission,
            created_by,
            "awaiting_inline_approval",
        )
        .await
    }

    async fn create_with_status(
        &self,
        conversation_id: i64,
        project_id: Option<i32>,
        prepared: &temps_ai_api_tools::PreparedWrite,
        required_permission: Option<String>,
        created_by: i32,
        status: &str,
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
            params: Set(
                encrypt_value(self.encryption.as_ref(), &prepared.params).map_err(|reason| {
                    PendingActionError::Encryption {
                        operation: "encrypt",
                        reason,
                    }
                })?,
            ),
            required_permission: Set(required_permission),
            status: Set(status.to_string()),
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
        project_id: Option<i32>,
        steps: &[(temps_ai_api_tools::PreparedWrite, Option<String>)],
        created_by: i32,
    ) -> Result<Vec<ai_pending_actions::Model>, PendingActionError> {
        self.create_plan_with_status(conversation_id, project_id, steps, created_by, "proposed")
            .await
    }

    /// Transactionally store a plan that is resolvable only by its active
    /// inline permission waiter.
    pub async fn create_inline_plan(
        &self,
        conversation_id: i64,
        project_id: Option<i32>,
        steps: &[(temps_ai_api_tools::PreparedWrite, Option<String>)],
        created_by: i32,
    ) -> Result<Vec<ai_pending_actions::Model>, PendingActionError> {
        self.create_plan_with_status(
            conversation_id,
            project_id,
            steps,
            created_by,
            "awaiting_inline_approval",
        )
        .await
    }

    async fn create_plan_with_status(
        &self,
        conversation_id: i64,
        project_id: Option<i32>,
        steps: &[(temps_ai_api_tools::PreparedWrite, Option<String>)],
        created_by: i32,
        status: &str,
    ) -> Result<Vec<ai_pending_actions::Model>, PendingActionError> {
        // Encrypt before opening the transaction so a local encryption failure
        // cannot leave even a transient partial plan in the database.
        let encrypted_steps = steps
            .iter()
            .map(|(prepared, required_permission)| {
                encrypt_value(self.encryption.as_ref(), &prepared.params)
                    .map(|params| (prepared, required_permission, params))
                    .map_err(|reason| PendingActionError::Encryption {
                        operation: "encrypt",
                        reason,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let transaction = self.db.begin().await?;
        let plan_public_id = uuid::Uuid::new_v4().simple().to_string();
        let now = Utc::now();
        let mut created = Vec::with_capacity(steps.len());
        for (idx, (prepared, required_permission, encrypted_params)) in
            encrypted_steps.into_iter().enumerate()
        {
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
                params: Set(encrypted_params),
                required_permission: Set(required_permission.clone()),
                status: Set(status.to_string()),
                result: Set(None),
                error: Set(None),
                created_by: Set(created_by),
                confirmed_by: Set(None),
                created_at: Set(now),
                confirmed_at: Set(None),
                executed_at: Set(None),
                ..Default::default()
            }
            .insert(&transaction)
            .await
            .map_err(PendingActionError::Database)?;
            created.push(model);
        }
        transaction.commit().await?;
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
        project_id: Option<i32>,
        user_id: i32,
        public_id: &str,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        let query = ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::PublicId.eq(public_id))
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id));
        let query = match project_id {
            Some(project_id) => query.filter(ai_pending_actions::Column::ProjectId.eq(project_id)),
            None => query.filter(ai_pending_actions::Column::ProjectId.is_null()),
        };
        query
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| PendingActionError::NotFound {
                public_id: public_id.to_string(),
            })
    }

    /// Load an action by owner without treating its optional project context
    /// as an authorization boundary.
    pub async fn get_owned(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::PublicId.eq(public_id))
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id))
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
        project_id: Option<i32>,
        user_id: i32,
        conversation_id: i64,
    ) -> Result<Vec<ai_pending_actions::Model>, PendingActionError> {
        let query = ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id))
            .filter(ai_pending_actions::Column::ConversationId.eq(conversation_id))
            .order_by_desc(ai_pending_actions::Column::CreatedAt);
        let query = match project_id {
            Some(project_id) => query.filter(ai_pending_actions::Column::ProjectId.eq(project_id)),
            None => query.filter(ai_pending_actions::Column::ProjectId.is_null()),
        };
        Ok(query.all(self.db.as_ref()).await?)
    }

    pub async fn list_owned_for_conversation(
        &self,
        user_id: i32,
        conversation_id: i64,
    ) -> Result<Vec<ai_pending_actions::Model>, PendingActionError> {
        Ok(ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id))
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
        project_id: Option<i32>,
        public_id: &str,
        auth: &AuthContext,
        confirmed_by: Option<i32>,
        origin: Option<AiToolOrigin>,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        self.confirm_with_status(
            project_id,
            public_id,
            auth,
            confirmed_by,
            origin,
            "proposed",
        )
        .await
    }

    /// Resolve an action that belongs to the currently active inline waiter.
    pub async fn confirm_inline(
        &self,
        project_id: Option<i32>,
        public_id: &str,
        auth: &AuthContext,
        confirmed_by: Option<i32>,
        origin: Option<AiToolOrigin>,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        self.confirm_with_status(
            project_id,
            public_id,
            auth,
            confirmed_by,
            origin,
            "awaiting_inline_approval",
        )
        .await
    }

    async fn confirm_with_status(
        &self,
        project_id: Option<i32>,
        public_id: &str,
        auth: &AuthContext,
        confirmed_by: Option<i32>,
        origin: Option<AiToolOrigin>,
        expected_status: &str,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        // 1. Load + project-scope check.
        let user_id = auth.user_id();
        let action = self.get(project_id, user_id, public_id).await?;

        // 2. Status pre-check. Project-level feature toggles do not participate
        // in authorization: the native harness approval and the confirming
        // user's current RBAC/project scope are the authoritative controls.
        ensure_expected_status(&action, public_id, expected_status)?;

        // 3b. Plan ordering: a chained step can only run once every earlier step
        //     has executed. Blocks before the atomic claim so a premature confirm
        //     never leaves a stuck "executing" row.
        self.ensure_plan_step_ready(&action, user_id).await?;

        // 4. Advisory permission check using the caller's auth.
        //    This MUST run before the atomic claim so a denied user never leaves
        //    a stuck "executing" row.
        if let Some(ref perm_str) = action.required_permission {
            if let Some(perm) = Permission::from_str(perm_str) {
                if !auth.has_permission(&perm) {
                    return Err(PendingActionError::PermissionDenied {
                        public_id: public_id.to_string(),
                        permission: perm_str.clone(),
                        decision: "confirm",
                    });
                }
            }
            // Unknown permission string → pass (router's permission_guard! is the
            // real boundary at execute time).
        }

        // Decrypt before claiming the row. A corrupt payload is an operator
        // error, not an execution attempt, and must not strand the action in
        // the transient `executing` state.
        let executable_params =
            decrypt_value(self.encryption.as_ref(), &action.params).map_err(|reason| {
                PendingActionError::Encryption {
                    operation: "decrypt",
                    reason,
                }
            })?;

        // 5. Atomic claim: flip to "executing" only from the lifecycle this
        // caller owns. Detached endpoints therefore cannot claim inline rows.
        let now = Utc::now();
        let rows_affected = ai_pending_actions::Entity::update_many()
            .col_expr(
                ai_pending_actions::Column::Status,
                sea_orm::sea_query::Expr::value("executing"),
            )
            .filter(ai_pending_actions::Column::Id.eq(action.id))
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id))
            .filter(ai_pending_actions::Column::Status.eq(expected_status))
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
            project_scope: project_id.map_or(ProjectSelectorScope::Unrestricted, |project_id| {
                ProjectSelectorScope::Allowed(vec![project_id])
            }),
            origin,
            // Bound to the action's owner and conversation, so an action can
            // only spend secrets its own user gave in its own chat.
            secrets: self.secret_slots.as_ref().map(|slots| {
                Arc::new(slots.resolver(user_id, action.conversation_id))
                    as Arc<dyn temps_ai_api_tools::SecretResolver>
            }),
        };
        let exec_result = caller
            .execute_write(&action.operation_id, executable_params, &scope)
            .await;

        // 8. Persist outcome regardless of success/failure.
        let updated = match exec_result {
            Ok(resp) => {
                let executed = ai_pending_actions::ActiveModel {
                    id: Set(action.id),
                    status: Set("executed".to_string()),
                    result: Set(Some(redact_value(&resp.body))),
                    confirmed_by: Set(confirmed_by),
                    confirmed_at: Set(Some(now)),
                    executed_at: Set(Some(Utc::now())),
                    ..Default::default()
                }
                .update(self.db.as_ref())
                .await?;
                // A deployment, backup or restore it started joins the chat's
                // operations list.
                crate::chat_operations::record_executed_action(self.db.as_ref(), &executed).await;
                executed
            }
            Err(e) => {
                let failed = ai_pending_actions::ActiveModel {
                    id: Set(action.id),
                    status: Set("failed".to_string()),
                    error: Set(Some(redact_text(&e.to_string()))),
                    confirmed_by: Set(confirmed_by),
                    confirmed_at: Set(Some(now)),
                    ..Default::default()
                }
                .update(self.db.as_ref())
                .await?;
                // Stop-and-report: a failed step halts the plan — the later steps
                // must not run against a failed prerequisite.
                self.skip_remaining_steps(&action, user_id).await;
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
        project_id: Option<i32>,
        public_id: &str,
        auth: &AuthContext,
        rejected_by: Option<i32>,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        self.reject_with_status(
            project_id,
            public_id,
            auth,
            rejected_by,
            "proposed",
            RejectAuthority::Check,
        )
        .await
    }

    pub async fn reject_inline(
        &self,
        project_id: Option<i32>,
        public_id: &str,
        auth: &AuthContext,
        rejected_by: Option<i32>,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        self.reject_with_status(
            project_id,
            public_id,
            auth,
            rejected_by,
            "awaiting_inline_approval",
            // The inline approval prompt already re-checked the operation's
            // permission against this auth before handing over the decision.
            RejectAuthority::AlreadyChecked,
        )
        .await
    }

    async fn reject_with_status(
        &self,
        project_id: Option<i32>,
        public_id: &str,
        auth: &AuthContext,
        rejected_by: Option<i32>,
        expected_status: &str,
        authority: RejectAuthority,
    ) -> Result<ai_pending_actions::Model, PendingActionError> {
        // Load first so we have the id and can return the updated row.
        let user_id = auth.user_id();
        let action = self.get(project_id, user_id, public_id).await?;

        // Name the real reason before attempting the claim: an inline approval
        // or an already-resolved action is not a "concurrent confirm/reject".
        ensure_expected_status(&action, public_id, expected_status)?;

        if authority == RejectAuthority::Check {
            ensure_reject_authority(&action, auth)?;
        }

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
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id))
            .filter(ai_pending_actions::Column::Status.eq(expected_status))
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
        let updated = self.get(project_id, user_id, public_id).await?;

        // Stop-and-report: rejecting a step halts the rest of the plan.
        self.skip_remaining_steps(&updated, user_id).await;

        // Audit is emitted by the handler with full RequestMetadata (ip/user_agent).
        Ok(updated)
    }

    /// Make abandoned inline approvals permanently non-executable. This is
    /// idempotent and intentionally does not require an auth snapshot: it can
    /// only move rows owned by the active inline lifecycle into `cancelled`.
    pub async fn cancel_inline(&self, action_public_ids: &[String], reason: &str) {
        if action_public_ids.is_empty() {
            return;
        }
        let result = ai_pending_actions::Entity::update_many()
            .col_expr(
                ai_pending_actions::Column::Status,
                sea_orm::sea_query::Expr::value("cancelled"),
            )
            .col_expr(
                ai_pending_actions::Column::Error,
                sea_orm::sea_query::Expr::value(Some(redact_text(reason))),
            )
            .filter(ai_pending_actions::Column::PublicId.is_in(action_public_ids.to_vec()))
            .filter(ai_pending_actions::Column::Status.eq("awaiting_inline_approval"))
            .exec(self.db.as_ref())
            .await;
        if let Err(error) = result {
            tracing::error!(
                action_count = action_public_ids.len(),
                %error,
                "failed to cancel abandoned inline AI actions"
            );
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
        user_id: i32,
    ) -> Result<(), PendingActionError> {
        let (Some(plan_id), true) = (action.plan_public_id.as_ref(), action.step_index > 0) else {
            return Ok(());
        };
        let earlier = ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::PlanPublicId.eq(plan_id))
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id))
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
    /// (`step_index` greater than `after`, same plan, still pending) as
    /// `skipped` so the UI stops offering them and the chain doesn't proceed.
    /// No-op for standalone actions. Best-effort — errors are swallowed since the
    /// triggering step's outcome is already persisted.
    async fn skip_remaining_steps(&self, action: &ai_pending_actions::Model, user_id: i32) {
        let Some(plan_id) = action.plan_public_id.as_ref() else {
            return;
        };
        let res = ai_pending_actions::Entity::update_many()
            .col_expr(
                ai_pending_actions::Column::Status,
                sea_orm::sea_query::Expr::value("skipped"),
            )
            .filter(ai_pending_actions::Column::PlanPublicId.eq(plan_id))
            .filter(ai_pending_actions::Column::CreatedBy.eq(user_id))
            .filter(ai_pending_actions::Column::StepIndex.gt(action.step_index))
            .filter(
                Condition::any()
                    .add(ai_pending_actions::Column::Status.eq("proposed"))
                    .add(ai_pending_actions::Column::Status.eq("awaiting_inline_approval")),
            )
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
            error: Set(Some(redact_text(reason))),
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
            project_id: Some(project_id),
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
            created_by: 1,
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
            image_retention_hours: None,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
            cloud_telemetry_fidelity:
                temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity::Metered,
            cloud_telemetry_write_mode:
                temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode::Local,
            cloud_analytics_write_mode:
                temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode::Local,
            instantiation_token: None,
            cloud_telemetry_attribute_allowlist: Vec::new(),
            name: "test-project".to_string(),
            repo_name: "repo".to_string(),
            repo_owner: "owner".to_string(),
            directory: ".".to_string(),
            pull_only_root_directory: false,
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Static,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "test-project".to_string(),
            template_slug: None,
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_api_traffic_summary_enabled: None,
            allow_alternate_sources: None,
            hosting: Default::default(),
            ai_debug_chat_enabled: Some(true),
            ai_write_actions_enabled: write_enabled,
            error_source_context_enabled: false,
            vulnerability_scanning_enabled: false,
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            project_type: temps_entities::types::ProjectType::Server,
            service_template: None,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            github_webhook_id: None,
            github_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: true,
            serves_own_temps_api: false,
        }
    }

    fn make_auth() -> AuthContext {
        AuthContext::new_session(test_user(), temps_auth::permissions::Role::Admin)
    }

    fn api_key_with_role(role: temps_auth::permissions::Role) -> AuthContext {
        AuthContext::new_api_key(test_user(), Some(role), None, "test-key".to_string(), 1)
    }

    fn test_user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: "t".to_string(),
            email: "t@t.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn noop_write_handle() -> Arc<WriteApiToolsHandle> {
        Arc::new(WriteApiToolsHandle::new())
    }

    fn test_encryption() -> Arc<temps_core::EncryptionService> {
        Arc::new(temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap())
    }

    fn make_svc(db: sea_orm::DatabaseConnection) -> PendingActionService {
        PendingActionService::new(Arc::new(db), noop_write_handle(), test_encryption())
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
        let result = svc.create(1, Some(7), &prepared, None, 1).await;
        assert!(result.is_ok(), "create should succeed: {:?}", result.err());
        let model = result.unwrap();
        assert_eq!(model.status, "proposed");
        assert_eq!(model.project_id, Some(7));
    }

    #[tokio::test]
    async fn inline_actions_use_a_non_legacy_lifecycle() {
        let mut row = make_proposed(1, "inline", 7);
        row.status = "awaiting_inline_approval".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .into_connection();
        let svc = make_svc(db);

        let created = svc
            .create_inline(1, Some(7), &make_prepared(), None, 1)
            .await
            .expect("inline action is stored");
        assert_eq!(created.status, "awaiting_inline_approval");
    }

    #[tokio::test]
    async fn detached_confirm_cannot_execute_an_inline_action() {
        let mut action = make_proposed(1, "inline", 7);
        action.status = "awaiting_inline_approval".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![action]])
            .append_query_results(vec![vec![make_project(7, true)]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();

        let error = svc
            .confirm(Some(7), "inline", &auth, Some(auth.user_id()), None)
            .await
            .expect_err("legacy confirmation must reject inline lifecycle rows");
        assert!(matches!(
            error,
            PendingActionError::InlineApprovalRequired { ref public_id } if public_id == "inline"
        ));
    }

    #[tokio::test]
    async fn detached_reject_of_an_inline_action_points_to_the_inline_flow() {
        let mut action = make_proposed(1, "inline", 7);
        action.status = "awaiting_inline_approval".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![action]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();

        let error = svc
            .reject(Some(7), "inline", &auth, Some(auth.user_id()))
            .await
            .expect_err("legacy rejection must not resolve an inline approval");

        assert!(
            matches!(error, PendingActionError::InlineApprovalRequired { ref public_id } if public_id == "inline"),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("inline approval"), "{message}");
        assert!(!message.contains("concurrent"), "{message}");
    }

    #[tokio::test]
    async fn detached_reject_of_a_resolved_action_reports_its_real_status() {
        let mut action = make_proposed(1, "done", 7);
        action.status = "executed".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![action]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();

        let error = svc
            .reject(Some(7), "done", &auth, Some(auth.user_id()))
            .await
            .expect_err("an executed action cannot be rejected");

        assert!(
            matches!(error, PendingActionError::InvalidState { ref status, .. } if status == "executed"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn inline_reject_still_resolves_an_inline_action() {
        let mut action = make_proposed(1, "inline", 7);
        action.status = "awaiting_inline_approval".to_string();
        let mut rejected = action.clone();
        rejected.status = "rejected".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![action]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![rejected]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();

        let result = svc
            .reject_inline(Some(7), "inline", &auth, Some(auth.user_id()))
            .await
            .expect("the inline flow resolves its own rows");
        assert_eq!(result.status, "rejected");
    }

    #[tokio::test]
    async fn create_never_persists_plaintext_write_secrets() {
        let row = make_proposed(1, "abc", 7);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![row]])
                .into_connection(),
        );
        let svc = PendingActionService::new(db.clone(), noop_write_handle(), test_encryption());
        let mut prepared = make_prepared();
        prepared.params = serde_json::json!({
            "name": "DATABASE_PASSWORD",
            "value": "must-not-reach-the-database-in-plaintext"
        });

        svc.create(1, Some(7), &prepared, None, 1)
            .await
            .expect("encrypted action is created");
        drop(svc);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains("must-not-reach-the-database-in-plaintext"));
        assert!(log.contains("__temps_encrypted_v1"));
        assert!(log.contains("***"));
    }

    // get: not-found returns NotFound.
    #[tokio::test]
    async fn test_get_not_found_returns_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_pending_actions::Model>::new()])
            .into_connection();
        let svc = make_svc(db);
        let err = svc
            .get(Some(7), 1, "nonexistent")
            .await
            .expect_err("should be not found");
        assert!(
            matches!(err, PendingActionError::NotFound { ref public_id } if public_id == "nonexistent"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn global_action_scope_uses_is_null_instead_of_equals_null() {
        let mut action = make_proposed(1, "global-inline", 7);
        action.project_id = None;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![action.clone()]])
                .append_query_results(vec![vec![action]])
                .into_connection(),
        );
        let svc = PendingActionService::new(db.clone(), noop_write_handle(), test_encryption());

        svc.get(None, 1, "global-inline")
            .await
            .expect("global action should be found");
        svc.list_for_conversation(None, 1, 1)
            .await
            .expect("global actions should be listed");

        drop(svc);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let log = format!("{:?}", db.into_transaction_log());
        assert_eq!(
            log.matches("IS NULL").count(),
            2,
            "both global queries must use SQL IS NULL: {log}"
        );
    }

    #[tokio::test]
    async fn pending_action_surfaces_fail_closed_for_other_or_null_creators() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<ai_pending_actions::Model>::new()])
                .append_query_results([Vec::<ai_pending_actions::Model>::new()])
                .append_query_results([Vec::<ai_pending_actions::Model>::new()])
                .append_query_results([Vec::<ai_pending_actions::Model>::new()])
                .into_connection(),
        );
        let svc = PendingActionService::new(db.clone(), noop_write_handle(), test_encryption());
        let auth = make_auth();

        assert!(svc
            .list_for_conversation(Some(7), auth.user_id(), 1)
            .await
            .expect("owned action list")
            .is_empty());
        assert!(matches!(
            svc.get(Some(7), auth.user_id(), "other-user-or-legacy")
                .await,
            Err(PendingActionError::NotFound { .. })
        ));
        assert!(matches!(
            svc.confirm(
                Some(7),
                "other-user-or-legacy",
                &auth,
                Some(auth.user_id()),
                None
            )
            .await,
            Err(PendingActionError::NotFound { .. })
        ));
        assert!(matches!(
            svc.reject(Some(7), "other-user-or-legacy", &auth, Some(auth.user_id()))
                .await,
            Err(PendingActionError::NotFound { .. })
        ));

        drop(svc);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let statements = db
            .into_transaction_log()
            .into_iter()
            .flat_map(|transaction| transaction.statements().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(statements.len(), 4);
        assert!(statements.iter().all(|statement| {
            statement.sql.contains("created_by")
                && format!("{statement:?}").contains(&auth.user_id().to_string())
        }));
    }

    // A legacy false project toggle must not veto a native-approved action.
    #[tokio::test]
    async fn test_confirm_ignores_legacy_write_toggle() {
        let action = make_proposed(1, "abc", 7);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![action]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![make_proposed(1, "abc", 7)]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .confirm(Some(7), "abc", &auth, Some(1), None)
            .await
            .expect_err("empty write handle remains unavailable");
        assert!(matches!(err, PendingActionError::Unavailable));
    }

    // Rejecting also remains available independently of the removed toggle.
    #[tokio::test]
    async fn test_reject_does_not_query_legacy_write_toggle() {
        let proposed = make_proposed(1, "abc", 7);
        let mut rejected = proposed.clone();
        rejected.status = "rejected".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![proposed]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![rejected]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let result = svc
            .reject(Some(7), "abc", &auth, Some(1))
            .await
            .expect("reject should not depend on a project feature flag");
        assert_eq!(result.status, "rejected");
    }

    // confirm on non-proposed → InvalidState.
    #[tokio::test]
    async fn test_confirm_non_proposed_returns_invalid_state() {
        let mut row = make_proposed(1, "abc", 7);
        row.status = "executed".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action (executed status)
            .append_query_results(vec![vec![row]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .confirm(Some(7), "abc", &auth, Some(1), None)
            .await
            .expect_err("should fail");
        assert!(
            matches!(err, PendingActionError::InvalidState { .. }),
            "unexpected: {err:?}"
        );
    }

    // confirm: a plan step whose earlier step hasn't executed → StepBlocked
    // (before any atomic claim). Query order: get(action) → status-proposed
    // ok → ensure_plan_step_ready loads earlier steps.
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

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![step2]]) // get()
            .append_query_results(vec![vec![step1]]) // ensure_plan_step_ready: earlier steps
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .confirm(Some(7), "step2", &auth, Some(1), None)
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
        assert!(svc.ensure_plan_step_ready(&standalone, 1).await.is_ok());
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
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action
            .append_query_results(vec![vec![proposed]])
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
        let result = svc.reject(Some(7), "abc", &auth, Some(1)).await;
        assert!(result.is_ok(), "reject should succeed: {:?}", result.err());
        let m = result.unwrap();
        assert_eq!(m.status, "rejected");
    }

    /// A proposal recording `permission` the way the write tools prepare it.
    fn proposed_requiring(public_id: &str, permission: Option<&str>) -> ai_pending_actions::Model {
        let mut action = make_proposed(1, public_id, 7);
        action.required_permission = permission.map(str::to_string);
        action
    }

    // A read-only key can't confirm a proposal, so it must not be able to
    // discard one either. The check runs before the claim: nothing is written.
    #[tokio::test]
    async fn read_only_api_keys_cannot_reject_a_proposal() {
        for role in [
            temps_auth::permissions::Role::Reader,
            temps_auth::permissions::Role::ApiReader,
        ] {
            for permission in [Some("deployments:create"), None] {
                let db = MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results(vec![vec![proposed_requiring("abc", permission)]])
                    .into_connection();
                let svc = make_svc(db);
                let auth = api_key_with_role(role.clone());

                let error = svc
                    .reject(Some(7), "abc", &auth, Some(auth.user_id()))
                    .await
                    .expect_err("a read-only key must not reject a proposal");

                let expected = permission.unwrap_or("projects:write");
                assert!(
                    matches!(
                        &error,
                        PendingActionError::PermissionDenied { public_id, permission, decision }
                            if public_id == "abc" && permission == expected && *decision == "reject"
                    ),
                    "{role:?} / {permission:?}: {error:?}"
                );
            }
        }
    }

    // Rejecting takes the operation's own permission, not a blanket project
    // write: a role holding it may reject, a role without it may not.
    #[tokio::test]
    async fn reject_requires_the_permission_the_action_declares() {
        let mut rejected = proposed_requiring("abc", Some("deployments:create"));
        rejected.status = "rejected".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![proposed_requiring(
                "abc",
                Some("deployments:create"),
            )]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![rejected]])
            .into_connection();
        let svc = make_svc(db);
        let user = api_key_with_role(temps_auth::permissions::Role::User);
        let result = svc
            .reject(Some(7), "abc", &user, Some(user.user_id()))
            .await
            .expect("a user who could confirm the action may reject it");
        assert_eq!(result.status, "rejected");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![proposed_requiring("abc", Some("users:create"))]])
            .into_connection();
        let svc = make_svc(db);
        let error = svc
            .reject(Some(7), "abc", &user, Some(user.user_id()))
            .await
            .expect_err("a user who could not confirm the action may not reject it");
        assert!(
            matches!(&error, PendingActionError::PermissionDenied { permission, .. } if permission == "users:create"),
            "{error:?}"
        );
    }

    // There is no router guard behind a reject, so an unrecognised permission
    // fails closed instead of passing like it does on confirm.
    #[tokio::test]
    async fn reject_fails_closed_on_an_unknown_permission() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![proposed_requiring(
                "abc",
                Some("not_a:permission"),
            )]])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let error = svc
            .reject(Some(7), "abc", &auth, Some(auth.user_id()))
            .await
            .expect_err("an unknown permission cannot be satisfied");
        assert!(
            matches!(&error, PendingActionError::PermissionDenied { permission, .. } if permission == "not_a:permission"),
            "{error:?}"
        );
        assert!(error.to_string().contains("reject pending action 'abc'"));
    }

    // reject on non-proposed → atomic update returns 0 rows → InvalidState.
    #[tokio::test]
    async fn test_reject_non_proposed_returns_invalid_state() {
        let mut row = make_proposed(1, "abc", 7);
        row.status = "rejected".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action (already rejected)
            .append_query_results(vec![vec![row]])
            // atomic update_many returns 0 rows (nothing to flip)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let svc = make_svc(db);
        let auth = make_auth();
        let err = svc
            .reject(Some(7), "abc", &auth, Some(1))
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
            .list_for_conversation(Some(7), 1, 1)
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
    // Sequence: get action → status pre-check passes (proposed) → permission
    // check passes (None) → atomic claim
    // (1 row affected) → handle.get() returns None → set_failed → Unavailable.
    #[tokio::test]
    async fn test_confirm_unavailable_write_handle() {
        let proposed = make_proposed(1, "abc", 7);
        let failed_model = {
            let mut m = proposed.clone();
            m.status = "failed".to_string();
            m
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get() for the action
            .append_query_results(vec![vec![proposed]])
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
            .confirm(Some(7), "abc", &auth, Some(1), None)
            .await
            .expect_err("should fail with Unavailable");
        assert!(
            matches!(err, PendingActionError::Unavailable),
            "unexpected: {err:?}"
        );
    }
}
