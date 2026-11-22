// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The signed-in gate on the agent's ingress (ADR 065 D8).
//!
//! Decisions are `temps_core::gate::decide`, the same function the
//! control-plane proxy calls. What is the agent's own is the transport: it
//! redeems sign-in codes and revokes sessions over its node-token channel
//! (`/internal/nodes/{id}/gate/*`), and the control plane accepts that only
//! for addresses in this node's own route snapshot.
//!
//! A worker cut off from the control plane keeps admitting the tokens it
//! can verify, until they expire, and admits nobody new: the sign-in page
//! then says which node cannot reach the control plane.
//!
//! The agent advertises the [`GATE_CAPABILITY`] in its listener report; the
//! control plane holds signed-in routes back from agents that do not, so an
//! older agent never serves one without the gate.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::Response;
use temps_core::gate::{
    self, GateAccessKeysRequest, GateDecision, GateLogoutRequest, GatePolicy, GateRedeemRequest,
    GateRedeemResponse, GateRequest, GateResponse, GATE_COOKIE,
};
use temps_core::gate_access::{self, AccessRejection};
use tracing::{error, info, warn};

pub use temps_core::gate::GATE_CAPABILITY;

/// How long after the last successful control-plane round the agent still
/// sends visitors to sign in. The route long-poll answers within 25 s, so
/// three missed rounds mean the control plane is out of reach.
const CONTROL_PLANE_STALE_AFTER_SECS: i64 = 90;

/// Timeout for one redeem or logout call.
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(10);

/// At most one "fetch the Access keys again" request per team per this many
/// seconds from this agent, however many requests carry an unknown key
/// (D9). The control plane rate-limits the fetch itself as well.
const ACCESS_KEYS_REQUEST_EVERY_SECS: i64 = 60;

/// How many teams get a request slot of their own. Teams share a slot only
/// when their hashes collide, so a bogus key sent to one team's address
/// cannot hold back a real rotation of another's; memory stays fixed.
const ACCESS_KEYS_SLOTS: usize = 64;

/// Unix seconds of the last Access keys request of the teams in each slot,
/// or 0.
static ACCESS_KEYS_REQUESTS: [AtomicI64; ACCESS_KEYS_SLOTS] =
    [const { AtomicI64::new(0) }; ACCESS_KEYS_SLOTS];

/// Seeded per process, so which teams share a slot cannot be predicted.
static ACCESS_KEYS_SLOT_SEED: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();

/// The request slot of the team whose JWTs carry `issuer`.
fn access_keys_slot(issuer: &str) -> &'static AtomicI64 {
    &ACCESS_KEYS_REQUESTS[access_keys_slot_index(issuer)]
}

fn access_keys_slot_index(issuer: &str) -> usize {
    use std::hash::BuildHasher;
    let seed = ACCESS_KEYS_SLOT_SEED.get_or_init(std::collections::hash_map::RandomState::new);
    (seed.hash_one(issuer) % ACCESS_KEYS_SLOTS as u64) as usize
}

/// Unix seconds of the last successful exchange with the control plane, or
/// 0 before the first one.
static LAST_CONTROL_PLANE_CONTACT: AtomicI64 = AtomicI64::new(0);

static CHANNEL: OnceLock<GateChannel> = OnceLock::new();

/// Record a successful exchange with the control plane (a route round, a
/// redeemed code).
pub fn mark_control_plane_contact() {
    LAST_CONTROL_PLANE_CONTACT.store(gate::unix_now(), Ordering::Release);
}

fn control_plane_out_of_reach(last_contact: i64, now: i64) -> bool {
    last_contact != 0 && now - last_contact > CONTROL_PLANE_STALE_AFTER_SECS
}

/// How the agent reaches the control plane's gate endpoints.
#[derive(Clone)]
pub struct GateChannel {
    control_plane_url: String,
    node_id: i32,
    node_token: String,
    /// This node's name, for "Sign-in is unavailable: `<node>` cannot reach
    /// the control plane".
    node_name: String,
    http: reqwest::Client,
}

/// Why a call to the control plane did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// No answer (network, TLS, timeout) or a server error.
    Unreachable(String),
    /// The control plane answered and refused (4xx).
    Refused(u16),
}

