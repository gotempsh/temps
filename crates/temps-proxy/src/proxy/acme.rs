// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ACME HTTP-01 challenge answers and the port-80 on-demand TLS responses (ADR-018).

use super::*;

/// Path prefix reserved for ACME HTTP-01 challenge validation (RFC 8555 §8.3).
///
/// Single source of truth: both the challenge responder and the HTTP→HTTPS
/// redirect gate compare against this, so a request that could be a Let's
/// Encrypt validation can never be redirected out from under the CA.
pub const ACME_HTTP01_PREFIX: &str = "/.well-known/acme-challenge/";

/// Map an on-demand cert in-process state to the port-80 503 response the end
/// user sees while the TLS handshake fast-fails (ADR-018 §5). Pure so the
/// status/body contract is unit-tested without a Pingora session.
fn on_demand_cert_state_response(
    state: &crate::on_demand_cert::OnDemandCertState,
) -> (u16, &'static [u8]) {
    match state {
        crate::on_demand_cert::OnDemandCertState::Pending
        | crate::on_demand_cert::OnDemandCertState::Issuing => (
            503,
            b"TLS certificate provisioning in progress. Retry in a few seconds.\n",
        ),
        crate::on_demand_cert::OnDemandCertState::Failed { .. } => (
            503,
            b"TLS certificate issuance failed. Contact your administrator.\n",
        ),
    }
}

/// Build the `redirect_to_env` Location for an ephemeral per-deployment host
/// (ADR-018 §2): the stable per-environment URL `<env_subdomain>.<preview>`
/// with the original path + query preserved. Returns `None` (→ serve plain
/// HTTP instead) when the env subdomain or preview domain is missing/empty, or
/// when the computed target equals the request host (which would loop). Pure so
/// the target derivation is unit-tested without a Pingora session.
fn ephemeral_redirect_location(
    env_subdomain: Option<&str>,
    preview_domain: &str,
    request_host: &str,
    request_path: &str,
    request_query: Option<&str>,
) -> Option<String> {
    let env_subdomain = env_subdomain.map(str::trim).filter(|s| !s.is_empty())?;
    let preview_domain = preview_domain.trim();
    if preview_domain.is_empty() {
        return None;
    }
    let target_host = format!("{}.{}", env_subdomain, preview_domain);
    // Avoid a redirect loop if the ephemeral host somehow equals its target.
    if target_host.eq_ignore_ascii_case(request_host) {
        return None;
    }
    let location = match request_query {
        Some(q) if !q.is_empty() => format!("https://{}{}?{}", target_host, request_path, q),
        _ => format!("https://{}{}", target_host, request_path),
    };
    Some(location)
}

