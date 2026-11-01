// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request access policy: IP restrictions, the request policy gate and the environment access decision (ADR-055 §11).

use super::*;

/// Decide whether a request should be denied by per-project/environment IP
/// restriction, given the client IP the proxy managed to resolve (if any).
///
/// Pulled out of `early_request_filter` as a pure function so the two cases
/// that matter — resolved IP denied by an active policy, and *unresolvable*
/// IP under an active policy — are unit-testable without a full pingora
/// session. See the call site's comment for why an unresolvable IP must fail
/// closed only when this project/environment actually has a policy
/// configured (`ProjectIpGate::has_active_policy`), not unconditionally.
fn ip_restriction_denies(
    gate: &dyn temps_core::ProjectIpGate,
    project_id: i32,
    environment_id: i32,
    parsed_ip: Option<std::net::IpAddr>,
) -> bool {
    match parsed_ip {
        Some(ip) => !gate.is_allowed(project_id, environment_id, ip),
        None => gate.has_active_policy(project_id, environment_id),
    }
}

/// Caps "denied by project IP restriction" log lines at 20 per 10 seconds,
/// process-wide. Denials are always counted in
/// `proxy.ip_access_denied`; only the log line is limited.
pub(super) static IP_DENY_LOG_LIMITER: crate::ip_access::DenyLogLimiter =
    crate::ip_access::DenyLogLimiter::new(10, 20);

/// Caps "shadow evaluation disagrees" log lines at 20 per 10 seconds,
/// process-wide. Every disagreement is counted in
/// `proxy.ingress_shadow_mismatches`; only the log line is limited.
static INGRESS_SHADOW_LOG_LIMITER: crate::ip_access::DenyLogLimiter =
    crate::ip_access::DenyLogLimiter::new(10, 20);

/// Caps "enforcing environment has no compiled endpoint" log lines at 20
/// per 10 seconds, process-wide: a missing snapshot refuses every request
/// of every enforcing environment, which must not become a log flood.
static INGRESS_UNAVAILABLE_LOG_LIMITER: crate::ip_access::DenyLogLimiter =
    crate::ip_access::DenyLogLimiter::new(10, 20);

/// The request facts [`LoadBalancer::decide_environment_access`] reads.
/// Plain borrows of the request context.
pub(super) struct AccessRequest<'a> {
    request: &'a RequestHeader,
    host: &'a str,
    path: &'a str,
    tls_fingerprint: Option<&'a str>,
    policy_decision: temps_core::RequestPolicyDecision,
    ip_restricted: bool,
    client_ip: Option<std::net::IpAddr>,
    rate_limit: Option<crate::rate_limiter::ResolvedRateLimit<'a>>,
    password: Option<crate::ingress::PasswordFacts>,
}

pub(crate) fn legacy_ip_gate_denies(
    decision: temps_core::RequestPolicyDecision,
    gate: &dyn temps_core::ProjectIpGate,
    project_id: i32,
    environment_id: i32,
    parsed_ip: Option<std::net::IpAddr>,
) -> bool {
    match decision {
        temps_core::RequestPolicyDecision::Continue => {
            ip_restriction_denies(gate, project_id, environment_id, parsed_ip)
        }
        temps_core::RequestPolicyDecision::Allow { .. } => {
            gate.is_explicitly_denied(project_id, environment_id, parsed_ip)
        }
        temps_core::RequestPolicyDecision::Deny { .. }
        | temps_core::RequestPolicyDecision::Unavailable { .. } => false,
    }
}

pub(crate) fn normalize_client_ip(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip {
        std::net::IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(std::net::IpAddr::V4)
            .unwrap_or(std::net::IpAddr::V6(v6)),
        ip => ip,
    }
}

