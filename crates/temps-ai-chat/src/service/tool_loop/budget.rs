// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a turn may spend, and the decisions that follow when it runs out.
//!
//! A turn is bounded by TIME, not by a number of steps.
//!
//! A step count is the wrong governor: it says nothing about cost or about how
//! long someone has been watching a spinner, and it cuts short exactly the
//! long, productive tasks people want the chat for. The user can already see
//! every tool call as it happens and press Stop through the explicit server
//! cancellation endpoint. What a deadline adds is the guarantee those controls
//! can't give: that an unattended turn still ends, whatever the model is doing.

use super::*;

/// Phrased without the number, since the limit is configurable and a hardcoded
/// "15-minute" would start lying the moment anyone changes it.
pub(super) const TURN_TIMEOUT_REASON: &str =
    "reached the time limit for a single turn (configurable in Settings → AI)";
pub(super) const UNPRODUCTIVE_REASON: &str =
    "stopped after several attempts in a row produced only errors";
pub(super) const ROUND_BACKSTOP_REASON: &str = "stopped after an unusually long run of steps";

/// Purely an anti-spin guard, NOT a task budget. If rounds return instantly (a
/// provider erroring fast, a model emitting empty calls) the deadline alone
/// would allow an enormous number of iterations, so there is a ceiling, set far
/// beyond anything a real task approaches, so it is never what ends a turn in
/// practice.
const MAX_ROUNDS: usize = 500;
/// Independent of both: a turn where every call was rejected several rounds
/// running is stuck, and stopping it in seconds beats letting it grind for the
/// rest of the deadline. This never shortens a turn that is getting real
/// results.
const MAX_CONSECUTIVE_UNPRODUCTIVE_ROUNDS: usize = 3;
/// Every tool result is replayed on each later round, so without a cap on the
/// carried transcript a long turn runs out of context rather than finishing.
/// Not a limit on the task, a limit on what it re-sends.
pub(super) const MAX_CARRIED_TOOL_BYTES: usize = 192 * 1024;

impl ConversationService {
    /// The time limit for one turn. Operator-tunable in Settings → AI, because
    /// the right ceiling is a property of the model: a full alert-suggestion
    /// turn runs ~10 minutes against a slow self-hosted model and seconds
    /// against a hosted one. Read per turn (the settings service caches) so a
    /// change applies to the next message rather than needing a restart.
    pub(super) async fn turn_time_limit(&self) -> Duration {
        match &self.config {
            Some(cfg) => match cfg.get_settings().await {
                Ok(settings) => settings.ai_chat_limits.turn_timeout(),
                Err(e) => {
                    // Never fail a turn because settings could not be read:
                    // fall back to the default ceiling and say why once.
                    tracing::warn!(
                        "Could not read AI chat limits ({e}); using the default turn timeout"
                    );
                    temps_config::AiChatLimitsSettings::default().turn_timeout()
                }
            },
            None => temps_config::AiChatLimitsSettings::default().turn_timeout(),
        }
    }
}

/// The per-round limits of a context, known before the clock starts.
#[derive(Clone, Copy, Debug)]
pub(super) struct RoundLimits {
    pub(super) max_rounds: usize,
    pub(super) max_calls_per_round: usize,
}

impl RoundLimits {
    pub(super) fn for_context(context_type: &str) -> Self {
        Self {
            max_rounds: max_rounds_for_context(context_type, MAX_ROUNDS),
            max_calls_per_round: max_calls_per_round_for_context(context_type),
        }
    }
}

/// The running budget of one turn: its clock and how the rounds have gone.
#[derive(Debug)]
pub(super) struct TurnBudget {
    limits: RoundLimits,
    max_duration: Duration,
    started: tokio::time::Instant,
    /// Consecutive rounds in which every tool call was rejected.
    unproductive_streak: usize,
    /// Set only when all anti-spin rounds complete normally. A provider error
    /// after a native tool call must not be mislabeled as this backstop being
    /// exhausted.
    backstop_exhausted: bool,
}

impl TurnBudget {
    /// Start the clock.
    pub(super) fn start(limits: RoundLimits, max_duration: Duration) -> Self {
        Self {
            limits,
            max_duration,
            started: tokio::time::Instant::now(),
            unproductive_streak: 0,
            backstop_exhausted: false,
        }
    }

    pub(super) fn max_rounds(&self) -> usize {
        self.limits.max_rounds
    }

    pub(super) fn max_calls_per_round(&self) -> usize {
        self.limits.max_calls_per_round
    }

    pub(super) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub(super) fn deadline_reached(&self) -> bool {
        self.elapsed() >= self.max_duration
    }

