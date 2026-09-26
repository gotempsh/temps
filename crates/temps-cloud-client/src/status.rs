// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The link state the console and CLI show the operator.
//!
//! A self-hosted operator debugs alone. Every state here is either "fine" or a
//! sentence naming what is wrong and what to do about it — never a spinner,
//! never a silent absence, and never a bare boolean the UI has to interpret.

use serde::Serialize;
use temps_cloud_protocol::Unavailable;

/// Whether this instance is linked to a managed account.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LinkStatus {
    /// State exists but cannot be decoded with the current configuration. No
    /// mutation is allowed because it could destroy recoverable credentials.
    StateUnreadable {
        state_path: String,
    },
    /// No backend configured. The UI should offer to connect one rather than
    /// hiding the feature — an unconfigured capability must onboard, not vanish.
    NotConfigured,
    /// Backend known, not yet linked.
    AwaitingEnrollment {
        base_url: String,
    },
    Linked {
        base_url: String,
    },
    /// Linked, but the credential was refused. Actionable, not fatal.
    CredentialRejected {
        base_url: String,
    },
}

impl LinkStatus {
    /// One line, written for a human with no support channel.
    pub fn message(&self) -> String {
        match self {
            LinkStatus::StateUnreadable { state_path } => format!(
                "Cloud link state at {state_path} cannot be read. Restore the encryption key used to create it, or back up and remove the state file before reconnecting."
            ),
            LinkStatus::NotConfigured => {
                "Not connected to a managed backend. Telemetry is stored locally only.".into()
            }
            LinkStatus::AwaitingEnrollment { base_url } => format!(
                "Backend {base_url} is configured but this instance is not linked. \
                 Paste an enrollment code to connect it."
            ),
            LinkStatus::Linked { base_url } => format!("Connected to {base_url}."),
            LinkStatus::CredentialRejected { base_url } => format!(
                "{base_url} rejected this instance's credential. Re-enroll to reconnect. \
                 Telemetry is still being stored locally."
            ),
        }
    }

    /// Whether the operator must do something. Drives whether the UI nags.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            LinkStatus::StateUnreadable { .. }
                | LinkStatus::AwaitingEnrollment { .. }
                | LinkStatus::CredentialRejected { .. }
        )
    }
}

/// Which guarantee the operator actually has for the telemetry being described
/// (ADR-041 §3e).
///
/// # Why the status type had to learn about this
///
/// [`MirrorHealth`]'s copy used to assert, literally, *"Source telemetry
/// remains in local Temps storage."* That sentence is true for a project in the
/// default `Local` write mode and **false** for a Cloud-primary one, where the
/// queued spans are the only copy. A false status string is worse than no
/// status: it tells an operator watching a backlog grow that they have nothing
/// to worry about, at the exact moment they do.
///
/// So the message is parameterised by mode rather than being softened into
/// something vague enough to be true in both cases. Vague reassurance is how a
/// gap goes unnoticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryDurability {
    /// Every project on this instance writes spans locally first. Cloud is a
    /// best-effort mirror, and anything it loses is still on disk here.
    #[default]
    LocalAuthoritative,
    /// At least one project is Cloud-primary: its spans are not stored on this
    /// instance at all, and the durable outbox is the only thing between them
    /// and being gone.
    CloudPrimary,
}

impl TelemetryDurability {
    /// The clause appended to a buffering/queued message.
    ///
    /// Both halves are concrete. Neither says "don't worry".
    pub fn reassurance(&self) -> &'static str {
        match self {
            TelemetryDurability::LocalAuthoritative => {
                "Source telemetry remains in local Temps storage."
            }
            TelemetryDurability::CloudPrimary => {
                "These spans are not stored on this instance — they exist only in the durable \
                 outbox until Cloud accepts them."
            }
        }
    }

    pub fn is_cloud_primary(&self) -> bool {
        matches!(self, TelemetryDurability::CloudPrimary)
    }
}

