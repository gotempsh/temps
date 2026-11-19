// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Precompiled inbound filter rules (ADR 074, D1.4) and how one event is
//! matched against them.
//!
//! Matching runs on the ingest path for every Sentry error event of a project
//! that has rules, so it allocates at most once per event and rule kind, and
//! every input it reads is cut to a fixed length first.

use serde::{Deserialize, Serialize};

use super::super::types::CreateErrorEventData;

/// Longest text a glob is matched against. Messages and releases are
/// SDK-controlled; the cut bounds the work per event.
pub const MAX_MATCHED_CHARS: usize = 1024;
/// Longest glob pattern a rule may hold.
pub const MAX_PATTERN_CHARS: usize = 200;
/// Stack frames inspected per exception for the browser-extension rule.
const MAX_FRAMES_INSPECTED: usize = 64;

/// What a rule drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InboundFilterKind {
    /// Errors raised inside a browser extension (a frame from
    /// `chrome-extension://`, `moz-extension://`, `safari-extension://`, ...).
    BrowserExtensions,
    /// Errors from a request whose user agent is a known crawler or bot.
    WebCrawlers,
    /// Errors from a page served on localhost, or from a loopback address.
    Localhost,
    /// Errors whose `Type: message` matches a glob pattern.
    ErrorMessage,
    /// Errors whose release matches a glob pattern.
    Release,
}

impl InboundFilterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BrowserExtensions => "browser_extensions",
            Self::WebCrawlers => "web_crawlers",
            Self::Localhost => "localhost",
            Self::ErrorMessage => "error_message",
            Self::Release => "release",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "browser_extensions" => Self::BrowserExtensions,
            "web_crawlers" => Self::WebCrawlers,
            "localhost" => Self::Localhost,
            "error_message" => Self::ErrorMessage,
            "release" => Self::Release,
            _ => return None,
        })
    }

    /// Whether the kind needs a glob pattern.
    pub fn takes_pattern(self) -> bool {
        matches!(self, Self::ErrorMessage | Self::Release)
    }
}

const EXTENSION_SCHEMES: [&str; 6] = [
    "chrome-extension://",
    "moz-extension://",
    "safari-extension://",
    "safari-web-extension://",
    "ms-browser-extension://",
    "edge-extension://",
];

/// User-agent tokens of crawlers, link previewers and monitoring bots,
/// lower case. A substring match on the lower-cased user agent.
const CRAWLER_TOKENS: [&str; 30] = [
    "googlebot",
    "adsbot-google",
    "mediapartners-google",
    "bingbot",
    "bingpreview",
    "slurp",
    "duckduckbot",
    "baiduspider",
    "yandexbot",
    "yandex.com/bots",
    "sogou",
    "exabot",
    "facebot",
    "facebookexternalhit",
    "ia_archiver",
    "applebot",
    "petalbot",
    "semrushbot",
    "ahrefsbot",
    "mj12bot",
    "dotbot",
    "bytespider",
    "gptbot",
    "claudebot",
    "ccbot",
    "amazonbot",
    "twitterbot",
    "linkedinbot",
    "pingdom",
    "uptimerobot",
];

/// A glob over lower-cased text: `*` matches any run of characters, `?`
/// exactly one. Everything else is literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    pattern: Vec<char>,
}

impl Glob {
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_lowercase().chars().collect(),
        }
    }

    /// Whether the glob matches the whole of `text`, which the caller has
    /// lower-cased. Iterative with one backtrack point, so it is linear in
    /// practice and O(text × pattern) at worst, both bounded.
    pub fn matches(&self, text: &str) -> bool {
        let text: Vec<char> = text.chars().take(MAX_MATCHED_CHARS).collect();
        let pattern = &self.pattern;
        let (mut t, mut p) = (0usize, 0usize);
        let mut star: Option<(usize, usize)> = None;
        while t < text.len() {
            if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
                t += 1;
                p += 1;
            } else if p < pattern.len() && pattern[p] == '*' {
                star = Some((p, t));
                p += 1;
            } else if let Some((star_p, star_t)) = star {
                p = star_p + 1;
                t = star_t + 1;
                star = Some((star_p, star_t + 1));
            } else {
                return false;
            }
        }
        pattern[p..].iter().all(|c| *c == '*')
    }
}