pub(super) fn evaluate_request_policy(
    gate: &dyn temps_core::RequestPolicyGate,
    request: &pingora_http::RequestHeader,
    host: &str,
    project_id: i32,
    environment_id: i32,
    client_ip: Option<std::net::IpAddr>,
) -> temps_core::RequestPolicyDecision {
    gate.evaluate(&temps_core::RequestPolicyContext {
        path: request.uri.path(),
        method: request.method.as_str(),
        host,
        project_id,
        environment_id,
        client_ip,
    })
}

impl LoadBalancer {
    /// The access decision for an environment request (ADR-055 §11).
    ///
    /// `legacy` runs the legacy chain. `enforce` runs the environment's
    /// compiled endpoint, and refuses whenever the route snapshot cannot
    /// provide it (never loaded, the endpoint did not compile, or no
    /// endpoint covers the path): a stored policy such as a Temps login
    /// lives only in the endpoint, so falling back to the legacy chain
    /// would drop it. `shadow` decides with the legacy chain and also
    /// evaluates the endpoint from the facts the chain recorded, counting
    /// every disagreement.
    ///
    /// The endpoint stands in for the legacy chain only behind the default
    /// request policy (`Continue`) and an IP gate that reads the
    /// `project_ip_access_rules` table the endpoint's IP policy is derived
    /// from; behind anything else the legacy chain decides.
    ///
    /// No lock, no I/O and no allocation beyond the legacy chain's own: one
    /// snapshot refcount, the compiled policies, the shared rate-limit
    /// counters and, only for a captcha, an in-memory clearance check.
    pub(super) async fn decide_environment_access(
        &self,
        project_ctx: &ProjectContext,
        access: AccessRequest<'_>,
    ) -> crate::ingress::AccessOutcome {
        use crate::ingress::{
            enforce_step, enforcing_endpoint, grant_host, legacy_access, legacy_after_captcha,
            shadow_step, AccessOutcome, EndpointRequest, GrantFacts, LegacyFacts, LegacyRequest,
            Step,
        };
        use temps_entities::ingress_mode::IngressMode;

        let environment = &project_ctx.environment;
        let environment_id = environment.id;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // The captcha's own assets and verification bypass attack mode.
        let captcha_path = access.path.starts_with("/api/_temps/captcha")
            || access.path.starts_with("/api/__temps/temps_captcha_wasm");
        let has_tls_fingerprint = access.tls_fingerprint.is_some();
        let mode = environment.ingress_mode;

        let snapshot = if mode.compiles()
            && matches!(
                access.policy_decision,
                temps_core::RequestPolicyDecision::Continue
            )
            && self.project_ip_gate.reads_project_ip_access_rules()
        {
            self.route_table
                .as_ref()
                .map(|table| table.ingress_snapshot())
        } else {
            None
        };
        // Shadow and enforce read the same endpoint: what enforcing would
        // decide with, or why it would refuse.
        let enforcing = snapshot
            .as_deref()
            .map(|snapshot| enforcing_endpoint(snapshot, environment_id, access.path));
        let mut host_buffer = [0u8; 256];
        let host = grant_host(access.host, &mut host_buffer);

        if mode == IngressMode::Enforce {
            if let (Some(snapshot), Some(enforcing)) = (snapshot.as_deref(), enforcing) {
                let endpoint = match enforcing {
                    Ok(endpoint) => endpoint,
                    Err(missing) => {
                        if let Some(suppressed) = INGRESS_UNAVAILABLE_LOG_LIMITER.admit(now_secs) {
                            warn!(
                                environment_id,
                                project_id = project_ctx.project.id,
                                reason = missing.label(),
                                suppressed_since_last_log = suppressed,
                                "Environment enforces an ingress endpoint the route snapshot cannot provide; refusing the request"
                            );
                        }
                        return AccessOutcome::Unavailable;
                    }
                };
                let request = EndpointRequest {
                    request: access.request,
                    environment_id,
                    client_ip: access.client_ip,
                    captcha_path,
                    has_tls_fingerprint,
                    password: access.password,
                    grants: GrantFacts {
                        master: self.crypto.ingress_grant_master_key(),
                        revocations: snapshot.revocations(),
                        host,
                        now_secs,
                    },
                };
                // A captcha step is Shield's to answer (ADR 066), after this
                // decision: resume past it.
                return match enforce_step(endpoint, &request, &self.rate_limiter, false, 0) {
                    Step::Done(outcome) => outcome,
                    Step::NeedsCaptcha { resume_at } => {
                        match enforce_step(endpoint, &request, &self.rate_limiter, true, resume_at)
                        {
                            Step::Done(outcome) => outcome,
                            Step::NeedsCaptcha { .. } => AccessOutcome::CaptchaChallenge,
                        }
                    }
                };
            }
        }

        let legacy = LegacyRequest {
            client_ip: access.client_ip,
            ip_restricted: access.ip_restricted,
            rate_limit: access.rate_limit,
            // Attack mode is a Shield mode (ADR 066), decided after this.
            attack_mode: false,
            captcha_path,
            has_tls_fingerprint,
            password: access.password,
        };
        let mut facts = LegacyFacts::default();
        let outcome = match legacy_access(&legacy, &self.rate_limiter, now_secs, &mut facts) {
            Step::Done(outcome) => outcome,
            Step::NeedsCaptcha { .. } => legacy_after_captcha(&legacy, true, &mut facts),
        };

        if mode == IngressMode::Shadow {
            let shadow = match (snapshot.as_deref(), enforcing) {
                (Some(snapshot), Some(Ok(endpoint))) => {
                    let request = EndpointRequest {
                        request: access.request,
                        environment_id,
                        client_ip: access.client_ip,
                        captcha_path,
                        has_tls_fingerprint,
                        password: access.password,
                        grants: GrantFacts {
                            master: self.crypto.ingress_grant_master_key(),
                            revocations: snapshot.revocations(),
                            host,
                            now_secs,
                        },
                    };
                    Some((endpoint.id, shadow_step(endpoint, &request, &facts)))
                }
                // Enforcing would refuse this request.
                (Some(_), Some(Err(_))) => Some((0, AccessOutcome::Unavailable)),
                // The snapshot is not consulted (a plugin request policy or
                // IP gate decides): nothing to compare.
                _ => None,
            };
            if let Some((endpoint_id, shadow)) = shadow {
                let mismatch = shadow.response_class() != outcome.response_class();
                self.proxy_metrics.record_ingress_shadow(mismatch);
                if mismatch {
                    if let Some(suppressed) = INGRESS_SHADOW_LOG_LIMITER.admit(now_secs) {
                        warn!(
                            environment_id,
                            project_id = project_ctx.project.id,
                            endpoint_id,
                            legacy = outcome.label(),
                            endpoint = shadow.label(),
                            suppressed_since_last_log = suppressed,
                            "Ingress shadow evaluation disagrees with the legacy access chain"
                        );
                    }
                }
            }
        }
        outcome
    }

