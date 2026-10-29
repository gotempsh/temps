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
//! never used. A rule with the same expression and action already there is
//! used as found and never journaled. Each rule Temps adds carries a `ref`
//! and a description naming this install and run, which is how a pending
//! one is found again.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;

use super::token_check::{call, Answer};

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

    fn action(self) -> &'static str {
        match self {
            EdgeRulePurpose::HttpsRedirect => "redirect",
            EdgeRulePurpose::StandardPorts => "block",
        }
    }

    fn short(self) -> &'static str {
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

/// A hostname safe to put in a rule expression: lowercase letters, digits,
/// `-` and `.`, as DNS allows. Anything else could break out of the quoted
/// string.
pub fn is_rule_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// The rule's expression for `host`. `host` must pass [`is_rule_host`].
pub fn expression(purpose: EdgeRulePurpose, host: &str) -> String {
    match purpose {
        EdgeRulePurpose::HttpsRedirect => format!("(http.host eq \"{host}\" and not ssl)"),
        EdgeRulePurpose::StandardPorts => {
            format!("(http.host eq \"{host}\" and not cf.edge.server_port in {{80 443}})")
        }
    }
}

/// The rule's Cloudflare `ref`: this install and run.
pub fn rule_ref(install: &str, run_id: i32, purpose: EdgeRulePurpose) -> String {
    let install: String = install
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("temps-{install}-run{run_id}-{}", purpose.short())
}

/// The description shown in the Cloudflare dashboard. Starts with the ref,
/// so a rule whose `ref` Cloudflare replaced is still found by it.
pub fn rule_description(reference: &str, purpose: EdgeRulePurpose, host: &str) -> String {
    let what = match purpose {
        EdgeRulePurpose::HttpsRedirect => "HTTPS only",
        EdgeRulePurpose::StandardPorts => "ports 80 and 443 only",
    };
    format!("{reference}: {what} for {host} (Temps, Make it reachable)")
}

/// The rule Temps adds.
pub fn rule_body(purpose: EdgeRulePurpose, host: &str, reference: &str) -> Value {
    let mut rule = json!({
        "ref": reference,
        "description": rule_description(reference, purpose, host),
        "expression": expression(purpose, host),
        "action": purpose.action(),
        "enabled": true,
    });
    if purpose == EdgeRulePurpose::HttpsRedirect {
        rule["action_parameters"] = json!({
            "from_value": {
                "target_url": {
                    "expression": "concat(\"https://\", http.host, http.request.uri.path)"
                },
                "status_code": 308,
                "preserve_query_string": true,
            }
        });
    }
    rule
}

/// Whether `host` is more than one label below `zone`: Cloudflare's free
/// certificate covers `*.zone` only, so HTTPS on it needs Advanced
/// Certificate Manager.
pub fn below_free_certificate(host: &str, zone: &str) -> bool {
    host.strip_suffix(&format!(".{zone}"))
        .is_some_and(|label| label.contains('.'))
}

/// Why `purpose`'s rule is not added for `host` in `zone`, the zone
/// Cloudflare names at run time: a redirect to HTTPS for a hostname beyond
/// the free certificate would take it down. The plan checks this only for
/// a zone it knows, so the run checks again with the real one.
pub fn rule_refusal(host: &str, zone: &str, purpose: EdgeRulePurpose) -> Option<String> {
    (purpose == EdgeRulePurpose::HttpsRedirect && below_free_certificate(host, zone)).then(|| {
        format!(
            "{host} is more than one level below the zone {zone}, where Cloudflare's free \
             certificate does not reach, so a redirect to HTTPS would take it down."
        )
    })
}

/// One rule of a ruleset, as far as Temps reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRuleSeen {
    pub id: String,
    pub reference: Option<String>,
    pub description: Option<String>,
    pub expression: String,
    pub action: String,
    pub enabled: bool,
}

