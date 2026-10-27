// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Check a pasted Cloudflare API token before anything is created with it
//! (ADR 064 D8).
//!
//! No call here can create anything:
//!
//! - **Zone · Zone · Read**: list the zones the token reaches. In *Make it
//!   reachable* the token must reach the chosen zone and no other; one that
//!   reaches more is refused, so a stored token can only touch the zone it
//!   serves.
//! - **Zone · DNS · Edit** and **Account · Cloudflare Tunnel · Edit**: read
//!   calls cannot tell Edit from Read, so each is probed with a create
//!   request whose body is deliberately invalid (an A record whose content
//!   is not an address; a tunnel with no name). Cloudflare authorizes before
//!   it validates: a refusal on authorization (HTTP 401/403, or code 10000,
//!   10001 or 9109) means the permission is missing, and a validation error
//!   (HTTP 400 with any other code) means it is present. Nothing is created.
//!   Should Cloudflare ever accept the request, what it made is deleted at
//!   once.
//! - Any other answer is `unverified`, never `present`, and a run refuses
//!   to start on it.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;

use super::plan::cloudflare_token_url;

const API: &str = "https://api.cloudflare.com/client/v4";
/// The most of one API answer Temps reads: a page of 100 Access apps with
/// their policies is well under it.
const MAX_ANSWER_BYTES: usize = 4 * 1024 * 1024;