impl LoadBalancer {
    /// Answers an ACME HTTP-01 request whose token is stored for this host,
    /// and nothing else: `Ok(false)` for every other path.
    pub(super) async fn answer_stored_acme_challenge(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<bool> {
        if !ctx.path.starts_with(ACME_HTTP01_PREFIX) {
            return Ok(false);
        }
        let Some(key_authorization) = self
            .handle_acme_http_challenge(&ctx.host, &ctx.path)
            .await?
        else {
            return Ok(false);
        };
        let body = Bytes::from(key_authorization);
        let mut resp = ResponseHeader::build(200, None)?;
        resp.insert_header("Content-Type", "text/plain")?;
        resp.insert_header("Cache-Control", "no-cache")?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        resp.insert_header("Content-Length", body.len().to_string())?;
        session.write_response_header(Box::new(resp), false).await?;
        session.write_response_body(Some(body), true).await?;
        ctx.routing_status = "acme_challenge".to_string();
        Ok(true)
    }

    pub(super) async fn handle_acme_http_challenge(&self, host: &str, path: &str) -> Result<Option<String>> {
        if !path.starts_with(ACME_HTTP01_PREFIX) {
            return Ok(None);
        }

        let token = &path[ACME_HTTP01_PREFIX.len()..];
        if token.is_empty() {
            debug!("Empty ACME challenge token in path: {}", path);
            return Ok(None);
        }

        debug!(
            "Looking up ACME HTTP-01 challenge for domain: {}, token: {}",
            host, token
        );

        let key_authorization = self
            .acme_challenges
            .lookup(host, token)
            .await
            .map_err(|e| {
                error!("ACME challenge lookup failed: {}", e);
                Error::new_str("Database error during ACME challenge lookup")
            })?;

        match &key_authorization {
            Some(_) => debug!(
                "Found ACME HTTP-01 challenge for domain: {}, returning key authorization",
                host
            ),
            None => debug!(
                "No matching ACME challenge found for domain: {}, token: {}",
                host, token
            ),
        }

        Ok(key_authorization)
    }

    /// On-demand HTTP-01 TLS UX on port 80 (ADR-018 §5, "what the end user
    /// sees"). Runs only for plain-HTTP (non-TLS) requests that are NOT ACME
    /// challenges — ACME handling already short-circuits before this is called,
    /// so a challenge can always complete.
    ///
    /// Two independent behaviors, both driven off the proxy's in-process caches
    /// (no DB I/O in the hot path):
    ///
    /// 1. **Cert-state 503.** When the host is currently in the on-demand cert
    ///    manager's in-process state cache, the TLS handshake is fast-failing
    ///    (Option B) and the user would otherwise see only an opaque TLS error.
    ///    We give them a human-readable signal on :80:
    ///      - `Pending` / `Issuing` → 503 "provisioning in progress, retry…".
    ///      - `Failed`              → 503 "issuance failed, contact admin".
    ///
    ///    A successful issuance removes the entry, so the next request flows
    ///    through to the HTTPS redirect / normal routing.
    ///
    /// 2. **Ephemeral `deployment_url_mode`.** Per-deployment hostnames are
    ///    `cert_eligible == false` and are NEVER certed (ADR §2). They are never
    ///    in an on-demand cert state, so this is independent of (1). When
    ///    `deployment_url_mode == "redirect_to_env"` we 308-redirect such a host
    ///    to its STABLE per-environment URL (`<env.subdomain>.<preview_domain>`),
    ///    which IS certed; otherwise (`"http"`, the default) we serve plain HTTP
    ///    by returning `Ok(false)` and letting normal routing proceed.
    ///
    /// The caller MUST have already confirmed this is a plain-HTTP connection
    /// (`!is_tls_connection`) and that the manager is wired, so this function
    /// performs NO TLS check and NO settings fetch in the common path. Settings
    /// are loaded lazily only when an ephemeral host actually reaches the
    /// `redirect_to_env` branch — `get_settings()` is TTL-cached in
    /// `temps_config::ConfigService`, so even on that rare branch it is a
    /// fast in-memory read rather than a Postgres round-trip.
    ///
    /// Returns `Ok(true)` when a response was written (caller must return early),
    /// `Ok(false)` when the request should continue down the normal path.
    pub(super) async fn handle_on_demand_http(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<bool> {
        // (1) Cert-state 503 — served purely from the in-process cache, no DB.
        if let Some(ref manager) = self.on_demand_cert_manager {
            if let Some(state) = manager.state_of(&ctx.host) {
                let (status, body) = on_demand_cert_state_response(&state);

                let body_bytes = Bytes::from_static(body);
                let mut resp = ResponseHeader::build(status, None)?;
                resp.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                resp.insert_header("Cache-Control", "no-store")?;
                resp.insert_header("Retry-After", "5")?;
                resp.insert_header("Content-Length", body_bytes.len().to_string())?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(Some(body_bytes), true).await?;
                ctx.routing_status = "on_demand_cert_provisioning".to_string();
                return Ok(true);
            }
        }

        // (2) Ephemeral `deployment_url_mode` redirect. Classify the host via the
        // in-memory route table FIRST (cheap), so we only pay the settings DB
        // fetch for a genuinely ephemeral, routed host.
        let Some(ref route_table) = self.route_table else {
            return Ok(false);
        };
        let Some(route) = route_table.get_route(&ctx.host) else {
            // Unknown host — leave normal routing (admin gate / 404) to decide.
            return Ok(false);
        };
        // Stable, cert-eligible hosts are handled by the normal HTTPS path; only
        // ephemeral per-deployment hostnames get the redirect treatment.
        if route.cert_eligible {
            return Ok(false);
        }

        // Only now — for an ephemeral routed host — do we need the setting that
        // decides http-vs-redirect. This is the rare branch; get_settings() is
        // TTL-cached so it is an in-memory read, not a Postgres round-trip.
        let settings = match self.config_service.get_settings().await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    request_id = %ctx.request_id,
                    host = %ctx.host,
                    error = %e,
                    "on-demand TLS: failed to load settings for ephemeral redirect; serving HTTP"
                );
                return Ok(false);
            }
        };
        if settings.on_demand_tls.deployment_url_mode != "redirect_to_env" {
            return Ok(false);
        }

