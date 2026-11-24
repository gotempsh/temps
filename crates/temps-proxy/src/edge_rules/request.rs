// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The engine's view of a live request.
//!
//! Everything comes from data the proxy already holds for the request; the
//! costly facts (bot class, challenge clearance) are computed only if a rule
//! asks for them, and at most once.

use pingora_http::RequestHeader;
use std::borrow::Cow;
use std::cell::OnceCell;
use std::net::IpAddr;
use temps_edge_rules::{GeoFacts, RequestView, Visit};

/// Owned GeoIP facts, looked up before evaluation when a rule needs them.
#[derive(Debug, Clone, Default)]
pub struct GeoSnapshot {
    pub country: Option<String>,
    pub continent: Option<String>,
    pub asn: Option<u32>,
    pub is_hosting_provider: Option<bool>,
}

impl From<temps_geo::GeoLocation> for GeoSnapshot {
    fn from(location: temps_geo::GeoLocation) -> Self {
        Self {
            country: location.country_code.map(|c| c.to_ascii_uppercase()),
            continent: location.continent_code.map(|c| c.to_ascii_uppercase()),
            asn: location.asn_number,
            is_hosting_provider: location.is_hosting_provider,
        }
    }
}

pub struct ProxyRequestView<'a> {
    header: &'a RequestHeader,
    host: Cow<'a, str>,
    client_ip: Option<IpAddr>,
    user_agent: &'a str,
    ja4: Option<&'a str>,
    geo: Option<&'a GeoSnapshot>,
    environment_id: i32,
    bot_class: OnceCell<&'static str>,
    cleared: OnceCell<bool>,
    clearance: &'a dyn Fn() -> bool,
}

impl<'a> ProxyRequestView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        header: &'a RequestHeader,
        host: &'a str,
        client_ip: Option<IpAddr>,
        user_agent: &'a str,
        ja4: Option<&'a str>,
        geo: Option<&'a GeoSnapshot>,
        environment_id: i32,
        clearance: &'a dyn Fn() -> bool,
    ) -> Self {
        Self {
            header,
            host: normalize_host(host),
            client_ip,
            user_agent,
            ja4,
            geo,
            environment_id,
            bot_class: OnceCell::new(),
            cleared: OnceCell::new(),
            clearance,
        }
    }
}

/// Lower-case, without a port or a trailing dot. Borrowed when already so.
fn normalize_host(host: &str) -> Cow<'_, str> {
    let without_port = if host.starts_with('[') {
        // [v6]:port
        host.find(']').map_or(host, |end| &host[..=end])
    } else {
        host.rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
            .map_or(host, |(name, _)| name)
    };
    let trimmed = without_port.strip_suffix('.').unwrap_or(without_port);
    if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(trimmed.to_ascii_lowercase())
    } else {
        Cow::Borrowed(trimmed)
    }
}

/// Classic search engine crawlers, checked before the AI agent list: some of
/// them also appear there (as search) but are search engines first.
const SEARCH_ENGINES: &[&str] = &[
    "googlebot",
    "bingbot",
    "duckduckbot",
    "yandexbot",
    "baiduspider",
    "applebot",
    "slurp",
    "qwantbot",
    "seznambot",
    "naverbot",
    "yeti/",
    "petalbot",
];

/// The bot classes of the `bot_class` field, from the user agent.
pub fn classify_bot(user_agent: &str) -> &'static str {
    let lower = user_agent.to_ascii_lowercase();
    if SEARCH_ENGINES.iter().any(|token| lower.contains(token)) {
        return "search_engine";
    }
    if let Some(agent) = crate::ai_agent_detector::detect(Some(user_agent)) {
        return match agent.purpose {
            crate::ai_agent_detector::AiAgentPurpose::UserFetch => "ai_agent",
            crate::ai_agent_detector::AiAgentPurpose::Training
            | crate::ai_agent_detector::AiAgentPurpose::Search
            | crate::ai_agent_detector::AiAgentPurpose::Mixed => "ai_crawler",
        };
    }
    if crate::crawler_detector::CrawlerDetector::is_bot(Some(user_agent)) {
        "unknown_bot"
    } else {
        "human"
    }
}

