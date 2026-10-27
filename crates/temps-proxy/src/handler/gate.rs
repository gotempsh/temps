// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The signed-in gate on the control-plane proxy (ADR 065 D8).
//!
//! Every decision is `temps_core::gate::decide`, the same function the
//! agent's ingress calls, so the two listeners cannot disagree about who is
//! let in. What lives here is the proxy's transport: it redeems sign-in codes
//! and revokes sessions in process through `GateService`, and renders the
//! gate's answers as Pingora responses.
//!
//! Nothing here runs for a host that is not signed in (D11): the route
//! snapshot only has a policy for `signed_in` addresses, and without one the
//! request goes on exactly as before.

use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue};
use bytes::Bytes;
use pingora_http::{RequestHeader, ResponseHeader};
use temps_core::gate::{
    self, GateClaims, GateDecision, GateLogoutRequest, GatePolicy, GateRedeemRequest, GateRequest,
    GateResponse, GATE_COOKIE,
};
use temps_core::gate_access::{self, AccessRejection};
use temps_routes::gate::{GateService, GateServiceError};
use temps_routes::CachedPeerTable;
use tracing::{error, info, warn};

/// What the proxy does with a request to a gated host.
#[derive(Debug)]
pub enum GateOutcome {
    /// Forward it, as this user.
    Admit(GateClaims),
    /// Answer it here; the second field is the proxy log's routing status.
    Respond(GateResponse, &'static str),
}

/// The gate's state on the proxy: the route table it reads policies from and
/// the service it redeems codes with.
pub struct ProxyGate {
    routes: Arc<CachedPeerTable>,
    service: Arc<GateService>,
}

impl ProxyGate {
    pub fn new(routes: Arc<CachedPeerTable>, service: Arc<GateService>) -> Self {
        Self { routes, service }
    }

    /// The gate for `host`, if it is a signed-in address. An in-memory
    /// lookup that returns at once when no address is signed in.
    pub fn policy_for(&self, host: &str) -> Option<Arc<GatePolicy>> {
        self.routes.gate_for_host(host)
    }

    /// Decide and, for the gate's own endpoints, act.
    pub async fn evaluate(
        &self,
        policy: &GatePolicy,
        request: &GateRequest<'_>,
        now: i64,
    ) -> GateOutcome {
        let decision = match gate::decide(policy, request, now) {
            Ok(decision) => decision,
            Err(error) => {
                error!(
                    address_id = %policy.address_id,
                    host = %request.hostname,
                    %error,
                    "could not start a sign-in"
                );
                return GateOutcome::Respond(
                    GateResponse::unavailable(
                        "Sign-in could not be started on this server. Try again in a moment.",
                    ),
                    "gate_unavailable",
                );
            }
        };
        match decision {
            GateDecision::Admit(claims) => GateOutcome::Admit(claims),
            GateDecision::SignIn {
                location,
                state_cookie,
                navigation,
            } => GateOutcome::Respond(
                GateResponse::sign_in(&location, &state_cookie, navigation),
                "gate_sign_in",
            ),
            GateDecision::UpgradeToHttps { location } => {
                GateOutcome::Respond(GateResponse::upgrade(&location), "gate_https_redirect")
            }
            GateDecision::Unavailable { reason } => {
                warn!(
                    address_id = %policy.address_id,
                    host = %request.hostname,
                    reason,
                    "signed-in address cannot be served: its gate is not configured"
                );
                GateOutcome::Respond(GateResponse::unavailable(reason), "gate_unavailable")
            }
            GateDecision::NotFound => {
                GateOutcome::Respond(GateResponse::not_found(), "gate_not_found")
            }
            // Cloudflare Access (D9) only sees tunnel traffic, and tunnels end
            // at a worker's tunnel origin, never at this proxy: an Access
            // address is refused here whatever the request carries.
            GateDecision::AdmitAccess(_) => GateOutcome::Respond(
                gate_access::access_denied(AccessRejection::NotThroughTunnel),
                "gate_access_denied",
            ),
            GateDecision::AccessDenied(rejection) => {
                info!(
                    address_id = %policy.address_id,
                    host = %request.hostname,
                    reason = rejection.as_str(),
                    "Cloudflare Access address refused on the control-plane proxy"
                );
                GateOutcome::Respond(gate_access::access_denied(rejection), "gate_access_denied")
            }
            GateDecision::Callback => self.callback(policy, request, now).await,
            GateDecision::Logout => self.logout(policy, request).await,
        }
    }

