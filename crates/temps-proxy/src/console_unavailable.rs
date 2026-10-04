// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Status page served in place of the console when it is not running.
//!
//! In `temps serve` the proxy and the console share a process, but the proxy
//! never waits for the console, so applications keep serving when the
//! console fails to start. Requests the proxy would forward to the console
//! (the console host, unknown hosts falling through, `/api/_temps/*`) then
//! hit a closed port. Instead of the generic "Service Unavailable" page this
//! module answers with what actually happened:
//!
//! - **Failed**: the console's startup failure (which check, the error, how
//!   to fix it), as HTML for browsers or an RFC 7807 problem for API clients.
//! - **Starting**: a short "still starting" page that refreshes itself,
//!   instead of telling the operator their *application* is down.
//!
//! The details are only shown to clients the operator already trusts with
//! the management surface: loopback, or an address inside a configured admin
//! IP allowlist. Everyone else gets a generic page that says the console is
//! unavailable and where the operator can find the cause, without leaking
//! paths, socket names or error text to the internet.
//!
//! Hot-path cost: requests routed to an application never reach this code.
//! For console-bound requests the check is an atomic phase load. The four
//! failure bodies are rendered once, on first use, and then served as shared
//! `Bytes` — a request flood against a broken console does no per-request
//! rendering or serialization.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use pingora_core::upstreams::peer::{HttpPeer, Peer};
use serde::Serialize;
use temps_core::admin_gate::AdminGateConfig;
use temps_core::console_startup::{ConsolePhase, ConsoleStartupFailure, ConsoleStartupState};

/// Problem `type` URI for a console that failed to start.
pub const CONSOLE_STARTUP_FAILED_TYPE: &str = "https://temps.sh/probs/console-startup-failed";
/// Problem `type` URI for a console that is still starting.
pub const CONSOLE_STARTING_TYPE: &str = "https://temps.sh/probs/console-starting";

/// How long a client should wait before retrying a still-starting console.
pub const CONSOLE_STARTING_RETRY_AFTER_SECS: u64 = 5;

/// Where the operator finds the full error and how to re-check.
const LOG_HINT: &str = "Search the output of `temps serve` (the terminal, your service \
                        manager's journal, or the container log) for \"Console API failed to \
                        start\", then run `temps doctor` on the server.";

/// Representation chosen for a console-bound request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    Html,
    Json,
}

impl ResponseFormat {
    /// API paths and clients that do not accept HTML get a problem document;
    /// browsers (and `Accept: */*`) get the HTML page.
    pub fn for_request(path: &str, accept: Option<&str>) -> Self {
        if path.starts_with("/api/") || path == "/api" {
            return Self::Json;
        }
        match accept {
            Some(accept) => {
                let accept = accept.to_ascii_lowercase();
                let wants_html = accept.contains("text/html") || accept.contains("*/*");
                let wants_json = accept.contains("json");
                if wants_json && !accept.contains("text/html") {
                    Self::Json
                } else if wants_html {
                    Self::Html
                } else {
                    Self::Json
                }
            }
            None => Self::Html,
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Json => "application/problem+json",
        }
    }
}