/// How the mirror is doing, independent of whether the link is valid.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MirrorHealth {
    /// Everything shipped.
    Healthy,
    /// Buffering in memory while the local Temps store remains authoritative.
    Buffering { spooled: usize, reason: String },
    /// The spool overflowed and telemetry was discarded. This is the one state
    /// that must never be quiet.
    Dropping {
        spooled: usize,
        dropped: u64,
        reason: String,
    },
    /// Accepted, but the backend is degrading us (e.g. over quota).
    Degraded { detail: Unavailable },
}

impl MirrorHealth {
    /// The message for an instance where every project writes spans locally.
    ///
    /// Kept as the no-argument form so the ~dozen existing call sites (CLI,
    /// console status, tests) are unchanged: an instance with no Cloud-primary
    /// project reads exactly as it did before ADR-041.
    pub fn message(&self) -> String {
        self.message_for(TelemetryDurability::LocalAuthoritative)
    }

    /// The message for a given durability guarantee.
    ///
    /// Callers that know a project is Cloud-primary must use this, because the
    /// default form's reassurance about local storage is false for them.
    pub fn message_for(&self, durability: TelemetryDurability) -> String {
        match self {
            MirrorHealth::Healthy => match durability {
                TelemetryDurability::LocalAuthoritative => "All telemetry mirrored.".into(),
                TelemetryDurability::CloudPrimary => {
                    "All telemetry delivered to Temps Cloud; the outbox is empty.".into()
                }
            },
            MirrorHealth::Buffering { spooled, reason } => {
                let reassurance = durability.reassurance();
                match durability {
                    TelemetryDurability::LocalAuthoritative => format!(
                        "{spooled} spans awaiting mirror delivery — {reason}. {reassurance}"
                    ),
                    TelemetryDurability::CloudPrimary => {
                        format!("{spooled} spans queued for Temps Cloud — {reason}. {reassurance}")
                    }
                }
            }
            MirrorHealth::Dropping {
                spooled,
                dropped,
                reason,
            } => match durability {
                TelemetryDurability::LocalAuthoritative => format!(
                    "Local buffer is full: {dropped} spans discarded, {spooled} still queued. \
                     Last delivery attempt failed: {reason}."
                ),
                TelemetryDurability::CloudPrimary => format!(
                    "The telemetry outbox is full: {dropped} spans were not captured anywhere \
                     and are recorded as a gap window, {spooled} are still queued. \
                     Last delivery attempt failed: {reason}."
                ),
            },
            MirrorHealth::Degraded { detail } => match detail {
                Unavailable::QuotaExhausted {
                    used_bytes,
                    limit_bytes,
                    resets_at,
                } => match durability {
                    TelemetryDurability::LocalAuthoritative => format!(
                        "Ingest allowance used ({used_bytes} of {limit_bytes} bytes). \
                         Sampling until {resets_at}; raise the cap or upgrade to keep full \
                         fidelity."
                    ),
                    // Under Cloud-primary writes the same response means
                    // sampling away the only copy, so the instance falls back to
                    // local storage instead (ADR-041 §7b) and the message has to
                    // say that rather than repeating the mirror's wording.
                    TelemetryDurability::CloudPrimary => format!(
                        "Temps Cloud ingest allowance exhausted ({used_bytes} of {limit_bytes} \
                         bytes). Cloud-primary projects are storing spans on this instance until \
                         {resets_at}. Raise the allowance or accept local storage."
                    ),
                },
                Unavailable::NotEntitled { required_plan } => {
                    format!("This capability requires the {required_plan} plan.")
                }
                Unavailable::NotEnrolled => "This instance is not linked to an account.".into(),
                Unavailable::Degraded {
                    retry_after_secs,
                    detail,
                } => format!(
                    "Backend degraded ({detail}); retrying in {retry_after_secs}s. {}",
                    durability.reassurance()
                ),
                _ => format!(
                    "The managed backend is unavailable. {}",
                    durability.reassurance()
                ),
            },
        }
    }