        // Derive the stable per-environment target: `<env.subdomain>.<preview>`.
        let env_subdomain = route.environment.as_ref().map(|e| e.subdomain.as_str());
        let Some(location) = ephemeral_redirect_location(
            env_subdomain,
            &settings.preview_domain,
            &ctx.host,
            &ctx.path,
            ctx.query_string.as_deref(),
        ) else {
            // Can't build a stable target (missing env subdomain / preview
            // domain, or it would loop) — fall back to serving HTTP.
            return Ok(false);
        };

        debug!(
            request_id = %ctx.request_id,
            host = %ctx.host,
            target = %location,
            "on-demand TLS: redirecting ephemeral deployment host to stable env URL"
        );

        // 308 Permanent Redirect preserves the method and body (ADR §2).
        let mut resp = ResponseHeader::build(308, None)?;
        resp.insert_header("Location", &location)?;
        resp.insert_header("Content-Length", "0")?;
        resp.insert_header("Cache-Control", "no-store")?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        session.write_response_header(Box::new(resp), true).await?;
        ctx.routing_status = "on_demand_deployment_redirect".to_string();
        Ok(true)
    }
}

#[cfg(test)]
mod on_demand_http_tests {
    //! Unit tests for the ADR-018 §5 port-80 on-demand TLS UX decision logic.
    //! The session-writing wrapper (`handle_on_demand_http`) is exercised in
    //! integration; here we pin the two pure helpers it delegates to so the
    //! 503 contract and the `redirect_to_env` target derivation are locked.
    use super::{
        ephemeral_redirect_location, on_demand_cert_state_response,
        should_lookup_sleeping_environment, unavailable_page,
    };
    use crate::on_demand_cert::OnDemandCertState;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use temps_routes::{BackendEntry, BackendType, CachedPeerTable, RouteInfo};