impl RequestView for ProxyRequestView<'_> {
    fn method(&self) -> &str {
        self.header.method.as_str()
    }

    fn host(&self) -> &str {
        &self.host
    }

    fn raw_path(&self) -> &str {
        self.header.uri.path()
    }

    fn raw_query(&self) -> &str {
        self.header.uri.query().unwrap_or("")
    }

    fn visit_header(&self, lower_name: &str, visit: &mut dyn FnMut(&str) -> bool) -> Visit {
        let mut result = Visit::default();
        for value in self.header.headers.get_all(lower_name) {
            result.found = true;
            // A header that is not valid UTF-8 is compared lossily so a rule
            // still sees it.
            let text = String::from_utf8_lossy(value.as_bytes());
            if visit(&text) {
                result.matched = true;
                break;
            }
        }
        result
    }

    fn client_ip(&self) -> Option<IpAddr> {
        self.client_ip
    }

    fn geo(&self) -> GeoFacts<'_> {
        match self.geo {
            Some(geo) => GeoFacts {
                country: geo.country.as_deref(),
                continent: geo.continent.as_deref(),
                asn: geo.asn,
                is_hosting_provider: geo.is_hosting_provider,
            },
            None => GeoFacts::default(),
        }
    }

    fn ja4(&self) -> Option<&str> {
        self.ja4
    }

    fn bot_class(&self) -> Option<&str> {
        Some(self.bot_class.get_or_init(|| classify_bot(self.user_agent)))
    }

    fn environment_id(&self) -> Option<i32> {
        Some(self.environment_id)
    }

    fn challenge_cleared(&self) -> bool {
        *self.cleared.get_or_init(|| (self.clearance)())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_edge_rules::{Decision, Program, RuleDefinition};

    fn header(method: &str, uri: &str, headers: &[(&str, &str)]) -> RequestHeader {
        let mut h = RequestHeader::build(method, uri.as_bytes(), None).expect("request header");
        for (name, value) in headers {
            h.append_header(name.to_string(), *value).expect("header");
        }
        h
    }

    fn program(expression: serde_json::Value, action: serde_json::Value) -> Program {
        let rule = RuleDefinition {
            id: 1,
            revision: 1,
            name: "r".into(),
            enabled: true,
            mode: temps_edge_rules::Mode::Enforce,
            environment_id: None,
            expression: serde_json::from_value(expression).expect("expression"),
            action: serde_json::from_value(action).expect("action"),
        };
        Program::compile(&[rule]).expect("compiles")
    }

    #[test]
    fn hosts_are_lowercased_without_port() {
        assert_eq!(normalize_host("App.Example.TEST:8443"), "app.example.test");
        assert_eq!(normalize_host("app.example.test."), "app.example.test");
        assert_eq!(normalize_host("[2001:db8::1]:443"), "[2001:db8::1]");
        assert!(matches!(normalize_host("plain.test"), Cow::Borrowed(_)));
    }

    #[test]
    fn bots_are_classified_from_the_user_agent() {
        assert_eq!(
            classify_bot("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/130.0 Safari/537.36"),
            "human"
        );
        assert_eq!(
            classify_bot(
                "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
            ),
            "search_engine"
        );
        assert_eq!(
            classify_bot(
                "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.2"
            ),
            "ai_crawler"
        );
        assert_eq!(classify_bot("curl/8.4.0"), "unknown_bot");
    }

    #[test]
    fn a_live_request_is_matched_on_path_query_and_headers() {
        let p = program(
            serde_json::json!({"combinator": "all", "children": [
                {"kind": "condition", "field": "path", "op": "starts_with", "values": ["/admin"], "case_insensitive": true},
                {"kind": "condition", "field": "header", "key": "X-Debug", "op": "exists"},
                {"kind": "condition", "field": "query", "key": "token", "op": "eq", "values": ["a b"]}
            ]}),
            serde_json::json!({"type": "block", "status": 404}),
        );
        let never = || false;
        let h = header("GET", "/ADMIN/users?token=a%20b", &[("x-debug", "1")]);
        let view = ProxyRequestView::new(&h, "App.test", None, "", None, None, 3, &never);
        assert!(matches!(
            p.evaluate(&view).decision,
            Decision::Block { status: 404, .. }
        ));

        let h = header("GET", "/admin/users?token=a%20b", &[]);
        let view = ProxyRequestView::new(&h, "app.test", None, "", None, None, 3, &never);
        assert_eq!(p.evaluate(&view).decision, Decision::Forward);
    }

    #[test]
    fn clearance_is_checked_once_and_only_when_a_challenge_rule_matches() {
        let p = program(
            serde_json::json!({"combinator": "all", "children": [
                {"kind": "condition", "field": "path", "op": "starts_with", "values": ["/login"]}
            ]}),
            serde_json::json!({"type": "challenge"}),
        );
        let calls = std::cell::Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            true
        };
        let h = header("GET", "/home", &[]);
        let view = ProxyRequestView::new(&h, "app.test", None, "", None, None, 3, &check);
        assert_eq!(p.evaluate(&view).decision, Decision::Forward);
        assert_eq!(calls.get(), 0, "no challenge rule matched");

        let h = header("GET", "/login", &[]);
        let view = ProxyRequestView::new(&h, "app.test", None, "", None, None, 3, &check);
        assert_eq!(
            p.evaluate(&view).decision,
            Decision::Forward,
            "cleared visitor passes"
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn geo_facts_come_from_the_lookup() {
        let p = program(
            serde_json::json!({"combinator": "all", "children": [
                {"kind": "condition", "field": "continent", "op": "eq", "values": ["EU"]}
            ]}),
            serde_json::json!({"type": "block", "status": 403}),
        );
        let never = || false;
        let geo = GeoSnapshot {
            country: Some("FR".into()),
            continent: Some("EU".into()),
            ..Default::default()
        };
        let h = header("GET", "/", &[]);
        let view = ProxyRequestView::new(&h, "app.test", None, "", None, Some(&geo), 3, &never);
        assert!(matches!(p.evaluate(&view).decision, Decision::Block { .. }));
        let view = ProxyRequestView::new(&h, "app.test", None, "", None, None, 3, &never);
        assert_eq!(
            p.evaluate(&view).decision,
            Decision::Forward,
            "no GeoIP, no match"
        );
    }
}