/// A rule ready to evaluate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    BrowserExtensions,
    WebCrawlers,
    Localhost,
    ErrorMessage(Glob),
    Release(Glob),
}

impl Matcher {
    /// Compile a stored rule; `None` when a pattern kind has no usable
    /// pattern (such a row cannot be written through the API).
    pub fn compile(kind: InboundFilterKind, pattern: Option<&str>) -> Option<Self> {
        let glob = || {
            pattern
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(Glob::new)
        };
        Some(match kind {
            InboundFilterKind::BrowserExtensions => Self::BrowserExtensions,
            InboundFilterKind::WebCrawlers => Self::WebCrawlers,
            InboundFilterKind::Localhost => Self::Localhost,
            InboundFilterKind::ErrorMessage => Self::ErrorMessage(glob()?),
            InboundFilterKind::Release => Self::Release(glob()?),
        })
    }

    pub fn matches(&self, event: &CreateErrorEventData) -> bool {
        match self {
            Self::BrowserExtensions => from_browser_extension(event),
            Self::WebCrawlers => event
                .user_agent
                .as_deref()
                .map(|ua| {
                    let ua = truncated_lowercase(ua);
                    CRAWLER_TOKENS.iter().any(|token| ua.contains(token))
                })
                .unwrap_or(false),
            Self::Localhost => from_localhost(event),
            Self::ErrorMessage(glob) => {
                error_messages(event).any(|message| glob.matches(&truncated_lowercase(&message)))
            }
            Self::Release(glob) => event
                .release_version
                .as_deref()
                .map(|release| glob.matches(&truncated_lowercase(release)))
                .unwrap_or(false),
        }
    }
}

fn truncated_lowercase(text: &str) -> String {
    text.chars()
        .take(MAX_MATCHED_CHARS)
        .flat_map(char::to_lowercase)
        .collect()
}

/// `Type: message` of every exception, then of the legacy top-level fields,
/// as Sentry's error-message filter reads them. A type alone, or a message
/// alone, is matched as it is.
fn error_messages(event: &CreateErrorEventData) -> impl Iterator<Item = String> + '_ {
    let exceptions = event.exceptions.iter().map(|e| {
        (
            Some(e.exception_type.as_str()),
            e.exception_value.as_deref(),
        )
    });
    let legacy = std::iter::once((
        event.exception_type.as_deref(),
        event.exception_value.as_deref(),
    ));
    exceptions.chain(legacy).filter_map(|(kind, value)| {
        match (kind.filter(|k| !k.is_empty()), value) {
            (Some(kind), Some(value)) => Some(format!("{kind}: {value}")),
            (Some(kind), None) => Some(kind.to_string()),
            (None, Some(value)) => Some(value.to_string()),
            (None, None) => None,
        }
    })
}

fn from_browser_extension(event: &CreateErrorEventData) -> bool {
    let stacks = event
        .exceptions
        .iter()
        .filter_map(|e| e.stack_trace.as_ref())
        .chain(event.stack_trace.as_ref());
    for stack in stacks {
        let frames = stack
            .as_array()
            .or_else(|| stack.get("frames").and_then(|f| f.as_array()));
        let Some(frames) = frames else { continue };
        // The innermost frames are last in Sentry's order.
        for frame in frames.iter().rev().take(MAX_FRAMES_INSPECTED) {
            let file = frame
                .get("abs_path")
                .and_then(|v| v.as_str())
                .or_else(|| frame.get("filename").and_then(|v| v.as_str()));
            if file.is_some_and(|file| {
                let head: String = file.chars().take(32).collect::<String>().to_lowercase();
                EXTENSION_SCHEMES
                    .iter()
                    .any(|scheme| head.starts_with(scheme))
            }) {
                return true;
            }
        }
    }
    false
}