fn parse_rule(value: &Value) -> Option<EdgeRuleSeen> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    Some(EdgeRuleSeen {
        id: text("id")?,
        reference: text("ref"),
        description: text("description"),
        expression: text("expression").unwrap_or_default(),
        action: text("action").unwrap_or_default(),
        enabled: value
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

/// A ruleset's id and rules, from a Cloudflare `result`.
pub fn parse_ruleset(result: &Value) -> Option<(String, Vec<EdgeRuleSeen>)> {
    let id = result.get("id")?.as_str()?.to_string();
    let rules = result
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| rules.iter().filter_map(parse_rule).collect())
        .unwrap_or_default();
    Some((id, rules))
}

fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `seen` does what this rule does for `host`: same expression (as
/// far as whitespace goes), same action, and enabled.
pub fn same_rule(seen: &EdgeRuleSeen, purpose: EdgeRulePurpose, host: &str) -> bool {
    seen.enabled
        && seen.action == purpose.action()
        && squeeze(&seen.expression) == squeeze(&expression(purpose, host))
}

/// Whether `seen` is the rule this run added: its `ref`, or a description
/// that starts with it.
pub fn is_ours(seen: &EdgeRuleSeen, reference: &str) -> bool {
    seen.reference.as_deref() == Some(reference)
        || seen
            .description
            .as_deref()
            .is_some_and(|d| d.starts_with(&format!("{reference}:")))
}

/// Why a rule could not be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRuleError {
    pub message: String,
    /// The token permission Cloudflare said is missing.
    pub missing_permission: Option<&'static str>,
    /// The DNS provider whose token was used, for the message.
    pub provider: Option<String>,
    /// No answer, a server error, or an answer Temps could not read after
    /// the create call: the rule may exist. `false` for every refusal and
    /// for failures before anything was asked to be created.
    pub maybe_applied: bool,
}

impl EdgeRuleError {
    fn refused(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            missing_permission: None,
            provider: None,
            maybe_applied: false,
        }
    }
}

/// An answer that did not succeed, as an [`EdgeRuleError`]. `creating`:
/// the answer is to the call that adds the rule.
pub fn refusal(
    purpose: EdgeRulePurpose,
    what: &str,
    answer: &Answer,
    creating: bool,
) -> EdgeRuleError {
    if answer.denied() {
        return EdgeRuleError {
            message: format!("Cloudflare refused to {what}: {}", answer.message()),
            missing_permission: Some(purpose.permission()),
            provider: None,
            maybe_applied: false,
        };
    }
    EdgeRuleError {
        message: format!("Cloudflare refused to {what}: {}", answer.message()),
        missing_permission: None,
        provider: None,
        maybe_applied: creating && !(400..500).contains(&answer.status),
    }
}

/// What adding the rule did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeRuleCreated {
    Created {
        ruleset_id: String,
        rule_id: String,
        expression: String,
    },
    /// A rule doing the same was there already: used as found, never
    /// journaled, never removed.
    AlreadyThere { description: String },
}

/// This run's rule in a ruleset answer.
pub fn created_rule(
    answer: &Answer,
    purpose: EdgeRulePurpose,
    host: &str,
    reference: &str,
) -> Result<EdgeRuleCreated, EdgeRuleError> {
    if !answer.success {
        return Err(refusal(purpose, "add the rule", answer, true));
    }
    let parsed = answer.result.as_ref().and_then(parse_ruleset);
    let Some((ruleset_id, rules)) = parsed else {
        return Err(EdgeRuleError {
            message: "Cloudflare added the rule but returned no ruleset.".into(),
            missing_permission: None,
            provider: None,
            maybe_applied: true,
        });
    };
    match rules.iter().find(|r| is_ours(r, reference)) {
        Some(rule) => Ok(EdgeRuleCreated::Created {
            ruleset_id,
            rule_id: rule.id.clone(),
            expression: rule.expression.clone(),
        }),
        None => Err(EdgeRuleError {
            message: format!(
                "Cloudflare answered, but its ruleset has no rule `{reference}` for {host}."
            ),
            missing_permission: None,
            provider: None,
            maybe_applied: true,
        }),
    }
}

