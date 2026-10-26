// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What Shield would do with a request, and why (ADR 066, D1).

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::net::Network;

/// The verdict a list gives the addresses on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ListVerdict {
    Log,
    Challenge,
    Block,
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
    fn from(verdict: ListVerdict) -> Self {
        match verdict {
            ListVerdict::Log => Self::Log,
            ListVerdict::Challenge => Self::Challenge,
            ListVerdict::Block => Self::Block,
        }
    }
}

/// One match: the list and the range on it that holds the address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub source: Arc<str>,
    pub network: Network,
    /// The list is older than its maximum age, so the match only logs.
    pub stale: bool,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.stale {
            write!(f, "{} ({}, stale)", self.source, self.network)
        } else {
            write!(f, "{} ({})", self.source, self.network)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub action: Action,
    pub reasons: Vec<Reason>,
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
}
