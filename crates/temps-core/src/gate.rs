// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The signed-in gate (ADR 065 D7/D8), shared by every listener.
//!
//! An address whose audience is `signed_in` is served with a gate in front of
//! the app. The control-plane proxy and the agent's ingress both enforce it,
//! and both call into this module so the two cannot drift: the cookie names
//! and attributes, the token format and its checks, PKCE, the state cookie,
//! the logout guard and the decision for a request all live here.
//!
//! What the listeners do themselves is only the transport: the proxy redeems
//! a sign-in code in process, an agent over its node-token channel.
//!
//! The token is `v1.<claims>.<signature>`, both parts base64url without
//! padding. The signature is Ed25519 over `temps-gate-v1.<claims>` by the
//! control plane's gate key; the public half travels in the route snapshot.

use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, Method};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer};
pub use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use utoipa::ToSchema;

/// The session cookie. `__Host-` makes the browser refuse it unless it is
/// `Secure`, `Path=/` and has no `Domain`, so it is never sent to a sibling
/// or child hostname.
pub const GATE_COOKIE: &str = "__Host-temps_gate";
/// The sign-in state cookie: `state` and the PKCE verifier, 10 minutes.
pub const GATE_STATE_COOKIE: &str = "__Host-temps_gate_state";
/// Response header naming why the gate answered instead of the app.
pub const GATE_HEADER: &str = "x-temps-gate";
/// `x-temps-gate` value on a sign-in answer (302 or 401).
pub const GATE_SIGN_IN: &str = "sign-in";
/// `x-temps-gate` value when Cloudflare Access verification fails (D9).
pub const GATE_ACCESS: &str = "access";
/// `x-temps-gate` value when the gate cannot run (no key, unknown gate).
pub const GATE_UNAVAILABLE: &str = "unavailable";
/// The opaque user id the app receives behind the gate.
pub const USER_HEADER: &str = "x-temps-user";
/// The signed-in user's email the app receives behind the gate.
pub const USER_EMAIL_HEADER: &str = "x-temps-user-email";
/// Headers only the gate may set on a request to the app. Stripped from
/// client input on every listener, gated host or not.
pub const IDENTITY_HEADERS: [&str; 2] = [USER_HEADER, USER_EMAIL_HEADER];
/// Every identity header Temps sets for an app starts with this: the gate's
/// [`IDENTITY_HEADERS`] and the tailnet login (`x-temps-user-login`). No
/// client copy of any name under it is ever passed on, so a name Temps adds
/// later is covered too.
pub const IDENTITY_HEADER_PREFIX: &str = "x-temps-user";
/// Paths owned by the gate on a gated host.
pub const GATE_PATH_PREFIX: &str = "/_temps/gate";
pub const CALLBACK_PATH: &str = "/_temps/gate/callback";
pub const LOGOUT_PATH: &str = "/_temps/gate/logout";
/// Console path that starts a sign-in.
pub const AUTHORIZE_PATH: &str = "/_temps/gate/authorize";
/// Capability an agent declares when it enforces the gate. A request that
/// does not declare it never receives a signed-in route.
pub const GATE_CAPABILITY: &str = "gate.v1";
/// The header an agent names its capabilities in on every request it makes
/// to the control plane, comma-separated; shared with ADR 065 D6's
/// `tailnet_identity.v1`.
pub use crate::tailnet::AGENT_CAPABILITIES_HEADER;

/// Whether any of a request's [`AGENT_CAPABILITIES_HEADER`] values names
/// `capability` exactly. Every value is read, so one header per capability
/// is understood as well as one comma-separated list.
pub fn declares_capability<'a>(
    values: impl IntoIterator<Item = &'a str>,
    capability: &str,
) -> bool {
    values
        .into_iter()
        .flat_map(|value| value.split(','))
        .any(|item| item.trim() == capability)
}

/// How long a sign-in may take between the redirect and the callback.
pub const STATE_TTL: Duration = Duration::from_secs(10 * 60);
/// How long a sign-in code is valid.
pub const CODE_TTL: Duration = Duration::from_secs(60);
/// The longest a gate session lives, whatever the console session's expiry.
pub const SESSION_MAX_TTL: Duration = Duration::from_secs(8 * 60 * 60);
/// Clock skew tolerated on `iat`.
const CLOCK_SKEW_SECS: i64 = 60;
/// Anything longer than this is not a gate token.
const MAX_TOKEN_LEN: usize = 4096;
const TOKEN_VERSION: &str = "v1";
const SIGNING_CONTEXT: &str = "temps-gate-v1.";
const STATE_VERSION: &str = "v1";
/// The longest return path the state cookie carries. Base64 makes it a
/// third longer, and the cookie has to stay inside the browser's 4 KB limit
/// for one cookie and [`MAX_TOKEN_LEN`], or the callback never sees it and
/// sign-in from that address fails every time. A longer path returns to `/`.
const MAX_RETURN_TO_LEN: usize = 2048;

#[derive(Debug, Error)]
pub enum GateError {
    #[error("Failed to generate random bytes for the gate {operation}: {reason}")]
    Randomness { operation: String, reason: String },
    #[error("Gate signing key is invalid: {reason}")]
    InvalidKey { reason: String },
    #[error("Failed to encode gate token claims for address {address_id}: {reason}")]
    Encode { address_id: String, reason: String },
}

/// Which gate an address has (D7). Unknown values read as `Unsupported`, and
/// an unsupported gate admits nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    #[default]
    Temps,
    CloudflareAccess,
    #[serde(other)]
    Unsupported,
}

impl GateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GateKind::Temps => "temps",
            GateKind::CloudflareAccess => "cloudflare_access",
            GateKind::Unsupported => "unsupported",
        }
    }

    /// Parse a stored value. Unknown values fail closed.
    pub fn parse(value: &str) -> GateKind {
        match value {
            "temps" => GateKind::Temps,
            "cloudflare_access" => GateKind::CloudflareAccess,
            _ => GateKind::Unsupported,
        }
    }
}

/// The gate for one address, as the route snapshot carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatePolicy {
    /// `custom:<id>` or `environment:<id>`: a token is valid only for its
    /// own address.
    pub address_id: String,
    pub gate: GateKind,
    /// Present for the Temps gate. Missing means the gate cannot run and the
    /// address answers `503` rather than open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temps: Option<TempsGate>,
    /// Present for Cloudflare Access (D9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessGate>,
}

/// What a listener needs to run the Temps gate for one address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TempsGate {
    /// The gate key's public half, base64url (32 bytes).
    pub public_key: String,
    /// `https://<console>/_temps/gate/authorize`.
    pub authorize_url: String,
    /// Tokens issued before this time (unix seconds) are refused: set when
    /// the audience, gate or list changes, or an admin signs everyone out.
    #[serde(default)]
    pub not_before: i64,
    /// Ids of revoked sessions that have not expired yet.
    #[serde(default)]
    pub revoked: Vec<String>,
}

/// What a listener needs to verify Cloudflare Access for one address (D9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccessGate {
    /// `https://<team>.cloudflareaccess.com`, the JWT issuer.
    pub issuer: String,
    /// The Access application's AUD tag.
    pub audience: String,
    /// The team's signing keys (RS256 only).
    #[serde(default)]
    pub keys: Vec<AccessKey>,
}