/// The zone `host` is in, as `(name, id)`, walking up from the hostname.
pub async fn find_zone(
    http: &reqwest::Client,
    token: &str,
    host: &str,
) -> Result<Option<(String, String)>, EdgeRuleError> {
    let labels: Vec<&str> = host.split('.').collect();
    for start in 0..labels.len().saturating_sub(1) {
        let candidate = labels[start..].join(".");
        let answer = call(
            http,
            token,
            reqwest::Method::GET,
            &format!("/zones?name={candidate}"),
            None,
        )
        .await;
        if !answer.success {
            let mut error = EdgeRuleError::refused(format!(
                "Cloudflare refused to look up the zone {candidate}: {}",
                answer.message()
            ));
            if answer.denied() {
                error.missing_permission = Some("Zone · Zone · Read");
            }
            return Err(error);
        }
        let found = answer
            .result
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|zones| zones.first())
            .and_then(|zone| {
                Some((
                    zone.get("name")?.as_str()?.to_string(),
                    zone.get("id")?.as_str()?.to_string(),
                ))
            });
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// The phase's entry point ruleset; `None` when the zone has none.
pub async fn entrypoint(
    http: &reqwest::Client,
    token: &str,
    zone_id: &str,
    purpose: EdgeRulePurpose,
) -> Result<Option<(String, Vec<EdgeRuleSeen>)>, EdgeRuleError> {
    let answer = call(
        http,
        token,
        reqwest::Method::GET,
        &format!(
            "/zones/{zone_id}/rulesets/phases/{}/entrypoint",
            purpose.phase()
        ),
        None,
    )
    .await;
    if answer.status == 404 {
        return Ok(None);
    }
    if !answer.success {
        return Err(refusal(purpose, "read the zone's rules", &answer, false));
    }
    Ok(answer.result.as_ref().and_then(parse_ruleset))
}

/// Add the rule for `host`, or find one doing the same already.
pub async fn add_rule(
    http: &reqwest::Client,
    token: &str,
    zone_id: &str,
    purpose: EdgeRulePurpose,
    host: &str,
    reference: &str,
) -> Result<EdgeRuleCreated, EdgeRuleError> {
    if !is_rule_host(host) {
        return Err(EdgeRuleError::refused(format!(
            "`{host}` is not a hostname Temps puts in a Cloudflare rule."
        )));
    }
    let body = rule_body(purpose, host, reference);
    // Twice at most: a create of the entry point that lost a race reads it
    // again and adds the rule to it.
    let mut last = String::from("no answer");
    for _ in 0..2 {
        match entrypoint(http, token, zone_id, purpose).await? {
            Some((ruleset_id, rules)) => {
                if let Some(rule) = rules.iter().find(|r| is_ours(r, reference)) {
                    return Ok(EdgeRuleCreated::Created {
                        ruleset_id,
                        rule_id: rule.id.clone(),
                        expression: rule.expression.clone(),
                    });
                }
                if let Some(rule) = rules.iter().find(|r| same_rule(r, purpose, host)) {
                    return Ok(EdgeRuleCreated::AlreadyThere {
                        description: rule
                            .description
                            .clone()
                            .filter(|d| !d.is_empty())
                            .unwrap_or_else(|| format!("rule {}", rule.id)),
                    });
                }
                let answer = call(
                    http,
                    token,
                    reqwest::Method::POST,
                    &format!("/zones/{zone_id}/rulesets/{ruleset_id}/rules"),
                    Some(body.clone()),
                )
                .await;
                return created_rule(&answer, purpose, host, reference);
            }
            None => {
                let answer = call(
                    http,
                    token,
                    reqwest::Method::POST,
                    &format!("/zones/{zone_id}/rulesets"),
                    Some(json!({
                        "name": "default",
                        "kind": "zone",
                        "phase": purpose.phase(),
                        "rules": [body.clone()],
                    })),
                )
                .await;
                if answer.success || answer.denied() || answer.status == 0 || answer.status >= 500 {
                    return created_rule(&answer, purpose, host, reference);
                }
                // Refused, most likely because the entry point exists now:
                // read it again and add to it.
                last = answer.message();
            }
        }
    }
    Err(EdgeRuleError::refused(format!(
        "Cloudflare refused to create the zone's {} rules ({last}), and none exist.",
        purpose.phase()
    )))
}

/// This run's rule, found by its reference.
pub async fn find_rule(
    http: &reqwest::Client,
    token: &str,
    zone_id: &str,
    purpose: EdgeRulePurpose,
    reference: &str,
) -> Result<Option<(String, EdgeRuleSeen)>, String> {
    let found = entrypoint(http, token, zone_id, purpose)
        .await
        .map_err(|e| e.message)?;
    Ok(found.and_then(|(ruleset_id, rules)| {
        rules
            .into_iter()
            .find(|r| is_ours(r, reference))
            .map(|rule| (ruleset_id, rule))
    }))
}

/// Rule `rule_id` of ruleset `ruleset_id`; `None` when either is gone.
pub async fn read_rule(
    http: &reqwest::Client,
    token: &str,
    zone_id: &str,
    ruleset_id: &str,
    rule_id: &str,
) -> Result<Option<EdgeRuleSeen>, String> {
    let answer = call(
        http,
        token,
        reqwest::Method::GET,
        &format!("/zones/{zone_id}/rulesets/{ruleset_id}"),
        None,
    )
    .await;
    if answer.status == 404 {
        return Ok(None);
    }
    if !answer.success {
        return Err(format!(
            "Cloudflare refused to read ruleset {ruleset_id}: {}",
            answer.message()
        ));
    }
    Ok(answer
        .result
        .as_ref()
        .and_then(parse_ruleset)
        .and_then(|(_, rules)| rules.into_iter().find(|r| r.id == rule_id)))
}

/// Delete rule `rule_id`. Gone already is done.
pub async fn delete_rule(
    http: &reqwest::Client,
    token: &str,
    zone_id: &str,
    ruleset_id: &str,
    rule_id: &str,
) -> Result<(), String> {
    let answer = call(
        http,
        token,
        reqwest::Method::DELETE,
        &format!("/zones/{zone_id}/rulesets/{ruleset_id}/rules/{rule_id}"),
        None,
    )
    .await;
    if answer.success || answer.status == 404 {
        Ok(())
    } else {
        Err(format!(
            "Cloudflare refused to delete rule {rule_id}: {}",
            answer.message()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(status: u16, body: &str) -> Answer {
        Answer::from_body(status, &serde_json::from_str(body).expect("json"))
    }

    const HOST: &str = "myapp.example.com";

    #[test]
    fn each_rule_names_the_hostname_only() {
        assert_eq!(
            expression(EdgeRulePurpose::HttpsRedirect, HOST),
            "(http.host eq \"myapp.example.com\" and not ssl)"
        );
        assert_eq!(
            expression(EdgeRulePurpose::StandardPorts, HOST),
            "(http.host eq \"myapp.example.com\" and not cf.edge.server_port in {80 443})"
        );
        let redirect = rule_body(
            EdgeRulePurpose::HttpsRedirect,
            HOST,
            "temps-abcd-run4-https",
        );
        assert_eq!(redirect["action"], "redirect");
        assert_eq!(redirect["ref"], "temps-abcd-run4-https");
        assert_eq!(
            redirect["action_parameters"]["from_value"]["status_code"],
            308
        );
        assert_eq!(
            redirect["action_parameters"]["from_value"]["preserve_query_string"],
            true
        );
        let ports = rule_body(
            EdgeRulePurpose::StandardPorts,
            HOST,
            "temps-abcd-run4-ports",
        );
        assert_eq!(ports["action"], "block");
        assert!(ports.get("action_parameters").is_none());
        assert!(ports["description"]
            .as_str()
            .is_some_and(|d| d.starts_with("temps-abcd-run4-ports:")));
    }

    #[test]
    fn each_policy_wants_only_its_rules_and_round_trips() {
        use EdgeRulePurpose::{HttpsRedirect, StandardPorts};
        assert!(HttpPolicy::HttpsOnly.wants(HttpsRedirect));
        assert!(HttpPolicy::HttpsOnly.wants(StandardPorts));
        assert!(!HttpPolicy::AllowHttp.wants(HttpsRedirect));
        assert!(HttpPolicy::AllowHttp.wants(StandardPorts));
        assert!(!HttpPolicy::AllowAll.wants(HttpsRedirect));
        assert!(!HttpPolicy::AllowAll.wants(StandardPorts));
        for policy in HttpPolicy::ALL {
            assert_eq!(HttpPolicy::parse(policy.as_str()), Some(policy));
        }
        assert_eq!(HttpPolicy::parse("sometimes"), None);
        assert!(HttpPolicy::HttpsOnly.exposure("a.example.com").is_none());
        assert!(HttpPolicy::AllowAll
            .exposure("a.example.com")
            .is_some_and(|e| e.contains("8443")));
    }

    #[test]
    fn only_a_plain_hostname_reaches_an_expression() {
        assert!(is_rule_host("myapp.example.com"));
        assert!(is_rule_host("a-1.b.example.com"));
        assert!(!is_rule_host("x\" or true or \"y.example.com"));
        assert!(!is_rule_host("MyApp.example.com"));
        assert!(!is_rule_host("*.example.com"));
        assert!(!is_rule_host(""));
        assert!(!is_rule_host("myapp.example.com."));
    }

    #[test]
    fn the_reference_names_the_install_and_run() {
        assert_eq!(
            rule_ref(
                "4CEEBD3C-7552-4d41-9249",
                12,
                EdgeRulePurpose::HttpsRedirect
            ),
            "temps-4ceebd3c-run12-https"
        );
    }

    #[test]
    fn a_deep_hostname_is_below_the_free_certificate() {
        assert!(!below_free_certificate("myapp.example.com", "example.com"));
        assert!(below_free_certificate("a.lab.example.com", "example.com"));
        assert!(!below_free_certificate("example.com", "example.com"));
    }

    /// The run checks the zone Cloudflare names: only the redirect is
    /// refused beyond the free certificate; refusing other ports is safe.
    #[test]
    fn the_redirect_is_refused_beyond_the_free_certificate_at_run_time() {
        let refusal = rule_refusal(
            "myapp.lab.example.co.uk",
            "example.co.uk",
            EdgeRulePurpose::HttpsRedirect,
        )
        .expect("refused");
        assert!(refusal.contains("example.co.uk"), "{refusal}");
        assert!(rule_refusal(
            "myapp.lab.example.co.uk",
            "example.co.uk",
            EdgeRulePurpose::StandardPorts
        )
        .is_none());
        assert!(rule_refusal(
            "myapp.example.co.uk",
            "example.co.uk",
            EdgeRulePurpose::HttpsRedirect
        )
        .is_none());
    }

    // Recorded shape of `POST /zones/{id}/rulesets/{id}/rules`: the whole
    // ruleset, the user's rule first and the new one last.
    const ADDED: &str = r#"{"success":true,"errors":[],"messages":[],"result":{
        "id":"rs-1","kind":"zone","phase":"http_request_dynamic_redirect","rules":[
          {"id":"user-rule","ref":"user-rule","expression":"(http.host eq \"blog.example.com\")","action":"redirect","enabled":true},
          {"id":"r-9","ref":"temps-abcd-run4-https","description":"temps-abcd-run4-https: HTTPS only for myapp.example.com (Temps, Make it reachable)","expression":"(http.host eq \"myapp.example.com\" and not ssl)","action":"redirect","enabled":true}
        ]}}"#;

    #[test]
    fn the_added_rule_is_found_by_its_reference_among_the_users() {
        let created = created_rule(
            &answer(200, ADDED),
            EdgeRulePurpose::HttpsRedirect,
            HOST,
            "temps-abcd-run4-https",
        )
        .expect("created");
        assert_eq!(
            created,
            EdgeRuleCreated::Created {
                ruleset_id: "rs-1".into(),
                rule_id: "r-9".into(),
                expression: "(http.host eq \"myapp.example.com\" and not ssl)".into(),
            }
        );
    }

    #[test]
    fn a_rule_whose_ref_cloudflare_replaced_is_found_by_its_description() {
        let (_, rules) = parse_ruleset(
            &serde_json::from_str::<Value>(
                r#"{"id":"rs-1","rules":[{"id":"r-9","ref":"r-9","description":"temps-abcd-run4-https: HTTPS only","expression":"x","action":"redirect"}]}"#,
            )
            .expect("json"),
        )
        .expect("ruleset");
        assert!(is_ours(&rules[0], "temps-abcd-run4-https"));
        assert!(!is_ours(&rules[0], "temps-abcd-run5-https"));
    }

    #[test]
    fn a_missing_permission_is_named_and_never_counts_as_maybe_created() {
        let denied = answer(
            403,
            r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"messages":[],"result":null}"#,
        );
        let error = created_rule(
            &denied,
            EdgeRulePurpose::StandardPorts,
            HOST,
            "temps-abcd-run4-ports",
        )
        .expect_err("refused");
        assert_eq!(error.missing_permission, Some("Zone · Zone WAF · Edit"));
        assert!(!error.maybe_applied);

        let read = refusal(EdgeRulePurpose::HttpsRedirect, "read", &denied, false);
        assert_eq!(
            read.missing_permission,
            Some("Zone · Single Redirect · Edit")
        );
    }

    #[test]
    fn a_limit_is_a_refusal_and_a_server_error_may_have_applied() {
        let limit = answer(
            400,
            r#"{"success":false,"errors":[{"code":20217,"message":"exceeded maximum number of rules"}],"messages":[],"result":null}"#,
        );
        let error =
            created_rule(&limit, EdgeRulePurpose::HttpsRedirect, HOST, "r").expect_err("refused");
        assert!(error.missing_permission.is_none());
        assert!(!error.maybe_applied);
        assert!(error.message.contains("exceeded maximum number of rules"));

        let server = answer(
            502,
            r#"{"success":false,"errors":[],"messages":[],"result":null}"#,
        );
        assert!(
            created_rule(&server, EdgeRulePurpose::HttpsRedirect, HOST, "r")
                .expect_err("unknown")
                .maybe_applied
        );
    }

    #[test]
    fn an_identical_rule_counts_only_when_enabled_with_the_same_action() {
        let rule = |expression: &str, action: &str, enabled: bool| EdgeRuleSeen {
            id: "u".into(),
            reference: None,
            description: None,
            expression: expression.into(),
            action: action.into(),
            enabled,
        };
        let ours = expression(EdgeRulePurpose::HttpsRedirect, HOST);
        let spaced = ours.replace(" and ", "  and\n ");
        assert!(same_rule(
            &rule(&spaced, "redirect", true),
            EdgeRulePurpose::HttpsRedirect,
            HOST
        ));
        assert!(!same_rule(
            &rule(&ours, "redirect", false),
            EdgeRulePurpose::HttpsRedirect,
            HOST
        ));
        assert!(!same_rule(
            &rule(&ours, "block", true),
            EdgeRulePurpose::HttpsRedirect,
            HOST
        ));
    }
}
