// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What an ingress records for every decision other than `pass` (ADR 066,
//! D9, D11 and I8). The control plane stores events in ClickHouse
//! `shield_events`, or in Postgres without ClickHouse; workers post them in
//! batches. The promoter (D9) reads them back to learn from the fleet.

use serde::{Deserialize, Serialize};

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The request had a verdict: recorded in `log` mode, acted on in
    /// `selective` and `attack`. `verdict` says which.
    Verdict,
    /// A challenge page was served.
    ChallengeIssued,
    /// A posted solution earned a clearance.
    ChallengeSolved,
    /// A posted solution was refused; `reasons` holds why.
    ChallengeFailed,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verdict => "verdict",
            Self::ChallengeIssued => "challenge_issued",
            Self::ChallengeSolved => "challenge_solved",
            Self::ChallengeFailed => "challenge_failed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Verdict,
            Self::ChallengeIssued,
            Self::ChallengeSolved,
            Self::ChallengeFailed,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == text)
    }
}

/// One Shield event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShieldEvent {
    /// Unix milliseconds.
    pub timestamp_ms: i64,
    pub project_id: i32,
    pub environment_id: i32,
    /// The worker that saw it; `None` on the control plane.
    pub node_id: Option<i32>,
    pub request_id: String,
    pub kind: EventKind,
    /// The client address, as the ingress resolved it.
    pub ip: String,
    pub ja4: Option<String>,
    /// `log`, `challenge` or `block`: what the lists and signals said.
    pub verdict: String,
    /// `off`, `log`, `selective` or `attack`: whether it was acted on.
    pub mode: String,
    pub reasons: Vec<String>,
    /// The exploit signature matched (D8), as `CVE-…` or the signature id.
    pub signature: Option<String>,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub difficulty: Option<u8>,
    pub solve_ms: Option<u32>,
}

/// Longest path and User-Agent an event keeps, so a hostile request cannot
/// make its event large.
pub const MAX_FIELD_CHARS: usize = 512;

/// `text` cut to [`MAX_FIELD_CHARS`] characters.
pub fn bounded(text: &str) -> String {
    text.chars().take(MAX_FIELD_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_fields_stay_bounded() {
        for kind in [
            EventKind::Verdict,
            EventKind::ChallengeIssued,
            EventKind::ChallengeSolved,
            EventKind::ChallengeFailed,
        ] {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(bounded(&"é".repeat(600)).chars().count(), MAX_FIELD_CHARS);
    }
}