    /// Answers a `TempsLogin` exchange at
    /// [`GRANT_EXCHANGE_PATH`](crate::ingress::GRANT_EXCHANGE_PATH): only
    /// reached when the environment enforces a compiled endpoint that asked
    /// for a login. Once per sign-in, so it may allocate.
    pub(super) fn exchange_ingress_grant(
        &self,
        environment_id: i32,
        host: &str,
        query: Option<&str>,
        secure: bool,
        cookie_path: Option<&str>,
    ) -> Result<crate::ingress::SessionExchange, crate::ingress::ExchangeError> {
        let owner = temps_workload_ingress::ProductRef {
            kind: temps_workload_ingress::ProductKind::Environment,
            id: i64::from(environment_id),
        };
        let snapshot = self
            .route_table
            .as_ref()
            .map(|table| table.ingress_snapshot());
        let endpoint = snapshot.as_deref().and_then(|snapshot| {
            snapshot.environment_endpoint(environment_id, crate::ingress::GRANT_EXCHANGE_PATH)
        });
        let (Some(snapshot), Some(endpoint)) = (snapshot.as_deref(), endpoint) else {
            return Err(crate::ingress::ExchangeError::NotLoginProtected { owner });
        };
        let mut host_buffer = [0u8; 256];
        let grants = crate::ingress::GrantFacts {
            master: self.crypto.ingress_grant_master_key(),
            revocations: snapshot.revocations(),
            host: crate::ingress::grant_host(host, &mut host_buffer),
            now_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        crate::ingress::exchange_grant(
            endpoint,
            &grants,
            &self.consumed_exchanges,
            query,
            secure,
            cookie_path,
        )
    }
}

#[cfg(test)]
mod ip_restriction_fail_closed_tests {
    use super::{ip_restriction_denies, legacy_ip_gate_denies, normalize_client_ip};
    use std::net::IpAddr;
    use temps_core::{ProjectIpGate, RequestPolicyDecision};

