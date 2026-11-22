// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a turn accumulates as it runs: the transcript the user sees, how it is
//! ending, and the provider session it is resuming.

use super::*;

/// Text deltas can be very small. Checkpoint immediately, then at most four
/// times per second; structural events always checkpoint.
const TEXT_CHECKPOINT_INTERVAL: Duration = Duration::from_millis(250);

/// The assistant message under construction.
#[derive(Debug, Default)]
pub(super) struct TurnTranscript {
    /// Structured record of each executed tool, persisted on the assistant
    /// message's metadata so the chat replays its tool work after a reload.
    pub(super) tools: Vec<serde_json::Value>,
    /// Ordered render segments (text / tool, in occurrence order). Persisted so
    /// a reload shows the same interleaving the live stream did.
    pub(super) parts: Vec<serde_json::Value>,
    /// The open text segment being accumulated (flushed into `parts` when a
    /// tool call interrupts it or the turn ends).
    pub(super) open_text: String,
    /// All assistant prose across the turn: the persisted `content` and the
    /// history replayed to the model on the next turn.
    pub(super) content: String,
    last_checkpoint: Option<Instant>,
}

impl TurnTranscript {
    /// The separator to emit before this text, when it starts a new block of
    /// prose after something already shown (e.g. a previous round's narration).
    pub(super) fn separator_before(&self, block_text: &str) -> Option<String> {
        (block_text.is_empty() && !self.content.is_empty() && !self.content.ends_with('\n'))
            .then(|| "\n\n".to_string())
    }

    /// Add prose to the open text segment.
    pub(super) fn push_open_text(&mut self, text: &str) {
        self.content.push_str(text);
        self.open_text.push_str(text);
    }

    /// Close the open text part, so order is preserved around what follows.
    pub(super) fn close_open_text(&mut self) {
        if !self.open_text.is_empty() {
            self.parts.push(serde_json::json!({
                "type": "text",
                "text": std::mem::take(&mut self.open_text),
            }));
        }
    }

    pub(super) fn record_call(&mut self, id: &str, name: &str, arguments: &str) {
        self.close_open_text();
        record_tool_call(&mut self.tools, &mut self.parts, id, name, arguments);
    }

    pub(super) fn record_result(&mut self, id: &str, name: &str, arguments: &str, result: &str) {
        record_tool_result(
            &mut self.tools,
            &mut self.parts,
            id,
            name,
            arguments,
            result,
        );
    }

    /// Say that the turn was cut short, as its own part, and return the text.
    pub(super) fn push_stop_note(&mut self, reason: &str) -> String {
        let note = format!(
            "\n\n_The assistant {reason}. What follows is based only on \
             what it gathered so far — ask it to continue if that looks incomplete._\n\n"
        );
        self.content.push_str(&note);
        self.parts
            .push(serde_json::json!({ "type": "text", "text": note.clone() }));
        note
    }

    /// Keep attempted tool calls for diagnosis, but discard the prose and
    /// replace it with the server-owned truth.
    pub(super) fn replace_prose(&mut self, correction: &str) {
        self.content = correction.to_string();
        self.parts
            .retain(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("tool"));
        self.parts
            .push(serde_json::json!({ "type": "text", "text": correction }));
    }

    /// True when the turn has produced nothing at all: no prose, no tool work.
    pub(super) fn is_empty(&self) -> bool {
        self.content.is_empty() && self.tools.is_empty()
    }

    pub(super) fn text_checkpoint_due(&self) -> bool {
        self.last_checkpoint
            .is_none_or(|last| last.elapsed() >= TEXT_CHECKPOINT_INTERVAL)
    }

    pub(super) fn mark_checkpointed(&mut self) {
        self.last_checkpoint = Some(Instant::now());
    }
}

