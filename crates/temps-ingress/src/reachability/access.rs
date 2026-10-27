// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cloudflare Access for tunneled signed-in addresses (ADR 065 D9): the
//! token check and the Access applications API.
//!
//! **The token** is separate from the DNS and tunnel token (ADR 064 D8):
//! Account · Access: Apps and Policies · Edit and Account · Access:
//! Organizations, Identity Providers, and Groups · Read. Both are
//! account-wide, so it can change or remove the protection of every Access
//! app in the account; the check says so. Each permission is proved before
//! the token is saved, the way `token_check` proves the DNS token's:
//!
//! - **Organizations Read**: `GET /accounts/{id}/access/organizations`,
//!   which also gives the team domain (`auth_domain`) JWTs are issued by.
//! - **Apps and Policies Edit**: a create request with a deliberately
//!   invalid body (no domain). An authorization refusal means missing, a
//!   validation refusal means present. Should Cloudflare accept it, what it
//!   made is deleted at once.
//!
//! **Apps.** An app that already covers the hostname is used as found:
//! Temps reads its AUD and never edits or deletes it. Otherwise a run
//! creates one, `self_hosted`, named `temps-<hostname>-<install>`, with one
//! allow policy from the plan's answers (emails, email domains). Undo
//! deletes only an app the run created, whose domain is still the hostname,
//! and whose policies are still the ones it wrote ([`policy_fingerprint`]).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use super::token_check::{call, read_permission, write_probe_permission, Answer, PermissionState};

/// Where to create the token. Account permissions cannot be filled in by a
/// template link, so the checklist names both.
pub const ACCESS_TOKEN_URL: &str = "https://dash.cloudflare.com/profile/api-tokens";

