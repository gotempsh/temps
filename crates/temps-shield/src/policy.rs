// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What an environment asks Shield to do (ADR 066, D1 and D7).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::bypass::TokenSet;

/// How an environment acts on verdicts (D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ShieldMode {
    /// Shield does nothing, and records nothing.
    Off,
    /// Verdicts are recorded with the request log and never acted on.
    #[default]
    Log,
    /// A block verdict answers 403, a challenge verdict challenges.
    Selective,
    /// As selective, and every other visitor is challenged too.
    Attack,
}

impl ShieldMode {
    pub const ALL: [ShieldMode; 4] = [Self::Off, Self::Log, Self::Selective, Self::Attack];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Log => "log",
            Self::Selective => "selective",
            Self::Attack => "attack",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == text)
    }

    /// The mode an environment runs in: its own when it sets one, else the
    /// project's. `attack_mode` is the column Shield modes replace (I2): the
    /// API writes both for one release, and a true one still means `attack`
    /// so a console that only knows attack mode keeps working.
    pub fn effective(
        environment: Option<&str>,
        project: &str,
        environment_attack: Option<bool>,
        project_attack: bool,
    ) -> Self {
        if environment_attack.unwrap_or(project_attack) {
            return Self::Attack;
        }
        environment
            .and_then(Self::parse)
            .or_else(|| Self::parse(project))
            .unwrap_or_default()
    }

    /// Whether verdicts are acted on, not only recorded.
    pub fn enforces(self) -> bool {
        matches!(self, Self::Selective | Self::Attack)
    }
}

impl fmt::Display for ShieldMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Most paths one environment may exempt, so matching stays cheap.
pub const MAX_EXEMPT_PATHS: usize = 64;

/// Paths an environment never challenges (D7): `/api/*` exempts everything
/// under `/api/` and `/api` itself; any other pattern is one exact path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExemptPaths {
    exact: Vec<String>,
    prefixes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExemptPathError {
    #[error("`{0}` is not a path: exempt paths start with /")]
    NotAPath(String),
    #[error("`{0}` has a * that is not at its end; only a trailing /* is supported")]
    Wildcard(String),
    #[error("at most {MAX_EXEMPT_PATHS} exempt paths")]
    TooMany,
}

impl ExemptPaths {
    /// Parses the patterns an environment stores, refusing any it would
    /// not match the way it reads.
    pub fn parse<S: AsRef<str>>(patterns: &[S]) -> Result<Self, ExemptPathError> {
        if patterns.len() > MAX_EXEMPT_PATHS {
            return Err(ExemptPathError::TooMany);
        }
        let mut paths = Self::default();
        for pattern in patterns {
            let pattern = pattern.as_ref().trim();
            if !pattern.starts_with('/') {
                return Err(ExemptPathError::NotAPath(pattern.to_string()));
            }
            match pattern.strip_suffix("/*") {
                Some(prefix) if !prefix.contains('*') => paths.prefixes.push(prefix.to_string()),
                _ if pattern.contains('*') => {
                    return Err(ExemptPathError::Wildcard(pattern.to_string()))
                }
                _ => paths.exact.push(pattern.to_string()),
            }
        }
        Ok(paths)
    }

    /// The stored patterns that parse; a bad one is dropped rather than
    /// exempting more than it says. The API refuses bad ones on the way in.
    pub fn lenient<S: AsRef<str>>(patterns: &[S]) -> Self {
        let mut paths = Self::default();
        for pattern in patterns.iter().take(MAX_EXEMPT_PATHS) {
            if let Ok(one) = Self::parse(std::slice::from_ref(pattern)) {
                paths.exact.extend(one.exact);
                paths.prefixes.extend(one.prefixes);
            }
        }
        paths
    }