impl GateChannel {
    pub fn new(
        control_plane_url: String,
        node_id: i32,
        node_token: String,
        node_name: String,
        control_plane_ca: Option<crate::ControlPlaneCa>,
    ) -> Result<GateChannel, reqwest::Error> {
        let http = crate::with_control_plane_trust(
            reqwest::Client::builder()
                .timeout(CHANNEL_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none()),
            control_plane_ca,
        )
        .build()?;
        Ok(GateChannel {
            control_plane_url: control_plane_url.trim_end_matches('/').to_string(),
            node_id,
            node_token,
            node_name,
            http,
        })
    }

    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    async fn post<T: serde::Serialize>(
        &self,
        operation: &str,
        body: &T,
    ) -> Result<reqwest::Response, ChannelError> {
        let url = format!(
            "{}/api/internal/nodes/{}/gate/{operation}",
            self.control_plane_url, self.node_id
        );
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.node_token)
            // The control plane serves gate requests only to a binary that
            // declares the gate on the request itself (ADR 065 D7).
            .header(
                temps_core::tailnet::AGENT_CAPABILITIES_HEADER,
                crate::route_sync_client::declared_capabilities(),
            )
            .json(body)
            .send()
            .await
            .map_err(|e| ChannelError::Unreachable(format!("POST {url}: {e}")))?;
        let status = response.status();
        if status.is_server_error() {
            return Err(ChannelError::Unreachable(format!(
                "control plane answered {status} for POST {url}"
            )));
        }
        if !status.is_success() {
            return Err(ChannelError::Refused(status.as_u16()));
        }
        Ok(response)
    }

    pub async fn redeem(
        &self,
        request: &GateRedeemRequest,
    ) -> Result<GateRedeemResponse, ChannelError> {
        let response = self.post("redeem", request).await?;
        let redeemed = response
            .json::<GateRedeemResponse>()
            .await
            .map_err(|e| ChannelError::Unreachable(format!("unreadable redeem answer: {e}")))?;
        mark_control_plane_contact();
        Ok(redeemed)
    }

    pub async fn logout(&self, request: &GateLogoutRequest) -> Result<(), ChannelError> {
        self.post("logout", request).await?;
        mark_control_plane_contact();
        Ok(())
    }

    pub async fn access_keys(&self, request: &GateAccessKeysRequest) -> Result<(), ChannelError> {
        self.post("access-keys", request).await?;
        mark_control_plane_contact();
        Ok(())
    }
}

/// Whether an Access keys request may go out at `now`, given the last one
/// at `*last`; claims the slot when it may. Lock-free: one compare-exchange.
fn claim_access_keys_slot(last: &AtomicI64, now: i64) -> bool {
    let previous = last.load(Ordering::Acquire);
    if previous != 0 && now - previous < ACCESS_KEYS_REQUEST_EVERY_SECS {
        return false;
    }
    last.compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Ask the control plane, in the background, to fetch the Access keys of
/// the team behind `address_id` (whose JWTs carry `issuer`) again. Never
/// delays the visitor's answer.
fn request_access_keys(channel: &GateChannel, issuer: &str, address_id: &str, now: i64) {
    if !claim_access_keys_slot(access_keys_slot(issuer), now) {
        return;
    }
    let channel = channel.clone();
    let request = GateAccessKeysRequest {
        address_id: address_id.to_string(),
    };
    tokio::spawn(async move {
        if let Err(error) = channel.access_keys(&request).await {
            warn!(address_id = %request.address_id, ?error, "could not ask the control plane to fetch the Cloudflare Access keys again");
        }
    });
}

/// Install the channel once, at agent start.
pub fn install(channel: GateChannel) {
    if CHANNEL.set(channel).is_err() {
        warn!("signed-in gate channel was already installed; keeping the first");
    }
}

fn channel() -> Option<&'static GateChannel> {
    CHANNEL.get()
}