fn from_localhost(event: &CreateErrorEventData) -> bool {
    let loopback_ip = event
        .user_ip_address
        .as_deref()
        .and_then(|ip| ip.trim().parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    loopback_ip || event.url.as_deref().is_some_and(url_is_localhost)
}

fn url_is_localhost(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "0.0.0.0"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::types::ExceptionData;

    fn event() -> CreateErrorEventData {
        CreateErrorEventData::default()
    }

    #[test]
    fn globs_match_whole_text_with_star_and_question_mark() {
        let glob = Glob::new("*ResizeObserver loop*");
        assert!(glob.matches("error: resizeobserver loop limit exceeded"));
        assert!(!glob.matches("typeerror: x is undefined"));
        assert!(Glob::new("1.?.*").matches("1.2.0-beta"));
        assert!(!Glob::new("1.?.*").matches("10.2.0"));
        assert!(Glob::new("*").matches(""));
        assert!(Glob::new("exact").matches("exact"));
        assert!(!Glob::new("exact").matches("exactly"));
        // A pathological pattern stays bounded by the cut text.
        let long = "a".repeat(10 * MAX_MATCHED_CHARS);
        assert!(!Glob::new("*a*a*a*a*a*b").matches(&long));
    }

    #[test]
    fn error_message_rule_reads_type_and_message() {
        let rule = Matcher::compile(InboundFilterKind::ErrorMessage, Some("TypeError: *null*"))
            .expect("compiles");
        let mut data = event();
        data.exceptions.push(ExceptionData {
            exception_type: "TypeError".into(),
            exception_value: Some("Cannot read properties of null".into()),
            stack_trace: None,
            mechanism: None,
            module: None,
            thread_id: None,
        });
        assert!(rule.matches(&data));
        data.exceptions[0].exception_type = "RangeError".into();
        assert!(!rule.matches(&data));
    }

    #[test]
    fn release_rule_matches_the_event_release() {
        let rule = Matcher::compile(InboundFilterKind::Release, Some("1.0.*")).expect("compiles");
        let mut data = event();
        assert!(!rule.matches(&data), "no release, no match");
        data.release_version = Some("1.0.7".into());
        assert!(rule.matches(&data));
        data.release_version = Some("2.0.0".into());
        assert!(!rule.matches(&data));
        assert!(Matcher::compile(InboundFilterKind::Release, Some("  ")).is_none());
    }

    #[test]
    fn browser_extension_frames_are_recognised() {
        let mut data = event();
        data.stack_trace = Some(serde_json::json!({ "frames": [
            { "filename": "https://app.example.test/main.js" },
            { "abs_path": "chrome-extension://abcdefghijklmnop/content.js" }
        ]}));
        assert!(Matcher::BrowserExtensions.matches(&data));
        data.stack_trace =
            Some(serde_json::json!([{ "filename": "https://app.example.test/x.js" }]));
        assert!(!Matcher::BrowserExtensions.matches(&data));
    }

    #[test]
    fn crawlers_and_localhost_are_recognised() {
        let mut data = event();
        data.user_agent = Some("Mozilla/5.0 (compatible; Googlebot/2.1)".into());
        assert!(Matcher::WebCrawlers.matches(&data));
        data.user_agent = Some("Mozilla/5.0 (Macintosh) Safari/605.1.15".into());
        assert!(!Matcher::WebCrawlers.matches(&data));

        for url in [
            "http://localhost:3000/checkout",
            "https://127.0.0.1/",
            "http://[::1]:8080/x",
            "http://app.localhost/",
        ] {
            data.url = Some(url.into());
            assert!(Matcher::Localhost.matches(&data), "{url}");
        }
        data.url = Some("https://shop.example.test/localhost".into());
        assert!(!Matcher::Localhost.matches(&data));
        data.user_ip_address = Some("127.0.0.1".into());
        assert!(Matcher::Localhost.matches(&data));
    }
}