    /// Time left on the clock; zero once the deadline has passed.
    pub(super) fn remaining(&self) -> Duration {
        self.max_duration.saturating_sub(self.elapsed())
    }

    /// Count one round of dispatched tool calls. A round where every call was
    /// rejected is not progress. Allow a couple (correcting a flag name is
    /// normal and the error messages are written to be self-correcting), then
    /// return why the turn must stop, rather than letting the model grind
    /// against the same mistake.
    pub(super) fn record_round(&mut self, produced_something: bool) -> Option<&'static str> {
        if produced_something {
            self.unproductive_streak = 0;
            return None;
        }
        self.unproductive_streak += 1;
        (self.unproductive_streak >= MAX_CONSECUTIVE_UNPRODUCTIVE_ROUNDS)
            .then_some(UNPRODUCTIVE_REASON)
    }

    /// The round at `round_index` ran to its end. Reaching the ceiling this way
    /// is what counts as exhausting it.
    pub(super) fn round_completed(&mut self, round_index: usize) {
        if round_index + 1 == self.limits.max_rounds {
            self.backstop_exhausted = true;
        }
    }

    /// The wrap-up note for the model once `round_index` has completed.
    pub(super) fn steps_left_note(&self, round_index: usize) -> Option<&'static str> {
        steps_left_note(self.limits.max_rounds, round_index + 1)
    }

    pub(super) fn backstop_exhausted(&self) -> bool {
        self.backstop_exhausted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: RoundLimits = RoundLimits {
        max_rounds: 10,
        max_calls_per_round: 4,
    };

    #[test]
    fn limits_come_from_the_context() {
        let assistant =
            RoundLimits::for_context(crate::providers::assistant::ASSISTANT_CONTEXT_TYPE);
        assert_eq!(assistant.max_rounds, ASSISTANT_MAX_ROUNDS);
        assert_eq!(assistant.max_calls_per_round, ASSISTANT_MAX_CALLS_PER_ROUND);

        let other = RoundLimits::for_context("project");
        assert_eq!(other.max_rounds, MAX_ROUNDS);
        assert_eq!(other.max_calls_per_round, usize::MAX);
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_is_reached_exactly_at_the_limit() {
        let budget = TurnBudget::start(LIMITS, Duration::from_secs(60));
        assert!(!budget.deadline_reached());
        assert_eq!(budget.remaining(), Duration::from_secs(60));

        tokio::time::advance(Duration::from_secs(59)).await;
        assert!(!budget.deadline_reached());
        assert_eq!(budget.remaining(), Duration::from_secs(1));

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(budget.deadline_reached());
        assert_eq!(budget.remaining(), Duration::ZERO);

        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(budget.remaining(), Duration::ZERO, "never underflows");
    }

    #[tokio::test]
    async fn three_unproductive_rounds_in_a_row_stop_the_turn() {
        let mut budget = TurnBudget::start(LIMITS, Duration::from_secs(60));
        assert_eq!(budget.record_round(false), None);
        assert_eq!(budget.record_round(false), None);
        assert_eq!(budget.record_round(false), Some(UNPRODUCTIVE_REASON));
    }

    #[tokio::test]
    async fn a_productive_round_resets_the_streak() {
        let mut budget = TurnBudget::start(LIMITS, Duration::from_secs(60));
        assert_eq!(budget.record_round(false), None);
        assert_eq!(budget.record_round(false), None);
        assert_eq!(budget.record_round(true), None);
        assert_eq!(budget.record_round(false), None);
        assert_eq!(budget.record_round(false), None);
        assert_eq!(budget.record_round(false), Some(UNPRODUCTIVE_REASON));
    }

    #[tokio::test]
    async fn only_completing_the_last_round_exhausts_the_backstop() {
        let mut budget = TurnBudget::start(LIMITS, Duration::from_secs(60));
        budget.round_completed(8);
        assert!(!budget.backstop_exhausted());
        budget.round_completed(9);
        assert!(budget.backstop_exhausted());
    }

    #[tokio::test]
    async fn the_wrap_up_note_comes_three_rounds_before_a_capped_end() {
        let budget = TurnBudget::start(LIMITS, Duration::from_secs(60));
        assert_eq!(budget.steps_left_note(5), None);
        assert_eq!(budget.steps_left_note(6), Some(STEPS_LEFT_NOTE));
        assert_eq!(budget.steps_left_note(7), None);

        let uncapped = TurnBudget::start(
            RoundLimits {
                max_rounds: MAX_ROUNDS,
                max_calls_per_round: usize::MAX,
            },
            Duration::from_secs(60),
        );
        assert_eq!(uncapped.steps_left_note(MAX_ROUNDS - 4), None);
    }
}