    /// Stands in for a cached project IP gate when a project/environment is
    /// on a closed/restricted mode: `is_allowed`
    /// denies (baring an explicit allowlist match, irrelevant here since we
    /// never reach it — the IP is unresolvable) and `has_active_policy`
    /// truthfully reports the restriction exists.
    struct RestrictedGate;
    impl ProjectIpGate for RestrictedGate {
        fn is_allowed(&self, _project_id: i32, _environment_id: i32, _ip: IpAddr) -> bool {
            false
        }
        fn has_active_policy(&self, _project_id: i32, _environment_id: i32) -> bool {
            true
        }
        fn is_explicitly_denied(
            &self,
            _project_id: i32,
            _environment_id: i32,
            _ip: Option<IpAddr>,
        ) -> bool {
            false
        }
    }

    /// Stands in for the common case: no restriction configured for this
    /// project/environment at all (also what `temps_core::OpenIpGate`,
    /// the OSS default, always reports).
    struct UnrestrictedGate;
    impl ProjectIpGate for UnrestrictedGate {
        fn is_allowed(&self, _project_id: i32, _environment_id: i32, _ip: IpAddr) -> bool {
            true
        }
        // has_active_policy uses the trait default (`false`).
    }

    struct ExplicitDenyGate;
    impl ProjectIpGate for ExplicitDenyGate {
        fn is_allowed(&self, _project_id: i32, _environment_id: i32, _ip: IpAddr) -> bool {
            true
        }
        fn is_explicitly_denied(
            &self,
            _project_id: i32,
            _environment_id: i32,
            ip: Option<IpAddr>,
        ) -> bool {
            ip.is_none() || ip == Some(IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7)))
        }
    }

    fn allow_list_gate() -> crate::ip_access::ProjectIpAccessGate {
        use temps_entities::project_ip_access_rules::{IpAccessCidrEntry, IpAccessCidrs, Model};
        let now = chrono::Utc::now();
        crate::ip_access::ProjectIpAccessGate::from_rows_for_tests(vec![Model {
            id: 1,
            project_id: 1,
            environment_id: None,
            mode: "allow".to_string(),
            cidrs: IpAccessCidrs(vec![IpAccessCidrEntry {
                cidr: "100.64.0.0/10".to_string(),
                description: None,
            }]),
            updated_by: None,
            revision: 1,
            created_at: now,
            updated_at: now,
        }])
    }

    /// Mirrors `resolve_session_client_ip` for a peer that is neither
    /// loopback nor a trusted CDN: the forwarding headers must be ignored and
    /// the socket peer used.
    fn resolved_client_ip(
        peer: IpAddr,
        headers: &axum::http::HeaderMap,
        trust_loopback: bool,
    ) -> IpAddr {
        normalize_client_ip(
            crate::client_ip::resolve_loopback_client_ip(peer, headers, trust_loopback)
                .unwrap_or(peer),
        )
    }

    #[test]
    fn spoofed_forwarding_headers_from_an_untrusted_peer_cannot_bypass_an_allow_list() {
        let gate = allow_list_gate();
        let mut headers = axum::http::HeaderMap::new();
        // The attacker claims to be an address on the allow-list.
        headers.insert("x-forwarded-for", "100.64.0.5".parse().unwrap());
        headers.insert("x-real-ip", "100.64.0.5".parse().unwrap());
        headers.insert("cf-connecting-ip", "100.64.0.5".parse().unwrap());
        let peer: IpAddr = "198.51.100.9".parse().unwrap();

        for trust_loopback in [false, true] {
            let ip = resolved_client_ip(peer, &headers, trust_loopback);
            assert_eq!(ip, peer, "headers from a public peer must be ignored");
            assert!(ip_restriction_denies(&gate, 1, 10, Some(ip)));
        }
    }

    #[test]
    fn loopback_forwarded_ip_is_only_honoured_when_opted_in() {
        let gate = allow_list_gate();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "100.64.0.5".parse().unwrap());
        let peer: IpAddr = "127.0.0.1".parse().unwrap();

        // Default: the loopback peer is what is evaluated, and is not allowed.
        let ip = resolved_client_ip(peer, &headers, false);
        assert!(ip_restriction_denies(&gate, 1, 10, Some(ip)));
        // Opt-in: the local proxy's appended entry is the client.
        let ip = resolved_client_ip(peer, &headers, true);
        assert!(!ip_restriction_denies(&gate, 1, 10, Some(ip)));
    }

    #[test]
    fn v4_mapped_peer_is_matched_as_its_v4_address() {
        let gate = allow_list_gate();
        let mapped: IpAddr = "::ffff:100.64.0.5".parse().unwrap();
        assert!(!ip_restriction_denies(
            &gate,
            1,
            10,
            Some(normalize_client_ip(mapped))
        ));
        let mapped_denied: IpAddr = "::ffff:198.51.100.9".parse().unwrap();
        assert!(ip_restriction_denies(
            &gate,
            1,
            10,
            Some(normalize_client_ip(mapped_denied))
        ));
    }

    #[test]
    fn unresolvable_ip_under_an_active_policy_is_denied() {
        // Regression: an unparseable/absent client IP must fail closed when
        // the project/environment actually has an IP restriction policy —
        // silently skipping enforcement here is exactly the gap an attacker
        // (or a proxy misconfiguration) would exploit.
        assert!(ip_restriction_denies(&RestrictedGate, 1, 1, None));
    }

    #[test]
    fn unresolvable_ip_under_no_policy_is_not_denied() {
        // Matches today's behavior for the common case: most projects have
        // no IP restriction configured, so a resolution failure (e.g. a
        // non-INET socket in local/test environments) must not start
        // denying that traffic.
        assert!(!ip_restriction_denies(&UnrestrictedGate, 1, 1, None));
    }

    #[test]
    fn resolved_ip_still_goes_through_is_allowed_as_before() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        assert!(ip_restriction_denies(&RestrictedGate, 1, 1, Some(ip)));
        assert!(!ip_restriction_denies(&UnrestrictedGate, 1, 1, Some(ip)));
    }

    #[test]
    fn policy_continue_keeps_legacy_restrictions_but_allow_owns_access() {
        assert!(legacy_ip_gate_denies(
            RequestPolicyDecision::Continue,
            &RestrictedGate,
            1,
            2,
            None
        ));
        assert!(!legacy_ip_gate_denies(
            RequestPolicyDecision::Allow {
                rule_id: Some(3),
                revision: Some(4)
            },
            &RestrictedGate,
            1,
            2,
            None
        ));
        assert!(!legacy_ip_gate_denies(
            RequestPolicyDecision::Deny {
                reason: "blocked",
                rule_id: Some(3),
                revision: Some(4)
            },
            &RestrictedGate,
            1,
            2,
            None
        ));
        assert!(legacy_ip_gate_denies(
            RequestPolicyDecision::Allow {
                rule_id: None,
                revision: None
            },
            &ExplicitDenyGate,
            1,
            2,
            None
        ));
    }

    #[test]
    fn mapped_ipv6_uses_ipv4_identity_for_both_gates() {
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        let ipv4: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(normalize_client_ip(mapped), ipv4);
        assert_eq!(normalize_client_ip(ipv4), ipv4);
    }
}