/// One RSA public key from the team's certs endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccessKey {
    pub kid: String,
    /// Modulus, base64url.
    pub n: String,
    /// Exponent, base64url.
    pub e: String,
}

/// Who may sign in to a Temps-gated address besides being able to see the
/// project (ADR 065 D7): stored in `gate_allow`. Empty lists mean no list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GateAllowList {
    #[serde(default)]
    pub teams: Vec<i32>,
    #[serde(default)]
    pub users: Vec<i32>,
}

impl GateAllowList {
    pub fn is_empty(&self) -> bool {
        self.teams.is_empty() && self.users.is_empty()
    }

    /// Sorted and deduplicated, so equal lists compare equal.
    pub fn normalized(mut self) -> GateAllowList {
        self.teams.sort_unstable();
        self.teams.dedup();
        self.users.sort_unstable();
        self.users.dedup();
        self
    }

    /// Whether `user_id`, a member of `user_teams`, is on the list. An empty
    /// list admits everyone with project access.
    pub fn admits(&self, user_id: i32, user_teams: &[i32]) -> bool {
        self.is_empty()
            || self.users.contains(&user_id)
            || user_teams.iter().any(|team| self.teams.contains(team))
    }
}

/// What a gate token says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateClaims {
    /// Gate session id (`gate_sessions.id`).
    pub sid: String,
    /// Console user id.
    pub uid: i32,
    /// The user's email when the session was created, passed to the app.
    pub email: String,
    /// Address id the token was issued for.
    pub aid: String,
    /// Issued at, unix seconds.
    pub iat: i64,
    /// Expiry, unix seconds.
    pub exp: i64,
}

/// Why a token was refused. Only ever logged at debug: the visitor is sent
/// to sign in whatever the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRejection {
    Malformed,
    BadKey,
    BadSignature,
    WrongAddress,
    Expired,
    IssuedInFuture,
    BeforeNotBefore,
    Revoked,
}

/// Sign `claims` with the gate key.
pub fn sign_token(key: &SigningKey, claims: &GateClaims) -> Result<String, GateError> {
    let json = serde_json::to_vec(claims).map_err(|error| GateError::Encode {
        address_id: claims.aid.clone(),
        reason: error.to_string(),
    })?;
    let payload = URL_SAFE_NO_PAD.encode(json);
    let signature = key.sign(format!("{SIGNING_CONTEXT}{payload}").as_bytes());
    Ok(format!(
        "{TOKEN_VERSION}.{payload}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

/// Decode a base64url Ed25519 public key.
pub fn decode_public_key(encoded: &str) -> Option<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded.trim()).ok()?;
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Encode a public key the way the snapshot carries it.
pub fn encode_public_key(key: &VerifyingKey) -> String {
    URL_SAFE_NO_PAD.encode(key.to_bytes())
}

/// Generate a new gate key from the OS CSPRNG.
pub fn generate_signing_key() -> Result<SigningKey, GateError> {
    let mut secret = [0u8; 32];
    random_bytes("signing key", &mut secret)?;
    Ok(SigningKey::from_bytes(&secret))
}

/// Restore a signing key from its 32 secret bytes.
pub fn signing_key_from_bytes(bytes: &[u8]) -> Result<SigningKey, GateError> {
    let secret: [u8; 32] = bytes.try_into().map_err(|_| GateError::InvalidKey {
        reason: format!("expected 32 bytes, got {}", bytes.len()),
    })?;
    Ok(SigningKey::from_bytes(&secret))
}

/// Check a token's signature only, for the control plane, which reads the
/// claims of a token a listener hands back (logout) before revoking it.
pub fn verify_signature(token: &str, key: &VerifyingKey) -> Result<GateClaims, TokenRejection> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(TokenRejection::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(version), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenRejection::Malformed);
    };
    if version != TOKEN_VERSION {
        return Err(TokenRejection::Malformed);
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .ok_or(TokenRejection::Malformed)?;
    key.verify_strict(format!("{SIGNING_CONTEXT}{payload}").as_bytes(), &signature)
        .map_err(|_| TokenRejection::BadSignature)?;
    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| TokenRejection::Malformed)?;
    serde_json::from_slice(&json).map_err(|_| TokenRejection::Malformed)
}

/// Every check a listener makes on a gate token (D8): signature, that it was
/// issued for this address, expiry, `not_before` and revoked ids.
pub fn verify_token(
    token: &str,
    address_id: &str,
    gate: &TempsGate,
    now: i64,
) -> Result<GateClaims, TokenRejection> {
    let key = decode_public_key(&gate.public_key).ok_or(TokenRejection::BadKey)?;
    let claims = verify_signature(token, &key)?;
    if claims.aid != address_id {
        return Err(TokenRejection::WrongAddress);
    }
    if claims.exp <= now {
        return Err(TokenRejection::Expired);
    }
    if claims.iat > now + CLOCK_SKEW_SECS {
        return Err(TokenRejection::IssuedInFuture);
    }
    if claims.iat < gate.not_before {
        return Err(TokenRejection::BeforeNotBefore);
    }
    if gate.revoked.iter().any(|id| id == &claims.sid) {
        return Err(TokenRejection::Revoked);
    }
    Ok(claims)
}

fn random_bytes(operation: &str, bytes: &mut [u8]) -> Result<(), GateError> {
    crate::ecies::fill_secure_random_bytes(operation, bytes).map_err(|error| {
        GateError::Randomness {
            operation: operation.to_string(),
            reason: error.to_string(),
        }
    })
}