    async fn callback(
        &self,
        policy: &GatePolicy,
        request: &GateRequest<'_>,
        now: i64,
    ) -> GateOutcome {
        let input = match gate::check_callback(request.query, request.headers, now) {
            Ok(input) => input,
            Err(refusal) => {
                info!(
                    address_id = %policy.address_id,
                    host = %request.hostname,
                    ?refusal,
                    "sign-in callback refused before redeeming"
                );
                return GateOutcome::Respond(
                    GateResponse::callback_refused(refusal),
                    "gate_callback_refused",
                );
            }
        };
        let redeem = GateRedeemRequest {
            address_id: policy.address_id.clone(),
            code: input.code,
            code_verifier: input.verifier,
        };
        match self.service.redeem(&redeem).await {
            Ok(redeemed) => GateOutcome::Respond(
                GateResponse::signed_in(&redeemed, &input.return_to, now),
                "gate_signed_in",
            ),
            Err(error) => {
                log_service_error(&error, &policy.address_id, "redeem a sign-in code");
                GateOutcome::Respond(
                    GateResponse::redeem_failed(error.http_status(), &error.visitor_message()),
                    "gate_callback_refused",
                )
            }
        }
    }

    async fn logout(&self, policy: &GatePolicy, request: &GateRequest<'_>) -> GateOutcome {
        if !gate::logout_allowed(request.headers, request.hostname) {
            warn!(
                address_id = %policy.address_id,
                host = %request.hostname,
                "cross-origin sign-out refused"
            );
            return GateOutcome::Respond(GateResponse::logout_refused(), "gate_logout_refused");
        }
        let mut revoked = true;
        for token in gate::cookie_values(request.headers, GATE_COOKIE) {
            let logout = GateLogoutRequest {
                address_id: policy.address_id.clone(),
                token: token.to_string(),
            };
            match self.service.logout(&logout).await {
                Ok(()) => {}
                // A token that does not verify has no session to revoke.
                Err(GateServiceError::InvalidToken { .. }) => {}
                Err(error) => {
                    log_service_error(&error, &policy.address_id, "revoke a gate session");
                    revoked = false;
                }
            }
        }
        GateOutcome::Respond(
            GateResponse::signed_out(request.hostname, revoked),
            "gate_signed_out",
        )
    }
}

fn log_service_error(error: &GateServiceError, address_id: &str, operation: &str) {
    if error.http_status() >= 500 {
        error!(address_id, %error, "gate could not {operation}");
    } else {
        info!(address_id, %error, "gate refused to {operation}");
    }
}

/// The hostname a gate policy is looked up by: no port, lower-case.
pub fn gate_hostname(host: &str) -> String {
    let host = host.trim();
    let without_port = match host.rsplit_once(':') {
        Some((name, port)) if !name.ends_with(']') && port.bytes().all(|b| b.is_ascii_digit()) => {
            name
        }
        _ => host,
    };
    without_port.trim_end_matches('.').to_ascii_lowercase()
}

/// Remove every identity header a client sent (`x-temps-user*`). Runs on
/// every request, gated or not: only Temps may tell an app who is signed in.
pub fn strip_client_identity(request: &mut RequestHeader) {
    let names: Vec<_> = request
        .headers
        .keys()
        .filter(|name| gate::is_user_identity_header(name.as_str()))
        .cloned()
        .collect();
    for name in names {
        let _ = request.remove_header(&name);
    }
}

/// Remove the gate's cookies from a request on its way to an app. Runs on
/// every upstream request, admitted or not: a cookie left from before an
/// address stopped being signed-in is still a token. A request without one
/// is left as it is.
pub fn strip_gate_cookies(request: &mut RequestHeader) -> pingora::Result<()> {
    if !gate::may_hold_gate_cookie(&request.headers) {
        return Ok(());
    }
    let mut cookies = HeaderMap::new();
    for value in request.headers.get_all(axum::http::header::COOKIE) {
        cookies.append(axum::http::header::COOKIE, value.clone());
    }
    gate::strip_gate_cookies(&mut cookies);
    let _ = request.remove_header("cookie");
    for value in cookies.get_all(axum::http::header::COOKIE) {
        request.append_header("cookie", value.clone())?;
    }
    Ok(())
}

/// An admitted request: the gate's cookies never reach the app, and the app
/// learns who signed in.
pub fn admit(request: &mut RequestHeader, claims: &GateClaims) -> pingora::Result<()> {
    strip_gate_cookies(request)?;
    strip_client_identity(request);
    for (name, value) in gate::identity_headers(claims) {
        request.insert_header(name, value)?;
    }
    Ok(())
}

/// An app's response to a gated host may neither pass for the gate
/// (`x-temps-gate`) nor set or clear the gate's cookies.
pub fn strip_gate_from_app_response(response: &mut ResponseHeader) -> pingora::Result<()> {
    let _ = response.remove_header(gate::GATE_HEADER);
    let set_cookie = axum::http::header::SET_COOKIE;
    if !response
        .headers
        .get_all(&set_cookie)
        .iter()
        .any(|value| gate::set_cookie_names_gate(value.as_bytes()))
    {
        return Ok(());
    }
    let kept: Vec<HeaderValue> = response
        .headers
        .get_all(&set_cookie)
        .iter()
        .filter(|value| !gate::set_cookie_names_gate(value.as_bytes()))
        .cloned()
        .collect();
    let _ = response.remove_header("set-cookie");
    for value in kept {
        response.append_header("set-cookie", value)?;
    }
    Ok(())
}

/// Render a gate answer as a Pingora response header and body.
pub fn render(
    answer: &GateResponse,
    request_id: &str,
) -> pingora::Result<(ResponseHeader, Option<Bytes>)> {
    let mut response = ResponseHeader::build(answer.status, None)?;
    for (name, value) in &answer.headers {
        response.append_header(*name, value.as_str())?;
    }
    response.insert_header("X-Request-ID", request_id)?;
    let body = answer.body.as_ref().map(|body| Bytes::from(body.clone()));
    response.insert_header(
        "Content-Length",
        body.as_ref().map_or(0, |body| body.len()).to_string(),
    )?;
    Ok((response, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use temps_core::gate::{GateKind, GateState, TempsGate, GATE_HEADER, GATE_SIGN_IN};

    fn service() -> Arc<GateService> {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        Arc::new(GateService::new(
            Arc::new(db),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "gate-test",
            )),
        ))
    }

    fn proxy_gate() -> ProxyGate {
        ProxyGate::new(
            Arc::new(CachedPeerTable::new(Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ))),
            service(),
        )
    }

