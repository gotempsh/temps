// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What surrounds a turn's task: the durable claim it must still own before it
//! starts, and the monitor that records how it ended.

use super::*;

/// The server-side claim a turn runs under. Unit-level callers do not claim a
/// durable turn, so they have no `turn_id`.
pub(super) struct TurnClaim {
    pub(super) db: Arc<DatabaseConnection>,
    pub(super) conversation_id: i64,
    pub(super) turn_id: Option<String>,
    pub(super) auto_approve: Arc<AtomicBool>,
    pub(super) permission_mode: Arc<Mutex<String>>,
    pub(super) events: TurnEvents,
}

impl TurnClaim {
    /// Make the turn's task reachable by Stop and by permission changes.
    pub(super) fn register(
        &self,
        active_turns: &Mutex<HashMap<i64, ActiveTurn>>,
        abort: AbortHandle,
    ) {
        let Some(turn_id) = self.turn_id.as_ref() else {
            return;
        };
        let active = ActiveTurn {
            turn_id: turn_id.clone(),
            abort,
            auto_approve_provider_tools: self.auto_approve.clone(),
            permission_mode: self.permission_mode.clone(),
        };
        active_turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(self.conversation_id, active);
    }

    /// The database is the source of truth for turn ownership. A Stop that
    /// raced workspace preparation clears this claim even when there was no
    /// in-memory abort handle yet; in that case the turn must not start.
    pub(super) async fn still_owned(&self) -> bool {
        let Some(turn_id) = self.turn_id.as_deref() else {
            return true;
        };
        let conv_id = self.conversation_id;
        match ai_conversations::Entity::find_by_id(conv_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(current)) => {
                // A permission change may race the workspace preparation
                // that precedes active-turn registration. Re-read the
                // durable value before releasing the startup barrier so
                // that change still applies to this turn.
                self.auto_approve.store(
                    permission_mode_auto_approves_provider_tools(&current.ai_permission_mode),
                    Ordering::Release,
                );
                self.permission_mode
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone_from(&current.ai_permission_mode);
                current.turn_status == "running"
                    && current.active_turn_id.as_deref() == Some(turn_id)
            }
            Ok(None) => false,
            Err(error) => {
                tracing::error!(
                    conv_id,
                    turn_id,
                    error = %error,
                    "failed to verify AI turn ownership before execution"
                );
                self.events.emit(Err(ChatError::Ai(
                    "The server could not verify this turn before execution. Please try again."
                        .to_string(),
                )));
                false
            }
        }
    }
}

/// What a finished conversation's title is refreshed with.
pub(super) struct TitleRefresh {
    pub(super) ai: Arc<dyn AiService>,
    pub(super) project_id: Option<i32>,
    pub(super) provider: String,
    pub(super) model: String,
}

/// A monitor, not the SSE response, owns the task handle. It publishes a
/// terminal event and releases the persisted claim even when no browser is
/// currently attached.
pub(super) struct TurnMonitor {
    pub(super) db: Arc<DatabaseConnection>,
    pub(super) live: broadcast::Sender<WireEvent>,
    pub(super) active_turns: Arc<Mutex<HashMap<i64, ActiveTurn>>>,
    pub(super) conversation_id: i64,
    pub(super) turn_id: Option<String>,
    pub(super) title: Option<TitleRefresh>,
    pub(super) started: tokio::time::Instant,
}

impl TurnMonitor {
    pub(super) async fn watch(self, turn_task: tokio::task::JoinHandle<bool>) {
        self.conclude(turn_task.await).await;
    }

    /// Record how the turn ended; `Ok(failed)` is what it reported itself.
    pub(super) async fn conclude(self, outcome: Result<bool, tokio::task::JoinError>) {
        if let Some(turn_id) = self.turn_id.as_deref() {
            self.release_active_turn(turn_id);
            let terminal_status = match &outcome {
                Ok(false) => "completed",
                Ok(true) | Err(_) => "failed",
            };
            if terminal_status == "failed" {
                self.persist_fallback_failure(turn_id).await;
            }
            self.finalize_claim(turn_id, terminal_status).await;
            if terminal_status == "completed" {
                self.refresh_title();
            }
            tracing::info!(
                component = "ai_turn_timing",
                turn_id,
                conversation_id = self.conversation_id,
                phase = "turn_finalized",
                terminal_status,
                total_ms = self.started.elapsed().as_millis() as u64,
                "AI turn timing"
            );
            if outcome.as_ref().is_err_and(|error| error.is_panic()) {
                let _ = self.live.send(WireEvent {
                    event: "error".to_string(),
                    data: "The server-side AI turn stopped unexpectedly.".to_string(),
                });
            }
        }
        let _ = self.live.send(WireEvent {
            event: "turn_complete".to_string(),
            data: String::new(),
        });
    }

    /// A newer turn may already hold this conversation's entry; leave it alone.
    fn release_active_turn(&self, turn_id: &str) {
        let mut turns = self
            .active_turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if turns
            .get(&self.conversation_id)
            .is_some_and(|active| active.turn_id == turn_id)
        {
            turns.remove(&self.conversation_id);
        }
    }

    async fn persist_fallback_failure(&self, turn_id: &str) {
        let fallback = crate::PublicChatFailure {
            code: "turn_failed_without_detail",
            title: "AI turn failed",
            detail: "The AI turn stopped before Temps retained a safe failure detail. Retry the message; if it fails again, open session diagnostics.".to_string(),
            retryable: true,
        };
        if let Err(error) = persist_conversation_failure(
            self.db.as_ref(),
            self.conversation_id,
            Some(turn_id),
            &fallback,
        )
        .await
        {
            tracing::error!(
                conversation_id = self.conversation_id,
                turn_id,
                %error,
                "failed to persist fallback AI turn failure"
            );
        }
    }

    async fn finalize_claim(&self, turn_id: &str, terminal_status: &'static str) {
        let conv_id = self.conversation_id;
        if let Err(error) = ai_conversations::Entity::update_many()
            .filter(ai_conversations::Column::Id.eq(conv_id))
            .filter(ai_conversations::Column::ActiveTurnId.eq(turn_id))
            .col_expr(
                ai_conversations::Column::TurnStatus,
                Expr::value(terminal_status),
            )
            .col_expr(
                ai_conversations::Column::ActiveTurnId,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                ai_conversations::Column::TurnStartedAt,
                Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .exec(self.db.as_ref())
            .await
        {
            tracing::error!(conv_id, turn_id, "failed to finalize AI turn: {error}");
        }
    }

    fn refresh_title(&self) {
        let Some(title) = self.title.as_ref() else {
            return;
        };
        let (ai, db, conv_id) = (title.ai.clone(), self.db.clone(), self.conversation_id);
        let (project_id, provider, model) = (
            title.project_id,
            title.provider.clone(),
            title.model.clone(),
        );
        tokio::spawn(async move {
            refresh_title_from_conversation(&ai, &db, conv_id, project_id, provider, model).await;
        });
    }
}