#[cfg(test)]
mod request_policy_path_handoff_tests {
    use super::evaluate_request_policy;
    use pingora_proxy::Session;
    use std::sync::{Arc, Mutex};
    use temps_core::{RequestPolicyContext, RequestPolicyDecision, RequestPolicyGate};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct RecordingAllowGate {
        paths: Arc<Mutex<Vec<String>>>,
    }

    impl RequestPolicyGate for RecordingAllowGate {
        fn evaluate(&self, context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
            assert_eq!(context.method, "POST");
            assert_eq!(context.host, "app.example.test");
            assert_eq!(context.project_id, 41);
            assert_eq!(context.environment_id, 73);
            assert_eq!(context.client_ip, None);
            self.paths
                .lock()
                .expect("recording gate mutex poisoned")
                .push(context.path.to_string());
            RequestPolicyDecision::Allow {
                rule_id: None,
                revision: None,
            }
        }
    }

    #[tokio::test]
    async fn pingora_preserves_policy_path_spelling_in_upstream_request_target() {
        let cases = [
            ("/hook/admin?token=x", "/hook/admin"),
            ("/hook%2fadmin?token=x", "/hook%2fadmin"),
            ("/hook%2Fadmin?token=x", "/hook%2Fadmin"),
            ("/hook/../admin?token=x", "/hook/../admin"),
            ("/hook//admin?token=x", "/hook//admin"),
            ("/hook\\admin?token=x", "/hook\\admin"),
        ];

        for (request_target, expected_policy_path) in cases {
            let request = format!(
                "POST {request_target} HTTP/1.1\r\nHost: app.example.test\r\nContent-Length: 0\r\n\r\n"
            );
            let (mut downstream_writer, downstream_reader) = tokio::io::duplex(2048);
            downstream_writer
                .write_all(request.as_bytes())
                .await
                .expect("write raw downstream request");

            let mut session =
                Session::new_h1(Box::new(downstream_reader) as pingora_core::protocols::Stream);
            session
                .read_request()
                .await
                .unwrap_or_else(|error| panic!("Pingora rejected {request_target}: {error}"));

            let paths = Arc::new(Mutex::new(Vec::new()));
            let gate = RecordingAllowGate {
                paths: Arc::clone(&paths),
            };
            assert!(matches!(
                evaluate_request_policy(
                    &gate,
                    session.req_header(),
                    "app.example.test",
                    41,
                    73,
                    None,
                ),
                RequestPolicyDecision::Allow { .. }
            ));
            assert_eq!(
                paths
                    .lock()
                    .expect("recording gate mutex poisoned")
                    .as_slice(),
                [expected_policy_path],
                "policy path changed for {request_target}"
            );

            let (upstream_writer, mut upstream_reader) = tokio::io::duplex(2048);
            let mut upstream = pingora_core::protocols::http::v1::client::HttpSession::new(
                Box::new(upstream_writer) as pingora_core::protocols::Stream,
            );
            upstream
                .write_request_header(Box::new(session.req_header().clone()))
                .await
                .unwrap_or_else(|error| panic!("serialize {request_target} upstream: {error}"));

            let mut serialized = vec![0; request.len() + 256];
            let bytes_read = upstream_reader
                .read(&mut serialized)
                .await
                .expect("read serialized upstream request");
            let serialized = String::from_utf8_lossy(&serialized[..bytes_read]);
            assert!(
                serialized.starts_with(&format!("POST {request_target} HTTP/1.1\r\n")),
                "upstream request target changed for {request_target}: {serialized:?}"
            );
        }
    }
}