    /// True when telemetry has actually been lost, as opposed to delayed.
    pub fn is_losing_data(&self) -> bool {
        matches!(self, MirrorHealth::Dropping { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconfigured_instance_is_told_what_it_is_missing_not_shown_nothing() {
        let m = LinkStatus::NotConfigured.message();
        assert!(
            m.contains("locally"),
            "must say where telemetry is going: {m}"
        );
        assert!(!LinkStatus::NotConfigured.needs_attention());
    }

    #[test]
    fn half_finished_setup_asks_the_operator_to_finish_it() {
        let s = LinkStatus::AwaitingEnrollment {
            base_url: "https://cloud.test".into(),
        };
        assert!(s.needs_attention());
        assert!(s.message().contains("enrollment code"));
    }

    #[test]
    fn unreadable_state_is_actionable_and_has_a_stable_serialized_shape() {
        let status = LinkStatus::StateUnreadable {
            state_path: "/data/cloud-link/state.json".into(),
        };
        assert!(status.needs_attention());
        let message = status.message();
        assert!(message.contains("encryption key"));
        assert!(message.contains("back up and remove"));

        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["state"], "state_unreadable");
        assert_eq!(value["state_path"], "/data/cloud-link/state.json");
    }

    #[test]
    fn a_rejected_credential_is_actionable_and_says_local_storage_is_unaffected() {
        let s = LinkStatus::CredentialRejected {
            base_url: "https://cloud.test".into(),
        };
        assert!(s.needs_attention());
        let m = s.message();
        assert!(m.contains("Re-enroll"), "must say what to do: {m}");
        assert!(m.contains("locally"), "must reassure about local data: {m}");
    }

    #[test]
    fn buffering_is_clearly_distinguished_from_losing_data() {
        let buffering = MirrorHealth::Buffering {
            spooled: 120,
            reason: "backend unreachable".into(),
        };
        assert!(!buffering.is_losing_data());
        assert!(buffering
            .message()
            .contains("Source telemetry remains in local Temps storage"));

        let dropping = MirrorHealth::Dropping {
            spooled: 10_000,
            dropped: 523,
            reason: "backend returned 500".into(),
        };
        assert!(dropping.is_losing_data());
        assert!(dropping.message().contains("523"), "must state how many");
        assert!(
            dropping.message().contains("backend returned 500"),
            "must state why: {}",
            dropping.message()
        );
    }

    #[test]
    fn quota_degradation_names_the_numbers_and_the_remedy() {
        let m = MirrorHealth::Degraded {
            detail: Unavailable::QuotaExhausted {
                used_bytes: 11_000_000_000,
                limit_bytes: 10_737_418_240,
                resets_at: chrono::Utc::now(),
            },
        }
        .message();
        assert!(m.contains("11000000000"), "must show real usage: {m}");
        assert!(m.contains("upgrade"), "must offer a remedy: {m}");
    }

    // ── ADR-041 §3e: the status must stop asserting local authority ──────

    #[test]
    fn cloud_primary_buffering_never_claims_the_spans_are_stored_locally() {
        // The whole reason `MirrorHealth` became mode-aware. Under Cloud-primary
        // writes there is no local copy, and a status string that says otherwise
        // tells an operator watching a backlog grow that they are safe at the
        // exact moment they are not.
        let buffering = MirrorHealth::Buffering {
            spooled: 4_000,
            reason: "backend unreachable".into(),
        };
        let message = buffering.message_for(TelemetryDurability::CloudPrimary);

        assert!(
            !message.contains("remains in local Temps storage"),
            "must not assert local authority for a Cloud-primary project: {message}"
        );
        assert!(
            message.contains("not stored on this instance"),
            "must state the actual guarantee: {message}"
        );
        assert!(message.contains("4000"), "must state how many: {message}");
        assert!(
            message.contains("backend unreachable"),
            "must state why: {message}"
        );
    }

    #[test]
    fn local_mode_copy_is_byte_for_byte_what_it_was_before() {
        // Deployment shape A (no Cloud-primary project anywhere) must read
        // exactly as it did before ADR-041 — including through the no-argument
        // `message()` every existing call site still uses.
        let buffering = MirrorHealth::Buffering {
            spooled: 120,
            reason: "backend unreachable".into(),
        };
        assert_eq!(
            buffering.message(),
            buffering.message_for(TelemetryDurability::LocalAuthoritative)
        );
        assert_eq!(
            buffering.message(),
            "120 spans awaiting mirror delivery — backend unreachable. Source telemetry remains \
             in local Temps storage."
        );
        assert_eq!(MirrorHealth::Healthy.message(), "All telemetry mirrored.");
    }

    #[test]
    fn cloud_primary_dropping_names_the_gap_rather_than_a_full_buffer() {
        let dropping = MirrorHealth::Dropping {
            spooled: 10_000,
            dropped: 523,
            reason: "backend returned 500".into(),
        };
        let message = dropping.message_for(TelemetryDurability::CloudPrimary);

        assert!(dropping.is_losing_data());
        assert!(message.contains("523"), "must state how many: {message}");
        assert!(
            message.contains("gap window"),
            "must point at where the hole is recorded: {message}"
        );
        assert!(
            message.contains("not captured anywhere"),
            "must be honest that the spans are gone: {message}"
        );
    }

    #[test]
    fn cloud_primary_quota_exhaustion_says_writes_moved_to_this_instance() {
        // Under a mirror this response means "sampling"; under Cloud-primary it
        // would mean sampling away the only copy, so the instance falls back and
        // the message must say so instead of repeating the mirror's wording.
        let degraded = MirrorHealth::Degraded {
            detail: Unavailable::QuotaExhausted {
                used_bytes: 11_000_000_000,
                limit_bytes: 10_737_418_240,
                resets_at: chrono::Utc::now(),
            },
        };
        let mirror = degraded.message_for(TelemetryDurability::LocalAuthoritative);
        let primary = degraded.message_for(TelemetryDurability::CloudPrimary);

        assert!(mirror.contains("Sampling until"), "{mirror}");
        assert!(
            !primary.contains("Sampling until"),
            "Cloud-primary must not describe sampling the only copy: {primary}"
        );
        assert!(
            primary.contains("storing spans on this instance"),
            "must say where the spans are going now: {primary}"
        );
    }

    #[test]
    fn every_mode_and_state_combination_produces_a_non_empty_message() {
        // No state, in either mode, may render as a blank or a spinner.
        let states = [
            MirrorHealth::Healthy,
            MirrorHealth::Buffering {
                spooled: 1,
                reason: "r".into(),
            },
            MirrorHealth::Dropping {
                spooled: 1,
                dropped: 1,
                reason: "r".into(),
            },
            MirrorHealth::Degraded {
                detail: Unavailable::NotEnrolled,
            },
        ];
        for state in &states {
            for durability in [
                TelemetryDurability::LocalAuthoritative,
                TelemetryDurability::CloudPrimary,
            ] {
                let message = state.message_for(durability);
                assert!(
                    !message.trim().is_empty(),
                    "{state:?} in {durability:?} rendered blank"
                );
            }
        }
    }

    #[test]
    fn the_default_durability_is_the_pre_adr_041_one() {
        // Anything that forgets to pass a mode gets the reading that was true
        // before this ADR, which is the safe direction: the Cloud-primary copy
        // is strictly more alarming, and over-alarming an instance that is fine
        // is better than reassuring one that is not.
        assert_eq!(
            TelemetryDurability::default(),
            TelemetryDurability::LocalAuthoritative
        );
        assert!(!TelemetryDurability::default().is_cloud_primary());
    }

    #[test]
    fn every_state_produces_a_non_empty_message() {
        // No state may render as a blank or a spinner.
        let states: Vec<Box<dyn Fn() -> String>> = vec![
            Box::new(|| LinkStatus::NotConfigured.message()),
            Box::new(|| {
                LinkStatus::Linked {
                    base_url: "u".into(),
                }
                .message()
            }),
            Box::new(|| MirrorHealth::Healthy.message()),
            Box::new(|| {
                MirrorHealth::Degraded {
                    detail: Unavailable::NotEnrolled,
                }
                .message()
            }),
        ];
        for f in states {
            assert!(!f().trim().is_empty());
        }
    }
}