/// How the turn is ending, gathered from every stage that can end it.
#[derive(Debug, Default)]
pub(super) struct TurnOutcome {
    /// Did a round answer in prose (no tool calls)? Then we have the final
    /// answer and stop; otherwise we may need a salvage call.
    pub(super) answered: bool,
    /// The last provider error seen while trying to produce this turn. Kept so
    /// that a turn which ends up with nothing to show can explain WHY instead
    /// of just stopping.
    pub(super) last_provider_error: Option<String>,
    /// `(purpose, reason)` of a retained harness diagnostic.
    pub(super) retained_diagnostic: Option<(String, String)>,
    pub(super) durable_failure: Option<crate::PublicChatFailure>,
    /// Set when the provider's reply stream ended before the provider said it
    /// was finished (dropped connection, mid-reply error, client timeout, no
    /// terminal chunk). The text received so far is partial: the turn is
    /// failed, not completed.
    pub(super) stream_interruption: Option<String>,
    /// Why generation stopped, when it was a bound rather than the model
    /// finishing. Reported to the user: a turn that halts for a reason nobody
    /// states looks identical to one that simply gave up.
    pub(super) stop_reason: Option<&'static str>,
}

impl TurnOutcome {
    /// Note what a provider error says about the turn. The caller decides
    /// whether it is also remembered as the turn's failure.
    pub(super) fn observe_error(&mut self, error: &temps_ai::AiError) {
        if let temps_ai::AiError::RetainedHarnessDiagnostic { purpose, reason } = error {
            self.retained_diagnostic = Some((purpose.clone(), reason.clone()));
        }
    }

    pub(super) fn remember_error(&mut self, reason: String) {
        remember_provider_error(&mut self.last_provider_error, reason);
    }

    /// An interruption fails the turn even when text arrived.
    pub(super) fn observe_interruption(&mut self, error: &temps_ai::AiError) {
        if let temps_ai::AiError::StreamInterrupted { reason, .. } = error {
            self.stream_interruption = Some(reason.clone());
        }
    }
}

/// The provider session this turn resumes, and what the provider reported.
#[derive(Debug, Default)]
pub(super) struct ProviderSession {
    pub(super) resume_id: Option<String>,
    pub(super) reset_retained: bool,
    pub(super) retry_used: bool,
    pub(super) reported_id: Option<String>,
    pub(super) reported_title: Option<String>,
}

impl ProviderSession {
    /// A new turn with no durable provider session must not attach a runtime
    /// that still carries an older, now-missing session.
    pub(super) fn new(resume_id: Option<String>, uses_harness: bool) -> Self {
        Self {
            reset_retained: uses_harness && resume_id.is_none(),
            resume_id,
            ..Default::default()
        }
    }

    /// The reset applies to the first request that sees it, and only that one.
    pub(super) fn take_reset(&mut self) -> bool {
        std::mem::take(&mut self.reset_retained)
    }

    pub(super) fn may_rebuild(&self, provider: &str, reason: &str, round_had_effect: bool) -> bool {
        can_retry_missing_provider_session(
            provider,
            reason,
            self.resume_id.is_some(),
            self.retry_used,
            round_had_effect,
        )
    }

    /// Forget the missing session, and what it reported, so the next round
    /// rebuilds it from durable conversation history.
    pub(super) fn begin_rebuild(&mut self) {
        self.resume_id = None;
        self.reset_retained = true;
        self.retry_used = true;
        self.reported_id = None;
        self.reported_title = None;
    }