    /// Whether `path` (without its query) is exempt. A path with a `.` or
    /// `..` segment, as sent or percent-encoded once or twice, never is:
    /// `/api/../admin` reaches `/admin` once the app normalises it.
    pub fn matches(&self, path: &str) -> bool {
        if has_dot_segment(path) {
            return false;
        }
        self.exact.iter().any(|exact| exact == path)
            || self.prefixes.iter().any(|prefix| {
                path.strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
    }

    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.prefixes.is_empty()
    }
}

/// Whether `path` has a `.` or `..` segment once decoded, counting `\` as
/// a separator the way some servers do.
fn has_dot_segment(path: &str) -> bool {
    let once = percent_decode(path.as_bytes());
    let twice = percent_decode(&once);
    let dotted = |bytes: &[u8]| {
        bytes
            .split(|&b| b == b'/' || b == b'\\')
            .any(|segment| segment == b"." || segment == b"..")
    };
    dotted(path.as_bytes()) || dotted(&once) || dotted(&twice)
}

fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(high << 4 | low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// One environment's Shield settings, as an ingress applies them.
#[derive(Debug, Clone, Default)]
pub struct EnvPolicy {
    pub project_id: i32,
    pub environment_id: i32,
    pub mode: ShieldMode,
    pub exempt_paths: ExemptPaths,
    /// The bypass tokens valid for this environment, its own and its
    /// project's (D7).
    pub bypass: TokenSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_mode_wins_and_attack_mode_still_means_attack() {
        assert_eq!(
            ShieldMode::effective(None, "log", None, false),
            ShieldMode::Log
        );
        assert_eq!(
            ShieldMode::effective(Some("selective"), "off", None, false),
            ShieldMode::Selective
        );
        assert_eq!(
            ShieldMode::effective(None, "selective", None, false),
            ShieldMode::Selective
        );
        assert_eq!(
            ShieldMode::effective(None, "log", None, true),
            ShieldMode::Attack
        );
        assert_eq!(
            ShieldMode::effective(Some("log"), "log", Some(true), false),
            ShieldMode::Attack
        );
        // An environment that turns attack mode off keeps its own mode.
        assert_eq!(
            ShieldMode::effective(Some("selective"), "log", Some(false), true),
            ShieldMode::Selective
        );
        assert_eq!(
            ShieldMode::effective(Some("nonsense"), "nonsense", None, false),
            ShieldMode::Log
        );
        assert!(!ShieldMode::Log.enforces() && ShieldMode::Selective.enforces());
    }

    #[test]
    fn exempt_paths_match_exactly_or_below_a_prefix() {
        let paths = ExemptPaths::parse(&["/api/*", "/healthz", "/hooks/stripe/*"]).unwrap();
        for exempt in [
            "/api",
            "/api/",
            "/api/v1/users",
            "/healthz",
            "/hooks/stripe/x",
        ] {
            assert!(paths.matches(exempt), "{exempt}");
        }
        for challenged in ["/apix", "/", "/healthz/x", "/hooks/stripex", "/v1/api"] {
            assert!(!paths.matches(challenged), "{challenged}");
        }
        for traversal in [
            "/api/../admin",
            "/api/./x",
            "/api/%2e%2e/admin",
            "/api/%2E%2e%2fadmin",
            "/api/%252e%252e/admin",
            "/api/..\\admin",
            "/api/..",
        ] {
            assert!(!paths.matches(traversal), "{traversal}");
        }
        assert!(
            paths.matches("/api/v1.2/file.tar.gz"),
            "dots inside a segment are fine"
        );
    }

    #[test]
    fn exempt_paths_refuse_what_they_would_misread() {
        assert_eq!(
            ExemptPaths::parse(&["api/*"]),
            Err(ExemptPathError::NotAPath("api/*".into()))
        );
        assert_eq!(
            ExemptPaths::parse(&["/a*b"]),
            Err(ExemptPathError::Wildcard("/a*b".into()))
        );
        assert_eq!(
            ExemptPaths::parse(&["/*/x/*"]),
            Err(ExemptPathError::Wildcard("/*/x/*".into()))
        );
        let lenient = ExemptPaths::lenient(&["/ok/*", "bad", "/a*b"]);
        assert!(lenient.matches("/ok/1") && !lenient.matches("/a1b"));
        let many: Vec<String> = (0..=MAX_EXEMPT_PATHS).map(|i| format!("/p{i}")).collect();
        assert_eq!(ExemptPaths::parse(&many), Err(ExemptPathError::TooMany));
    }
}