    fn test_route() -> RouteInfo {
        RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![BackendEntry {
                    address: "127.0.0.1:8080".to_string(),
                    container_id: None,
                    container_name: None,
                }],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        }
    }

    #[test]
    fn active_or_reserved_host_suppresses_sleeping_wildcard_lookup() {
        let table = CachedPeerTable::new(Arc::new(sea_orm::DatabaseConnection::Disconnected));
        table.insert_route_for_test("api.apps.example.com", test_route());
        table.insert_tls_route_for_test("tcp.apps.example.com", test_route());
        table.reserve_hostname_for_test("console.apps.example.com");

        assert!(!should_lookup_sleeping_environment(
            Some(&table),
            "api.apps.example.com"
        ));
        assert!(!should_lookup_sleeping_environment(
            Some(&table),
            "tcp.apps.example.com"
        ));
        assert!(!should_lookup_sleeping_environment(
            Some(&table),
            "console.apps.example.com"
        ));
        assert!(should_lookup_sleeping_environment(
            Some(&table),
            "preview.apps.example.com"
        ));
    }

    /// A workspace preview that cannot be reached is not "your deployment":
    /// its page names the preview gateway and where to look at it.
    #[test]
    fn an_unreachable_preview_names_the_gateway_not_a_deployment() {
        let preview = unavailable_page(true);
        assert!(
            preview.contains("<h1>Preview Unavailable</h1>"),
            "{preview}"
        );
        assert!(preview.contains("preview gateway"), "{preview}");
        assert!(
            preview.contains("AI runtime settings &rarr; Preview Gateway"),
            "{preview}"
        );
        assert!(!preview.contains("deployment"), "{preview}");

        let app = unavailable_page(false);
        assert!(app.contains("<h1>Service Unavailable</h1>"), "{app}");
        assert!(
            app.contains("check that your deployment is running"),
            "{app}"
        );
    }

    #[test]
    fn pending_and_issuing_map_to_provisioning_503() {
        let (status, body) = on_demand_cert_state_response(&OnDemandCertState::Pending);
        assert_eq!(status, 503);
        assert_eq!(
            body,
            b"TLS certificate provisioning in progress. Retry in a few seconds.\n"
        );

        let (status, body) = on_demand_cert_state_response(&OnDemandCertState::Issuing);
        assert_eq!(status, 503);
        assert_eq!(
            body,
            b"TLS certificate provisioning in progress. Retry in a few seconds.\n"
        );
    }

    #[test]
    fn failed_maps_to_issuance_failed_503() {
        let (status, body) = on_demand_cert_state_response(&OnDemandCertState::Failed {
            backoff_until_epoch: 12345,
        });
        assert_eq!(status, 503);
        assert_eq!(
            body,
            b"TLS certificate issuance failed. Contact your administrator.\n"
        );
    }

    #[test]
    fn redirect_target_is_stable_env_url_preserving_path() {
        // Ephemeral host `myapp-prod-42.1.2.3.4.sslip.io` (env subdomain
        // `myapp-prod`) → stable `myapp-prod.1.2.3.4.sslip.io`.
        let location = ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/dashboard",
            None,
        )
        .expect("should build a redirect target");
        assert_eq!(location, "https://myapp-prod.1.2.3.4.sslip.io/dashboard");
    }

    #[test]
    fn redirect_target_preserves_query_string() {
        let location = ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/search",
            Some("q=temps&page=2"),
        )
        .expect("should build a redirect target");
        assert_eq!(
            location,
            "https://myapp-prod.1.2.3.4.sslip.io/search?q=temps&page=2"
        );
    }

    #[test]
    fn redirect_target_ignores_empty_query_string() {
        let location = ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/",
            Some(""),
        )
        .expect("should build a redirect target");
        assert_eq!(location, "https://myapp-prod.1.2.3.4.sslip.io/");
    }

    #[test]
    fn no_redirect_without_env_subdomain() {
        assert!(ephemeral_redirect_location(
            None,
            "1.2.3.4.sslip.io",
            "myapp-prod-42.1.2.3.4.sslip.io",
            "/",
            None,
        )
        .is_none());
    }

    #[test]
    fn no_redirect_with_blank_env_subdomain_or_preview_domain() {
        assert!(ephemeral_redirect_location(
            Some("   "),
            "1.2.3.4.sslip.io",
            "ephemeral.host",
            "/",
            None,
        )
        .is_none());
        assert!(ephemeral_redirect_location(
            Some("myapp-prod"),
            "   ",
            "ephemeral.host",
            "/",
            None,
        )
        .is_none());
    }

    #[test]
    fn no_redirect_when_target_equals_request_host_avoids_loop() {
        // If the computed target is the request host (case-insensitive), we must
        // not redirect — that would loop forever.
        assert!(ephemeral_redirect_location(
            Some("myapp-prod"),
            "1.2.3.4.sslip.io",
            "MyApp-Prod.1.2.3.4.sslip.io",
            "/",
            None,
        )
        .is_none());
    }
}