    /// Later metadata that omits a field keeps what was already reported.
    pub(super) fn report(&mut self, session_id: Option<String>, title: Option<String>) {
        if session_id.is_some() {
            self.reported_id = session_id;
        }
        if title.is_some() {
            self.reported_title = title;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(transcript: &TurnTranscript) -> Vec<&str> {
        transcript
            .parts
            .iter()
            .map(|part| part["type"].as_str().unwrap_or("?"))
            .collect()
    }

    #[test]
    fn a_separator_only_starts_a_new_block_after_unterminated_prose() {
        let mut transcript = TurnTranscript::default();
        assert_eq!(transcript.separator_before(""), None, "nothing shown yet");

        transcript.push_open_text("First");
        assert_eq!(transcript.separator_before(""), Some("\n\n".to_string()));
        assert_eq!(
            transcript.separator_before("already in this block"),
            None,
            "only the start of a block is separated"
        );

        transcript.push_open_text("\n");
        assert_eq!(
            transcript.separator_before(""),
            None,
            "already on a new line"
        );
    }

    #[test]
    fn a_tool_call_closes_the_open_text_so_order_is_preserved() {
        let mut transcript = TurnTranscript::default();
        transcript.push_open_text("Looking");
        transcript.record_call("c1", "echo", "{}");
        transcript.record_result("c1", "echo", "{}", "done");
        transcript.push_open_text("Finished");
        transcript.close_open_text();

        assert_eq!(kinds(&transcript), vec!["text", "tool", "text"]);
        assert_eq!(transcript.content, "LookingFinished");
        assert!(transcript.open_text.is_empty());
        assert_eq!(transcript.tools[0]["result"], "done");
    }

    #[test]
    fn closing_with_no_open_text_adds_no_part() {
        let mut transcript = TurnTranscript::default();
        transcript.close_open_text();
        assert!(transcript.parts.is_empty());
    }

    #[test]
    fn the_stop_note_is_its_own_part_and_names_the_reason() {
        let mut transcript = TurnTranscript::default();
        let note = transcript.push_stop_note("ran out of road");
        assert!(note.contains("_The assistant ran out of road."));
        assert_eq!(transcript.content, note);
        assert_eq!(transcript.parts[0]["text"], note.as_str());
    }

    #[test]
    fn replacing_prose_keeps_the_tool_parts() {
        let mut transcript = TurnTranscript::default();
        transcript.push_open_text("Claimed");
        transcript.record_call("c1", "echo", "{}");
        transcript.replace_prose("Corrected");

        assert_eq!(transcript.content, "Corrected");
        assert_eq!(kinds(&transcript), vec!["tool", "text"]);
        assert_eq!(transcript.parts[1]["text"], "Corrected");
        assert_eq!(transcript.tools.len(), 1);
    }

    #[test]
    fn a_transcript_with_only_tool_work_is_not_empty() {
        let mut transcript = TurnTranscript::default();
        assert!(transcript.is_empty());
        transcript.record_call("c1", "echo", "{}");
        assert!(!transcript.is_empty());
    }

    #[test]
    fn the_first_text_checkpoints_immediately_then_is_throttled() {
        let mut transcript = TurnTranscript::default();
        assert!(transcript.text_checkpoint_due());
        transcript.mark_checkpointed();
        assert!(!transcript.text_checkpoint_due());
    }

    #[test]
    fn a_session_reset_applies_only_to_the_next_request() {
        let mut fresh = ProviderSession::new(None, true);
        assert!(fresh.take_reset());
        assert!(!fresh.take_reset());

        assert!(!ProviderSession::new(Some("s1".to_string()), true).reset_retained);
        assert!(!ProviderSession::new(None, false).reset_retained);
    }

    #[test]
    fn rebuilding_a_session_forgets_what_it_reported_and_allows_one_retry() {
        let missing = "Thread not found for id old-session";
        let mut session = ProviderSession::new(Some("old".to_string()), true);
        session.report(Some("old".to_string()), Some("Stale".to_string()));
        assert!(session.may_rebuild("codex_cli", missing, false));
        assert!(!session.may_rebuild("codex_cli", missing, true));

        session.begin_rebuild();
        assert_eq!(session.resume_id, None);
        assert_eq!(session.reported_id, None);
        assert_eq!(session.reported_title, None);
        assert!(session.reset_retained);
        assert!(!session.may_rebuild("codex_cli", missing, false));
    }

    #[test]
    fn metadata_that_omits_a_field_keeps_what_was_reported() {
        let mut session = ProviderSession::default();
        session.report(Some("s1".to_string()), Some("Title".to_string()));
        session.report(None, None);
        assert_eq!(session.reported_id.as_deref(), Some("s1"));
        assert_eq!(session.reported_title.as_deref(), Some("Title"));
    }

    #[test]
    fn an_interruption_is_noted_only_from_its_own_error() {
        let mut outcome = TurnOutcome::default();
        outcome.observe_interruption(&temps_ai::AiError::NotAvailable);
        assert_eq!(outcome.stream_interruption, None);
        outcome.observe_interruption(&temps_ai::AiError::StreamInterrupted {
            purpose: "chat.test".to_string(),
            reason: "dropped".to_string(),
        });
        assert_eq!(outcome.stream_interruption.as_deref(), Some("dropped"));
    }
}