fn random_token(operation: &str) -> Result<String, GateError> {
    let mut bytes = [0u8; 32];
    random_bytes(operation, &mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// A new PKCE verifier: 32 random bytes, base64url (43 characters).
pub fn new_verifier() -> Result<String, GateError> {
    random_token("PKCE verifier")
}

/// A new random value, base64url: for `state` and for sign-in codes.
pub fn new_random_value(operation: &str) -> Result<String, GateError> {
    random_token(operation)
}

/// The S256 PKCE challenge for `verifier`.
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// RFC 7636 verifier syntax: 43–128 unreserved characters.
pub fn valid_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// A base64url value of 43 characters (32 bytes): `state`, a challenge, a code.
pub fn valid_random_value(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// What the state cookie holds between the redirect and the callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateState {
    pub state: String,
    pub verifier: String,
    /// Same-origin path to return to after sign-in, already sanitised.
    pub return_to: String,
    /// Unix seconds.
    pub expires_at: i64,
}

impl GateState {
    pub fn new(return_to: &str, now: i64) -> Result<GateState, GateError> {
        Ok(GateState {
            state: new_random_value("sign-in state")?,
            verifier: new_verifier()?,
            return_to: bounded_return_to(return_to).to_string(),
            expires_at: now + STATE_TTL.as_secs() as i64,
        })
    }

    pub fn encode(&self) -> String {
        format!(
            "{STATE_VERSION}.{}.{}.{}.{}",
            self.state,
            self.verifier,
            self.expires_at,
            URL_SAFE_NO_PAD.encode(self.return_to.as_bytes())
        )
    }

    /// Parse a state cookie, refusing anything malformed or expired.
    pub fn decode(value: &str, now: i64) -> Option<GateState> {
        if value.len() > MAX_TOKEN_LEN {
            return None;
        }
        let mut parts = value.split('.');
        let (Some(version), Some(state), Some(verifier), Some(expires_at), Some(return_to), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        if version != STATE_VERSION || !valid_random_value(state) || !valid_verifier(verifier) {
            return None;
        }
        let expires_at: i64 = expires_at.parse().ok()?;
        if expires_at <= now || expires_at > now + STATE_TTL.as_secs() as i64 + CLOCK_SKEW_SECS {
            return None;
        }
        let return_to = String::from_utf8(URL_SAFE_NO_PAD.decode(return_to).ok()?).ok()?;
        Some(GateState {
            state: state.to_string(),
            verifier: verifier.to_string(),
            return_to: sanitize_return_to(&return_to).to_string(),
            expires_at,
        })
    }
}

/// A same-origin path, or `/` (the password wall's rule).
pub fn sanitize_return_to(path: &str) -> &str {
    crate::preview_grant::sanitize_preview_next_ref(path)
}

/// [`sanitize_return_to`], and `/` for a path too long for the state cookie
/// ([`MAX_RETURN_TO_LEN`]).
fn bounded_return_to(path: &str) -> &str {
    let path = sanitize_return_to(path);
    if path.len() > MAX_RETURN_TO_LEN {
        "/"
    } else {
        path
    }
}

/// `Set-Cookie` for the gate session, valid for `max_age_secs`.
pub fn session_cookie(token: &str, max_age_secs: i64) -> String {
    format!(
        "{GATE_COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
        max_age_secs.max(0)
    )
}

/// `Set-Cookie` that clears the gate session.
pub fn clear_session_cookie() -> String {
    format!("{GATE_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// `Set-Cookie` for the sign-in state. `Lax` so it comes back on the
/// top-level redirect from the console.
pub fn state_cookie(state: &GateState) -> String {
    format!(
        "{GATE_STATE_COOKIE}={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
        state.encode(),
        STATE_TTL.as_secs()
    )
}

/// `Set-Cookie` that clears the sign-in state (single use).
pub fn clear_state_cookie() -> String {
    format!("{GATE_STATE_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0")
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = value.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Read one query parameter. Repeated parameters are refused, so a callback
/// cannot smuggle a second `code`.
pub fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    let mut found = None;
    for pair in query.unwrap_or_default().split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if percent_decode(key).as_deref() == Some(name) {
            if found.is_some() {
                return None;
            }
            found = Some(percent_decode(value)?);
        }
    }
    found
}

/// The console URL a visitor is sent to (D8 step 1).
pub fn authorize_location(
    authorize_url: &str,
    address_id: &str,
    state: &str,
    challenge: &str,
) -> String {
    format!(
        "{authorize_url}?address={}&state={}&code_challenge={}&code_challenge_method=S256",
        percent_encode(address_id),
        percent_encode(state),
        percent_encode(challenge)
    )
}

/// The callback URL on the address (D8 step 2). The host comes from the
/// server's own record of the address, never from the request.
pub fn callback_location(hostname: &str, code: &str, state: &str) -> String {
    format!(
        "https://{hostname}{CALLBACK_PATH}?code={}&state={}",
        percent_encode(code),
        percent_encode(state)
    )
}

/// Whether `path` belongs to the gate on a gated host.
pub fn is_gate_path(path: &str) -> bool {
    path == GATE_PATH_PREFIX
        || path
            .strip_prefix(GATE_PATH_PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Every value of cookie `name` across all `Cookie` fields.
pub fn cookie_values<'a>(headers: &'a HeaderMap, name: &str) -> Vec<&'a str> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|field| field.to_str().ok())
        .flat_map(|field| field.split(';'))
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then_some(value.trim())
        })
        .collect()
}

fn is_gate_cookie_name(name: &str) -> bool {
    let name = name.trim();
    name.eq_ignore_ascii_case(GATE_COOKIE) || name.eq_ignore_ascii_case(GATE_STATE_COOKIE)
}

/// Whether any `Cookie` field may hold a gate cookie: a check that
/// allocates nothing, so requests without one pass untouched.
pub fn may_hold_gate_cookie(headers: &HeaderMap) -> bool {
    const NEEDLE: &[u8] = b"__host-temps_gate";
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .any(|field| {
            field
                .as_bytes()
                .windows(NEEDLE.len())
                .any(|window| window.eq_ignore_ascii_case(NEEDLE))
        })
}

/// Remove the gate cookies from the request before the app sees it. Leaves a
/// single `Cookie` field with the rest, or none if nothing is left. A request
/// without a gate cookie is left as it is.
pub fn strip_gate_cookies(headers: &mut HeaderMap) {
    if !may_hold_gate_cookie(headers) {
        return;
    }
    let fields: Vec<String> = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .map(|field| String::from_utf8_lossy(field.as_bytes()).into_owned())
        .collect();
    if fields.is_empty() {
        return;
    }
    let kept: Vec<&str> = fields
        .iter()
        .flat_map(|field| field.split(';'))
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !is_gate_cookie_name(pair.split_once('=').map_or(*pair, |(k, _)| k)))
        .collect();
    headers.remove(axum::http::header::COOKIE);
    if !kept.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&kept.join("; ")) {
            headers.insert(axum::http::header::COOKIE, value);
        }
    }
}

/// Whether an app's `Set-Cookie` names a gate cookie. Such a header is
/// dropped: the app can neither set nor clear the gate's cookies.
pub fn set_cookie_names_gate(value: &[u8]) -> bool {
    let value = String::from_utf8_lossy(value);
    let name = value.split(';').next().unwrap_or_default();
    let name = name.split_once('=').map_or(name, |(k, _)| k);
    is_gate_cookie_name(name)
}

/// Remove every `Set-Cookie` naming a gate cookie from an app's response.
pub fn drop_gate_set_cookies(headers: &mut HeaderMap) {
    let set_cookie = axum::http::header::SET_COOKIE;
    if !headers
        .get_all(&set_cookie)
        .iter()
        .any(|v| set_cookie_names_gate(v.as_bytes()))
    {
        return;
    }
    let kept: Vec<HeaderValue> = headers
        .get_all(&set_cookie)
        .iter()
        .filter(|v| !set_cookie_names_gate(v.as_bytes()))
        .cloned()
        .collect();
    headers.remove(&set_cookie);
    for value in kept {
        headers.append(&set_cookie, value);
    }
}

/// Whether `name` is under [`IDENTITY_HEADER_PREFIX`]. Lower-case `name`,
/// as `HeaderName` always is.
pub fn is_user_identity_header(name: &str) -> bool {
    name.starts_with(IDENTITY_HEADER_PREFIX)
}

