// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTPS only, on standard ports, for an address published through a
//! Cloudflare tunnel (ADR 070).
//!
//! Cloudflare's edge answers a proxied hostname on plain HTTP and on its
//! alternate ports (8080, 8443, …). A run adds two rules, each scoped to the
//! one hostname, never the zone:
//!
//! - **HTTPS redirect** (Single Redirects, `http_request_dynamic_redirect`):
//!   `(http.host eq "<host>" and not ssl)` → 308 to `https://`.
//! - **Standard ports only** (WAF custom rules, `http_request_firewall_custom`):
//!   `(http.host eq "<host>" and not cf.edge.server_port in {80 443})` → block.
//!
//! Rules are only ever added: to the phase's entry point with
//! `POST …/rulesets/{id}/rules`, or, when the zone has none, by creating the
//! entry point with `POST …/rulesets`, which refuses rather than overwrites
//! one another tool created meanwhile. `PUT`, which replaces every rule, is
//! never used. A rule already there that does the same ([`same_rule`]) is
//! used as found and never journaled. Each rule Temps adds carries a `ref`
//! and a description naming this install and run, which is how a pending
//! one is found again.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Single Redirects' phase.
pub const REDIRECT_PHASE: &str = "http_request_dynamic_redirect";
/// WAF custom rules' phase.
pub const CUSTOM_RULES_PHASE: &str = "http_request_firewall_custom";

/// Which of the two rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRulePurpose {
    /// Plain HTTP redirects to HTTPS.
    HttpsRedirect,
    /// Any port other than 80 and 443 is refused.
    StandardPorts,
}

impl EdgeRulePurpose {
    pub fn phase(self) -> &'static str {
        match self {
            EdgeRulePurpose::HttpsRedirect => REDIRECT_PHASE,
            EdgeRulePurpose::StandardPorts => CUSTOM_RULES_PHASE,
        }
    }

    /// The Cloudflare token permission the rule needs.
    pub fn permission(self) -> &'static str {
        match self {
            EdgeRulePurpose::HttpsRedirect => "Zone · Single Redirect · Edit",
            EdgeRulePurpose::StandardPorts => "Zone · Zone WAF · Edit",
        }
    }

    pub(crate) fn action(self) -> &'static str {
        match self {
            EdgeRulePurpose::HttpsRedirect => "redirect",
            EdgeRulePurpose::StandardPorts => "block",
        }
    }

    pub(crate) fn short(self) -> &'static str {
        match self {
            EdgeRulePurpose::HttpsRedirect => "https",
            EdgeRulePurpose::StandardPorts => "ports",
        }
    }

    /// The rule, in a few words.
    pub fn describe(self, host: &str) -> String {
        match self {
            EdgeRulePurpose::HttpsRedirect => {
                format!("Cloudflare redirect rule sending http://{host} to HTTPS")
            }
            EdgeRulePurpose::StandardPorts => {
                format!("Cloudflare WAF rule refusing {host} on ports other than 80 and 443")
            }
        }
    }

    /// What stays exposed without the rule.
    pub fn exposure(self, host: &str) -> String {
        match self {
            EdgeRulePurpose::HttpsRedirect => format!(
                "http://{host} still answers without encryption between visitors and Cloudflare"
            ),
            EdgeRulePurpose::StandardPorts => {
                format!("{host} still answers on Cloudflare's other ports (such as 8080 and 8443)")
            }
        }
    }
}

/// What a tunnel address answers at Cloudflare's edge, as the user chose
/// it (ADR 070 D1). Stored on the address (`project_custom_domains.
/// http_policy`); `None` there means never chosen, and nothing is changed
/// for such an address until a run records a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HttpPolicy {
    /// HTTPS only, on 443: plain HTTP redirects, other ports are refused.
    HttpsOnly,
    /// Plain HTTP on port 80 is served too; Cloudflare's other ports are
    /// refused.
    AllowHttp,
    /// Plain HTTP and Cloudflare's other ports are all served: no rule.
    AllowAll,
}

impl HttpPolicy {
    pub const ALL: [HttpPolicy; 3] = [
        HttpPolicy::HttpsOnly,
        HttpPolicy::AllowHttp,
        HttpPolicy::AllowAll,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HttpPolicy::HttpsOnly => "https_only",
            HttpPolicy::AllowHttp => "allow_http",
            HttpPolicy::AllowAll => "allow_all",
        }
    }

    /// A stored or answered value; `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value.trim())
    }

    /// Whether the policy wants `purpose`'s rule.
    pub fn wants(self, purpose: EdgeRulePurpose) -> bool {
        matches!(
            (self, purpose),
            (HttpPolicy::HttpsOnly, _) | (HttpPolicy::AllowHttp, EdgeRulePurpose::StandardPorts)
        )
    }

    /// What the address answers besides HTTPS on 443, in words; `None` for
    /// HTTPS only.
    pub fn exposure(self, host: &str) -> Option<String> {
        match self {
            HttpPolicy::HttpsOnly => None,
            HttpPolicy::AllowHttp => Some(format!(
                "{host} will also answer over unencrypted HTTP (http://{host}), so what visitors \
                 send and get back there is readable on the way to Cloudflare."
            )),
            HttpPolicy::AllowAll => Some(format!(
                "{host} will also answer over unencrypted HTTP (http://{host}) and on \
                 Cloudflare's other ports (such as 8080 and 8443), so what visitors send and get \
                 back over HTTP is readable on the way to Cloudflare."
            )),
        }
    }

    /// The choice, in a few words.
    pub fn describe(self) -> &'static str {
        match self {
            HttpPolicy::HttpsOnly => "HTTPS only",
            HttpPolicy::AllowHttp => "HTTPS, and plain HTTP on port 80",
            HttpPolicy::AllowAll => "HTTPS, plain HTTP and Cloudflare's other ports",
        }
    }
}

/// Whether `host` is more than one label below `zone`: Cloudflare's free
/// certificate covers `*.zone` only, so HTTPS on it needs Advanced
/// Certificate Manager.
pub fn below_free_certificate(host: &str, zone: &str) -> bool {
    host.strip_suffix(&format!(".{zone}"))
        .is_some_and(|label| label.contains('.'))
}