/// Cloudflare's codes for "this token may not do that".
const DENIED_CODES: [i64; 3] = [10000, 10001, 9109];

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TokenCheckRequest {
    /// The token, as pasted. Used for this check only; not stored. A
    /// secret field, so the AI assistant takes it through a secret slot.
    pub token: temps_core::secret_string::SecretString,
    /// The zone the token is meant for. With it, a token that reaches any
    /// other zone is not ready.
    pub zone: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Present,
    Missing,
    /// Cloudflare's answer did not prove either.
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenPermission {
    ZoneRead,
    DnsEdit,
    TunnelEdit,
}

impl TokenPermission {
    pub fn label(self) -> &'static str {
        match self {
            TokenPermission::ZoneRead => "Zone · Zone · Read",
            TokenPermission::DnsEdit => "Zone · DNS · Edit",
            TokenPermission::TunnelEdit => "Account · Cloudflare Tunnel · Edit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PermissionCheck {
    pub permission: TokenPermission,
    pub label: String,
    pub state: PermissionState,
    /// Why, and what to change on the token when it is not present.
    pub detail: String,
}

/// Whether the token reaches only the zone it is meant for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ZoneScope {
    pub zone: String,
    /// Zones it reaches besides `zone`, as far as Cloudflare listed them.
    pub other_zones: Vec<String>,
    /// How many other zones it reaches in all, listed or not.
    pub other_count: u64,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TokenCheckResult {
    /// Cloudflare accepted the token at all.
    pub valid: bool,
    /// Why it did not, when it did not.
    pub error: Option<String>,
    /// Zones the token reaches (the first 50).
    pub zones: Vec<String>,
    /// The zone the write permissions were probed in.
    pub checked_zone: Option<String>,
    pub permissions: Vec<PermissionCheck>,
    /// Set when the check was for one zone.
    pub scope: Option<ZoneScope>,
    /// Every permission is present and, for one zone, the token reaches no
    /// other.
    pub ready: bool,
    /// Where to create a token with the permissions filled in.
    pub template_url: String,
}

impl TokenCheckResult {
    /// Why a run must not start with this token, in words the user can act
    /// on; `None` when it may.
    pub fn refusal(&self) -> Option<String> {
        if !self.valid {
            return Some(
                self.error
                    .clone()
                    .unwrap_or_else(|| "Cloudflare did not accept the token.".into()),
            );
        }
        let missing: Vec<&str> = self
            .permissions
            .iter()
            .filter(|p| p.state == PermissionState::Missing)
            .map(|p| p.label.as_str())
            .collect();
        if !missing.is_empty() {
            return Some(format!(
                "The token lacks {}. Edit it in Cloudflare and add {}, then paste it again.",
                missing.join(", "),
                if missing.len() == 1 { "it" } else { "them" }
            ));
        }
        if let Some(unverified) = self
            .permissions
            .iter()
            .find(|p| p.state == PermissionState::Unverified)
        {
            return Some(unverified.detail.clone());
        }
        match &self.scope {
            Some(scope) if !scope.ok => Some(scope.detail.clone()),
            _ => None,
        }
    }
}

/// One Cloudflare answer, as far as the check reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub success: bool,
    pub codes: Vec<i64>,
    pub messages: Vec<String>,
    pub result: Option<Value>,
    /// `result_info.total_count`, for lists.
    pub total_count: Option<u64>,
}

impl Answer {
    /// Parse a Cloudflare v4 response body.
    pub fn from_body(status: u16, body: &Value) -> Self {
        let errors = body
            .get("errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Answer {
            status,
            success: body.get("success").and_then(Value::as_bool) == Some(true),
            codes: errors
                .iter()
                .filter_map(|e| e.get("code").and_then(Value::as_i64))
                .collect(),
            messages: errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
            result: body.get("result").cloned(),
            total_count: body
                .get("result_info")
                .and_then(|i| i.get("total_count"))
                .and_then(Value::as_u64),
        }
    }

    pub(crate) fn denied(&self) -> bool {
        self.status == 401
            || self.status == 403
            || self.codes.iter().any(|code| DENIED_CODES.contains(code))
    }

    pub(crate) fn message(&self) -> String {
        if self.messages.is_empty() {
            format!("HTTP {}", self.status)
        } else {
            self.messages.join("; ")
        }
    }
}

/// A read: present when it works, missing when Cloudflare refuses it.
pub fn read_permission(answer: &Answer) -> PermissionState {
    if answer.success && (200..300).contains(&answer.status) {
        PermissionState::Present
    } else if answer.denied() {
        PermissionState::Missing
    } else {
        PermissionState::Unverified
    }
}

/// A write probe with an invalid body: an authorization refusal means
/// missing, a validation refusal (HTTP 400 with a code that is not about
/// authorization) means present, anything else proves nothing. A probe
/// Cloudflare accepted also proves the permission; the caller removes what
/// it made.
pub fn write_probe_permission(answer: &Answer) -> PermissionState {
    if answer.denied() {
        PermissionState::Missing
    } else if (answer.status == 400 && !answer.success && !answer.codes.is_empty())
        || (answer.success && (200..300).contains(&answer.status))
    {
        PermissionState::Present
    } else {
        PermissionState::Unverified
    }
}

fn check(permission: TokenPermission, state: PermissionState, detail: String) -> PermissionCheck {
    PermissionCheck {
        permission,
        label: permission.label().to_string(),
        state,
        detail,
    }
}

fn missing_detail(permission: TokenPermission) -> String {
    format!(
        "The token lacks {}. Edit the token in Cloudflare and add it, then check again.",
        permission.label()
    )
}

fn unverified_detail(permission: TokenPermission, zone: &str, answer: Option<&Answer>) -> String {
    let what = match permission {
        TokenPermission::DnsEdit => format!("edit DNS in `{zone}`"),
        TokenPermission::TunnelEdit => {
            format!("edit Cloudflare Tunnels in the account `{zone}` is in")
        }
        TokenPermission::ZoneRead => format!("read `{zone}`"),
    };
    format!(
        "Temps could not confirm that this token can {what}{}. Check the token has {}, or \
         create a new one.",
        answer
            .map(|a| format!(" (Cloudflare answered {})", a.message()))
            .unwrap_or_default(),
        permission.label()
    )
}

/// The answers to each call, assembled. `None` for a call not made.
pub fn assemble(
    verify: &Answer,
    zones: Option<&Answer>,
    tunnel_probe: Option<&Answer>,
    dns_probe: Option<&Answer>,
    wanted_zone: Option<&str>,
) -> TokenCheckResult {
    let template_url = cloudflare_token_url();
    if !verify.success || verify.denied() {
        return TokenCheckResult {
            valid: false,
            error: Some(format!(
                "Cloudflare did not accept the token ({}). Copy it again from Cloudflare; it is \
                 shown once, when it is created.",
                verify.message()
            )),
            zones: Vec::new(),
            checked_zone: None,
            permissions: Vec::new(),
            scope: None,
            ready: false,
            template_url,
        };
    }
    let zone_names = zones.map(zone_names).unwrap_or_default();
    let zone_state = zones.map_or(PermissionState::Unverified, read_permission);
    let mut permissions = vec![check(
        TokenPermission::ZoneRead,
        if zone_state == PermissionState::Present && zone_names.is_empty() {
            PermissionState::Missing
        } else {
            zone_state
        },
        match zone_state {
            PermissionState::Present if zone_names.is_empty() => {
                "The token reaches no zone. Add your domain under Zone Resources.".to_string()
            }
            PermissionState::Present => format!("Reaches {}.", zone_names.join(", ")),
            PermissionState::Missing => missing_detail(TokenPermission::ZoneRead),
            PermissionState::Unverified => zones.map_or_else(
                || "Not checked.".to_string(),
                |a| format!("Cloudflare answered {}", a.message()),
            ),
        },
    )];
    let checked_zone = match wanted_zone {
        Some(zone) if zone_names.iter().any(|z| z == zone) => Some(zone.to_string()),
        Some(_) => None,
        None => zone_names.first().cloned(),
    };
    let probe = |permission: TokenPermission, answer: Option<&Answer>| {
        let state = answer.map_or(PermissionState::Unverified, write_probe_permission);
        let detail = match (state, &checked_zone, wanted_zone) {
            (_, None, Some(zone)) => format!(
                "The token cannot reach {zone}. Add it under Zone Resources, then check again."
            ),
            (_, None, None) => "Not checked: the token reaches no zone.".to_string(),
            (PermissionState::Present, Some(zone), _) => match permission {
                TokenPermission::TunnelEdit => {
                    format!("Can manage tunnels in the account {zone} is in.")
                }
                _ => format!("Can edit DNS in {zone}."),
            },
            (PermissionState::Missing, _, _) => missing_detail(permission),
            (PermissionState::Unverified, Some(zone), _) => {
                unverified_detail(permission, zone, answer)
            }
        };
        let state = if checked_zone.is_none() {
            PermissionState::Unverified
        } else {
            state
        };
        check(permission, state, detail)
    };
    permissions.push(probe(TokenPermission::DnsEdit, dns_probe));
    permissions.push(probe(TokenPermission::TunnelEdit, tunnel_probe));

    let scope = wanted_zone.map(|zone| {
        let other_zones: Vec<String> = zone_names.iter().filter(|z| *z != zone).cloned().collect();
        let total = zones
            .and_then(|a| a.total_count)
            .unwrap_or(zone_names.len() as u64);
        let reaches_wanted = zone_names.iter().any(|z| z == zone);
        let other_count = total
            .saturating_sub(u64::from(reaches_wanted))
            .max(other_zones.len() as u64);
        let ok = reaches_wanted && other_count == 0;
        let detail = if ok {
            format!("Limited to {zone}.")
        } else if !reaches_wanted {
            format!("The token cannot reach {zone}. Add it under Zone Resources, then check again.")
        } else {
            let unlisted = other_count.saturating_sub(other_zones.len() as u64);
            let named = if unlisted > 0 {
                format!("{} and {unlisted} more", other_zones.join(", "))
            } else {
                other_zones.join(", ")
            };
            format!(
                "The token also reaches {named}. Temps keeps a token here only for the zone it \
                 serves: in Cloudflare, edit the token, set Zone Resources to Include · Specific \
                 zone · {zone}, save, and paste it again. To manage several zones with one \
                 token, add it under DNS providers instead."
            )
        };
        ZoneScope {
            zone: zone.to_string(),
            other_zones,
            other_count,
            ok,
            detail,
        }
    });
    let ready = permissions
        .iter()
        .all(|p| p.state == PermissionState::Present)
        && scope.as_ref().is_none_or(|s| s.ok);
    TokenCheckResult {
        valid: true,
        error: None,
        zones: zone_names,
        checked_zone,
        permissions,
        scope,
        ready,
        template_url,
    }
}

fn zone_names(answer: &Answer) -> Vec<String> {
    answer
        .result
        .as_ref()
        .and_then(Value::as_array)
        .map(|zones| {
            zones
                .iter()
                .filter_map(|z| z.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn zone_field<'a>(answer: &'a Answer, zone: &str, path: &[&str]) -> Option<&'a str> {
    answer
        .result
        .as_ref()?
        .as_array()?
        .iter()
        .find(|z| z.get("name").and_then(Value::as_str) == Some(zone))
        .and_then(|z| {
            path.iter()
                .try_fold(z, |value, key| value.get(*key))
                .and_then(Value::as_str)
        })
}

pub(crate) async fn call(
    http: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let mut request = http
        .request(method, format!("{API}{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    match request.send().await {
        Err(error) => Answer {
            status: 0,
            success: false,
            codes: Vec::new(),
            messages: vec![format!(
                "could not reach api.cloudflare.com ({error}); the control plane needs \
                 outbound HTTPS to it"
            )],
            result: None,
            total_count: None,
        },
        Ok(response) => {
            let status = response.status().as_u16();
            match temps_routes::access_keys::read_capped(response, MAX_ANSWER_BYTES).await {
                Ok(bytes) => {
                    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                    Answer::from_body(status, &body)
                }
                Err(error) => Answer {
                    status,
                    success: false,
                    codes: Vec::new(),
                    messages: vec![format!(
                        "could not read api.cloudflare.com's answer to {path}: {error}"
                    )],
                    result: None,
                    total_count: None,
                },
            }
        }
    }
}

/// The id in a probe Cloudflare accepted, so it can be removed.
fn created_id(answer: &Answer) -> Option<&str> {
    if !answer.success {
        return None;
    }
    answer.result.as_ref()?.get("id")?.as_str()
}

/// The DNS Edit probe: an A record whose content is not an address.
pub fn dns_probe_body(zone: &str) -> Value {
    json!({
        "type": "A",
        "name": format!("temps-permission-check.{zone}"),
        "content": "not-an-address",
        "ttl": 60,
    })
}

/// The Tunnel Edit probe: a tunnel with an empty name.
pub fn tunnel_probe_body() -> Value {
    json!({ "name": "", "config_src": "cloudflare" })
}

/// Check `token` against Cloudflare.
pub async fn check_token(
    http: &reqwest::Client,
    token: &str,
    wanted_zone: Option<&str>,
) -> TokenCheckResult {
    let token = token.trim();
    let wanted_zone = wanted_zone
        .map(|z| z.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|z| !z.is_empty());
    let verify = call(
        http,
        token,
        reqwest::Method::GET,
        "/user/tokens/verify",
        None,
    )
    .await;
    if !verify.success || verify.denied() {
        return assemble(&verify, None, None, None, wanted_zone.as_deref());
    }
    let zones = call(
        http,
        token,
        reqwest::Method::GET,
        "/zones?per_page=50",
        None,
    )
    .await;
    let names = zone_names(&zones);
    let target = match &wanted_zone {
        Some(zone) => names.iter().find(|n| *n == zone).cloned(),
        None => names.first().cloned(),
    };
    let (mut tunnel_probe, mut dns_probe) = (None, None);
    if let Some(zone) = &target {
        if let Some(account) = zone_field(&zones, zone, &["account", "id"]) {
            let probe = call(
                http,
                token,
                reqwest::Method::POST,
                &format!("/accounts/{account}/cfd_tunnel"),
                Some(tunnel_probe_body()),
            )
            .await;
            if let Some(id) = created_id(&probe) {
                let _ = call(
                    http,
                    token,
                    reqwest::Method::DELETE,
                    &format!("/accounts/{account}/cfd_tunnel/{id}"),
                    None,
                )
                .await;
            }
            tunnel_probe = Some(probe);
        }
        if let Some(zone_id) = zone_field(&zones, zone, &["id"]) {
            let probe = call(
                http,
                token,
                reqwest::Method::POST,
                &format!("/zones/{zone_id}/dns_records"),
                Some(dns_probe_body(zone)),
            )
            .await;
            if let Some(id) = created_id(&probe) {
                let _ = call(
                    http,
                    token,
                    reqwest::Method::DELETE,
                    &format!("/zones/{zone_id}/dns_records/{id}"),
                    None,
                )
                .await;
            }
            dns_probe = Some(probe);
        }
    }
    assemble(
        &verify,
        Some(&zones),
        tunnel_probe.as_ref(),
        dns_probe.as_ref(),
        wanted_zone.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Response bodies in the shape Cloudflare's v4 API returns them. The
    // authorization refusal (403, code 10000 "Authentication error") is the
    // answer Cloudflare documents and users report for a token without the
    // permission; the validation refusals are what a token with it gets for
    // these bodies. They pin how each answer is read: changing the mapping
    // has to change these.
    const VERIFY_OK: &str = r#"{"result":{"id":"ed17574386854bf78a67040be0a770b0","status":"active"},"success":true,"errors":[],"messages":[{"code":10000,"message":"This API Token is valid and active","type":null}]}"#;
    const VERIFY_INVALID: &str = r#"{"success":false,"errors":[{"code":1000,"message":"Invalid API Token"}],"messages":[],"result":null}"#;
    const ZONES_ONE: &str = r#"{"result":[{"id":"023e105f4ecef8ad9ca31a8372d0c353","name":"example.com","status":"active","account":{"id":"01a7362d577a6c3019a474fd6f485823","name":"Example"}}],"result_info":{"page":1,"per_page":50,"count":1,"total_count":1},"success":true,"errors":[],"messages":[]}"#;
    const ZONES_THREE: &str = r#"{"result":[{"id":"z1","name":"example.com","account":{"id":"a1"}},{"id":"z2","name":"other.org","account":{"id":"a1"}}],"result_info":{"page":1,"per_page":2,"count":2,"total_count":3},"success":true,"errors":[],"messages":[]}"#;
    const NOT_AUTHORIZED: &str = r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"messages":[],"result":null}"#;
    const DNS_INVALID_CONTENT: &str = r#"{"result":null,"success":false,"errors":[{"code":9005,"message":"Content for A record must be a valid IPv4 address."}],"messages":[]}"#;
    const TUNNEL_INVALID_NAME: &str = r#"{"result":null,"success":false,"errors":[{"code":1003,"message":"Tunnel name is required"}],"messages":[]}"#;
    const SERVER_ERROR: &str = r#"{"success":false,"errors":[],"messages":[],"result":null}"#;

    fn answer(status: u16, body: &str) -> Answer {
        Answer::from_body(status, &serde_json::from_str(body).expect("recorded body"))
    }

    fn state(result: &TokenCheckResult, permission: TokenPermission) -> PermissionState {
        result
            .permissions
            .iter()
            .find(|p| p.permission == permission)
            .map(|p| p.state)
            .expect("permission listed")
    }

    #[test]
    fn a_rejected_token_lists_no_permissions() {
        let result = assemble(&answer(401, VERIFY_INVALID), None, None, None, None);
        assert!(!result.valid && !result.ready);
        assert!(result.error.expect("error").contains("Invalid API Token"));
        assert!(result.permissions.is_empty());
    }

    #[test]
    fn validation_errors_on_both_probes_prove_both_writes() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ZONES_ONE)),
            Some(&answer(400, TUNNEL_INVALID_NAME)),
            Some(&answer(400, DNS_INVALID_CONTENT)),
            Some("example.com"),
        );
        assert!(result.valid && result.ready, "{result:?}");
        assert_eq!(result.zones, vec!["example.com"]);
        assert_eq!(result.checked_zone.as_deref(), Some("example.com"));
        assert!(result.scope.as_ref().is_some_and(|s| s.ok));
        assert_eq!(result.refusal(), None);
    }

    #[test]
    fn authorization_refusals_mean_missing_and_are_named() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ZONES_ONE)),
            Some(&answer(403, NOT_AUTHORIZED)),
            Some(&answer(403, NOT_AUTHORIZED)),
            Some("example.com"),
        );
        assert!(!result.ready);
        assert_eq!(
            state(&result, TokenPermission::ZoneRead),
            PermissionState::Present
        );
        assert_eq!(
            state(&result, TokenPermission::DnsEdit),
            PermissionState::Missing
        );
        assert_eq!(
            state(&result, TokenPermission::TunnelEdit),
            PermissionState::Missing
        );
        let refusal = result.refusal().expect("refused");
        assert!(refusal.contains("Zone · DNS · Edit"), "{refusal}");
        assert!(
            refusal.contains("Account · Cloudflare Tunnel · Edit"),
            "{refusal}"
        );
    }

    #[test]
    fn an_answer_that_proves_nothing_is_unverified_and_refused() {
        for (status, body) in [
            (502, SERVER_ERROR),
            (404, SERVER_ERROR),
            (400, SERVER_ERROR),
        ] {
            assert_eq!(
                write_probe_permission(&answer(status, body)),
                PermissionState::Unverified,
                "HTTP {status}"
            );
        }
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ZONES_ONE)),
            Some(&answer(400, TUNNEL_INVALID_NAME)),
            Some(&answer(502, SERVER_ERROR)),
            Some("example.com"),
        );
        assert!(!result.ready);
        assert_eq!(
            state(&result, TokenPermission::DnsEdit),
            PermissionState::Unverified
        );
        let refusal = result.refusal().expect("refused");
        assert!(
            refusal.starts_with(
                "Temps could not confirm that this token can edit DNS in `example.com`"
            ),
            "{refusal}"
        );
        assert!(refusal.contains("Check the token has Zone · DNS · Edit, or create a new one."));
    }

    #[test]
    fn a_token_reaching_other_zones_is_refused_with_them_named() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ZONES_THREE)),
            Some(&answer(400, TUNNEL_INVALID_NAME)),
            Some(&answer(400, DNS_INVALID_CONTENT)),
            Some("example.com"),
        );
        assert!(!result.ready);
        let scope = result.scope.clone().expect("scope");
        assert_eq!(scope.other_zones, vec!["other.org"]);
        assert_eq!(scope.other_count, 2);
        let refusal = result.refusal().expect("refused");
        assert!(
            refusal.contains("also reaches other.org and 1 more"),
            "{refusal}"
        );
        assert!(refusal.contains("Specific zone · example.com"), "{refusal}");

        // Without a zone to keep to (the DNS providers page), several are fine.
        let unscoped = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ZONES_THREE)),
            Some(&answer(400, TUNNEL_INVALID_NAME)),
            Some(&answer(400, DNS_INVALID_CONTENT)),
            None,
        );
        assert!(unscoped.ready && unscoped.scope.is_none());
    }

    #[test]
    fn a_token_that_reaches_no_zone_is_missing_zone_read() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(
                200,
                r#"{"result":[],"result_info":{"total_count":0},"success":true,"errors":[]}"#,
            )),
            None,
            None,
            None,
        );
        assert_eq!(
            state(&result, TokenPermission::ZoneRead),
            PermissionState::Missing
        );
        assert_eq!(
            state(&result, TokenPermission::DnsEdit),
            PermissionState::Unverified
        );
        assert_eq!(
            state(&result, TokenPermission::TunnelEdit),
            PermissionState::Unverified
        );
    }

    #[test]
    fn a_zone_the_token_cannot_reach_says_so() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ZONES_ONE)),
            None,
            None,
            Some("other.org"),
        );
        let dns = result
            .permissions
            .iter()
            .find(|p| p.permission == TokenPermission::DnsEdit)
            .expect("dns");
        assert!(dns.detail.contains("cannot reach other.org"));
        assert!(!result.scope.expect("scope").ok);
    }

    #[test]
    fn the_probes_cannot_be_valid_requests() {
        // An A record needs an address, and a tunnel needs a name.
        assert_eq!(dns_probe_body("example.com")["content"], "not-an-address");
        assert_eq!(tunnel_probe_body()["name"], "");
    }
}