/// Remove every identity header a client sent: only Temps sets them.
pub fn strip_identity_headers(headers: &mut HeaderMap) {
    if !headers
        .keys()
        .any(|name| is_user_identity_header(name.as_str()))
    {
        return;
    }
    let names: Vec<_> = headers
        .keys()
        .filter(|name| is_user_identity_header(name.as_str()))
        .cloned()
        .collect();
    for name in names {
        headers.remove(name);
    }
}

/// The identity headers for an admitted request.
pub fn identity_headers(claims: &GateClaims) -> Vec<(&'static str, HeaderValue)> {
    let mut out = Vec::with_capacity(2);
    if let Ok(value) = HeaderValue::from_str(&claims.uid.to_string()) {
        out.push((USER_HEADER, value));
    }
    if let Ok(value) = HeaderValue::from_str(&claims.email) {
        out.push((USER_EMAIL_HEADER, value));
    }
    out
}

/// Whether a request is a browser navigation, so a sign-in answer can be a
/// redirect. Anything else gets `401`, so an API call is never redirected
/// into an HTML page (D8 step 1).
pub fn is_navigation(method: &Method, headers: &HeaderMap) -> bool {
    if method != Method::GET {
        return false;
    }
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(mode) = header("sec-fetch-mode") {
        return mode.eq_ignore_ascii_case("navigate");
    }
    header("accept").is_some_and(|accept| {
        accept.split(',').any(|part| {
            part.split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("text/html"))
        })
    })
}

/// The logout guard (D8): `Sec-Fetch-Site: same-origin`, or with no Fetch
/// Metadata at all, an `Origin` equal to the address.
pub fn logout_allowed(headers: &HeaderMap, hostname: &str) -> bool {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(site) = header("sec-fetch-site") {
        return site.eq_ignore_ascii_case("same-origin");
    }
    header("origin").is_some_and(|origin| {
        origin
            .trim_end_matches('/')
            .eq_ignore_ascii_case(&format!("https://{hostname}"))
    })
}

/// What a listener does with a request to a host with a Temps gate, before
/// it reaches the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Valid token: strip gate cookies, set identity headers, forward.
    Admit(GateClaims),
    /// Send the visitor to sign in. `redirect` is `Some` for a navigation
    /// (`302` with the state cookie); otherwise answer `401`.
    SignIn {
        location: String,
        state_cookie: String,
        navigation: bool,
    },
    /// `/_temps/gate/callback`: exchange the code.
    Callback,
    /// `POST /_temps/gate/logout`.
    Logout,
    /// Another `/_temps/gate/*` path, or a wrong method on one: `404`.
    NotFound,
    /// Plain HTTP: the cookie is `Secure`, so send navigations to HTTPS.
    UpgradeToHttps { location: String },
    /// The gate cannot run here (no key in the snapshot, unknown gate):
    /// `503`, never the app.
    Unavailable { reason: &'static str },
    /// Cloudflare Access (D9): a valid Access JWT for this address, through
    /// the tunnel. Forward with the email header.
    AdmitAccess(crate::gate_access::AccessIdentity),
    /// Cloudflare Access (D9): `403` with `x-temps-gate: access`, never the
    /// app. An unknown key also asks for the team's keys again.
    AccessDenied(crate::gate_access::AccessRejection),
}

/// The parts of a request the gate decides on.
#[derive(Debug, Clone, Copy)]
pub struct GateRequest<'a> {
    /// The route's hostname (lower-case, no port).
    pub hostname: &'a str,
    pub method: &'a Method,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub headers: &'a HeaderMap,
    /// The browser reached this listener over HTTPS (directly or through a
    /// tunnel's edge).
    pub secure: bool,
    /// The request came in through a tunnel's origin listener. Cloudflare
    /// Access only sees tunnel traffic, so an Access-gated address is
    /// refused anywhere else (D9).
    pub via_tunnel: bool,
}

/// Decide what to do with a request to a gated host (D8). Pure, so both
/// listeners make exactly the same decision.
pub fn decide(
    policy: &GatePolicy,
    request: &GateRequest<'_>,
    now: i64,
) -> Result<GateDecision, GateError> {
    if policy.gate == GateKind::CloudflareAccess {
        return Ok(decide_access(policy, request, now));
    }
    let GateRequest {
        hostname,
        method,
        path,
        query,
        headers,
        secure,
        via_tunnel: _,
    } = *request;
    let Some(gate) = (match policy.gate {
        GateKind::Temps => policy.temps.as_ref(),
        GateKind::CloudflareAccess | GateKind::Unsupported => None,
    }) else {
        return Ok(GateDecision::Unavailable {
            reason: "the sign-in for this address is not configured on this listener",
        });
    };
    let path_and_query = match query {
        Some(query) if !query.is_empty() => format!("{path}?{query}"),
        _ => path.to_string(),
    };
    if !secure {
        return Ok(GateDecision::UpgradeToHttps {
            location: format!("https://{hostname}{}", sanitize_return_to(&path_and_query)),
        });
    }
    if is_gate_path(path) {
        return Ok(match path {
            CALLBACK_PATH if method == Method::GET => GateDecision::Callback,
            LOGOUT_PATH if method == Method::POST => GateDecision::Logout,
            _ => GateDecision::NotFound,
        });
    }
    for token in cookie_values(headers, GATE_COOKIE) {
        if let Ok(claims) = verify_token(token, &policy.address_id, gate, now) {
            return Ok(GateDecision::Admit(claims));
        }
    }
    let navigation = is_navigation(method, headers);
    let state = GateState::new(&path_and_query, now)?;
    Ok(GateDecision::SignIn {
        location: authorize_location(
            &gate.authorize_url,
            &policy.address_id,
            &state.state,
            &code_challenge(&state.verifier),
        ),
        state_cookie: state_cookie(&state),
        navigation,
    })
}

/// The Cloudflare Access decision (D9): tunnel only, then the JWT. There is
/// no sign-in to send anyone to (Access does that at Cloudflare's edge), no
/// HTTPS upgrade (the tunnel's edge is HTTPS), and `/_temps/gate/*` is
/// Temps' own, so it is not found here.
fn decide_access(policy: &GatePolicy, request: &GateRequest<'_>, now: i64) -> GateDecision {
    use crate::gate_access::{verify_access_request, AccessRejection};
    let Some(access) = policy.access.as_ref() else {
        return GateDecision::Unavailable {
            reason: "the Cloudflare Access app for this address is not configured on this listener",
        };
    };
    if !request.via_tunnel {
        return GateDecision::AccessDenied(AccessRejection::NotThroughTunnel);
    }
    if is_gate_path(request.path) {
        return GateDecision::NotFound;
    }
    match verify_access_request(request.headers, access, now) {
        Ok(identity) => GateDecision::AdmitAccess(identity),
        Err(rejection) => GateDecision::AccessDenied(rejection),
    }
}

/// A callback's inputs, checked against the state cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackInput {
    pub code: String,
    pub verifier: String,
    pub return_to: String,
}

/// Why a callback was refused before any code was redeemed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackRefusal {
    /// No `code`/`state`, or not the expected shape.
    MissingParameters,
    /// No state cookie, or it expired: the sign-in took over 10 minutes or
    /// did not start here.
    MissingState,
    /// `state` does not match the cookie: a callback this browser did not
    /// start (sign-in CSRF).
    StateMismatch,
}

