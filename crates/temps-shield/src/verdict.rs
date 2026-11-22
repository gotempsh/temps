// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What Shield would do with a request, and why (ADR 066, D1).

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::net::Network;

/// The verdict a list gives the addresses or fingerprints on it.
///
/// Stored lists encode this by position, so new variants go last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ListVerdict {
    Log,
    Challenge,
    Block,
    /// An operator rule that lets the matching clients through whatever
    /// any other list says (D1: operator allow rules win).
    Allow,
}

impl ListVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Challenge => "challenge",
            Self::Block => "block",
            Self::Allow => "allow",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "log" => Some(Self::Log),
            "challenge" => Some(Self::Challenge),
            "block" => Some(Self::Block),
            "allow" => Some(Self::Allow),
            _ => None,
        }
    }
}

/// The verdict for one request, strongest last so the strongest match wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Pass,
    Log,
    Challenge,
    Block,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Log => "log",
            Self::Challenge => "challenge",
            Self::Block => "block",
        }
    }
}

impl From<ListVerdict> for Action {
    /// An allow rule passes: it is the absence of a verdict, and the
    /// lookup that finds one discards every other match.
    fn from(verdict: ListVerdict) -> Self {
        match verdict {
            ListVerdict::Log => Self::Log,
            ListVerdict::Challenge => Self::Challenge,
            ListVerdict::Block => Self::Block,
            ListVerdict::Allow => Self::Pass,
        }
    }
}

/// What a reason matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// The range on a list that holds the client address.
    Network(Network),
    /// The client's TLS fingerprint, on a list of fingerprints.
    Ja4(Arc<str>),
    /// A known exploit signature (D8), and the path it was sent to.
    Signature {
        id: &'static str,
        cve: Option<&'static str>,
        path: Arc<str>,
    },
    /// A User-Agent that claims a browser from a client whose JA4 is a
    /// known HTTP library or tool (D4, use 2).
    ClientMismatch {
        claimed: &'static str,
        tool: &'static str,
    },
}

/// One match: the list or rule, and what of the request it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub source: Arc<str>,
    pub subject: Subject,
    /// The list is older than its maximum age, so the match only logs.
    pub stale: bool,
}

/// Longest path a reason names, so a reason stays one readable line.
const REASON_PATH_CHARS: usize = 64;

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stale = if self.stale { ", stale" } else { "" };
        match &self.subject {
            Subject::Network(network) => write!(f, "{} ({network}{stale})", self.source),
            Subject::Ja4(ja4) => write!(f, "{} (ja4 {ja4}{stale})", self.source),
            Subject::Signature { id, cve, path } => {
                let shown: String = path.chars().take(REASON_PATH_CHARS).collect();
                let more = if shown.len() < path.len() { "…" } else { "" };
                write!(f, "{} on {shown}{more}", cve.unwrap_or(id))
            }
            Subject::ClientMismatch { claimed, tool } => {
                write!(f, "{} (claims {claimed}, ja4 of {tool})", self.source)
            }
        }
    }
}

impl Reason {
    /// The signature this reason names, as events record it.
    pub fn signature(&self) -> Option<&'static str> {
        match self.subject {
            Subject::Signature { id, cve, .. } => Some(cve.unwrap_or(id)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub action: Action,
    pub reasons: Vec<Reason>,
}

impl Default for Verdict {
    fn default() -> Self {
        Self::PASS
    }
}

impl Verdict {
    pub const PASS: Self = Self {
        action: Action::Pass,
        reasons: Vec::new(),
    };

    /// The reasons as the strings the request log stores.
    pub fn reason_strings(&self) -> Vec<String> {
        self.reasons.iter().map(ToString::to_string).collect()
    }

    /// Adds one match, keeping the strongest action.
    pub fn add(&mut self, action: Action, reason: Reason) {
        self.action = self.action.max(action);
        self.reasons.push(reason);
    }

    /// Both verdicts' reasons, and the stronger action.
    pub fn merge(&mut self, other: Verdict) {
        self.action = self.action.max(other.action);
        self.reasons.extend(other.reasons);
    }

    /// The first signature among the reasons.
    pub fn signature(&self) -> Option<&'static str> {
        self.reasons.iter().find_map(Reason::signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_read_as_one_line() {
        let network = Reason {
            source: "spamhaus_drop".into(),
            subject: Subject::Network(Network::parse("185.93.89.0/24").unwrap()),
            stale: false,
        };
        assert_eq!(network.to_string(), "spamhaus_drop (185.93.89.0/24)");
        let signature = Reason {
            source: "signatures".into(),
            subject: Subject::Signature {
                id: "apache-traversal",
                cve: Some("CVE-2021-41773"),
                path: format!("/cgi-bin/{}", "a".repeat(80)).into(),
            },
            stale: false,
        };
        let text = signature.to_string();
        assert!(text.starts_with("CVE-2021-41773 on /cgi-bin/aaa"), "{text}");
        assert!(text.ends_with('…'), "{text}");
        assert_eq!(signature.signature(), Some("CVE-2021-41773"));
        let mismatch = Reason {
            source: "client_mismatch".into(),
            subject: Subject::ClientMismatch {
                claimed: "Chrome",
                tool: "curl",
            },
            stale: false,
        };
        assert_eq!(
            mismatch.to_string(),
            "client_mismatch (claims Chrome, ja4 of curl)"
        );
        let ja4 = Reason {
            source: "observed".into(),
            subject: Subject::Ja4("t13d1516h2_8daaf6152771_02713d6af862".into()),
            stale: true,
        };
        assert_eq!(
            ja4.to_string(),
            "observed (ja4 t13d1516h2_8daaf6152771_02713d6af862, stale)"
        );
    }

    #[test]
    fn a_list_verdict_keeps_its_stored_position() {
        // Stored lists encode the verdict by position: Allow must come last.
        assert_eq!(postcard::to_allocvec(&ListVerdict::Log).unwrap(), [0]);
        assert_eq!(postcard::to_allocvec(&ListVerdict::Block).unwrap(), [2]);
        assert_eq!(postcard::to_allocvec(&ListVerdict::Allow).unwrap(), [3]);
        for verdict in ["log", "challenge", "block", "allow"] {
            assert_eq!(ListVerdict::parse(verdict).unwrap().as_str(), verdict);
        }
    }
}