/// Whether this client may see the startup error itself.
///
/// Trusted: a loopback client (the operator on the server, or a same-host
/// reverse proxy whose forwarded address the proxy already resolved into
/// `client_ip`), or an address inside a non-empty admin IP allowlist that
/// passes the admin gate. A host-only allowlist is not enough: the `Host`
/// header is client-controlled. An unknown client IP is never trusted.
pub fn client_may_see_details(
    client_ip: Option<IpAddr>,
    host: &str,
    gate: Option<&AdminGateConfig>,
) -> bool {
    let Some(ip) = client_ip else {
        return false;
    };
    if is_loopback(ip) {
        return true;
    }
    gate.is_some_and(|gate| !gate.allowed_nets.is_empty() && gate.would_allow(ip, Some(host)))
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Bodies for a failed console, rendered once.
struct FailurePages {
    detailed_html: Bytes,
    generic_html: Bytes,
    detailed_json: Bytes,
    generic_json: Bytes,
}

/// Decides when a console-bound request gets a status page, and renders it.
pub struct ConsoleUnavailableResponder {
    state: Arc<ConsoleStartupState>,
    console_address: String,
    /// `console_address` resolved the same way `HttpPeer::new` resolves it,
    /// so a hostname-form address (`localhost:8081`) still matches the peer.
    console_socket: Option<SocketAddr>,
    failure_pages: OnceLock<FailurePages>,
}

impl ConsoleUnavailableResponder {
    pub fn new(state: Arc<ConsoleStartupState>, console_address: &str) -> Self {
        let console_socket = console_address
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next());
        Self {
            state,
            console_address: console_address.to_string(),
            console_socket,
            failure_pages: OnceLock::new(),
        }
    }

    /// Current console phase. One atomic load.
    pub fn phase(&self) -> ConsolePhase {
        self.state.phase()
    }

    /// The recorded failure, when the console failed to start.
    pub fn failure(&self) -> Option<&ConsoleStartupFailure> {
        match self.state.phase() {
            ConsolePhase::Failed => self.state.failure(),
            ConsolePhase::Starting | ConsolePhase::Running => None,
        }
    }

    /// Whether `peer` is this process's console listener.
    pub fn is_console_peer(&self, peer: &HttpPeer) -> bool {
        match (peer.address().as_inet(), self.console_socket) {
            (Some(peer_addr), Some(console)) if *peer_addr == console => true,
            _ => self.is_console_address(&peer.address().to_string()),
        }
    }

    /// Whether `address` (as recorded in `ctx.upstream_host`) is the console.
    pub fn is_console_address(&self, address: &str) -> bool {
        if address.is_empty() {
            return false;
        }
        if address == self.console_address {
            return true;
        }
        match (address.parse::<SocketAddr>(), self.console_socket) {
            (Ok(addr), Some(console)) => addr == console,
            _ => false,
        }
    }

    /// Body for a console that failed to start, or `None` when it has not.
    pub fn failure_body(&self, detailed: bool, format: ResponseFormat) -> Option<Bytes> {
        let failure = self.failure()?;
        let pages = self.failure_pages.get_or_init(|| FailurePages {
            detailed_html: Bytes::from(render_failure_html(Some(failure))),
            generic_html: Bytes::from(render_failure_html(None)),
            detailed_json: Bytes::from(render_failure_json(Some(failure))),
            generic_json: Bytes::from(render_failure_json(None)),
        });
        Some(match (detailed, format) {
            (true, ResponseFormat::Html) => pages.detailed_html.clone(),
            (false, ResponseFormat::Html) => pages.generic_html.clone(),
            (true, ResponseFormat::Json) => pages.detailed_json.clone(),
            (false, ResponseFormat::Json) => pages.generic_json.clone(),
        })
    }

    /// Body for a console that is still starting.
    pub fn starting_body(format: ResponseFormat) -> Bytes {
        static HTML: OnceLock<Bytes> = OnceLock::new();
        static JSON: OnceLock<Bytes> = OnceLock::new();
        match format {
            ResponseFormat::Html => HTML
                .get_or_init(|| Bytes::from(render_starting_html()))
                .clone(),
            ResponseFormat::Json => JSON
                .get_or_init(|| Bytes::from(render_starting_json()))
                .clone(),
        }
    }
}

/// RFC 7807 body for an unavailable console.
#[derive(Debug, Serialize)]
struct ConsoleUnavailableProblem<'a> {
    #[serde(rename = "type")]
    problem_type: &'static str,
    title: &'static str,
    status: u16,
    detail: &'a str,
    /// Stable identifier of the failed check (`docker`, `initial_admin`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remediation: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_at: Option<String>,
    hint: &'static str,
}