    fn policy(key: &gate::SigningKey) -> GatePolicy {
        GatePolicy {
            address_id: "custom:7".into(),
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

    fn request<'a>(
        method: &'a Method,
        path: &'a str,
        query: Option<&'a str>,
        headers: &'a HeaderMap,
    ) -> GateRequest<'a> {
        GateRequest {
            hostname: "app.example.test",
            method,
            path,
            query,
            headers,
            secure: true,
            via_tunnel: false,
        }
    }

    fn claims(address_id: &str, now: i64) -> GateClaims {
        GateClaims {
            sid: "s".repeat(43),
            uid: 3,
            email: "person@example.test".into(),
            aid: address_id.into(),
            iat: now - 10,
            exp: now + 3_600,
        }
    }

    fn header<'a>(answer: &'a GateResponse, name: &str) -> Vec<&'a str> {
        answer
            .headers
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[tokio::test]
    async fn a_cloudflare_access_address_is_always_refused_on_the_proxy() {
        let policy = GatePolicy {
            address_id: "custom:7".into(),
            gate: GateKind::CloudflareAccess,
            temps: None,
            access: Some(temps_core::gate::AccessGate {
                issuer: "https://team.cloudflareaccess.com".into(),
                audience: "aud-1".into(),
                keys: vec![],
            }),
        };
        let mut headers = HeaderMap::new();
        // Whatever the request carries: a JWT header here never came
        // through Access.
        headers.insert(
            gate_access::ACCESS_JWT_HEADER,
            HeaderValue::from_static("a.b.c"),
        );
        for path in ["/", "/_temps/gate/callback"] {
            let outcome = proxy_gate()
                .evaluate(&policy, &request(&Method::GET, path, None, &headers), 1_000)
                .await;
            let GateOutcome::Respond(answer, status) = outcome else {
                panic!("an Access address was admitted on the proxy");
            };
            assert_eq!(status, "gate_access_denied");
            assert_eq!(answer.status, 403);
            assert_eq!(header(&answer, GATE_HEADER), vec![gate::GATE_ACCESS]);
        }
    }

    #[tokio::test]
    async fn signed_out_navigation_goes_to_the_console_with_the_gate_header() {
        let key = gate::generate_signing_key().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        let outcome = proxy_gate()
            .evaluate(
                &policy(&key),
                &request(&Method::GET, "/dash", None, &headers),
                1_000,
            )
            .await;
        let GateOutcome::Respond(answer, status) = outcome else {
            panic!("a signed-out visitor was admitted");
        };
        assert_eq!(status, "gate_sign_in");
        assert_eq!(answer.status, 302);
        assert_eq!(header(&answer, GATE_HEADER), vec![GATE_SIGN_IN]);
        let location = header(&answer, "location")[0];
        assert!(location.starts_with("https://console.example.test/_temps/gate/authorize?"));
        assert!(location.contains("address=custom%3A7"));
        let cookie = header(&answer, "set-cookie")[0];
        assert!(cookie.starts_with("__Host-temps_gate_state="));
        assert!(cookie.contains("Secure") && cookie.contains("HttpOnly"));
        assert!(cookie.contains("Max-Age=600"));
    }

    #[tokio::test]
    async fn a_token_for_another_address_is_refused_and_its_own_admitted() {
        let key = gate::generate_signing_key().unwrap();
        let now = gate::unix_now();
        let other = gate::sign_token(&key, &claims("custom:8", now)).unwrap();
        let own = gate::sign_token(&key, &claims("custom:7", now)).unwrap();
        let gate = proxy_gate();
        let method = Method::POST;

        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("{GATE_COOKIE}={other}")).unwrap(),
        );
        let outcome = gate
            .evaluate(
                &policy(&key),
                &request(&method, "/api", None, &headers),
                now,
            )
            .await;
        assert!(
            matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 401),
            "a token for another app opened this one: {outcome:?}"
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("{GATE_COOKIE}={own}")).unwrap(),
        );
        let outcome = gate
            .evaluate(
                &policy(&key),
                &request(&method, "/api", None, &headers),
                now,
            )
            .await;
        assert!(matches!(outcome, GateOutcome::Admit(ref c) if c.uid == 3));
    }

    #[tokio::test]
    async fn revocation_in_the_snapshot_signs_the_visitor_out() {
        let key = gate::generate_signing_key().unwrap();
        let now = gate::unix_now();
        let claims = claims("custom:7", now);
        let token = gate::sign_token(&key, &claims).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("{GATE_COOKIE}={token}")).unwrap(),
        );
        headers.insert("accept", HeaderValue::from_static("text/html"));
        let method = Method::GET;
        let gate = proxy_gate();

        let mut revoked = policy(&key);
        if let Some(temps) = revoked.temps.as_mut() {
            temps.revoked = vec![claims.sid.clone()];
        }
        let outcome = gate
            .evaluate(&revoked, &request(&method, "/", None, &headers), now)
            .await;
        assert!(matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 302));

        let mut everyone_out = policy(&key);
        if let Some(temps) = everyone_out.temps.as_mut() {
            temps.not_before = now;
        }
        let outcome = gate
            .evaluate(&everyone_out, &request(&method, "/", None, &headers), now)
            .await;
        assert!(matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 302));
    }

    #[tokio::test]
    async fn callback_without_the_state_cookie_is_refused_before_redeeming() {
        // The mock database has no results queued: any redeem attempt would
        // fail with a database error (500), so a 400 proves none was made.
        let key = gate::generate_signing_key().unwrap();
        let headers = HeaderMap::new();
        let code = gate::new_random_value("test").unwrap();
        let state = gate::new_random_value("test").unwrap();
        let query = format!("code={code}&state={state}");
        let method = Method::GET;
        let outcome = proxy_gate()
            .evaluate(
                &policy(&key),
                &request(&method, gate::CALLBACK_PATH, Some(&query), &headers),
                1_000,
            )
            .await;
        let GateOutcome::Respond(answer, _) = outcome else {
            panic!("callback admitted");
        };
        assert_eq!(answer.status, 400);
    }

    #[tokio::test]
    async fn callback_with_another_browsers_state_is_refused() {
        let key = gate::generate_signing_key().unwrap();
        let now = 1_000;
        let mine = GateState::new("/", now).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("{}={}", gate::GATE_STATE_COOKIE, mine.encode()))
                .unwrap(),
        );
        let code = gate::new_random_value("test").unwrap();
        let theirs = gate::new_random_value("test").unwrap();
        let query = format!("code={code}&state={theirs}");
        let method = Method::GET;
        let outcome = proxy_gate()
            .evaluate(
                &policy(&key),
                &request(&method, gate::CALLBACK_PATH, Some(&query), &headers),
                now,
            )
            .await;
        assert!(matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 400));
    }

    #[tokio::test]
    async fn cross_site_logout_is_refused() {
        let key = gate::generate_signing_key().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        let method = Method::POST;
        let outcome = proxy_gate()
            .evaluate(
                &policy(&key),
                &request(&method, gate::LOGOUT_PATH, None, &headers),
                1_000,
            )
            .await;
        assert!(
            matches!(outcome, GateOutcome::Respond(ref a, "gate_logout_refused") if a.status == 403)
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            HeaderValue::from_static("https://evil.example.test"),
        );
        let outcome = proxy_gate()
            .evaluate(
                &policy(&key),
                &request(&method, gate::LOGOUT_PATH, None, &headers),
                1_000,
            )
            .await;
        assert!(matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 403));
    }

    #[tokio::test]
    async fn same_origin_logout_without_a_cookie_clears_it() {
        let key = gate::generate_signing_key().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        let method = Method::POST;
        let outcome = proxy_gate()
            .evaluate(
                &policy(&key),
                &request(&method, gate::LOGOUT_PATH, None, &headers),
                1_000,
            )
            .await;
        let GateOutcome::Respond(answer, _) = outcome else {
            panic!("logout admitted");
        };
        assert_eq!(answer.status, 200);
        assert_eq!(
            header(&answer, "set-cookie"),
            vec![gate::clear_session_cookie().as_str()]
        );
    }

    #[tokio::test]
    async fn gate_paths_other_than_its_endpoints_are_not_found_and_never_reach_the_app() {
        let key = gate::generate_signing_key().unwrap();
        let headers = HeaderMap::new();
        for (method, path) in [
            (Method::GET, "/_temps/gate/anything"),
            (Method::GET, gate::LOGOUT_PATH),
            (Method::POST, gate::CALLBACK_PATH),
        ] {
            let outcome = proxy_gate()
                .evaluate(
                    &policy(&key),
                    &request(&method, path, None, &headers),
                    1_000,
                )
                .await;
            assert!(
                matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 404),
                "{method} {path}: {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn plain_http_is_sent_to_https_and_a_missing_key_fails_closed() {
        let key = gate::generate_signing_key().unwrap();
        let headers = HeaderMap::new();
        let method = Method::GET;
        let mut plain = request(&method, "/x", None, &headers);
        plain.secure = false;
        let outcome = proxy_gate().evaluate(&policy(&key), &plain, 1_000).await;
        assert!(matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 308));

        let mut keyless = policy(&key);
        keyless.temps = None;
        let outcome = proxy_gate()
            .evaluate(&keyless, &request(&method, "/x", None, &headers), 1_000)
            .await;
        assert!(matches!(outcome, GateOutcome::Respond(ref a, _) if a.status == 503));
    }

    #[test]
    fn admitted_requests_lose_gate_cookies_and_spoofed_identity() {
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request
            .insert_header(
                "cookie",
                "a=1; __Host-temps_gate=tok; __Host-temps_gate_state=s; b=2",
            )
            .unwrap();
        request.insert_header("x-temps-user", "999").unwrap();
        request
            .insert_header("x-temps-user-email", "spoof@example.test")
            .unwrap();
        admit(&mut request, &claims("custom:7", 1_000)).unwrap();
        let cookie = request.headers.get("cookie").unwrap().to_str().unwrap();
        assert!(!cookie.contains("temps_gate"), "{cookie}");
        assert!(cookie.contains("a=1") && cookie.contains("b=2"));
        assert_eq!(request.headers.get("x-temps-user").unwrap(), "3");
        assert_eq!(
            request.headers.get("x-temps-user-email").unwrap(),
            "person@example.test"
        );
        assert_eq!(request.headers.get_all("x-temps-user").iter().count(), 1);
    }

    #[test]
    fn identity_headers_are_stripped_from_every_client_request() {
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request.insert_header("X-Temps-User", "1").unwrap();
        request
            .insert_header("X-Temps-User-Email", "spoof@example.test")
            .unwrap();
        request
            .insert_header("X-Temps-User-Login", "someone@example.test")
            .unwrap();
        request.insert_header("X-Temps-User-Role", "admin").unwrap();
        request.insert_header("X-Other", "kept").unwrap();
        strip_client_identity(&mut request);
        assert!(request.headers.get("x-temps-user").is_none());
        assert!(request.headers.get("x-temps-user-email").is_none());
        assert!(request.headers.get("x-temps-user-login").is_none());
        assert!(request.headers.get("x-temps-user-role").is_none());
        assert_eq!(request.headers.get("x-other").unwrap(), "kept");
    }

    /// A request the gate never admitted -- a host that is no longer
    /// signed-in -- still loses the gate's cookies on its way to the app.
    #[test]
    fn gate_cookies_never_reach_an_app_unadmitted() {
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request
            .append_header("cookie", format!("a=1; {GATE_COOKIE}=old-token"))
            .unwrap();
        request.append_header("cookie", "b=2").unwrap();
        strip_gate_cookies(&mut request).unwrap();
        let cookies: Vec<_> = request.headers.get_all("cookie").iter().collect();
        assert_eq!(cookies, vec![HeaderValue::from_static("a=1; b=2")]);

        let mut plain = RequestHeader::build("GET", b"/", None).unwrap();
        plain.append_header("cookie", "a=1").unwrap();
        plain.append_header("cookie", "b=2").unwrap();
        strip_gate_cookies(&mut plain).unwrap();
        assert_eq!(plain.headers.get_all("cookie").iter().count(), 2);
    }

    #[test]
    fn app_responses_cannot_set_gate_cookies_or_pass_for_the_gate() {
        let mut response = ResponseHeader::build(302, None).unwrap();
        response.append_header("x-temps-gate", "sign-in").unwrap();
        response
            .append_header("set-cookie", "__Host-temps_gate=forged; Path=/")
            .unwrap();
        response
            .append_header("set-cookie", "app=1; Path=/")
            .unwrap();
        strip_gate_from_app_response(&mut response).unwrap();
        assert!(response.headers.get("x-temps-gate").is_none());
        let cookies: Vec<_> = response.headers.get_all("set-cookie").iter().collect();
        assert_eq!(cookies, vec![HeaderValue::from_static("app=1; Path=/")]);
    }

    #[test]
    fn rendering_keeps_every_set_cookie() {
        let redeemed = gate::GateRedeemResponse {
            token: "v1.a.b".into(),
            expires_at: 2_000,
        };
        let answer = GateResponse::signed_in(&redeemed, "/", 1_000);
        let (response, body) = render(&answer, "req-1").unwrap();
        assert_eq!(response.status.as_u16(), 302);
        assert_eq!(response.headers.get_all("set-cookie").iter().count(), 2);
        assert!(body.is_none());
        assert_eq!(response.headers.get("content-length").unwrap(), "0");
    }

    #[test]
    fn gate_hostname_drops_the_port_and_case() {
        assert_eq!(gate_hostname("App.Example.Test:8443"), "app.example.test");
        assert_eq!(gate_hostname("app.example.test."), "app.example.test");
        assert_eq!(gate_hostname("[::1]"), "[::1]");
    }
}