/// Said before the token is saved, and next to every Access option.
pub const ACCOUNT_WIDE_WARNING: &str = "Both permissions are account-wide: this token can \
     change or remove the protection of every Access application in the Cloudflare account, not \
     only the ones Temps creates. That is why it is kept apart from the DNS and tunnel token.";

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AccessTokenCheckRequest {
    /// The token, as pasted. Used for this check only; not stored.
    pub token: temps_core::secret_string::SecretString,
    /// The Cloudflare account the Access applications live in (32 hex
    /// characters, from the dashboard URL or the account's overview page).
    pub account_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessPermission {
    AppsEdit,
    OrganizationsRead,
}

impl AccessPermission {
    pub fn label(self) -> &'static str {
        match self {
            AccessPermission::AppsEdit => "Account · Access: Apps and Policies · Edit",
            AccessPermission::OrganizationsRead => {
                "Account · Access: Organizations, Identity Providers, and Groups · Read"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AccessPermissionCheck {
    pub permission: AccessPermission,
    pub label: String,
    pub state: PermissionState,
    /// Why, and what to change on the token when it is not present.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AccessTokenCheckResult {
    /// Cloudflare accepted the token at all.
    pub valid: bool,
    /// Why it did not, or why the account could not be checked.
    pub error: Option<String>,
    /// `<team>.cloudflareaccess.com`, when Organizations Read is present.
    pub team_domain: Option<String>,
    pub permissions: Vec<AccessPermissionCheck>,
    /// Every permission is present and the team domain is known.
    pub ready: bool,
    /// What the token can do beyond Temps' own apps.
    pub warning: String,
    /// Where to create a token.
    pub token_url: String,
}

impl AccessTokenCheckResult {
    /// Why the token must not be saved, in words the user can act on.
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
        if self.team_domain.is_none() {
            return Some(
                "Cloudflare did not say which Access team this account uses. Set up Zero Trust \
                 for the account (it picks a team name), then check again."
                    .into(),
            );
        }
        None
    }
}

/// A Cloudflare account id: 32 hex characters.
pub fn valid_account_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The Apps Edit probe: an app with no domain.
pub fn apps_probe_body() -> Value {
    json!({ "type": "self_hosted", "name": "temps-permission-check", "domain": "" })
}

fn permission(
    permission: AccessPermission,
    state: PermissionState,
    detail: String,
) -> AccessPermissionCheck {
    AccessPermissionCheck {
        permission,
        label: permission.label().to_string(),
        state,
        detail,
    }
}

/// The answers to each call, assembled. `None` for a call not made.
pub fn assemble(
    verify: &Answer,
    organizations: Option<&Answer>,
    apps_probe: Option<&Answer>,
) -> AccessTokenCheckResult {
    let base = AccessTokenCheckResult {
        valid: false,
        error: None,
        team_domain: None,
        permissions: Vec::new(),
        ready: false,
        warning: ACCOUNT_WIDE_WARNING.to_string(),
        token_url: ACCESS_TOKEN_URL.to_string(),
    };
    if !verify.success || verify.denied() {
        return AccessTokenCheckResult {
            error: Some(format!(
                "Cloudflare did not accept the token ({}). Copy it again from Cloudflare; it is \
                 shown once, when it is created.",
                verify.message()
            )),
            ..base
        };
    }
    let organizations_state = organizations.map_or(PermissionState::Unverified, read_permission);
    let team_domain = organizations
        .filter(|_| organizations_state == PermissionState::Present)
        .and_then(|a| a.result.as_ref())
        .and_then(|r| r.get("auth_domain"))
        .and_then(Value::as_str)
        .and_then(temps_routes::access_keys::team_domain);
    let organizations_detail = match (organizations_state, &team_domain) {
        (PermissionState::Present, Some(team)) => format!("Access team {team}."),
        (PermissionState::Present, None) => {
            "Readable, but the account has no Access team domain yet.".to_string()
        }
        (PermissionState::Missing, _) => missing_detail(AccessPermission::OrganizationsRead),
        (PermissionState::Unverified, _) => {
            unverified_detail(AccessPermission::OrganizationsRead, organizations)
        }
    };
    let apps_state = apps_probe.map_or(PermissionState::Unverified, write_probe_permission);
    let apps_detail = match apps_state {
        PermissionState::Present => "Can create and remove Access applications.".to_string(),
        PermissionState::Missing => missing_detail(AccessPermission::AppsEdit),
        PermissionState::Unverified => unverified_detail(AccessPermission::AppsEdit, apps_probe),
    };
    let permissions = vec![
        permission(AccessPermission::AppsEdit, apps_state, apps_detail),
        permission(
            AccessPermission::OrganizationsRead,
            organizations_state,
            organizations_detail,
        ),
    ];
    let ready = team_domain.is_some()
        && permissions
            .iter()
            .all(|p| p.state == PermissionState::Present);
    AccessTokenCheckResult {
        valid: true,
        team_domain,
        permissions,
        ready,
        ..base
    }
}

fn missing_detail(permission: AccessPermission) -> String {
    format!(
        "The token lacks {}. Edit the token in Cloudflare and add it, then check again.",
        permission.label()
    )
}

fn unverified_detail(permission: AccessPermission, answer: Option<&Answer>) -> String {
    format!(
        "Temps could not confirm that this token has {}{}. Check the token, or create a new one.",
        permission.label(),
        answer
            .map(|a| format!(" (Cloudflare answered {})", a.message()))
            .unwrap_or_default()
    )
}

/// Check `token` for `account` against Cloudflare. Nothing is left created.
pub async fn check_access_token(
    http: &reqwest::Client,
    token: &str,
    account: &str,
) -> AccessTokenCheckResult {
    let token = token.trim();
    let account = account.trim().to_ascii_lowercase();
    if !valid_account_id(&account) {
        return AccessTokenCheckResult {
            valid: false,
            error: Some(format!(
                "'{account}' is not a Cloudflare account id: it is 32 hexadecimal characters, \
                 shown on the account's overview page."
            )),
            team_domain: None,
            permissions: Vec::new(),
            ready: false,
            warning: ACCOUNT_WIDE_WARNING.to_string(),
            token_url: ACCESS_TOKEN_URL.to_string(),
        };
    }
    // An account-owned token verifies only under its account; a user token
    // only under /user.
    let mut verify = call(
        http,
        token,
        reqwest::Method::GET,
        &format!("/accounts/{account}/tokens/verify"),
        None,
    )
    .await;
    if !verify.success {
        verify = call(
            http,
            token,
            reqwest::Method::GET,
            "/user/tokens/verify",
            None,
        )
        .await;
    }
    if !verify.success || verify.denied() {
        return assemble(&verify, None, None);
    }
    let organizations = call(
        http,
        token,
        reqwest::Method::GET,
        &format!("/accounts/{account}/access/organizations"),
        None,
    )
    .await;
    let probe = call(
        http,
        token,
        reqwest::Method::POST,
        &format!("/accounts/{account}/access/apps"),
        Some(apps_probe_body()),
    )
    .await;
    if let Some(id) = probe
        .result
        .as_ref()
        .filter(|_| probe.success)
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
    {
        let _ = call(
            http,
            token,
            reqwest::Method::DELETE,
            &format!("/accounts/{account}/access/apps/{id}"),
            None,
        )
        .await;
    }
    assemble(&verify, Some(&organizations), Some(&probe))
}

/// Why an Access address cannot be served off the tunnel (ADR 065 D9).
pub const ACCESS_TUNNEL_ONLY: &str = "This address is protected by Cloudflare Access, which only \
                                      sees traffic through the tunnel. Switch it to the Temps \
                                      sign-in first.";

/// An Access application, as far as Temps reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccessApp {
    pub id: String,
    pub name: String,
    /// Cloudflare's app type. Only a `self_hosted` app puts a sign-in in
    /// front of a hostname; a bookmark or SaaS app protects nothing here.
    #[serde(default)]
    pub app_type: String,
    /// The app's primary domain (`host` or `host/path`).
    pub domain: String,
    /// The AUD tag its JWTs carry.
    pub aud: String,
    /// Every domain it protects (`self_hosted_domains`, `destinations`).
    #[serde(default)]
    pub domains: Vec<String>,
    /// [`policy_fingerprint`] of its policies when read.
    #[serde(default)]
    pub policies: String,
}

/// A stable digest of an app's policies: what undo compares to tell
/// whether someone edited them since the run created the app.
pub fn policy_fingerprint(policies: Option<&Value>) -> String {
    let mut parts: Vec<String> = policies
        .and_then(Value::as_array)
        .map(|policies| {
            policies
                .iter()
                .map(|p| {
                    json!({
                        "id": p.get("id"),
                        "decision": p.get("decision"),
                        "include": p.get("include"),
                        "exclude": p.get("exclude"),
                        "require": p.get("require"),
                    })
                    .to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    parts.sort();
    hex::encode(Sha256::digest(parts.join("\n").as_bytes()))
}

/// Read one app from a Cloudflare answer's `result` (or one list item).
pub fn parse_app(value: &Value) -> Option<AccessApp> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let mut domains: Vec<String> = Vec::new();
    if let Some(list) = value.get("self_hosted_domains").and_then(Value::as_array) {
        domains.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(list) = value.get("destinations").and_then(Value::as_array) {
        domains.extend(
            list.iter()
                .filter(|d| {
                    d.get("type")
                        .and_then(Value::as_str)
                        .is_none_or(|t| t == "public")
                })
                .filter_map(|d| d.get("uri").and_then(Value::as_str))
                .map(str::to_string),
        );
    }
    let domain = text("domain").unwrap_or_default();
    if !domain.is_empty() {
        domains.push(domain.clone());
    }
    domains.sort();
    domains.dedup();
    Some(AccessApp {
        id: text("id")?,
        name: text("name").unwrap_or_default(),
        app_type: text("type").unwrap_or_default(),
        domain,
        aud: text("aud")?,
        domains,
        policies: policy_fingerprint(value.get("policies")),
    })
}

/// The app type that puts a sign-in in front of a hostname.
pub const SELF_HOSTED: &str = "self_hosted";

/// How an app's domain matches a host. Cloudflare enforces the most
/// specific app for a request, so an exact one wins over a wildcard, and
/// only its AUD is in the tokens the address receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Coverage {
    Exact,
    Wildcard,
}

/// Whether `app` is a `self_hosted` app protecting every path of `host`.
pub fn covers(app: &AccessApp, host: &str) -> bool {
    coverage(app, host).is_some()
}

/// How a `self_hosted` app's domains cover every path of `host`: the host
/// itself (optionally with `/` or `/*`), or a wildcard one label above it.
/// A path-scoped domain (`host/admin`) protects only part of the address,
/// so it does not count; neither does an app of any other type.
pub fn coverage(app: &AccessApp, host: &str) -> Option<Coverage> {
    if app.app_type != SELF_HOSTED {
        return None;
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    app.domains
        .iter()
        .filter_map(|domain| domain_coverage(domain, &host))
        .min()
}

/// The apps covering `host`, the one Cloudflare enforces first: exact
/// before wildcard, then by name and id so a plan is stable.
pub fn covering(apps: Vec<AccessApp>, host: &str) -> Vec<AccessApp> {
    let mut ranked: Vec<(Coverage, AccessApp)> = apps
        .into_iter()
        .filter_map(|app| coverage(&app, host).map(|c| (c, app)))
        .collect();
    ranked.sort_by(|(a, x), (b, y)| (a, &x.name, &x.id).cmp(&(b, &y.name, &y.id)));
    ranked.into_iter().map(|(_, app)| app).collect()
}

fn domain_coverage(domain: &str, host: &str) -> Option<Coverage> {
    let domain = domain.trim().to_ascii_lowercase();
    let domain = domain
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let domain = domain
        .strip_suffix("/*")
        .or_else(|| domain.strip_suffix('/'))
        .unwrap_or(domain);
    if domain.contains('/') {
        return None;
    }
    if domain == host {
        return Some(Coverage::Exact);
    }
    domain
        .strip_prefix("*.")
        .is_some_and(|parent| {
            host.split_once('.')
                .is_some_and(|(label, rest)| !label.is_empty() && rest == parent)
        })
        .then_some(Coverage::Wildcard)
}

/// The name of an app Temps creates for `host` on this install: the install
/// part, as in tunnel names, keeps two installs sharing an account apart.
pub fn app_name(host: &str, install: &str) -> String {
    let install: String = install
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if install.is_empty() {
        format!("temps-{host}")
    } else {
        format!("temps-{host}-{install}")
    }
}

/// The name a run gives the app it creates: [`app_name`] and the run. A
/// create that timed out is found again by name and domain (ADR 065 D9), so
/// the name has to be this run's alone, or an app another run, or someone,
/// made for the same address would be taken for it, and undo would then
/// delete it.
pub fn run_app_name(planned: &str, run_id: i32) -> String {
    format!("{planned}-r{run_id}")
}

/// Who an app Temps creates lets in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessPolicyAnswer {
    pub emails: Vec<String>,
    pub email_domains: Vec<String>,
}

impl AccessPolicyAnswer {
    /// Lower-cased, trimmed, de-duplicated; `@` stripped from domains.
    pub fn normalized(self) -> Self {
        let clean = |values: Vec<String>, strip_at: bool| {
            let mut out: Vec<String> = values
                .into_iter()
                .map(|v| {
                    let v = v.trim().to_ascii_lowercase();
                    if strip_at {
                        v.trim_start_matches('@').to_string()
                    } else {
                        v
                    }
                })
                .filter(|v| !v.is_empty())
                .collect();
            out.sort();
            out.dedup();
            out
        };
        Self {
            emails: clean(self.emails, false),
            email_domains: clean(self.email_domains, true),
        }
    }

    /// Why it cannot make a policy, or `None`.
    pub fn problem(&self) -> Option<String> {
        if self.emails.is_empty() && self.email_domains.is_empty() {
            return Some(
                "Say who Cloudflare Access lets in: at least one email address or email domain."
                    .into(),
            );
        }
        if let Some(bad) = self.emails.iter().find(|e| {
            let Some((local, domain)) = e.split_once('@') else {
                return true;
            };
            local.is_empty() || !domain.contains('.') || e.contains(char::is_whitespace)
        }) {
            return Some(format!("'{bad}' is not an email address."));
        }
        if let Some(bad) = self
            .email_domains
            .iter()
            .find(|d| !d.contains('.') || d.contains(['@', '/', ' ', '*']))
        {
            return Some(format!(
                "'{bad}' is not an email domain such as example.com."
            ));
        }
        None
    }
}

/// The body that creates the app for `host`, with one allow policy.
pub fn app_body(name: &str, host: &str, policy: &AccessPolicyAnswer) -> Value {
    let mut include: Vec<Value> = policy
        .emails
        .iter()
        .map(|email| json!({ "email": { "email": email } }))
        .collect();
    include.extend(
        policy
            .email_domains
            .iter()
            .map(|domain| json!({ "email_domain": { "domain": domain } })),
    );
    json!({
        "name": name,
        "type": "self_hosted",
        "domain": host,
        "session_duration": "24h",
        "app_launcher_visible": false,
        "policies": [{
            "name": format!("{name} allow"),
            "decision": "allow",
            "include": include,
        }],
    })
}

/// Errors from the Access applications API, with what was being done.
fn api_error(what: &str, answer: &Answer) -> String {
    format!("Cloudflare refused to {what}: {}", answer.message())
}

/// Every Access app in the account (at most 1,000, ten pages).
pub async fn list_apps(
    http: &reqwest::Client,
    token: &str,
    account: &str,
) -> Result<Vec<AccessApp>, String> {
    let mut apps = Vec::new();
    for page in 1..=10 {
        let answer = call(
            http,
            token,
            reqwest::Method::GET,
            &format!("/accounts/{account}/access/apps?per_page=100&page={page}"),
            None,
        )
        .await;
        if !answer.success {
            return Err(api_error("list the Access applications", &answer));
        }
        let items = answer
            .result
            .as_ref()
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let count = items.len();
        apps.extend(items.iter().filter_map(parse_app));
        if count < 100 {
            break;
        }
    }
    Ok(apps)
}

/// One app by id; `None` when Cloudflare says it does not exist.
pub async fn get_app(
    http: &reqwest::Client,
    token: &str,
    account: &str,
    id: &str,
) -> Result<Option<AccessApp>, String> {
    let answer = call(
        http,
        token,
        reqwest::Method::GET,
        &format!("/accounts/{account}/access/apps/{id}"),
        None,
    )
    .await;
    if answer.status == 404 {
        return Ok(None);
    }
    if !answer.success {
        return Err(api_error(&format!("read Access application {id}"), &answer));
    }
    Ok(answer.result.as_ref().and_then(parse_app))
}

/// Why creating an app failed, and whether Cloudflare may have created it
/// anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAppError {
    pub message: String,
    /// No answer, a server error, or an answer Temps could not read: the
    /// app may exist. `false` when Cloudflare refused the request.
    pub maybe_applied: bool,
}

pub async fn create_app(
    http: &reqwest::Client,
    token: &str,
    account: &str,
    body: Value,
) -> Result<AccessApp, CreateAppError> {
    let answer = call(
        http,
        token,
        reqwest::Method::POST,
        &format!("/accounts/{account}/access/apps"),
        Some(body),
    )
    .await;
    if !answer.success {
        return Err(CreateAppError {
            message: api_error("create the Access application", &answer),
            maybe_applied: !(400..500).contains(&answer.status),
        });
    }
    answer
        .result
        .as_ref()
        .and_then(parse_app)
        .ok_or_else(|| CreateAppError {
            message: "Cloudflare created the Access application but returned no id or AUD.".into(),
            maybe_applied: true,
        })
}

/// Delete app `id`. Gone already is done.
pub async fn delete_app(
    http: &reqwest::Client,
    token: &str,
    account: &str,
    id: &str,
) -> Result<(), String> {
    let answer = call(
        http,
        token,
        reqwest::Method::DELETE,
        &format!("/accounts/{account}/access/apps/{id}"),
        None,
    )
    .await;
    if answer.success || answer.status == 404 {
        Ok(())
    } else {
        Err(api_error(
            &format!("delete Access application {id}"),
            &answer,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERIFY_OK: &str =
        r#"{"result":{"id":"t1","status":"active"},"success":true,"errors":[],"messages":[]}"#;
    const VERIFY_INVALID: &str = r#"{"success":false,"errors":[{"code":1000,"message":"Invalid API Token"}],"messages":[],"result":null}"#;
    const ORGS_OK: &str = r#"{"result":{"name":"Team","auth_domain":"team.cloudflareaccess.com"},"success":true,"errors":[],"messages":[]}"#;
    const ORGS_ODD_DOMAIN: &str = r#"{"result":{"name":"Team","auth_domain":"login.example.com"},"success":true,"errors":[],"messages":[]}"#;
    const NOT_AUTHORIZED: &str = r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"messages":[],"result":null}"#;
    const APPS_INVALID: &str = r#"{"success":false,"errors":[{"code":12130,"message":"domain is required"}],"messages":[],"result":null}"#;
    const SERVER_ERROR: &str = r#"{"success":false,"errors":[],"messages":[],"result":null}"#;

    fn answer(status: u16, body: &str) -> Answer {
        Answer::from_body(status, &serde_json::from_str(body).expect("recorded body"))
    }

    fn state(result: &AccessTokenCheckResult, permission: AccessPermission) -> PermissionState {
        result
            .permissions
            .iter()
            .find(|p| p.permission == permission)
            .map(|p| p.state)
            .expect("listed")
    }

    #[test]
    fn a_token_with_both_permissions_is_ready_and_names_the_team() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ORGS_OK)),
            Some(&answer(400, APPS_INVALID)),
        );
        assert!(result.ready, "{result:?}");
        assert_eq!(
            result.team_domain.as_deref(),
            Some("team.cloudflareaccess.com")
        );
        assert_eq!(result.refusal(), None);
        assert!(result.warning.contains("account-wide"));
    }

    #[test]
    fn each_missing_permission_is_named() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(403, NOT_AUTHORIZED)),
            Some(&answer(403, NOT_AUTHORIZED)),
        );
        assert!(!result.ready);
        assert_eq!(
            state(&result, AccessPermission::AppsEdit),
            PermissionState::Missing
        );
        assert_eq!(
            state(&result, AccessPermission::OrganizationsRead),
            PermissionState::Missing
        );
        let refusal = result.refusal().expect("refused");
        assert!(refusal.contains("Apps and Policies · Edit"), "{refusal}");
        assert!(refusal.contains("Organizations"), "{refusal}");
    }

    #[test]
    fn an_answer_that_proves_nothing_is_unverified_and_refused() {
        let result = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ORGS_OK)),
            Some(&answer(500, SERVER_ERROR)),
        );
        assert_eq!(
            state(&result, AccessPermission::AppsEdit),
            PermissionState::Unverified
        );
        assert!(!result.ready && result.refusal().is_some());
    }

    #[test]
    fn a_rejected_token_or_a_team_domain_that_is_not_cloudflares_is_refused() {
        let rejected = assemble(&answer(401, VERIFY_INVALID), None, None);
        assert!(!rejected.valid && rejected.permissions.is_empty());
        assert!(rejected
            .refusal()
            .expect("refused")
            .contains("Invalid API Token"));
        // Keys are only ever fetched from <team>.cloudflareaccess.com.
        let odd = assemble(
            &answer(200, VERIFY_OK),
            Some(&answer(200, ORGS_ODD_DOMAIN)),
            Some(&answer(400, APPS_INVALID)),
        );
        assert_eq!(odd.team_domain, None);
        assert!(!odd.ready);
        assert!(odd.refusal().expect("refused").contains("Zero Trust"));
    }

    #[test]
    fn account_ids_are_32_hex_characters() {
        assert!(valid_account_id("01a7362d577a6c3019a474fd6f485823"));
        assert!(!valid_account_id("01a7362d577a6c3019a474fd6f48582"));
        assert!(!valid_account_id("../../user/tokens/verify00000000"));
    }

    fn app(domains: &[&str]) -> AccessApp {
        AccessApp {
            id: "a1".into(),
            name: "Grafana".into(),
            app_type: SELF_HOSTED.into(),
            domain: domains.first().map(|d| d.to_string()).unwrap_or_default(),
            aud: "aud".into(),
            domains: domains.iter().map(|d| d.to_string()).collect(),
            policies: String::new(),
        }
    }

    #[test]
    fn an_app_covers_a_host_only_for_every_path() {
        assert!(covers(&app(&["app.example.com"]), "App.Example.com."));
        assert!(covers(&app(&["app.example.com/*"]), "app.example.com"));
        assert!(covers(&app(&["*.example.com"]), "app.example.com"));
        assert!(!covers(&app(&["*.example.com"]), "a.b.example.com"));
        assert!(!covers(&app(&["*.example.com"]), "example.com"));
        assert!(!covers(&app(&["app.example.com/admin"]), "app.example.com"));
        assert!(!covers(&app(&["other.example.com"]), "app.example.com"));
        assert!(covers(
            &app(&["other.example.com", "app.example.com"]),
            "app.example.com"
        ));
    }

    /// ADR 065 D9: the app bound is the one Cloudflare enforces for the
    /// hostname, or every request would carry another app's AUD.
    #[test]
    fn only_self_hosted_apps_count_and_the_exact_one_wins() {
        let typed = |id: &str, name: &str, kind: &str, domain: &str| AccessApp {
            id: id.into(),
            name: name.into(),
            app_type: kind.into(),
            ..app(&[domain])
        };
        assert!(!covers(
            &typed("b", "Links", "bookmark", "app.example.com"),
            "app.example.com"
        ));
        assert!(!covers(
            &typed("s", "SaaS", "saas", "app.example.com"),
            "app.example.com"
        ));
        let ranked = covering(
            vec![
                typed("w", "A wildcard", SELF_HOSTED, "*.example.com"),
                typed("b", "A bookmark", "bookmark", "app.example.com"),
                typed("e", "Zed exact", SELF_HOSTED, "app.example.com/*"),
            ],
            "app.example.com",
        );
        let ids: Vec<&str> = ranked.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["e", "w"]);
        assert_eq!(
            coverage(
                &typed("w", "W", SELF_HOSTED, "*.example.com"),
                "app.example.com"
            ),
            Some(Coverage::Wildcard)
        );
    }

    #[test]
    fn apps_are_read_with_every_domain_and_a_policy_fingerprint() {
        let value = json!({
            "id": "a1", "name": "Grafana", "aud": "aud1", "domain": "grafana.example.com",
            "self_hosted_domains": ["grafana.example.com", "g.example.com"],
            "destinations": [{"type": "public", "uri": "g2.example.com"}, {"type": "private", "cidr": "10.0.0.0/8"}],
            "policies": [{"id": "p1", "decision": "allow", "include": [{"email": {"email": "a@example.com"}}]}]
        });
        let parsed = parse_app(&value).expect("app");
        assert_eq!(
            parsed.domains,
            vec!["g.example.com", "g2.example.com", "grafana.example.com"]
        );
        let mut edited = value.clone();
        edited["policies"][0]["include"] = json!([{"everyone": {}}]);
        assert_ne!(parse_app(&edited).expect("app").policies, parsed.policies);
        assert!(parse_app(&json!({"name": "no id"})).is_none());
    }

    #[test]
    fn the_policy_answer_is_cleaned_and_checked() {
        let answer = AccessPolicyAnswer {
            emails: vec![" A@Example.com ".into(), "a@example.com".into()],
            email_domains: vec!["@Example.org".into(), "".into()],
        }
        .normalized();
        assert_eq!(answer.emails, vec!["a@example.com"]);
        assert_eq!(answer.email_domains, vec!["example.org"]);
        assert_eq!(answer.problem(), None);
        assert!(AccessPolicyAnswer::default().problem().is_some());
        for bad in ["nobody", "@example.com", "a@localhost"] {
            let answer = AccessPolicyAnswer {
                emails: vec![bad.into()],
                email_domains: vec![],
            };
            assert!(answer.problem().is_some(), "{bad}");
        }
        let everyone = AccessPolicyAnswer {
            emails: vec![],
            email_domains: vec!["*".into()],
        };
        assert!(everyone.problem().is_some());
    }

    #[test]
    fn the_created_app_is_self_hosted_for_the_host_with_one_allow_policy() {
        let body = app_body(
            &app_name("app.example.com.", "1a2b3c4d"),
            "app.example.com",
            &AccessPolicyAnswer {
                emails: vec!["a@example.com".into()],
                email_domains: vec!["example.org".into()],
            },
        );
        assert_eq!(body["name"], "temps-app.example.com-1a2b3c4d");
        assert_eq!(body["type"], "self_hosted");
        assert_eq!(body["domain"], "app.example.com");
        let policies = body["policies"].as_array().expect("policies");
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0]["decision"], "allow");
        assert_eq!(policies[0]["include"].as_array().map(Vec::len), Some(2));
    }
}