fn render_failure_json(failure: Option<&ConsoleStartupFailure>) -> String {
    let generic_detail = "The Temps console failed to start. Details are only shown to \
                          requests from the server itself or from an admin-allowed IP address.";
    let problem = match failure {
        Some(failure) => ConsoleUnavailableProblem {
            problem_type: CONSOLE_STARTUP_FAILED_TYPE,
            title: "Console failed to start",
            status: 503,
            detail: &failure.detail,
            check: Some(failure.check.code()),
            summary: Some(&failure.summary),
            remediation: Some(&failure.remediation),
            failed_at: Some(
                failure
                    .failed_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
            hint: LOG_HINT,
        },
        None => ConsoleUnavailableProblem {
            problem_type: CONSOLE_STARTUP_FAILED_TYPE,
            title: "Console failed to start",
            status: 503,
            detail: generic_detail,
            check: None,
            summary: None,
            remediation: None,
            failed_at: None,
            hint: LOG_HINT,
        },
    };
    // Serializing a struct of strings cannot fail; fall back to a fixed
    // document rather than panicking if it ever does.
    serde_json::to_string(&problem).unwrap_or_else(|_| {
        format!(
            r#"{{"type":"{CONSOLE_STARTUP_FAILED_TYPE}","title":"Console failed to start","status":503}}"#
        )
    })
}

fn render_starting_json() -> String {
    let problem = ConsoleUnavailableProblem {
        problem_type: CONSOLE_STARTING_TYPE,
        title: "Console is starting",
        status: 503,
        detail: "The Temps console is still starting. Retry in a few seconds.",
        check: None,
        summary: None,
        remediation: None,
        failed_at: None,
        hint: "If this persists for more than a few minutes, check the output of `temps serve`.",
    };
    serde_json::to_string(&problem).unwrap_or_else(|_| {
        format!(
            r#"{{"type":"{CONSOLE_STARTING_TYPE}","title":"Console is starting","status":503}}"#
        )
    })
}

/// HTML-escape text for element content and double-quoted attributes.
fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_failure_html(failure: Option<&ConsoleStartupFailure>) -> String {
    let body = match failure {
        Some(failure) => {
            let steps: String = failure
                .remediation
                .iter()
                .map(|step| format!("<li>{}</li>", escape(step)))
                .collect();
            let fix = if steps.is_empty() {
                String::new()
            } else {
                format!("<h2>How to fix it</h2><ol>{steps}</ol>")
            };
            format!(
                r#"<div class="chip">CONSOLE_STARTUP_FAILED · 503</div>
  <h1>The Temps console failed to start</h1>
  <p class="lede">{summary}. Deployed applications are still being served; only the management console is down.</p>
  <dl class="meta">
    <dt>Failed check</dt><dd>{check}</dd>
    <dt>Failed at</dt><dd>{failed_at}</dd>
  </dl>
  <h2>Error</h2>
  <pre>{detail}</pre>
  {fix}
  <p class="note">After fixing it, restart <code>temps serve</code>. {log_hint}</p>
  <p class="note">You see these details because this request comes from the server itself or from an admin-allowed IP address.</p>"#,
                summary = escape(failure.summary.trim_end_matches('.')),
                check = escape(failure.check.label()),
                failed_at = escape(
                    &failure
                        .failed_at
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ),
                detail = escape(&failure.detail),
                log_hint = escape(LOG_HINT),
            )
        }
        None => format!(
            r#"<div class="chip">CONSOLE_UNAVAILABLE · 503</div>
  <h1>The Temps console is unavailable</h1>
  <p class="lede">The management console failed to start. Deployed applications are not affected.</p>
  <p class="note">If you operate this server: open this page from the server itself (for example <code>curl -H 'Accept: application/json' http://127.0.0.1/</code> on the host) or from an admin-allowed IP address to see the cause. {log_hint}</p>"#,
            log_hint = escape(LOG_HINT),
        ),
    };
    page("Console failed to start", "", &body)
}

fn render_starting_html() -> String {
    let body = format!(
        r#"<div class="chip">CONSOLE_STARTING · 503</div>
  <h1>The Temps console is starting</h1>
  <p class="lede">Startup checks are still running (Docker, database, plugins). This page reloads every {CONSOLE_STARTING_RETRY_AFTER_SECS} seconds.</p>
  <p class="note">If it does not come up within a few minutes, check the output of <code>temps serve</code>.</p>"#
    );
    page(
        "Console is starting",
        &format!(r#"<meta http-equiv="refresh" content="{CONSOLE_STARTING_RETRY_AFTER_SECS}">"#),
        &body,
    )
}

/// Self-contained page shell: no external assets, dark/light aware, same
/// visual language as the branded 404.
fn page(title: &str, extra_head: &str, body: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow">
{extra_head}
<title>{title} · Temps</title>
<style>
  :root {{ color-scheme: dark light; --bg:#09090b; --fg:#fafafa; --muted:#a1a1aa; --line:#27272a; --chip-bg:#18181b; --accent:#fafafa; }}
  @media (prefers-color-scheme: light) {{ :root {{ --bg:#fafafa; --fg:#09090b; --muted:#52525b; --line:#e4e4e7; --chip-bg:#f4f4f5; --accent:#09090b; }} }}
  * {{ box-sizing: border-box; }}
  body {{ margin:0; min-height:100vh; background:var(--bg); color:var(--fg); font:15px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, Arial, sans-serif; display:grid; place-items:center; padding:24px; }}
  main {{ width:100%; max-width:720px; }}
  .mark {{ display:inline-flex; align-items:center; justify-content:center; width:48px; height:48px; border-radius:11px; background:var(--accent); color:var(--bg); font-weight:900; font-size:30px; line-height:1; margin-bottom:20px; user-select:none; }}
  .chip {{ display:inline-block; padding:4px 10px; border:1px solid var(--line); border-radius:999px; background:var(--chip-bg); color:var(--muted); font:12px/1 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; letter-spacing:.04em; margin-bottom:16px; }}
  h1 {{ margin:0 0 8px; font-size:22px; font-weight:600; }}
  h2 {{ margin:24px 0 8px; font-size:15px; font-weight:600; }}
  p.lede {{ margin:0 0 20px; color:var(--muted); }}
  p.note {{ color:var(--muted); font-size:13px; }}
  dl.meta {{ margin:0 0 8px; padding:12px 14px; border:1px solid var(--line); border-radius:10px; background:var(--chip-bg); display:grid; grid-template-columns:auto 1fr; gap:4px 16px; font:12.5px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}
  dl.meta dt {{ color:var(--muted); }} dl.meta dd {{ margin:0; overflow-wrap:anywhere; }}
  pre {{ margin:0; padding:12px 14px; border:1px solid var(--line); border-radius:10px; background:var(--chip-bg); white-space:pre-wrap; overflow-wrap:anywhere; font:12.5px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}
  ol {{ margin:0; padding-left:20px; }} li {{ margin:4px 0; }}
  code {{ font:12.5px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; background:var(--chip-bg); border:1px solid var(--line); border-radius:4px; padding:0 4px; }}
</style>
</head>
<body>
<main>
  <div class="mark" aria-hidden="true">t</div><br>
  {body}
</main>
</body>
</html>
"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::admin_gate::AdminGateSource;
    use temps_core::console_startup::ConsoleStartupCheck;

    fn docker_failure() -> ConsoleStartupFailure {
        ConsoleStartupFailure::new(
            ConsoleStartupCheck::Docker,
            "Docker is not reachable at unix:///tmp/missing.sock (from DOCKER_HOST)",
            "Socket not found: /tmp/missing.sock <script>alert(1)</script>",
            vec!["Start the Docker daemon".to_string()],
        )
    }

    fn failed_responder() -> ConsoleUnavailableResponder {
        let state = Arc::new(ConsoleStartupState::new());
        state.record_failure(docker_failure());
        ConsoleUnavailableResponder::new(state, "127.0.0.1:8081")
    }

    fn gate(ips: &[&str], hosts: &[&str]) -> AdminGateConfig {
        let ips: Vec<String> = ips.iter().map(|s| s.to_string()).collect();
        let hosts: Vec<String> = hosts.iter().map(|s| s.to_string()).collect();
        AdminGateConfig::from_parts(&ips, &hosts, false, AdminGateSource::Db)
            .expect("valid gate config")
    }

    #[test]
    fn api_paths_and_json_clients_get_problem_json() {
        assert_eq!(
            ResponseFormat::for_request("/api/projects", Some("text/html")),
            ResponseFormat::Json
        );
        assert_eq!(
            ResponseFormat::for_request("/", Some("application/json")),
            ResponseFormat::Json
        );
        assert_eq!(
            ResponseFormat::for_request("/", Some("text/plain")),
            ResponseFormat::Json
        );
    }

    #[test]
    fn browsers_and_wildcard_clients_get_html() {
        assert_eq!(
            ResponseFormat::for_request(
                "/",
                Some("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            ),
            ResponseFormat::Html
        );
        assert_eq!(
            ResponseFormat::for_request("/projects", Some("*/*")),
            ResponseFormat::Html
        );
        assert_eq!(ResponseFormat::for_request("/", None), ResponseFormat::Html);
    }

    #[test]
    fn loopback_clients_see_details() {
        assert!(client_may_see_details(
            Some("127.0.0.1".parse().unwrap()),
            "temps.example.test",
            None
        ));
        assert!(client_may_see_details(
            Some("::1".parse().unwrap()),
            "temps.example.test",
            None
        ));
        assert!(client_may_see_details(
            Some("::ffff:127.0.0.1".parse().unwrap()),
            "temps.example.test",
            None
        ));
    }

    #[test]
    fn remote_clients_without_an_ip_allowlist_get_the_generic_page() {
        let remote = Some("203.0.113.7".parse().unwrap());
        assert!(!client_may_see_details(remote, "temps.example.test", None));
        // An open (noop) gate allows everyone, which is not trust.
        assert!(!client_may_see_details(
            remote,
            "temps.example.test",
            Some(&gate(&[], &[]))
        ));
        // A host-only allowlist is client-controlled, so not trust either.
        assert!(!client_may_see_details(
            remote,
            "temps.example.test",
            Some(&gate(&[], &["temps.example.test"]))
        ));
        assert!(!client_may_see_details(None, "temps.example.test", None));
    }

    #[test]
    fn admin_allowlisted_clients_see_details() {
        let gate = gate(&["203.0.113.0/24"], &[]);
        assert!(client_may_see_details(
            Some("203.0.113.7".parse().unwrap()),
            "temps.example.test",
            Some(&gate)
        ));
        assert!(!client_may_see_details(
            Some("198.51.100.1".parse().unwrap()),
            "temps.example.test",
            Some(&gate)
        ));
    }

    #[test]
    fn only_the_console_peer_matches() {
        let responder = failed_responder();
        assert!(responder.is_console_peer(&HttpPeer::new("127.0.0.1:8081", false, String::new())));
        assert!(!responder.is_console_peer(&HttpPeer::new("127.0.0.1:9000", false, String::new())));
        assert!(responder.is_console_address("127.0.0.1:8081"));
        assert!(!responder.is_console_address("10.0.0.5:8081"));
        assert!(!responder.is_console_address(""));
    }

    #[test]
    fn hostname_console_address_matches_resolved_peer() {
        let state = Arc::new(ConsoleStartupState::new());
        let responder = ConsoleUnavailableResponder::new(state, "localhost:8081");
        let peer = HttpPeer::new("localhost:8081", false, String::new());
        assert!(responder.is_console_peer(&peer));
    }

    #[test]
    fn no_failure_body_until_startup_fails() {
        let state = Arc::new(ConsoleStartupState::new());
        let responder = ConsoleUnavailableResponder::new(state.clone(), "127.0.0.1:8081");
        assert!(responder.failure_body(true, ResponseFormat::Html).is_none());
        state.mark_running();
        assert!(responder.failure_body(true, ResponseFormat::Html).is_none());
    }

    #[test]
    fn detailed_html_names_the_cause_and_escapes_it() {
        let body = failed_responder()
            .failure_body(true, ResponseFormat::Html)
            .expect("failed console renders a body");
        let html = std::str::from_utf8(&body).expect("utf-8");
        assert!(html.contains("The Temps console failed to start"));
        assert!(html.contains("Docker is not reachable at unix:///tmp/missing.sock"));
        assert!(html.contains("Start the Docker daemon"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("Console API failed to start"));
    }

    #[test]
    fn generic_html_hides_the_cause() {
        let body = failed_responder()
            .failure_body(false, ResponseFormat::Html)
            .expect("failed console renders a body");
        let html = std::str::from_utf8(&body).expect("utf-8");
        assert!(html.contains("The Temps console is unavailable"));
        assert!(!html.contains("missing.sock"));
        assert!(!html.contains("Docker is not reachable"));
        assert!(html.contains("temps doctor"));
    }

    #[test]
    fn detailed_json_is_a_problem_document_with_the_check() {
        let body = failed_responder()
            .failure_body(true, ResponseFormat::Json)
            .expect("failed console renders a body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(value["type"], CONSOLE_STARTUP_FAILED_TYPE);
        assert_eq!(value["status"], 503);
        assert_eq!(value["check"], "docker");
        assert_eq!(value["remediation"][0], "Start the Docker daemon");
        assert!(value["failed_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z')));
    }

    #[test]
    fn generic_json_omits_the_cause() {
        let body = failed_responder()
            .failure_body(false, ResponseFormat::Json)
            .expect("failed console renders a body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(value["type"], CONSOLE_STARTUP_FAILED_TYPE);
        assert!(value.get("check").is_none());
        assert!(value.get("remediation").is_none());
        assert!(!String::from_utf8_lossy(&body).contains("missing.sock"));
    }

    #[test]
    fn starting_pages_refresh_and_identify_themselves() {
        let html = ConsoleUnavailableResponder::starting_body(ResponseFormat::Html);
        let html = std::str::from_utf8(&html).expect("utf-8");
        assert!(html.contains("http-equiv=\"refresh\""));
        assert!(html.contains("The Temps console is starting"));

        let json = ConsoleUnavailableResponder::starting_body(ResponseFormat::Json);
        let value: serde_json::Value = serde_json::from_slice(&json).expect("valid JSON");
        assert_eq!(value["type"], CONSOLE_STARTING_TYPE);
    }
}
