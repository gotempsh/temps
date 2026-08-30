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
    pub fn message(&self) -> String {
        match self {
            MirrorHealth::Healthy => "All telemetry mirrored.".into(),
            MirrorHealth::Buffering { spooled, reason } => format!(
                "{spooled} spans awaiting mirror delivery — {reason}. Source telemetry remains \
                 in local Temps storage."
            ),
            MirrorHealth::Dropping {
                spooled,
                dropped,
                reason,
            } => format!(
                "Local buffer is full: {dropped} spans discarded, {spooled} still queued. \
                 Last delivery attempt failed: {reason}."
            ),
            MirrorHealth::Degraded { detail } => match detail {
                Unavailable::QuotaExhausted {
                    used_bytes,
                    limit_bytes,
                    resets_at,
                } => format!(
                    "Ingest allowance used ({used_bytes} of {limit_bytes} bytes). \
                     Sampling until {resets_at}; raise the cap or upgrade to keep full fidelity."
                ),
                Unavailable::NotEntitled { required_plan } => {
                    format!("This capability requires the {required_plan} plan.")
                }
                Unavailable::NotEnrolled => "This instance is not linked to an account.".into(),
                Unavailable::Degraded {
                    retry_after_secs,
                    detail,
                } => format!(
                    "Backend degraded ({detail}); retrying in {retry_after_secs}s. \
                     Telemetry is buffered locally in the meantime."
                ),
                _ => "The managed backend is unavailable; telemetry is buffered locally.".into(),
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