/// What the ingress does with a request to a gated host.
pub enum Enforced {
    /// Forward it with these identity headers (gate cookies already removed
    /// from the request).
    Admit(Vec<(&'static str, HeaderValue)>),
    /// Answer it here.
    Respond(Response),
}

/// Run the gate for a request to `hostname`, whose route carries `policy`.
/// `secure` is whether the browser reached this listener over HTTPS
/// (the HTTPS listener, or a tunnel whose edge terminated TLS);
/// `via_tunnel` whether this listener is a tunnel's origin.
pub async fn enforce(
    policy: &GatePolicy,
    hostname: &str,
    secure: bool,
    via_tunnel: bool,
    method: &Method,
    uri: &axum::http::Uri,
    headers: &mut HeaderMap,
) -> Enforced {
    enforce_with(
        policy,
        hostname,
        secure,
        via_tunnel,
        method,
        uri,
        headers,
        channel(),
        LAST_CONTROL_PLANE_CONTACT.load(Ordering::Acquire),
        gate::unix_now(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn enforce_with(
    policy: &GatePolicy,
    hostname: &str,
    secure: bool,
    via_tunnel: bool,
    method: &Method,
    uri: &axum::http::Uri,
    headers: &mut HeaderMap,
    channel: Option<&GateChannel>,
    last_contact: i64,
    now: i64,
) -> Enforced {
    let node = channel.map_or("this node", GateChannel::node_name);
    let request = GateRequest {
        hostname,
        method,
        path: uri.path(),
        query: uri.query(),
        headers,
        secure,
        via_tunnel,
    };
    let decision = match gate::decide(policy, &request, now) {
        Ok(decision) => decision,
        Err(e) => {
            error!(address_id = %policy.address_id, host = hostname, error = %e, "could not start a sign-in");
            return respond(GateResponse::unavailable(
                "Sign-in could not be started on this server. Try again in a moment.",
            ));
        }
    };
    let answer = match decision {
        GateDecision::Admit(claims) => {
            gate::strip_gate_cookies(headers);
            return Enforced::Admit(gate::identity_headers(&claims));
        }
        GateDecision::SignIn { .. }
            if control_plane_out_of_reach(last_contact, now) || channel.is_none() =>
        {
            GateResponse::unavailable(&gate::unreachable_message(node))
        }
        GateDecision::SignIn {
            location,
            state_cookie,
            navigation,
        } => GateResponse::sign_in(&location, &state_cookie, navigation),
        GateDecision::UpgradeToHttps { location } => GateResponse::upgrade(&location),
        GateDecision::Unavailable { reason } => {
            warn!(address_id = %policy.address_id, host = hostname, reason, "signed-in route without a usable gate");
            GateResponse::unavailable(reason)
        }
        GateDecision::AdmitAccess(identity) => {
            return Enforced::Admit(gate_access::access_identity_headers(&identity));
        }
        GateDecision::AccessDenied(rejection) => {
            if rejection == AccessRejection::UnknownKey {
                if let (Some(channel), Some(access)) = (channel, policy.access.as_ref()) {
                    request_access_keys(channel, &access.issuer, &policy.address_id, now);
                }
            }
            info!(address_id = %policy.address_id, host = hostname, reason = rejection.as_str(), "Cloudflare Access refused a request");
            gate_access::access_denied(rejection)
        }
        GateDecision::NotFound => GateResponse::not_found(),
        GateDecision::Callback => callback(policy, hostname, uri, headers, channel, now).await,
        GateDecision::Logout => logout(policy, hostname, headers, channel).await,
    };
    respond(answer)
}

async fn callback(
    policy: &GatePolicy,
    hostname: &str,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
    channel: Option<&GateChannel>,
    now: i64,
) -> GateResponse {
    let input = match gate::check_callback(uri.query(), headers, now) {
        Ok(input) => input,
        Err(refusal) => {
            info!(address_id = %policy.address_id, host = hostname, ?refusal, "sign-in callback refused before redeeming");
            return GateResponse::callback_refused(refusal);
        }
    };
    let Some(channel) = channel else {
        return GateResponse::unavailable(&gate::unreachable_message("this node"));
    };
    let request = GateRedeemRequest {
        address_id: policy.address_id.clone(),
        code: input.code,
        code_verifier: input.verifier,
    };
    match channel.redeem(&request).await {
        Ok(redeemed) => GateResponse::signed_in(&redeemed, &input.return_to, now),
        Err(ChannelError::Unreachable(reason)) => {
            warn!(address_id = %policy.address_id, host = hostname, %reason, "could not redeem a sign-in code");
            GateResponse::unavailable(&gate::unreachable_message(channel.node_name()))
        }
        Err(ChannelError::Refused(status)) => {
            info!(address_id = %policy.address_id, host = hostname, status, "control plane refused a sign-in code");
            GateResponse::redeem_failed(
                if status == 404 || status == 409 {
                    409
                } else {
                    400
                },
                refused_message(status),
            )
        }
    }
}

/// What a visitor reads when the control plane refused the code. The
/// control plane's own message carries ids and stays in its log.
fn refused_message(status: u16) -> &'static str {
    match status {
        403 => {
            "This server does not serve this address any more. Open the address again to sign in."
        }
        404 | 409 => "This address no longer asks for sign-in. Open it again.",
        _ => "This sign-in link expired or was already used. Open the address again to sign in.",
    }
}

async fn logout(
    policy: &GatePolicy,
    hostname: &str,
    headers: &HeaderMap,
    channel: Option<&GateChannel>,
) -> GateResponse {
    if !gate::logout_allowed(headers, hostname) {
        warn!(address_id = %policy.address_id, host = hostname, "cross-origin sign-out refused");
        return GateResponse::logout_refused();
    }
    let mut revoked = true;
    for token in gate::cookie_values(headers, GATE_COOKIE) {
        let Some(channel) = channel else {
            revoked = false;
            break;
        };
        let request = GateLogoutRequest {
            address_id: policy.address_id.clone(),
            token: token.to_string(),
        };
        match channel.logout(&request).await {
            Ok(()) => {}
            // A token that does not verify has no session to revoke.
            Err(ChannelError::Refused(400)) => {}
            Err(error) => {
                warn!(address_id = %policy.address_id, host = hostname, ?error, "could not revoke a gate session");
                revoked = false;
            }
        }
    }
    GateResponse::signed_out(hostname, revoked)
}

/// Render a gate answer.
pub fn respond(answer: GateResponse) -> Enforced {
    Enforced::Respond(into_response(answer))
}

pub fn into_response(answer: GateResponse) -> Response {
    let mut response = Response::new(answer.body.map(Body::from).unwrap_or_else(Body::empty));
    *response.status_mut() =
        StatusCode::from_u16(answer.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    for (name, value) in answer.headers {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response
                .headers_mut()
                .append(HeaderName::from_static(name), value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::Json;
    use std::sync::Arc;
    use temps_core::gate::{GateKind, GateState, TempsGate, GATE_HEADER, GATE_SIGN_IN};

    fn policy(key: &gate::SigningKey) -> GatePolicy {
        GatePolicy {
            address_id: "environment:4".into(),
            gate: GateKind::Temps,
            temps: Some(TempsGate {
                public_key: gate::encode_public_key(&key.verifying_key()),
                authorize_url: "https://console.example.test/_temps/gate/authorize".into(),
                not_before: 0,
                revoked: vec![],
            }),
            access: None,
        }
    }

    fn claims(now: i64) -> gate::GateClaims {
        gate::GateClaims {
            sid: "x".repeat(43),
            uid: 11,
            email: "member@example.test".into(),
            aid: "environment:4".into(),
            iat: now - 5,
            exp: now + 600,
        }
    }

    async fn run(
        policy: &GatePolicy,
        method: Method,
        uri: &str,
        headers: &mut HeaderMap,
        channel: Option<&GateChannel>,
        now: i64,
    ) -> Enforced {
        let uri: axum::http::Uri = uri.parse().unwrap();
        enforce_with(
            policy,
            "app.example.test",
            true,
            false,
            &method,
            &uri,
            headers,
            channel,
            0,
            now,
        )
        .await
    }

    fn response(outcome: Enforced) -> Response {
        match outcome {
            Enforced::Respond(response) => response,
            Enforced::Admit(_) => panic!("admitted"),
        }
    }

    /// A stand-in control plane on loopback that answers redeem/logout.
    async fn control_plane(
        redeem_status: StatusCode,
    ) -> (GateChannel, Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_redeem = Arc::clone(&seen);
        let seen_logout = Arc::clone(&seen);
        let seen_keys = Arc::clone(&seen);
        let app = axum::Router::new()
            .route(
                "/api/internal/nodes/9/gate/access-keys",
                post(move |Json(body): Json<GateAccessKeysRequest>| {
                    let seen = Arc::clone(&seen_keys);
                    async move {
                        seen.lock()
                            .unwrap()
                            .push(format!("access-keys {}", body.address_id));
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/api/internal/nodes/9/gate/redeem",
                post(
                    move |headers: HeaderMap, Json(body): Json<GateRedeemRequest>| {
                        let seen = Arc::clone(&seen_redeem);
                        async move {
                            seen.lock().unwrap().push(format!(
                                "redeem {} {} {}",
                                body.address_id,
                                headers
                                    .get("authorization")
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or_default(),
                                body.code_verifier.len()
                            ));
                            if redeem_status != StatusCode::OK {
                                return Err(redeem_status);
                            }
                            Ok(Json(GateRedeemResponse {
                                token: "v1.tok.sig".into(),
                                expires_at: gate::unix_now() + 600,
                            }))
                        }
                    },
                ),
            )
            .route(
                "/api/internal/nodes/9/gate/logout",
                post(move |Json(body): Json<GateLogoutRequest>| {
                    let seen = Arc::clone(&seen_logout);
                    async move {
                        seen.lock().unwrap().push(format!("logout {}", body.token));
                        StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let channel = GateChannel::new(
            format!("http://{address}"),
            9,
            "node-token".into(),
            "worker-1".into(),
            None,
        )
        .unwrap();
        (channel, seen)
    }

    fn callback_request(now: i64) -> (String, HeaderMap) {
        let state = GateState::new("/after?x=1", now).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("{}={}", gate::GATE_STATE_COOKIE, state.encode()))
                .unwrap(),
        );
        let code = gate::new_random_value("test").unwrap();
        (
            format!("{}?code={code}&state={}", gate::CALLBACK_PATH, state.state),
            headers,
        )
    }

    #[tokio::test]
    async fn a_valid_token_is_admitted_without_its_cookie_and_with_identity() {
        let key = gate::generate_signing_key().unwrap();
        let now = gate::unix_now();
        let token = gate::sign_token(&key, &claims(now)).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("app=1; {GATE_COOKIE}={token}")).unwrap(),
        );
        let outcome = run(&policy(&key), Method::GET, "/", &mut headers, None, now).await;
        let Enforced::Admit(identity) = outcome else {
            panic!("a valid token was refused");
        };
        assert_eq!(identity[0].0, gate::USER_HEADER);
        assert_eq!(identity[0].1, "11");
        assert_eq!(identity[1].1, "member@example.test");
        assert_eq!(headers.get("cookie").unwrap(), "app=1");
    }

    #[tokio::test]
    async fn signed_out_navigation_redirects_and_api_calls_get_401() {
        let key = gate::generate_signing_key().unwrap();
        let (channel, _) = control_plane(StatusCode::OK).await;
        let mut nav = HeaderMap::new();
        nav.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        let answer = response(
            run(
                &policy(&key),
                Method::GET,
                "/",
                &mut nav,
                Some(&channel),
                1_000,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::FOUND);
        assert_eq!(answer.headers().get(GATE_HEADER).unwrap(), GATE_SIGN_IN);

        let mut api = HeaderMap::new();
        let answer = response(
            run(
                &policy(&key),
                Method::POST,
                "/api",
                &mut api,
                Some(&channel),
                1_000,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(answer.headers().get(GATE_HEADER).unwrap(), GATE_SIGN_IN);
    }

    #[tokio::test]
    async fn callback_redeems_over_the_node_channel_and_sets_the_cookie() {
        let key = gate::generate_signing_key().unwrap();
        let (channel, seen) = control_plane(StatusCode::OK).await;
        let now = gate::unix_now();
        let (uri, mut headers) = callback_request(now);
        let answer = response(
            run(
                &policy(&key),
                Method::GET,
                &uri,
                &mut headers,
                Some(&channel),
                now,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::FOUND);
        assert_eq!(answer.headers().get("location").unwrap(), "/after?x=1");
        let cookies: Vec<_> = answer.headers().get_all("set-cookie").iter().collect();
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0]
            .to_str()
            .unwrap()
            .starts_with("__Host-temps_gate=v1.tok.sig; Path=/; Secure; HttpOnly; SameSite=Lax"));
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["redeem environment:4 Bearer node-token 43"]
        );
    }

    #[tokio::test]
    async fn a_refused_code_is_a_sign_in_failure_without_the_control_planes_details() {
        let key = gate::generate_signing_key().unwrap();
        let (channel, _) = control_plane(StatusCode::BAD_REQUEST).await;
        let now = gate::unix_now();
        let (uri, mut headers) = callback_request(now);
        let answer = response(
            run(
                &policy(&key),
                Method::GET,
                &uri,
                &mut headers,
                Some(&channel),
                now,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::BAD_REQUEST);
        assert!(answer
            .headers()
            .get_all("set-cookie")
            .iter()
            .all(|c| c.to_str().unwrap().starts_with("__Host-temps_gate_state=;")));
    }

    #[tokio::test]
    async fn an_unreachable_control_plane_names_the_node() {
        let key = gate::generate_signing_key().unwrap();
        let channel = GateChannel::new(
            // Nothing listens on port 9 of loopback.
            "http://127.0.0.1:9".into(),
            9,
            "node-token".into(),
            "worker-1".into(),
            None,
        )
        .unwrap();
        let now = gate::unix_now();
        let (uri, mut headers) = callback_request(now);
        let answer = response(
            run(
                &policy(&key),
                Method::GET,
                &uri,
                &mut headers,
                Some(&channel),
                now,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(answer.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("Sign-in is unavailable: worker-1 cannot reach the control plane."),
            "{body}"
        );
    }

    #[test]
    fn a_stale_control_plane_contact_is_out_of_reach() {
        let now = 10_000;
        assert!(control_plane_out_of_reach(
            now - CONTROL_PLANE_STALE_AFTER_SECS - 1,
            now
        ));
        assert!(!control_plane_out_of_reach(now - 5, now));
        // Before the first round there is nothing to judge by.
        assert!(!control_plane_out_of_reach(0, now));
    }

    #[tokio::test]
    async fn a_stale_control_plane_turns_sign_in_into_the_unavailable_page() {
        let key = gate::generate_signing_key().unwrap();
        let (channel, _) = control_plane(StatusCode::OK).await;
        let now = 10_000;
        let mut nav = HeaderMap::new();
        nav.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        let uri: axum::http::Uri = "/".parse().unwrap();
        let answer = response(
            enforce_with(
                &policy(&key),
                "app.example.test",
                true,
                false,
                &Method::GET,
                &uri,
                &mut nav,
                Some(&channel),
                now - 600,
                now,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(answer.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("worker-1 cannot reach the control plane"));
    }

    #[tokio::test]
    async fn without_a_channel_nobody_new_is_admitted_but_tokens_still_are() {
        let key = gate::generate_signing_key().unwrap();
        let now = gate::unix_now();
        let mut nav = HeaderMap::new();
        nav.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        let answer = response(run(&policy(&key), Method::GET, "/", &mut nav, None, now).await);
        assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);

        let token = gate::sign_token(&key, &claims(now)).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("{GATE_COOKIE}={token}")).unwrap(),
        );
        assert!(matches!(
            run(&policy(&key), Method::GET, "/", &mut headers, None, now).await,
            Enforced::Admit(_)
        ));
    }

    #[tokio::test]
    async fn logout_is_same_origin_only_and_revokes_over_the_channel() {
        let key = gate::generate_signing_key().unwrap();
        let (channel, seen) = control_plane(StatusCode::OK).await;
        let mut cross = HeaderMap::new();
        cross.insert(
            "origin",
            HeaderValue::from_static("https://elsewhere.example.test"),
        );
        cross.insert(
            "cookie",
            HeaderValue::from_static("__Host-temps_gate=v1.a.b"),
        );
        let answer = response(
            run(
                &policy(&key),
                Method::POST,
                gate::LOGOUT_PATH,
                &mut cross,
                Some(&channel),
                1_000,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::FORBIDDEN);
        assert!(
            seen.lock().unwrap().is_empty(),
            "a cross-site form revoked a session"
        );

        let mut same = HeaderMap::new();
        same.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        same.insert(
            "cookie",
            HeaderValue::from_static("__Host-temps_gate=v1.a.b"),
        );
        let answer = response(
            run(
                &policy(&key),
                Method::POST,
                gate::LOGOUT_PATH,
                &mut same,
                Some(&channel),
                1_000,
            )
            .await,
        );
        assert_eq!(answer.status(), StatusCode::OK);
        assert_eq!(seen.lock().unwrap().as_slice(), ["logout v1.a.b"]);
    }

    fn access_policy() -> GatePolicy {
        GatePolicy {
            address_id: "custom:5".into(),
            gate: GateKind::CloudflareAccess,
            temps: None,
            access: Some(temps_core::gate::AccessGate {
                issuer: "https://team.cloudflareaccess.com".into(),
                audience: "aud-1".into(),
                keys: vec![],
            }),
        }
    }

    /// A JWT whose header names RS256 and `kid`; the rest is never read
    /// once the key is unknown.
    fn jwt_with_kid(kid: &str) -> HeaderValue {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = b64.encode(format!(r#"{{"alg":"RS256","kid":"{kid}","typ":"JWT"}}"#));
        let claims = b64.encode(r#"{"aud":["aud-1"]}"#);
        HeaderValue::from_str(&format!("{header}.{claims}.c2ln")).unwrap()
    }

    async fn run_access(
        headers: &mut HeaderMap,
        via_tunnel: bool,
        channel: Option<&GateChannel>,
        now: i64,
    ) -> Response {
        let uri: axum::http::Uri = "/".parse().unwrap();
        response(
            enforce_with(
                &access_policy(),
                "app.example.test",
                true,
                via_tunnel,
                &Method::GET,
                &uri,
                headers,
                channel,
                now,
                now,
            )
            .await,
        )
    }

    #[tokio::test]
    async fn an_access_address_off_the_tunnel_is_refused_with_x_temps_gate_access() {
        let mut headers = HeaderMap::new();
        headers.insert(gate_access::ACCESS_JWT_HEADER, jwt_with_kid("k1"));
        let answer = run_access(&mut headers, false, None, 1_000).await;
        assert_eq!(answer.status(), StatusCode::FORBIDDEN);
        assert_eq!(answer.headers()[GATE_HEADER], gate::GATE_ACCESS);
        let body = axum::body::to_bytes(answer.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("only through its"));
        // Through the tunnel with no JWT: refused just the same.
        let answer = run_access(&mut HeaderMap::new(), true, None, 1_000).await;
        assert_eq!(answer.status(), StatusCode::FORBIDDEN);
        assert_eq!(answer.headers()[GATE_HEADER], gate::GATE_ACCESS);
    }

    #[tokio::test]
    async fn an_unknown_access_key_asks_for_the_keys_again_at_most_once_a_minute() {
        let (channel, seen) = control_plane(StatusCode::OK).await;
        let mut headers = HeaderMap::new();
        headers.insert(gate_access::ACCESS_JWT_HEADER, jwt_with_kid("rotated"));
        let now = gate::unix_now();
        access_keys_slot("https://team.cloudflareaccess.com").store(0, Ordering::Release);
        for _ in 0..3 {
            let answer = run_access(&mut headers, true, Some(&channel), now).await;
            assert_eq!(answer.status(), StatusCode::FORBIDDEN);
        }
        for _ in 0..50 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(seen.lock().unwrap().as_slice(), ["access-keys custom:5"]);
    }

    /// A bogus key sent to one team's address every minute never holds back
    /// the request a real rotation of another team needs.
    #[test]
    fn each_team_has_its_own_access_keys_slot() {
        // Clear of the slot the other Access test uses, as tests run at once.
        let taken = access_keys_slot_index("https://team.cloudflareaccess.com");
        let free = |team: &String| access_keys_slot_index(team) != taken;
        let first = (0..)
            .map(|n| format!("https://one{n}.cloudflareaccess.com"))
            .find(free)
            .expect("some team hashes to a free slot");
        let first = first.as_str();
        let other = (0..)
            .map(|n| format!("https://team{n}.cloudflareaccess.com"))
            .find(|team| {
                free(team) && access_keys_slot_index(team) != access_keys_slot_index(first)
            })
            .expect("some team hashes to another slot");
        assert_eq!(access_keys_slot_index(first), access_keys_slot_index(first));
        let now = 5_000_000;
        access_keys_slot(first).store(0, Ordering::Release);
        access_keys_slot(&other).store(0, Ordering::Release);
        assert!(claim_access_keys_slot(access_keys_slot(first), now));
        assert!(!claim_access_keys_slot(access_keys_slot(first), now + 59));
        assert!(claim_access_keys_slot(access_keys_slot(&other), now + 59));
    }

    #[test]
    fn the_access_keys_slot_is_claimed_once_per_minute() {
        let last = AtomicI64::new(0);
        assert!(claim_access_keys_slot(&last, 1_000));
        assert!(!claim_access_keys_slot(&last, 1_030));
        assert!(claim_access_keys_slot(&last, 1_060));
    }
}