impl CallbackRefusal {
    pub fn message(self) -> &'static str {
        match self {
            CallbackRefusal::MissingParameters => {
                "This sign-in link is incomplete. Open the address again to sign in."
            }
            CallbackRefusal::MissingState => {
                "This sign-in expired or was started in another browser. Open the address again to sign in."
            }
            CallbackRefusal::StateMismatch => {
                "This sign-in was not started in this browser. Open the address again to sign in."
            }
        }
    }
}

/// Check a callback against the state cookie (D8 step 3).
pub fn check_callback(
    query: Option<&str>,
    headers: &HeaderMap,
    now: i64,
) -> Result<CallbackInput, CallbackRefusal> {
    let (Some(code), Some(state)) = (query_param(query, "code"), query_param(query, "state"))
    else {
        return Err(CallbackRefusal::MissingParameters);
    };
    if !valid_random_value(&code) || !valid_random_value(&state) {
        return Err(CallbackRefusal::MissingParameters);
    }
    let states: Vec<GateState> = cookie_values(headers, GATE_STATE_COOKIE)
        .into_iter()
        .filter_map(|value| GateState::decode(value, now))
        .collect();
    if states.is_empty() {
        return Err(CallbackRefusal::MissingState);
    }
    let Some(found) = states
        .into_iter()
        .find(|s| constant_time_eq(s.state.as_bytes(), state.as_bytes()))
    else {
        return Err(CallbackRefusal::StateMismatch);
    };
    Ok(CallbackInput {
        code,
        verifier: found.verifier,
        return_to: found.return_to,
    })
}

/// Compare two byte strings without an early exit.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A listener asks the control plane to exchange a sign-in code.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GateRedeemRequest {
    pub address_id: String,
    pub code: String,
    pub code_verifier: String,
}

/// A redeemed code: the token for the gate cookie.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GateRedeemResponse {
    pub token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// A listener asks the control plane to revoke the session behind a token.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GateLogoutRequest {
    pub address_id: String,
    pub token: String,
}

/// A listener met a Cloudflare Access JWT signed with a key it does not
/// have, and asks the control plane to fetch the team's keys again (D9).
/// The control plane rate-limits the fetch per team.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GateAccessKeysRequest {
    pub address_id: String,
}

/// Minimal HTML page for a gate answer a person will read.
pub fn page(title: &str, message: &str) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width,initial-scale=1\"><title>{t}</title><style>body{{font-family:\
         system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1rem;color:#111}}h1{{font-size:\
         1.25rem}}p{{color:#444;line-height:1.5}}</style></head><body><h1>{t}</h1><p>{m}</p></body></html>",
        t = escape(title),
        m = escape(message)
    )
}

/// The sign-in page when a worker cannot reach the control plane (D8).
pub fn unreachable_message(node: &str) -> String {
    format!("Sign-in is unavailable: {node} cannot reach the control plane.")
}

/// Unix seconds now.
pub fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A response the gate answers itself, in a shape both listeners render
/// (Pingora on the proxy, hyper on the agent) so the two send the same
/// status, headers and body for the same decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateResponse {
    pub status: u16,
    /// Header name and value, in order. `set-cookie` may repeat.
    pub headers: Vec<(&'static str, String)>,
    /// HTML body, when a person will read it.
    pub body: Option<String>,
}

impl GateResponse {
    fn new(status: u16) -> GateResponse {
        GateResponse {
            status,
            headers: vec![("cache-control", "no-store".to_string())],
            body: None,
        }
    }

    fn header(mut self, name: &'static str, value: impl Into<String>) -> GateResponse {
        self.headers.push((name, value.into()));
        self
    }

    fn html(mut self, title: &str, message: &str) -> GateResponse {
        self.headers
            .push(("content-type", "text/html; charset=utf-8".to_string()));
        self.body = Some(page(title, message));
        self
    }

    /// `302` to the console for a navigation, with the state cookie;
    /// `401` otherwise, so an API call is not redirected into a page. Both
    /// carry `x-temps-gate: sign-in` (D8 step 1, D10).
    pub fn sign_in(location: &str, state_cookie: &str, navigation: bool) -> GateResponse {
        if navigation {
            GateResponse::new(302)
                .header(GATE_HEADER, GATE_SIGN_IN)
                .header("location", location)
                .header("set-cookie", state_cookie)
        } else {
            GateResponse::new(401)
                .header(GATE_HEADER, GATE_SIGN_IN)
                .html(
                    "Sign in required",
                    "Open this address in a browser to sign in.",
                )
        }
    }

    /// Plain HTTP on a gated host: the cookie is `Secure`.
    pub fn upgrade(location: &str) -> GateResponse {
        GateResponse::new(308)
            .header(GATE_HEADER, GATE_SIGN_IN)
            .header("location", location)
    }

    /// The gate cannot run on this listener: `503`, never the app.
    pub fn unavailable(message: &str) -> GateResponse {
        GateResponse::new(503)
            .header(GATE_HEADER, GATE_UNAVAILABLE)
            .html("Sign-in is unavailable", message)
    }

    /// An unknown `/_temps/gate/*` path or method.
    pub fn not_found() -> GateResponse {
        GateResponse::new(404).html("Not found", "There is nothing at this address.")
    }

    /// A callback refused before any code was redeemed. The state cookie
    /// is single use, so it is cleared whatever happened.
    pub fn callback_refused(refusal: CallbackRefusal) -> GateResponse {
        GateResponse::new(400)
            .header(GATE_HEADER, GATE_SIGN_IN)
            .header("set-cookie", clear_state_cookie())
            .html("Sign-in failed", refusal.message())
    }

    /// The control plane refused the code (expired, used, wrong address or
    /// verifier, session ended): `status` is its answer mapped to 4xx/5xx.
    pub fn redeem_failed(status: u16, message: &str) -> GateResponse {
        GateResponse::new(status)
            .header(GATE_HEADER, GATE_SIGN_IN)
            .header("set-cookie", clear_state_cookie())
            .html("Sign-in failed", message)
    }

    /// The code was redeemed: set the gate cookie, clear the state cookie
    /// and go back to where the visitor started (same-origin only).
    pub fn signed_in(redeemed: &GateRedeemResponse, return_to: &str, now: i64) -> GateResponse {
        GateResponse::new(302)
            .header("location", sanitize_return_to(return_to))
            .header(
                "set-cookie",
                session_cookie(&redeemed.token, redeemed.expires_at - now),
            )
            .header("set-cookie", clear_state_cookie())
    }

    /// `POST /_temps/gate/logout` from another origin.
    pub fn logout_refused() -> GateResponse {
        GateResponse::new(403).html("Sign-out refused", "Sign out from a page on this address.")
    }

    /// Signed out: the cookie is cleared whatever the control plane said.
    /// `revoked` is false when the session could not be revoked (the
    /// listener cannot reach the control plane), so the page says so.
    pub fn signed_out(hostname: &str, revoked: bool) -> GateResponse {
        let message = if revoked {
            format!("You are signed out of {hostname}.")
        } else {
            format!(
                "You are signed out of {hostname} in this browser. The sign-in could not be \
                 revoked everywhere because this listener cannot reach the control plane; \
                 sign out of apps from the console to end it."
            )
        };
        GateResponse::new(200)
            .header("set-cookie", clear_session_cookie())
            .html("Signed out", &message)
    }
}

/// `x-temps-gate` on an app's response to a gated host would let the app
/// pass for the gate (D10 reads it), so listeners drop it, along with any
/// `Set-Cookie` naming the gate cookies.
pub fn strip_gate_response_headers(headers: &mut HeaderMap) {
    headers.remove(GATE_HEADER);
    drop_gate_set_cookies(headers);
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_capability_is_declared_only_by_its_exact_name() {
        use super::{declares_capability, GATE_CAPABILITY};
        assert!(declares_capability(["gate.v1"], GATE_CAPABILITY));
        assert!(declares_capability(
            ["tailnet_identity.v1, gate.v1"],
            GATE_CAPABILITY
        ));
        assert!(declares_capability(
            ["tailnet_identity.v1", "gate.v1"],
            GATE_CAPABILITY
        ));
        assert!(!declares_capability(
            ["gate.v10", "xgate.v1"],
            GATE_CAPABILITY
        ));
        assert!(!declares_capability([], GATE_CAPABILITY));
    }

    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn gate() -> TempsGate {
        TempsGate {
            public_key: encode_public_key(&key().verifying_key()),
            authorize_url: "https://console.example.test/_temps/gate/authorize".into(),
            not_before: 0,
            revoked: vec![],
        }
    }

    fn policy() -> GatePolicy {
        GatePolicy {
            address_id: "custom:7".into(),
            gate: GateKind::Temps,
            temps: Some(gate()),
            access: None,
        }
    }

    fn claims(aid: &str, iat: i64, exp: i64) -> GateClaims {
        GateClaims {
            sid: "s-1".into(),
            uid: 42,
            email: "dev@example.test".into(),
            aid: aid.into(),
            iat,
            exp,
        }
    }

    fn token(c: &GateClaims) -> String {
        sign_token(&key(), c).unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn a_valid_token_for_its_address_is_admitted() {
        let t = token(&claims("custom:7", 1000, 2000));
        let got = verify_token(&t, "custom:7", &gate(), 1500).unwrap();
        assert_eq!(got.uid, 42);
    }

    #[test]
    fn a_token_for_another_address_is_refused() {
        let t = token(&claims("custom:8", 1000, 2000));
        assert_eq!(
            verify_token(&t, "custom:7", &gate(), 1500),
            Err(TokenRejection::WrongAddress)
        );
    }

    #[test]
    fn expired_and_future_tokens_are_refused() {
        let t = token(&claims("custom:7", 1000, 2000));
        assert_eq!(
            verify_token(&t, "custom:7", &gate(), 2000),
            Err(TokenRejection::Expired)
        );
        let t = token(&claims("custom:7", 5000, 9000));
        assert_eq!(
            verify_token(&t, "custom:7", &gate(), 1500),
            Err(TokenRejection::IssuedInFuture)
        );
    }

    #[test]
    fn revocation_in_the_snapshot_refuses_the_token() {
        let t = token(&claims("custom:7", 1000, 2000));
        let mut g = gate();
        g.revoked = vec!["s-1".into()];
        assert_eq!(
            verify_token(&t, "custom:7", &g, 1500),
            Err(TokenRejection::Revoked)
        );
        let mut g = gate();
        g.not_before = 1001;
        assert_eq!(
            verify_token(&t, "custom:7", &g, 1500),
            Err(TokenRejection::BeforeNotBefore)
        );
    }

    #[test]
    fn a_token_signed_by_another_key_or_tampered_is_refused() {
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let t = sign_token(&other, &claims("custom:7", 1000, 2000)).unwrap();
        assert_eq!(
            verify_token(&t, "custom:7", &gate(), 1500),
            Err(TokenRejection::BadSignature)
        );
        // Swap in claims for another address, keeping the signature.
        let good = token(&claims("custom:7", 1000, 2000));
        let forged_payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims("custom:7", 1000, 999_999)).unwrap());
        let sig = good.rsplit('.').next().unwrap();
        let forged = format!("v1.{forged_payload}.{sig}");
        assert_eq!(
            verify_token(&forged, "custom:7", &gate(), 1500),
            Err(TokenRejection::BadSignature)
        );
        assert_eq!(
            verify_token("v1.x", "custom:7", &gate(), 1500),
            Err(TokenRejection::Malformed)
        );
        assert_eq!(
            verify_token(&format!("v2{}", &good[2..]), "custom:7", &gate(), 1500),
            Err(TokenRejection::Malformed)
        );
    }

    #[test]
    fn cookies_have_the_attributes_d8_requires() {
        let c = session_cookie("tok", 3600);
        assert!(c.starts_with("__Host-temps_gate=tok;"));
        for attr in [
            "Path=/",
            "Secure",
            "HttpOnly",
            "SameSite=Lax",
            "Max-Age=3600",
        ] {
            assert!(c.split("; ").any(|a| a == attr), "{attr} missing in {c}");
        }
        assert!(!c.to_ascii_lowercase().contains("domain="));
        let s = state_cookie(&GateState::new("/x", 0).unwrap());
        assert!(s.starts_with("__Host-temps_gate_state="));
        for attr in [
            "Path=/",
            "Secure",
            "HttpOnly",
            "SameSite=Lax",
            "Max-Age=600",
        ] {
            assert!(s.split("; ").any(|a| a == attr), "{attr} missing in {s}");
        }
        assert!(!s.to_ascii_lowercase().contains("domain="));
        assert!(clear_session_cookie().contains("Max-Age=0"));
        assert!(clear_state_cookie().contains("Max-Age=0"));
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // RFC 7636 appendix B.
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = new_verifier().unwrap();
        assert!(valid_verifier(&v));
        assert!(!valid_verifier("short"));
    }

    #[test]
    fn state_round_trips_and_expires() {
        let s = GateState::new("/dash?x=1", 100).unwrap();
        let back = GateState::decode(&s.encode(), 200).unwrap();
        assert_eq!(back, s);
        assert!(GateState::decode(&s.encode(), 100 + 600).is_none());
        // An open redirect in the return path is neutralised.
        let s = GateState::new("//evil.example", 100).unwrap();
        assert_eq!(s.return_to, "/");
        let s = GateState::new("https://evil.example/", 100).unwrap();
        assert_eq!(s.return_to, "/");
    }

    /// The longest kept path still fits the cookie; one byte more returns
    /// to `/` rather than break sign-in from that address.
    #[test]
    fn a_return_path_too_long_for_the_state_cookie_returns_to_root() {
        let longest = format!("/a?q={}", "x".repeat(MAX_RETURN_TO_LEN - 5));
        let s = GateState::new(&longest, 100).unwrap();
        assert_eq!(s.return_to, longest);
        let cookie = state_cookie(&s);
        let value = cookie
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
            .map(|(_, value)| value)
            .unwrap();
        assert!(value.len() <= MAX_TOKEN_LEN);
        assert!(cookie.split(';').next().unwrap().len() < 4096);
        assert_eq!(GateState::decode(value, 200), Some(s));

        let too_long = format!("{longest}x");
        assert_eq!(GateState::new(&too_long, 100).unwrap().return_to, "/");
    }

    #[test]
    fn unauthenticated_navigation_redirects_and_api_calls_get_401() {
        let nav = headers(&[("sec-fetch-mode", "navigate")]);
        match decide(
            &policy(),
            &GateRequest {
                hostname: "app.example.test",
                method: &Method::GET,
                path: "/p",
                query: Some("a=b"),
                headers: &nav,
                secure: true,
                via_tunnel: false,
            },
            10,
        )
        .unwrap()
        {
            GateDecision::SignIn {
                location,
                state_cookie,
                navigation,
            } => {
                assert!(navigation);
                assert!(location.starts_with(
                    "https://console.example.test/_temps/gate/authorize?address=custom%3A7&state="
                ));
                assert!(location.contains("&code_challenge="));
                assert!(state_cookie.starts_with(GATE_STATE_COOKIE));
            }
            other => panic!("expected sign-in, got {other:?}"),
        }
        let api = headers(&[("accept", "application/json")]);
        assert!(matches!(
            decide(
                &policy(),
                &GateRequest {
                    hostname: "app.example.test",
                    method: &Method::GET,
                    path: "/api",
                    query: None,
                    headers: &api,
                    secure: true,
                    via_tunnel: false,
                },
                10
            )
            .unwrap(),
            GateDecision::SignIn {
                navigation: false,
                ..
            }
        ));
        assert!(matches!(
            decide(
                &policy(),
                &GateRequest {
                    hostname: "app.example.test",
                    method: &Method::POST,
                    path: "/",
                    query: None,
                    headers: &nav,
                    secure: true,
                    via_tunnel: false,
                },
                10
            )
            .unwrap(),
            GateDecision::SignIn {
                navigation: false,
                ..
            }
        ));
    }

    #[test]
    fn a_valid_cookie_is_admitted_and_a_cross_address_cookie_is_not() {
        let good = token(&claims("custom:7", 5, 5000));
        let h = headers(&[("cookie", &format!("a=1; {GATE_COOKIE}={good}"))]);
        assert!(matches!(
            decide(
                &policy(),
                &GateRequest {
                    hostname: "h",
                    method: &Method::GET,
                    path: "/",
                    query: None,
                    headers: &h,
                    secure: true,
                    via_tunnel: false,
                },
                10
            )
            .unwrap(),
            GateDecision::Admit(_)
        ));
        let other = token(&claims("custom:8", 5, 5000));
        let h = headers(&[("cookie", &format!("{GATE_COOKIE}={other}"))]);
        assert!(matches!(
            decide(
                &policy(),
                &GateRequest {
                    hostname: "h",
                    method: &Method::GET,
                    path: "/",
                    query: None,
                    headers: &h,
                    secure: true,
                    via_tunnel: false,
                },
                10
            )
            .unwrap(),
            GateDecision::SignIn { .. }
        ));
    }

    #[test]
    fn gate_paths_and_plain_http_and_missing_key() {
        let h = HeaderMap::new();
        let d = |m: &Method, p: &str| {
            decide(
                &policy(),
                &GateRequest {
                    hostname: "h",
                    method: m,
                    path: p,
                    query: None,
                    headers: &h,
                    secure: true,
                    via_tunnel: false,
                },
                10,
            )
            .unwrap()
        };
        assert_eq!(d(&Method::GET, CALLBACK_PATH), GateDecision::Callback);
        assert_eq!(d(&Method::POST, LOGOUT_PATH), GateDecision::Logout);
        assert_eq!(d(&Method::GET, LOGOUT_PATH), GateDecision::NotFound);
        assert_eq!(
            d(&Method::GET, "/_temps/gate/other"),
            GateDecision::NotFound
        );
        assert!(matches!(
            d(&Method::GET, "/_temps/gateway"),
            GateDecision::SignIn { .. }
        ));
        assert_eq!(
            decide(
                &policy(),
                &GateRequest {
                    hostname: "h",
                    method: &Method::GET,
                    path: "/a",
                    query: None,
                    headers: &h,
                    secure: false,
                    via_tunnel: false,
                },
                10
            )
            .unwrap(),
            GateDecision::UpgradeToHttps {
                location: "https://h/a".into()
            }
        );
        let mut p = policy();
        p.temps = None;
        assert!(matches!(
            decide(
                &p,
                &GateRequest {
                    hostname: "h",
                    method: &Method::GET,
                    path: "/",
                    query: None,
                    headers: &h,
                    secure: true,
                    via_tunnel: false,
                },
                10
            )
            .unwrap(),
            GateDecision::Unavailable { .. }
        ));
        p.temps = Some(gate());
        p.gate = GateKind::Unsupported;
        assert!(matches!(
            decide(
                &p,
                &GateRequest {
                    hostname: "h",
                    method: &Method::GET,
                    path: "/",
                    query: None,
                    headers: &h,
                    secure: true,
                    via_tunnel: false,
                },
                10
            )
            .unwrap(),
            GateDecision::Unavailable { .. }
        ));
    }

    #[test]
    fn callback_requires_the_matching_state_cookie() {
        let s = GateState::new("/back", 100).unwrap();
        let code = new_random_value("code").unwrap();
        let q = format!("code={code}&state={}", s.state);
        let with_cookie = headers(&[("cookie", &format!("{GATE_STATE_COOKIE}={}", s.encode()))]);
        let input = check_callback(Some(&q), &with_cookie, 200).unwrap();
        assert_eq!(input.verifier, s.verifier);
        assert_eq!(input.return_to, "/back");
        assert_eq!(
            check_callback(Some(&q), &HeaderMap::new(), 200),
            Err(CallbackRefusal::MissingState)
        );
        let other = GateState::new("/", 100).unwrap();
        let wrong = headers(&[("cookie", &format!("{GATE_STATE_COOKIE}={}", other.encode()))]);
        assert_eq!(
            check_callback(Some(&q), &wrong, 200),
            Err(CallbackRefusal::StateMismatch)
        );
        // Expired state.
        assert_eq!(
            check_callback(Some(&q), &with_cookie, 100 + 601),
            Err(CallbackRefusal::MissingState)
        );
        // A duplicated code parameter is refused.
        let dup = format!("{q}&code={code}");
        assert_eq!(
            check_callback(Some(&dup), &with_cookie, 200),
            Err(CallbackRefusal::MissingParameters)
        );
    }

    #[test]
    fn gate_cookies_are_stripped_and_app_set_cookies_dropped() {
        let mut h = headers(&[
            ("cookie", &format!("a=1; {GATE_COOKIE}=t")),
            ("cookie", &format!("{GATE_STATE_COOKIE}=s; b=2")),
        ]);
        strip_gate_cookies(&mut h);
        assert_eq!(h.get_all("cookie").iter().count(), 1);
        assert_eq!(h.get("cookie").unwrap(), "a=1; b=2");
        let mut only = headers(&[("cookie", &format!("{GATE_COOKIE}=t"))]);
        strip_gate_cookies(&mut only);
        assert!(only.get("cookie").is_none());
        let mut lower = headers(&[("cookie", "__host-temps_gate=t; a=1")]);
        strip_gate_cookies(&mut lower);
        assert_eq!(lower.get("cookie").unwrap(), "a=1");
        // Without a gate cookie the fields are left exactly as sent.
        let mut none = headers(&[("cookie", "a=1"), ("cookie", "b=2")]);
        strip_gate_cookies(&mut none);
        assert_eq!(none.get_all("cookie").iter().count(), 2);

        let mut resp = headers(&[
            ("set-cookie", "app=1; Path=/"),
            ("set-cookie", &format!("{GATE_COOKIE}=evil; Path=/")),
            ("set-cookie", "__host-TEMPS_gate_state=x"),
        ]);
        drop_gate_set_cookies(&mut resp);
        let left: Vec<_> = resp.get_all("set-cookie").iter().collect();
        assert_eq!(left, vec!["app=1; Path=/"]);
    }

    #[test]
    fn identity_headers_are_stripped_from_client_input() {
        let mut h = headers(&[
            ("x-temps-user", "1"),
            ("X-Temps-User-Email", "admin@example.test"),
            ("x-temps-user-login", "someone@example.test"),
            ("x-temps-user-role", "admin"),
            ("x-other", "kept"),
        ]);
        strip_identity_headers(&mut h);
        assert!(h.get("x-temps-user").is_none());
        assert!(h.get("x-temps-user-email").is_none());
        assert!(h.get("x-temps-user-login").is_none());
        assert!(h.get("x-temps-user-role").is_none());
        assert!(h.get("x-other").is_some());
    }

    #[test]
    fn logout_requires_same_origin() {
        let host = "app.example.test";
        assert!(logout_allowed(
            &headers(&[("sec-fetch-site", "same-origin")]),
            host
        ));
        assert!(!logout_allowed(
            &headers(&[("sec-fetch-site", "cross-site")]),
            host
        ));
        assert!(!logout_allowed(
            &headers(&[("sec-fetch-site", "same-site")]),
            host
        ));
        // Fetch metadata wins over a matching Origin.
        assert!(!logout_allowed(
            &headers(&[
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://app.example.test")
            ]),
            host
        ));
        assert!(logout_allowed(
            &headers(&[("origin", "https://app.example.test")]),
            host
        ));
        assert!(!logout_allowed(
            &headers(&[("origin", "https://evil.example")]),
            host
        ));
        assert!(!logout_allowed(
            &headers(&[("origin", "http://app.example.test")]),
            host
        ));
        assert!(!logout_allowed(&HeaderMap::new(), host));
    }

    #[test]
    fn unknown_gate_kind_fails_closed() {
        let p: GatePolicy =
            serde_json::from_str(r#"{"address_id":"custom:1","gate":"something_new"}"#).unwrap();
        assert_eq!(p.gate, GateKind::Unsupported);
        assert_eq!(GateKind::parse("nope"), GateKind::Unsupported);
    }

    #[test]
    fn page_escapes_its_input() {
        let p = page("<t>", "a & <script>");
        assert!(p.contains("&lt;t&gt;"));
        assert!(p.contains("a &amp; &lt;script&gt;"));
    }

    fn header_values<'a>(response: &'a GateResponse, name: &str) -> Vec<&'a str> {
        response
            .headers
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[test]
    fn sign_in_redirects_navigations_and_refuses_api_calls() {
        let nav = GateResponse::sign_in("https://console/x", "c=1", true);
        assert_eq!(nav.status, 302);
        assert_eq!(header_values(&nav, GATE_HEADER), vec![GATE_SIGN_IN]);
        assert_eq!(header_values(&nav, "location"), vec!["https://console/x"]);
        assert_eq!(header_values(&nav, "set-cookie"), vec!["c=1"]);

        let api = GateResponse::sign_in("https://console/x", "c=1", false);
        assert_eq!(api.status, 401);
        assert_eq!(header_values(&api, GATE_HEADER), vec![GATE_SIGN_IN]);
        assert!(header_values(&api, "location").is_empty());
        assert!(
            header_values(&api, "set-cookie").is_empty(),
            "no state cookie for a request that cannot follow the redirect"
        );
    }

    #[test]
    fn signed_in_sets_the_cookie_and_never_redirects_off_host() {
        let redeemed = GateRedeemResponse {
            token: "v1.a.b".into(),
            expires_at: 1_000 + 3_600,
        };
        let response = GateResponse::signed_in(&redeemed, "//evil.example/x", 1_000);
        assert_eq!(response.status, 302);
        assert_eq!(header_values(&response, "location"), vec!["/"]);
        let cookies = header_values(&response, "set-cookie");
        assert_eq!(cookies.len(), 2);
        assert_eq!(
            cookies[0],
            "__Host-temps_gate=v1.a.b; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=3600"
        );
        assert!(cookies[0].starts_with("__Host-"));
        assert!(!cookies[0].contains("Domain"));
        assert!(cookies[1].starts_with("__Host-temps_gate_state=;"));
        assert!(cookies[1].ends_with("Max-Age=0"));

        let kept = GateResponse::signed_in(&redeemed, "/dash?tab=1", 1_000);
        assert_eq!(header_values(&kept, "location"), vec!["/dash?tab=1"]);
    }

    #[test]
    fn refused_callbacks_clear_the_single_use_state_cookie() {
        for response in [
            GateResponse::callback_refused(CallbackRefusal::StateMismatch),
            GateResponse::redeem_failed(400, "used"),
        ] {
            assert_eq!(response.status, 400);
            assert_eq!(
                header_values(&response, "set-cookie"),
                vec![clear_state_cookie().as_str()]
            );
        }
    }

    #[test]
    fn signed_out_clears_the_cookie_and_says_when_it_could_not_revoke() {
        let ok = GateResponse::signed_out("app.example.test", true);
        assert_eq!(
            header_values(&ok, "set-cookie"),
            vec![clear_session_cookie().as_str()]
        );
        let offline = GateResponse::signed_out("app.example.test", false);
        assert!(offline
            .body
            .unwrap_or_default()
            .contains("cannot reach the control plane"));
        assert_eq!(GateResponse::logout_refused().status, 403);
    }

    #[test]
    fn app_responses_cannot_pass_for_the_gate() {
        let mut headers = HeaderMap::new();
        headers.insert(GATE_HEADER, HeaderValue::from_static("sign-in"));
        headers.append(
            axum::http::header::SET_COOKIE,
            HeaderValue::from_static("__Host-temps_gate=forged; Path=/"),
        );
        headers.append(
            axum::http::header::SET_COOKIE,
            HeaderValue::from_static("app=1; Path=/"),
        );
        strip_gate_response_headers(&mut headers);
        assert!(headers.get(GATE_HEADER).is_none());
        let cookies: Vec<_> = headers
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .collect();
        assert_eq!(cookies, vec![HeaderValue::from_static("app=1; Path=/")]);
    }
}
