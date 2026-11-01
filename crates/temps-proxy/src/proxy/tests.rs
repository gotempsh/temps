// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[cfg(test)]
mod domain_redirect_location_tests {
    use super::domain_redirect_location;

    #[test]
    fn origin_target_keeps_request_path() {
        assert_eq!(
            domain_redirect_location("https://example.com", "/pricing", None),
            "https://example.com/pricing"
        );
    }

    #[test]
    fn origin_target_keeps_request_query() {
        assert_eq!(
            domain_redirect_location("https://example.com", "/pricing", Some("x=1&y=two")),
            "https://example.com/pricing?x=1&y=two"
        );
    }

    #[test]
    fn empty_query_is_dropped() {
        assert_eq!(
            domain_redirect_location("https://example.com", "/pricing", Some("")),
            "https://example.com/pricing"
        );
    }

    #[test]
    fn root_request_lands_on_target_root() {
        assert_eq!(
            domain_redirect_location("https://example.com", "/", None),
            "https://example.com/"
        );
        assert_eq!(
            domain_redirect_location("https://example.com", "", None),
            "https://example.com/"
        );
    }

    #[test]
    fn trailing_slashes_are_preserved_without_doubling() {
        assert_eq!(
            domain_redirect_location("https://example.com/", "/docs/", None),
            "https://example.com/docs/"
        );
        assert_eq!(
            domain_redirect_location("https://example.com/", "/", Some("a=1")),
            "https://example.com/?a=1"
        );
    }

    #[test]
    fn target_port_and_scheme_are_kept() {
        assert_eq!(
            domain_redirect_location("http://example.com:8080", "/a/b", Some("c=d")),
            "http://example.com:8080/a/b?c=d"
        );
    }

    #[test]
    fn target_with_its_own_path_is_used_verbatim() {
        assert_eq!(
            domain_redirect_location("https://example.com/welcome", "/pricing", Some("x=1")),
            "https://example.com/welcome"
        );
        assert_eq!(
            domain_redirect_location("https://example.com/blog/", "/post", None),
            "https://example.com/blog/"
        );
    }

    #[test]
    fn target_with_its_own_query_or_fragment_is_used_verbatim() {
        assert_eq!(
            domain_redirect_location("https://example.com?ref=old", "/pricing", None),
            "https://example.com?ref=old"
        );
        assert_eq!(
            domain_redirect_location("https://example.com/#top", "/pricing", None),
            "https://example.com/#top"
        );
    }

    #[test]
    fn encoded_request_path_is_not_reencoded() {
        assert_eq!(
            domain_redirect_location("https://example.com", "/caf%C3%A9/a%20b", Some("q=%2F")),
            "https://example.com/caf%C3%A9/a%20b?q=%2F"
        );
    }

    #[test]
    fn protocol_relative_looking_path_stays_on_target_host() {
        // `//evil.example` as a request path must not become a host: the
        // target origin always precedes it.
        assert_eq!(
            domain_redirect_location("https://example.com", "//evil.example/x", None),
            "https://example.com//evil.example/x"
        );
    }

    #[test]
    fn non_absolute_legacy_target_is_returned_unchanged() {
        assert_eq!(
            domain_redirect_location("example.com", "/pricing", None),
            "example.com"
        );
    }
}

#[cfg(test)]
mod deployment_asset_scope_tests {
    use super::{
        deployment_asset_scope, inherited_https_policy, legacy_deployment_asset_scope,
        should_apply_production_https_default, should_redirect_to_https,
    };
    use crate::service::static_asset_lookup::LegacyAssetOrigin;

    #[test]
    fn prefixed_asset_resolves_current_or_reused_source_artifact() {
        assert_eq!(
            deployment_asset_scope("deploy-a", 10, 20, None, None, None, "deploy-a"),
            Some((10, 20))
        );
        assert_eq!(
            deployment_asset_scope(
                "promoted-b",
                11,
                21,
                Some("deploy-a"),
                Some(10),
                Some(20),
                "deploy-a",
            ),
            Some((10, 20))
        );
        assert_eq!(
            deployment_asset_scope(
                "promoted-b",
                11,
                21,
                Some("deploy-a"),
                Some(10),
                Some(20),
                "unknown",
            ),
            None
        );
    }

    #[test]
    fn legacy_prefixed_asset_maps_both_old_current_and_original_slugs_to_origin() {
        let origin = LegacyAssetOrigin {
            deployment_id: 10,
            environment_id: 20,
            slug: "original-build".to_string(),
        };

        assert_eq!(
            legacy_deployment_asset_scope("legacy-promotion", &origin, "legacy-promotion"),
            Some((20, 10))
        );
        assert_eq!(
            legacy_deployment_asset_scope("legacy-promotion", &origin, "original-build"),
            Some((20, 10))
        );
        assert_eq!(
            legacy_deployment_asset_scope("legacy-promotion", &origin, "unrelated"),
            None
        );
    }

    #[test]
    fn production_https_cannot_be_bypassed_with_an_unknown_host() {
        assert!(inherited_https_policy(true, false));
        assert!(!inherited_https_policy(false, false));
        assert!(inherited_https_policy(false, true));
    }

    #[test]
    fn production_https_default_never_applies_without_a_resolved_environment() {
        // Unresolved Host (admin/console UI, internal cluster API like node
        // registration) — must not be redirected even though every other gate
        // would otherwise allow it.
        assert!(!should_apply_production_https_default(
            false,
            false,
            "/api/internal/nodes/register",
            None,
            false
        ));
        // Resolved project traffic with everything else the same — applies.
        assert!(should_apply_production_https_default(
            false, false, "/", None, true
        ));
    }

    /// Cluster bootstrap speaks plaintext HTTP before any TLS material exists,
    /// so internal node calls are never redirected — not by the production
    /// default, and not by an explicit `force_https` either. A worker registers
    /// against the console host, which is exactly the host an operator is
    /// likeliest to force to HTTPS.
    #[test]
    fn internal_cluster_calls_are_never_redirected() {
        assert!(!should_apply_production_https_default(
            false,
            false,
            "/api/internal/nodes/1/heartbeat",
            None,
            true
        ));
        // Even with the override switched on and a certificate present.
        assert!(!should_redirect_to_https(
            false,
            false,
            "/api/internal/nodes/register",
            Some(true),
            || true
        ));
    }

    /// The console gets no implicit HTTPS default. Temps cannot distinguish
    /// "TLS terminated by an upstream CDN" from "plain HTTP" — both arrive as
    /// a plaintext connection with no local certificate — so redirecting on the
    /// strength of an `https://` external_url would loop the browser between
    /// the CDN and Temps forever. Only an explicit operator override redirects.
    #[test]
    fn console_https_is_opt_in_not_inferred() {
        // No override: falls through to the per-host certificate heuristic.
        // No cert (CDN-fronted, or plain HTTP install) → no redirect.
        assert!(!should_redirect_to_https(
            false,
            false,
            "/login",
            None,
            || false
        ));
        // Cert provisioned through Temps → redirect, as it always has.
        assert!(should_redirect_to_https(
            false,
            false,
            "/login",
            None,
            || true
        ));
        // Explicit opt-in redirects even with no local certificate.
        assert!(should_redirect_to_https(
            false,
            false,
            "/login",
            Some(true),
            || false
        ));
        // Explicit opt-out wins over a provisioned certificate.
        assert!(!should_redirect_to_https(
            false,
            false,
            "/login",
            Some(false),
            || true
        ));
        // The global kill switch still outranks the override.
        assert!(!should_redirect_to_https(
            true,
            false,
            "/login",
            Some(true),
            || true
        ));
    }

    /// Regression: the CDN redirect loop.
    ///
    /// Browser →(https)→ CDN →(http)→ Temps. `is_tls` is false because Temps
    /// only trusts its own TLS digest, and no certificate exists locally
    /// because the CDN holds it. If any rule inferred "redirect" from the
    /// `https://` external_url alone, Temps would 301 back to the CDN, which
    /// would forward plain HTTP again — forever, with the global kill switch
    /// as the only escape.
    #[test]
    fn a_cdn_fronted_console_is_never_redirected_by_inference() {
        let cdn_fronted_console = || {
            should_redirect_to_https(
                /* globally_disabled */ false,
                /* is_tls */ false,
                "/login",
                // No environment resolved, and the operator set no override.
                None,
                // TLS terminated upstream, so Temps holds no certificate.
                || false,
            )
        };
        assert!(!cdn_fronted_console());

        // And the production-HTTPS default cannot reach this request either:
        // it is gated on a resolved environment, which the console never has.
        assert!(!should_apply_production_https_default(
            false, false, "/login", None, false
        ));
    }

    #[test]
    fn production_https_default_respects_the_other_gates() {
        assert!(!should_apply_production_https_default(
            true, false, "/", None, true
        ));
        assert!(!should_apply_production_https_default(
            false, true, "/", None, true
        ));
        assert!(!should_apply_production_https_default(
            false,
            false,
            "/.well-known/acme-challenge/token",
            None,
            true
        ));
        assert!(!should_apply_production_https_default(
            false,
            false,
            "/",
            Some(false),
            true
        ));
    }
}

#[cfg(test)]
mod https_redirect_tests {
    use super::*;
    use std::cell::Cell;

    /// Convenience wrapper for the common "cert lookup returns X" case.
    fn decide(
        globally_disabled: bool,
        is_tls: bool,
        path: &str,
        env_force_https: Option<bool>,
        host_has_cert: bool,
    ) -> bool {
        should_redirect_to_https(globally_disabled, is_tls, path, env_force_https, || {
            host_has_cert
        })
    }

    #[test]
    fn proxy_https_redirect_response_has_monitor_marker() {
        let response = https_redirect_response("https://app.example.test/health", "request-1")
            .expect("build HTTPS redirect response");
        assert_eq!(response.status.as_u16(), 301);
        assert_eq!(
            response
                .headers
                .get("x-temps-proxy-https-redirect")
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        assert_eq!(
            response
                .headers
                .get("x-temps-proxy-probe-capable")
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        assert_eq!(
            response
                .headers
                .get("location")
                .and_then(|value| value.to_str().ok()),
            Some("https://app.example.test/health")
        );
    }

    #[test]
    fn application_cannot_spoof_proxy_https_redirect_marker() {
        let mut response = ResponseHeader::build(302, None).expect("build application response");
        response
            .insert_header("X-Temps-Proxy-Https-Redirect", "1")
            .expect("insert spoofed marker");
        response
            .insert_header("X-Temps-Proxy-Probe-Capable", "1")
            .expect("insert spoofed capability");

        strip_proxy_owned_response_headers(&mut response);

        assert!(response
            .headers
            .get("x-temps-proxy-https-redirect")
            .is_none());
        assert!(response
            .headers
            .get("x-temps-proxy-probe-capable")
            .is_none());
    }

    /// ADR 065 D10: a monitor takes `x-temps-gate` for the gate's own
    /// answer, so an app on any host, gated or not, cannot send it.
    #[test]
    fn application_cannot_pass_for_the_gate() {
        let mut response = ResponseHeader::build(500, None).expect("build application response");
        response
            .insert_header(
                temps_core::gate::GATE_HEADER,
                temps_core::gate::GATE_SIGN_IN,
            )
            .expect("insert spoofed gate answer");

        strip_proxy_owned_response_headers(&mut response);

        assert!(response
            .headers
            .get(temps_core::gate::GATE_HEADER)
            .is_none());
    }

    #[test]
    fn default_behaviour_follows_certificate_presence() {
        // No per-environment override → the pre-existing heuristic is unchanged:
        // hosts with a provisioned certificate are redirected, HTTP-only installs
        // (sslip.io quick/local modes) are not.
        assert!(decide(false, false, "/", None, true));
        assert!(!decide(false, false, "/", None, false));
    }

    #[test]
    fn force_https_true_redirects_without_a_local_certificate() {
        // The motivating case: TLS terminated by an upstream CDN, so the control
        // plane holds no certificate for the host and the default heuristic would
        // happily keep serving a full 200 over plain HTTP alongside HTTPS.
        assert!(decide(false, false, "/", Some(true), false));
    }

    #[test]
    fn force_https_false_suppresses_redirect_even_with_a_certificate() {
        // Escape hatch for environments that must stay reachable over plain HTTP
        // (appliances, hardware clients with no modern TLS stack).
        assert!(!decide(false, false, "/", Some(false), true));
    }

    #[test]
    fn https_requests_are_never_redirected() {
        for force in [None, Some(true), Some(false)] {
            assert!(
                !decide(false, true, "/", force, true),
                "already-TLS request must never redirect (force_https={force:?})"
            );
        }
    }

    #[test]
    fn global_kill_switch_outranks_the_environment_override() {
        // `disable_https_redirect` is set by the service unit in local/testing
        // mode. A per-environment force_https must not resurrect the redirect
        // there, or a developer's local rig bounces to a port serving no cert.
        assert!(!decide(true, false, "/", Some(true), true));
        assert!(!decide(true, false, "/", None, true));
    }

    #[test]
    fn acme_challenge_is_exempt_under_every_override() {
        let path = "/.well-known/acme-challenge/some-token";
        for force in [None, Some(true), Some(false)] {
            for has_cert in [true, false] {
                assert!(
                    !decide(false, false, path, force, has_cert),
                    "ACME challenge must never redirect \
                     (force_https={force:?}, has_cert={has_cert})"
                );
            }
        }
    }

    /// Regression guard for silent renewal failure. A host that already has a
    /// certificate is exactly the host that will renew, and renewal happens
    /// while the old certificate is still installed. If the challenge request
    /// were redirected, the CA would follow it to an HTTPS endpoint presenting
    /// the certificate that is about to expire — issuance fails, nothing logs an
    /// error at the proxy, and the site breaks weeks later when the old cert
    /// finally lapses.
    #[test]
    fn acme_challenge_is_exempt_during_renewal_of_an_existing_certificate() {
        assert!(!decide(
            false,
            false,
            "/.well-known/acme-challenge/renewal-token",
            None,
            true,
        ));
    }

    /// The exemption is anchored to the full challenge prefix, not a loose
    /// `.well-known` match — other well-known resources should still be pushed
    /// to HTTPS like any normal path.
    #[test]
    fn other_well_known_paths_still_redirect() {
        assert!(decide(
            false,
            false,
            "/.well-known/security.txt",
            None,
            true
        ));
        assert!(decide(
            false,
            false,
            "/.well-known/acme-challenge-not-really",
            None,
            true
        ));
    }

    #[test]
    fn certificate_lookup_is_skipped_when_it_cannot_change_the_answer() {
        // The cert-cache read is a lock-free snapshot, but it runs on every
        // plain-HTTP request, so the cases that cannot possibly need it must not
        // pay for it.
        let calls = Cell::new(0);
        let counting_lookup = || {
            calls.set(calls.get() + 1);
            true
        };

        // Already HTTPS — the overwhelmingly common case.
        assert!(!should_redirect_to_https(
            false,
            true,
            "/",
            None,
            counting_lookup
        ));
        // Global kill switch.
        assert!(!should_redirect_to_https(
            true,
            false,
            "/",
            None,
            counting_lookup
        ));
        // ACME challenge.
        assert!(!should_redirect_to_https(
            false,
            false,
            "/.well-known/acme-challenge/t",
            None,
            counting_lookup
        ));
        // Explicit environment override — answer is known without the lookup.
        assert!(should_redirect_to_https(
            false,
            false,
            "/",
            Some(true),
            counting_lookup
        ));
        assert_eq!(calls.get(), 0, "cert cache must not have been consulted");

        // Only the inherit-the-default path consults it.
        assert!(should_redirect_to_https(
            false,
            false,
            "/",
            None,
            counting_lookup
        ));
        assert_eq!(calls.get(), 1);
    }
}

#[cfg(test)]
mod admin_gate_tests {
    use super::*;
    use temps_core::admin_gate::{AdminGateConfig, AdminGateSource};

    fn gated(hosts: &[&str]) -> AdminGateConfig {
        let owned: Vec<String> = hosts.iter().map(|s| s.to_string()).collect();
        AdminGateConfig::from_parts(&[], &owned, false, AdminGateSource::Db)
            .expect("valid gate config")
    }

    #[test]
    fn noop_gate_short_circuits_consultation() {
        let config = AdminGateConfig::from_parts(&[], &[], false, AdminGateSource::Default)
            .expect("empty noop config");
        assert!(config.is_noop());
        assert!(!LoadBalancer::should_consult_admin_gate(
            &config, "/", false,
        ));
    }

    #[test]
    fn preview_routes_bypass_gate() {
        let config = gated(&["app.temps.kfs.es"]);
        assert!(!LoadBalancer::should_consult_admin_gate(
            &config,
            "/some/path",
            true,
        ));
    }

    #[test]
    fn temps_ingest_paths_bypass_gate() {
        let config = gated(&["app.temps.kfs.es"]);
        // Public ingest like /api/_temps/event must reach the console from any host.
        assert!(!LoadBalancer::should_consult_admin_gate(
            &config,
            "/api/_temps/event",
            false,
        ));
    }

    #[test]
    fn normal_request_consults_gate_when_configured() {
        let config = gated(&["app.temps.kfs.es"]);
        assert!(LoadBalancer::should_consult_admin_gate(&config, "/", false,));
    }

    // ADR-051 D4: the three-way console fall-through.

    #[test]
    fn allowlisted_client_reaches_the_full_console() {
        let config = AdminGateConfig::from_parts(
            &["203.0.113.0/24".to_string()],
            &[],
            false,
            AdminGateSource::Db,
        )
        .expect("valid gate");
        let admin: std::net::IpAddr = "203.0.113.9".parse().unwrap();
        for has_listener in [true, false] {
            assert_eq!(
                LoadBalancer::console_fall_through(
                    &config,
                    admin,
                    "console.example.com",
                    has_listener
                ),
                ConsoleFallThrough::Console
            );
        }
    }

    #[test]
    fn denied_client_gets_only_the_public_surface() {
        let config = AdminGateConfig::from_parts(
            &["203.0.113.0/24".to_string()],
            &[],
            false,
            AdminGateSource::Db,
        )
        .expect("valid gate");
        let stranger: std::net::IpAddr = "198.51.100.20".parse().unwrap();
        assert_eq!(
            LoadBalancer::console_fall_through(&config, stranger, "console.example.com", true),
            ConsoleFallThrough::PublicSurfaceOnly,
            "a mail client or webhook sender must reach the public routes"
        );
    }

    #[test]
    fn denied_client_keeps_the_404_without_a_public_only_listener() {
        let config = gated(&["console.example.com"]);
        let stranger: std::net::IpAddr = "198.51.100.20".parse().unwrap();
        assert_eq!(
            LoadBalancer::console_fall_through(&config, stranger, "other.example.com", false),
            ConsoleFallThrough::NotFound,
            "a split proxy with no ingress address must behave as before"
        );
    }

    #[test]
    fn host_allowlist_denial_also_falls_through_to_the_public_surface() {
        let config = gated(&["console.example.com"]);
        let client: std::net::IpAddr = "198.51.100.20".parse().unwrap();
        assert_eq!(
            LoadBalancer::console_fall_through(&config, client, "console.example.com", true),
            ConsoleFallThrough::Console
        );
        assert_eq!(
            LoadBalancer::console_fall_through(&config, client, "203.0.113.5", true),
            ConsoleFallThrough::PublicSurfaceOnly
        );
    }

    #[test]
    fn the_public_ingress_host_is_never_treated_as_a_workspace_preview() {
        let preview_shaped = "ws-0123456789abcdef-3000.apps.example.com";

        // Ordinarily that host is a sandbox preview…
        let preview = preview_host_for_request(preview_shaped, "apps.example.com", || false)
            .expect("a ws- host under the preview domain is a preview");
        assert_eq!(preview.port, 3000);
        // …but not when it is the reserved callback hostname.
        assert!(preview_host_for_request(preview_shaped, "apps.example.com", || true).is_none());
        // An ordinary host is not a preview, and the ingress check is not
        // even consulted for it.
        assert!(
            preview_host_for_request("hooks.example.com", "apps.example.com", || {
                unreachable!("only preview-shaped hosts are checked")
            })
            .is_none()
        );
    }

    #[test]
    fn public_only_listener_gets_the_console_timeout() {
        let console = "127.0.0.1:8081";
        let ingress = "127.0.0.1:8082";
        assert_eq!(
            control_plane_address(ingress, console, Some(ingress)),
            ingress
        );
        assert_eq!(
            control_plane_address(console, console, Some(ingress)),
            console
        );
        assert_eq!(
            control_plane_address("10.0.0.9:3000", console, None),
            console
        );
        // A request routed to the public-only listener is control-plane
        // traffic: it must not inherit a customer timeout (or lack of one).
        assert_eq!(
            upstream_io_timeout(
                ingress,
                control_plane_address(ingress, console, Some(ingress)),
                false,
                None
            ),
            Some(std::time::Duration::from_secs(CONSOLE_IO_TIMEOUT_SECS))
        );
    }

    // Regression: setting an admin host (e.g. `app.temps.kfs.es`) used to
    // 404 every project deployment because the gate consulted
    // `has_custom_route`, which only knows about operator-defined LB
    // overrides — not the in-memory project route table. The fix is the
    // new `has_route_for_host` trait method; this test pins the contract:
    // a resolver that reports the host via `has_route_for_host` is treated
    // as known, even when `has_custom_route` says no.
    #[tokio::test]
    async fn has_route_for_host_recognizes_project_hosts_outside_custom_routes() {
        use crate::traits::{PeerSelection, UpstreamResolver};
        use async_trait::async_trait;
        use pingora_core::upstreams::peer::HttpPeer;
        use std::collections::HashSet;

        struct ProjectRouteOnlyResolver {
            project_hosts: HashSet<String>,
        }

        #[async_trait]
        impl UpstreamResolver for ProjectRouteOnlyResolver {
            async fn resolve_peer(
                &self,
                _host: &str,
                _path: &str,
                _sni: Option<&str>,
            ) -> pingora_core::Result<PeerSelection> {
                Ok(PeerSelection {
                    peer: Box::new(HttpPeer::new("127.0.0.1:1".to_string(), false, "".into())),
                    container_id: None,
                    container_name: None,
                })
            }

            async fn has_custom_route(&self, _host: &str) -> bool {
                // Simulates the old behavior: no entry in `custom_routes`.
                false
            }

            async fn has_route_for_host(&self, host: &str) -> bool {
                // Simulates the route_table check: project hosts are known here.
                self.project_hosts.contains(host)
            }

            async fn get_lb_strategy(&self, _host: &str) -> Option<String> {
                None
            }
        }

        let resolver = ProjectRouteOnlyResolver {
            project_hosts: ["myproject.example.com".to_string()].into_iter().collect(),
        };

        // Old check missed the project — would have triggered the gate deny path.
        assert!(!resolver.has_custom_route("myproject.example.com").await);
        // New check finds it — gate path correctly skips deny.
        assert!(resolver.has_route_for_host("myproject.example.com").await);
        // Truly unknown hosts still fall through and would hit the gate.
        assert!(!resolver.has_route_for_host("evil.example.com").await);
    }
}

#[cfg(test)]
mod shield_log_tests {
    /// ADR 066 phase 2: a listed client's verdict and reasons are what the
    /// request log stores; an unlisted one stores neither.
    #[test]
    fn the_verdict_is_recorded_with_the_request() {
        let lists = crate::shield::ShieldLists::default();
        let now = temps_shield::unix_now() as i64;
        let mut ctx = super::markdown_tests::make_ctx();
        assert_eq!(ctx.shield_verdict(), None);
        assert!(ctx.shield_reasons().is_empty());

        let unlisted = lists.evaluate("8.8.8.8".parse().unwrap(), now);
        assert_eq!(unlisted.action, temps_shield::Action::Pass);

        ctx.shield = Some(lists.evaluate("192.0.2.7".parse().unwrap(), now));
        assert_eq!(ctx.shield_verdict().as_deref(), Some("log"));
        assert_eq!(ctx.shield_reasons(), ["iana_reserved (192.0.2.0/24)"]);
    }
}

#[cfg(test)]
mod traceparent_tests {
    use super::*;
    use std::collections::HashMap;

    fn headers_with(name: &str, value: &str) -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert(name.to_string(), value.to_string());
        h
    }

    #[test]
    fn extracts_valid_traceparent_trace_id() {
        let h = headers_with(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        );
        assert_eq!(
            LoadBalancer::extract_traceparent_trace_id(Some(&h)),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }

    #[test]
    fn returns_none_when_header_absent() {
        let h: HashMap<String, String> = HashMap::new();
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(None), None);
    }

    #[test]
    fn returns_none_for_all_zero_trace_id() {
        let h = headers_with(
            "traceparent",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        );
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
    }

    #[test]
    fn returns_none_for_wrong_length() {
        let h = headers_with("traceparent", "00-deadbeef-00f067aa0ba902b7-01");
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
    }

    #[test]
    fn returns_none_for_non_hex_chars() {
        let h = headers_with(
            "traceparent",
            "00-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-00f067aa0ba902b7-01",
        );
        assert_eq!(LoadBalancer::extract_traceparent_trace_id(Some(&h)), None);
    }

    #[test]
    fn lowercases_uppercase_hex() {
        let h = headers_with(
            "traceparent",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
        );
        assert_eq!(
            LoadBalancer::extract_traceparent_trace_id(Some(&h)),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }
}

#[cfg(test)]
mod content_type_tests {
    use super::is_event_stream_content_type;

    #[test]
    fn accepts_plain_and_parameterised_event_stream() {
        assert!(is_event_stream_content_type("text/event-stream"));
        assert!(is_event_stream_content_type(
            "text/event-stream; charset=utf-8"
        ));
        assert!(is_event_stream_content_type(
            "text/event-stream;charset=utf-8"
        ));
        // Media types are case-insensitive, and surrounding space is legal.
        assert!(is_event_stream_content_type("  TEXT/Event-Stream  "));
    }

    #[test]
    fn rejects_event_stream_hidden_in_a_parameter() {
        // The reason this is not a substring match: an upstream that smuggles
        // the token into a parameter would otherwise classify itself as a
        // streaming session and drop out of the proxy latency histograms.
        assert!(!is_event_stream_content_type(
            "text/html; note=text/event-stream"
        ));
        assert!(!is_event_stream_content_type(
            "application/json; x=\"text/event-stream\""
        ));
    }

    #[test]
    fn rejects_ordinary_content_types() {
        assert!(!is_event_stream_content_type("text/html"));
        assert!(!is_event_stream_content_type("application/json"));
        assert!(!is_event_stream_content_type(""));
        // A prefix match must not count either.
        assert!(!is_event_stream_content_type("text/event-stream-x"));
    }
}

#[cfg(test)]
mod traffic_classification_tests {
    use super::LoadBalancer;

    #[test]
    fn classifies_temps_monitor_as_synthetic_system_traffic() {
        assert_eq!(
            LoadBalancer::traffic_classification("/api/health", "Temps-Status-Monitor/1.0"),
            ("temps_monitor", true)
        );
    }

    #[test]
    fn leaves_customer_requests_as_proxy_traffic() {
        assert_eq!(
            LoadBalancer::traffic_classification("/api/health", "Mozilla/5.0"),
            ("proxy", false)
        );
    }
}

#[cfg(test)]
mod forwarded_authority_tests {
    use super::{
        coalesce_cookie_headers, find_request_cookie, parse_public_authority,
        strip_untrusted_client_ip_headers, PublicAuthority,
    };
    use axum::http::HeaderValue;
    use pingora_http::RequestHeader;

    #[test]
    fn preserves_non_default_public_port() {
        assert_eq!(
            parse_public_authority("keycloak-production.localho.st:8200"),
            Some(PublicAuthority {
                host: "keycloak-production.localho.st".to_string(),
                forwarded_host: "keycloak-production.localho.st:8200".to_string(),
                port: Some(8200),
            })
        );
    }

    #[test]
    fn accepts_bracketed_ipv6_authority() {
        assert_eq!(
            parse_public_authority("[::1]:8200"),
            Some(PublicAuthority {
                host: "::1".to_string(),
                forwarded_host: "[::1]:8200".to_string(),
                port: Some(8200),
            })
        );
    }

    #[test]
    fn leaves_default_port_to_the_request_scheme_and_normalizes_route_host() {
        assert_eq!(
            parse_public_authority("KEYCLOAK-production.example.com"),
            Some(PublicAuthority {
                host: "keycloak-production.example.com".to_string(),
                forwarded_host: "keycloak-production.example.com".to_string(),
                port: None,
            })
        );
    }

    #[test]
    fn rejects_malformed_or_ambiguous_authorities() {
        for authority in [
            "example.com:invalid",
            "example.com:0",
            "valid-route.example:80.evil",
            "user@valid-route.example",
            "::1:8200",
            "",
        ] {
            assert_eq!(
                parse_public_authority(authority),
                None,
                "authority {authority:?} must be rejected"
            );
        }
    }

    #[test]
    fn removes_client_supplied_rfc_7239_forwarded_header() {
        let mut request =
            RequestHeader::build("GET", b"/", Some(2)).expect("test request header must be valid");
        request
            .insert_header(
                "forwarded",
                HeaderValue::from_static("for=192.0.2.1;proto=https;host=evil.example"),
            )
            .expect("Forwarded test header must be valid");
        request
            .insert_header("x-unrelated", HeaderValue::from_static("preserved"))
            .expect("unrelated test header must be valid");

        strip_untrusted_client_ip_headers(&mut request, None);

        assert!(!request.headers.contains_key("forwarded"));
        assert_eq!(
            request.headers.get("x-unrelated"),
            Some(&HeaderValue::from_static("preserved"))
        );
    }

    /// A direct client that bypasses Bunny/Cloudflare entirely can still set
    /// `X-Real-IP` / `CF-Connecting-IP` itself. Those raw headers must never
    /// reach the tenant app upstream — only the platform's own resolved
    /// `X-Forwarded-For` (set separately by the caller) is trustworthy.
    #[test]
    fn removes_client_supplied_cdn_ip_headers() {
        let mut request =
            RequestHeader::build("GET", b"/", Some(2)).expect("test request header must be valid");
        request
            .insert_header("x-real-ip", HeaderValue::from_static("203.0.113.99"))
            .expect("X-Real-IP test header must be valid");
        request
            .insert_header("cf-connecting-ip", HeaderValue::from_static("203.0.113.99"))
            .expect("CF-Connecting-IP test header must be valid");
        request
            .insert_header("x-unrelated", HeaderValue::from_static("preserved"))
            .expect("unrelated test header must be valid");

        strip_untrusted_client_ip_headers(&mut request, None);

        assert!(!request.headers.contains_key("x-real-ip"));
        assert!(!request.headers.contains_key("cf-connecting-ip"));
        assert_eq!(
            request.headers.get("x-unrelated"),
            Some(&HeaderValue::from_static("preserved"))
        );
    }

    /// ADR 065 D3: tailnet identity reaches an app only from a worker's Serve
    /// socket. A client of the control-plane proxy cannot set it.
    #[test]
    fn removes_client_supplied_tailnet_identity_headers() {
        let mut request =
            RequestHeader::build("GET", b"/", Some(2)).expect("test request header must be valid");
        for name in [
            "tailscale-user-login",
            "Tailscale-User-Name",
            "tailscale-user-profile-pic",
            "tailscale-funnel-request",
            "x-temps-user-login",
        ] {
            request
                .append_header(name, HeaderValue::from_static("alice@example.com"))
                .expect("identity test header must be valid");
        }
        request
            .insert_header("x-unrelated", HeaderValue::from_static("preserved"))
            .expect("unrelated test header must be valid");

        strip_untrusted_client_ip_headers(&mut request, None);

        assert_eq!(request.headers.len(), 1, "{:?}", request.headers);
        assert_eq!(
            request.headers.get("x-unrelated"),
            Some(&HeaderValue::from_static("preserved"))
        );
    }

    /// The CDN, edge and configured local JA4 headers are only read from the
    /// peers that set them, and an app never sees one a visitor could have
    /// written.
    #[test]
    fn removes_client_supplied_ja4_headers() {
        let names = [
            "cf-ja4",
            "cdn-ja4",
            "cloudfront-viewer-ja4-fingerprint",
            "x-temps-edge-client",
            "x-ja4",
        ];
        assert!(temps_shield::ja4::FORWARDED_JA4_HEADERS
            .iter()
            .all(|name| names.contains(name)));
        let mut request =
            RequestHeader::build("GET", b"/", Some(2)).expect("test request header must be valid");
        for name in names {
            request
                .insert_header(
                    name,
                    HeaderValue::from_static("t13d1516h2_8daaf6152771_e5627efa2ab1"),
                )
                .expect("JA4 test header must be valid");
        }
        request
            .insert_header("x-unrelated", HeaderValue::from_static("preserved"))
            .expect("unrelated test header must be valid");

        strip_untrusted_client_ip_headers(
            &mut request,
            Some(&axum::http::HeaderName::from_static("x-ja4")),
        );

        for name in names {
            assert!(
                !request.headers.contains_key(name),
                "{name} must be removed"
            );
        }
        assert_eq!(
            request.headers.get("x-unrelated"),
            Some(&HeaderValue::from_static("preserved"))
        );
    }

    fn request_with_cookies(crumbs: &[&'static [u8]]) -> RequestHeader {
        let mut request =
            RequestHeader::build("GET", b"/", Some(2)).expect("test request header must be valid");
        for crumb in crumbs {
            request
                .append_header(
                    "cookie",
                    HeaderValue::from_bytes(crumb).expect("cookie crumb must be a valid value"),
                )
                .expect("Cookie test header must be valid");
        }
        request
    }

    fn cookie_values(request: &RequestHeader) -> Vec<&[u8]> {
        request
            .headers
            .get_all("cookie")
            .iter()
            .map(|value| value.as_bytes())
            .collect()
    }

    /// Regression: an HTTP/2 browser sends each cookie as its own `cookie`
    /// field (RFC 9113 §8.2.3). The upstream must get one joined header,
    /// otherwise PHP-style servers keep only the first cookie
    /// (`_temps_visitor_id`) and drop the app's session cookie.
    #[test]
    fn joins_http2_cookie_crumbs_into_one_header() {
        let mut request = request_with_cookies(&[
            b"_temps_visitor_id=abc",
            b"woocommerce_items_in_cart=1",
            b"zzz=3",
        ]);

        coalesce_cookie_headers(&mut request);

        assert_eq!(
            cookie_values(&request),
            vec![&b"_temps_visitor_id=abc; woocommerce_items_in_cart=1; zzz=3"[..]]
        );
    }

    #[test]
    fn leaves_single_cookie_header_untouched() {
        let mut request = request_with_cookies(&[b"a=1; b=2"]);

        coalesce_cookie_headers(&mut request);

        assert_eq!(cookie_values(&request), vec![&b"a=1; b=2"[..]]);
    }

    #[test]
    fn no_cookie_header_stays_absent() {
        let mut request = request_with_cookies(&[]);

        coalesce_cookie_headers(&mut request);

        assert!(!request.headers.contains_key("cookie"));
    }

    #[test]
    fn joins_multi_cookie_crumbs_and_skips_empty_ones() {
        let mut request = request_with_cookies(&[b"a=1; b=2", b"", b"  c=3  ", b"   "]);

        coalesce_cookie_headers(&mut request);

        assert_eq!(cookie_values(&request), vec![&b"a=1; b=2; c=3"[..]]);
    }

    #[test]
    fn removes_cookie_header_when_every_crumb_is_empty() {
        let mut request = request_with_cookies(&[b"", b" "]);

        coalesce_cookie_headers(&mut request);

        assert!(!request.headers.contains_key("cookie"));
    }

    /// Cookie values may carry obs-text bytes that are not valid UTF-8; they
    /// must be forwarded byte-for-byte, never dropped.
    #[test]
    fn preserves_non_utf8_cookie_bytes() {
        let mut request = request_with_cookies(&[b"a=v\xe9\xff", b"b=2"]);

        coalesce_cookie_headers(&mut request);

        assert_eq!(cookie_values(&request), vec![&b"a=v\xe9\xff; b=2"[..]]);
    }

    /// Regression (review of #1149): once crumbs are joined, a non-UTF-8
    /// crumb shares a field with Temps' tracking cookies. They must stay
    /// readable, otherwise returning visitors get fresh identities.
    #[test]
    fn tracking_cookies_stay_readable_next_to_a_non_utf8_crumb() {
        let mut request = request_with_cookies(&[
            b"_temps_visitor_id=visitor",
            b"legacy=v\xe9\xff",
            b"_temps_sid=session",
        ]);

        coalesce_cookie_headers(&mut request);

        let value = |name: &str| find_request_cookie(&request, name, |v| Some(v.to_string()));
        assert_eq!(value("_temps_visitor_id").as_deref(), Some("visitor"));
        assert_eq!(value("_temps_sid").as_deref(), Some("session"));
    }

    #[test]
    fn find_request_cookie_searches_every_field_and_filters_by_accept() {
        let request = request_with_cookies(&[b"a=1; token=bad", b"token=good"]);

        let accepted = find_request_cookie(&request, "token", |v| (v == "good").then_some(v.len()));
        assert_eq!(accepted, Some(4));
        assert_eq!(
            find_request_cookie(&request, "token", |v| Some(v.to_string())).as_deref(),
            Some("bad"),
            "first cookie with the name comes first"
        );
        assert_eq!(find_request_cookie(&request, "missing", |_| Some(())), None);
    }

    #[test]
    fn keeps_unrelated_headers_when_coalescing() {
        let mut request = request_with_cookies(&[b"a=1", b"b=2"]);
        request
            .insert_header("x-unrelated", HeaderValue::from_static("preserved"))
            .expect("unrelated test header must be valid");

        coalesce_cookie_headers(&mut request);

        assert_eq!(cookie_values(&request), vec![&b"a=1; b=2"[..]]);
        assert_eq!(
            request.headers.get("x-unrelated"),
            Some(&HeaderValue::from_static("preserved"))
        );
    }
}

/// Tests for the CDN client-IP resolution chain in `resolve_session_client_ip`.
///
/// `PingoraSession` cannot be constructed in isolation in unit tests (it
/// requires a live I/O object). We therefore test the resolution logic through
/// the underlying trust-store methods directly — `BunnyIpTrust::resolve_client_ip`
/// and `CloudflareIpTrust::resolve_client_ip` — which is where the anti-spoofing
/// invariant and header parsing actually live. This follows the same pattern as
/// the unit tests in `cloudflare_ips.rs` and `bunny_ips.rs`.
#[cfg(test)]
mod cdn_client_ip_tests {
    use crate::bunny_ips::BunnyIpTrust;
    use std::collections::HashSet;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn bunny_trust_with(ips: &[&str]) -> BunnyIpTrust {
        let set: HashSet<IpAddr> = ips.iter().map(|s| ip(s)).collect();
        BunnyIpTrust::with_ips(set)
    }

    /// Simulates a request arriving from a Bunny edge IP with a valid
    /// `X-Real-IP` header. The resolved client IP must be the header value.
    #[test]
    fn bunny_edge_peer_with_valid_x_real_ip_uses_header() {
        let bunny_edge = "185.152.66.10";
        let real_client = "203.0.113.42";
        let t = bunny_trust_with(&[bunny_edge]);
        assert_eq!(
            t.resolve_client_ip(ip(bunny_edge), Some(real_client)),
            ip(real_client),
            "verified Bunny peer must use the X-Real-IP header value"
        );
    }

    /// Simulates a direct connection (no CDN): an arbitrary peer with a
    /// spoofed `X-Real-IP` must NOT be trusted — the peer address is returned.
    #[test]
    fn non_bunny_peer_with_spoofed_x_real_ip_uses_peer() {
        let attacker = "203.0.113.99";
        let spoofed_client = "10.0.0.1";
        // Trust set does NOT include the attacker's IP.
        let t = bunny_trust_with(&["185.152.66.10"]);
        assert_eq!(
            t.resolve_client_ip(ip(attacker), Some(spoofed_client)),
            ip(attacker),
            "untrusted peer must ignore X-Real-IP even when the header value is valid"
        );
    }

    /// A Bunny edge peer with a malformed or missing header falls back to peer.
    #[test]
    fn bunny_edge_peer_with_bad_header_falls_back_to_peer() {
        let bunny_edge = "185.152.66.10";
        let t = bunny_trust_with(&[bunny_edge]);
        let peer = ip(bunny_edge);
        for bad in ["not-an-ip", "1.2.3.4, 5.6.7.8", "1.2.3.4:8080", ""] {
            assert_eq!(
                t.resolve_client_ip(peer, Some(bad)),
                peer,
                "malformed X-Real-IP {bad:?} must fall back to peer"
            );
        }
        assert_eq!(t.resolve_client_ip(peer, None), peer);
    }

    /// Verify the Cloudflare trust chain still works correctly alongside Bunny
    /// (regression guard: adding Bunny must not break the existing CF path).
    #[test]
    fn cloudflare_peer_still_uses_cf_connecting_ip() {
        use crate::cloudflare_ips::CloudflareIpTrust;
        let t = CloudflareIpTrust::new();
        // 104.16.1.1 is inside the builtin Cloudflare ranges.
        let cf_edge = ip("104.16.1.1");
        let real_client = ip("198.51.100.7");
        assert_eq!(
            t.resolve_client_ip(cf_edge, Some("198.51.100.7")),
            real_client
        );
    }
}

#[cfg(test)]
mod proxy_failure_kind_tests {
    use super::upstream_failure_key;
    use super::ProxyFailureKind;
    use pingora::{Error, ErrorSource, ErrorType};

    fn error(etype: ErrorType, source: ErrorSource) -> Box<Error> {
        let mut e = Error::new(etype);
        e.esource = source;
        e
    }

    #[test]
    fn client_disconnects_are_not_errors() {
        for etype in [
            ErrorType::ConnectionClosed,
            ErrorType::ReadError,
            ErrorType::WriteError,
        ] {
            assert_eq!(
                ProxyFailureKind::of(&error(etype, ErrorSource::Downstream)),
                ProxyFailureKind::ClientGone
            );
        }
    }

    #[test]
    fn independent_unscoped_upstreams_keep_independent_outages() {
        let latch = temps_core::log_transitions::KeyedFailureLatch::default();
        let a = upstream_failure_key(None, None, Some("service-a:80"), "a.example.com");
        let b = upstream_failure_key(None, None, Some("service-b:80"), "b.example.com");
        assert!(latch.record_failure(a.clone()).should_log());
        assert!(latch.record_failure(b.clone()).should_log());
        assert!(!latch.record_failure(a.clone()).should_log());
        latch.record_success(&a);
        assert!(latch.is_failing(&b));
        assert!(latch.record_failure(a).should_log());
        assert_eq!(
            upstream_failure_key(Some(1), Some(2), Some("host"), "route"),
            (1, 2, None)
        );
    }

    #[test]
    fn upstream_failures_are_classified_as_upstream() {
        for etype in [
            ErrorType::ConnectRefused,
            ErrorType::ConnectTimedout,
            ErrorType::ConnectionClosed,
        ] {
            assert_eq!(
                ProxyFailureKind::of(&error(etype, ErrorSource::Upstream)),
                ProxyFailureKind::Upstream
            );
        }
    }

    #[test]
    fn everything_else_is_internal() {
        assert_eq!(
            ProxyFailureKind::of(&error(
                ErrorType::InvalidHTTPHeader,
                ErrorSource::Downstream
            )),
            ProxyFailureKind::Internal
        );
        assert_eq!(
            ProxyFailureKind::of(&error(ErrorType::InternalError, ErrorSource::Internal)),
            ProxyFailureKind::Internal
        );
        assert_eq!(
            ProxyFailureKind::of(&error(ErrorType::InternalError, ErrorSource::Unset)),
            ProxyFailureKind::Internal
        );
    }
}

#[cfg(test)]
mod stopped_app_tests {
    use super::LoadBalancer;
    use crate::config::ProxyConfig;
    use crate::services::{ProjectContextResolverImpl, UpstreamResolverImpl};
    use crate::test_utils::*;
    use crate::traits::{ProjectContextResolver, UpstreamResolver};
    use anyhow::Result;
    use pingora::upstreams::peer::Peer;
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use temps_routes::CachedPeerTable;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn create_crypto_cookie_crypto() -> Arc<temps_core::CookieCrypto> {
        let encryption_key = "default-32-byte-key-for-testing!";
        Arc::new(
            temps_core::CookieCrypto::new(encryption_key).expect("Failed to create cookie crypto"),
        )
    }

    fn create_test_config_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_config::ConfigService> {
        let config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test@localhost/test".to_string(),
            None,
            None,
        )
        .expect("Failed to create test ServerConfig");

        Arc::new(temps_config::ConfigService::new(Arc::new(config), db))
    }

    fn create_mock_ip_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_geo::IpAddressService> {
        // Force mock mode for tests by setting environment variable
        std::env::set_var("TEMPS_GEO_MOCK", "true");

        let geoip_service =
            Arc::new(temps_geo::GeoIpService::new().expect("Failed to create GeoIpService"));
        Arc::new(temps_geo::IpAddressService::new(db, geoip_service))
    }

    /// Stand-in for the console: answers every request with the console's
    /// SPA shell and HTTP 200, as the real console does for any path.
    async fn start_console_stand_in() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let _ = stream.read(&mut request).await;
                    let body = "<!DOCTYPE html><html><head><title>Temps</title></head>\
                                <body><div id=\"root\"></div></body></html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        address
    }

    /// Issue #1334: a request to an app whose containers are all down must
    /// get the proxy's 503 page, never the console. Before the fix the host
    /// vanished from the route table, the request fell through to the console
    /// and was answered with the console SPA and HTTP 200, so uptime monitors
    /// reported the stopped app as operational.
    ///
    /// Drives the real `ProxyHttp` hooks over an in-memory HTTP/1.1 session,
    /// in the order Pingora calls them.
    #[tokio::test]
    async fn test_stopped_app_gets_503_instead_of_the_console() -> Result<()> {
        use pingora_proxy::{ProxyHttp, Session};
        use sea_orm::{ActiveModelTrait, Set};

        let test_db_mock = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping stopped-app proxy test: Docker runtime unavailable");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let db = test_db_mock.connection_arc().clone();
        let test_db = TestDBMockOperations::new(db.clone()).await.unwrap();

        let host = "stopped-app.example.com";
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain(host)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        // The request below is plain HTTP. A resolved production environment
        // is redirected to HTTPS by default, exactly as a running app would
        // be; opt out so the request reaches upstream selection.
        temps_entities::environments::ActiveModel {
            id: Set(environment.id),
            force_https: Set(Some(false)),
            ..Default::default()
        }
        .update(db.as_ref())
        .await?;
        temps_entities::deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("stopped-app-container".to_string()),
            container_name: Set("stopped-app-container".to_string()),
            container_port: Set(Some(3000)),
            host_port: Set(None),
            status: Set(Some("exited".to_string())),
            deployed_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let route_table = Arc::new(CachedPeerTable::new(db.clone()));
        route_table.load_routes().await?;

        // The console is a live server: if the request reached it, the test
        // would see its 200 instead of the 503.
        let console_addr = start_console_stand_in().await;
        let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
            Arc::new(ProxyConfig {
                console_address: console_addr,
                ..ProxyConfig::default()
            }),
            Arc::new(crate::service::lb_service::LbService::new(db.clone())),
            route_table.clone(),
        )) as Arc<dyn UpstreamResolver>;
        let project_context_resolver = Arc::new(ProjectContextResolverImpl::new(route_table))
            as Arc<dyn ProjectContextResolver>;

        let ip_service = create_mock_ip_service(db.clone());
        let proxy_log_storage: Arc<dyn crate::storage::ProxyLogStorage> = Arc::new(
            crate::storage::TimescaleDbProxyLogStore::new(db.clone(), ip_service.clone()),
        );
        let (proxy_log_handle, tracking_handle, _proxy_log_writer) =
            crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
                db.clone(),
                ip_service,
                proxy_log_storage,
            );
        let lb = LoadBalancer::new(
            upstream_resolver,
            proxy_log_handle,
            tracking_handle,
            project_context_resolver,
            create_crypto_cookie_crypto(),
            db.clone(),
            create_test_config_service(db.clone()),
            Arc::new(
                crate::service::ip_access_control_service::IpAccessControlService::new(db.clone()),
            ),
            Arc::new(temps_core::OpenIpGate),
            Arc::new(crate::shield::test_gate()),
            Arc::new(crate::service::cert_host_cache::CertHostCache::new(
                db.clone(),
            )),
            false,
        );

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        client
            .write_all(
                format!(
                    "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Temps-Status-Monitor/1.0\r\n\r\n"
                )
                .as_bytes(),
            )
            .await?;
        let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
        assert!(session.read_request().await?, "request must parse");

        let mut ctx = lb.new_ctx();
        lb.early_request_filter(&mut session, &mut ctx).await?;
        assert!(
            !lb.request_filter(&mut session, &mut ctx).await?,
            "nothing in request_filter may answer for the stopped app (answered as {})",
            ctx.routing_status
        );
        assert_eq!(
            ctx.project.as_ref().map(|project| project.id),
            Some(project.id),
            "the failed request is attributed to the app's project"
        );

        let error = match lb.upstream_peer(&mut session, &mut ctx).await {
            Ok(peer) => panic!("stopped app was proxied to {}", peer.address()),
            Err(error) => error,
        };
        let outcome = lb.fail_to_proxy(&mut session, &error, &mut ctx).await;
        assert_eq!(outcome.error_code, 503);
        drop(session);

        let mut raw = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut raw),
        )
        .await??;
        let response = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = response
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("malformed response: {response:?}"));
        assert!(
            head.starts_with("HTTP/1.1 503"),
            "a stopped app must answer 503: {head}"
        );
        assert!(
            head.lines()
                .any(|line| line.eq_ignore_ascii_case("cache-control: private, no-store")),
            "the outage page must not be cached: {head}"
        );
        assert!(
            body.contains("<title>Service Unavailable</title>"),
            "{body}"
        );
        assert!(
            !body.contains("<title>Temps</title>"),
            "the console must never answer for an application host: {body}"
        );

        Ok(())
    }
}

/// Status pages on custom hostnames (#1412): the proxy is the only thing that
/// may answer such a host, and it allows exactly two paths.
#[cfg(test)]
mod status_page_host_tests {
    use super::LoadBalancer;
    use crate::config::ProxyConfig;
    use crate::services::{ProjectContextResolverImpl, UpstreamResolverImpl};
    use crate::traits::{ProjectContextResolver, UpstreamResolver};
    use anyhow::Result;
    use pingora::upstreams::peer::Peer;
    use pingora_proxy::{ProxyHttp, Session};
    use std::sync::Arc;
    use temps_core::public_status_page::{NOT_FOUND_HTML, NOT_FOUND_JSON, STATUS_HOST_HEADER};
    use temps_database::test_utils::TestDatabase;
    use temps_routes::CachedPeerTable;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn create_crypto_cookie_crypto() -> Arc<temps_core::CookieCrypto> {
        Arc::new(
            temps_core::CookieCrypto::new("default-32-byte-key-for-testing!")
                .expect("Failed to create cookie crypto"),
        )
    }

    fn create_test_config_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_config::ConfigService> {
        let config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test@localhost/test".to_string(),
            None,
            None,
        )
        .expect("Failed to create test ServerConfig");
        Arc::new(temps_config::ConfigService::new(Arc::new(config), db))
    }

    fn create_mock_ip_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_geo::IpAddressService> {
        std::env::set_var("TEMPS_GEO_MOCK", "true");
        let geoip_service =
            Arc::new(temps_geo::GeoIpService::new().expect("Failed to create GeoIpService"));
        Arc::new(temps_geo::IpAddressService::new(db, geoip_service))
    }

    const SLUG: &str = "0123456789abcdef0123456789abcdef";
    const HOST: &str = "status.example.com";
    const INGRESS: &str = "127.0.0.1:1";

    pub(super) struct Harness {
        pub(super) _db: TestDatabase,
        pub(super) lb: LoadBalancer,
        pub(super) route_table: Arc<CachedPeerTable>,
    }

    pub(super) async fn harness(
        ingress: Option<&str>,
        disable_https_redirect: bool,
    ) -> Result<Option<Harness>> {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping status-page host proxy test: Docker runtime unavailable");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let db = test_db.connection_arc().clone();
        let route_table = Arc::new(CachedPeerTable::new(db.clone()));
        let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
            Arc::new(ProxyConfig {
                console_address: "127.0.0.1:2".to_string(),
                console_ingress_address: ingress.map(str::to_string),
                ..ProxyConfig::default()
            }),
            Arc::new(crate::service::lb_service::LbService::new(db.clone())),
            route_table.clone(),
        )) as Arc<dyn UpstreamResolver>;
        let project_context_resolver =
            Arc::new(ProjectContextResolverImpl::new(route_table.clone()))
                as Arc<dyn ProjectContextResolver>;
        let ip_service = create_mock_ip_service(db.clone());
        let proxy_log_storage: Arc<dyn crate::storage::ProxyLogStorage> = Arc::new(
            crate::storage::TimescaleDbProxyLogStore::new(db.clone(), ip_service.clone()),
        );
        let (proxy_log_handle, tracking_handle, _writer) =
            crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
                db.clone(),
                ip_service,
                proxy_log_storage,
            );
        let lb = LoadBalancer::new(
            upstream_resolver,
            proxy_log_handle,
            tracking_handle,
            project_context_resolver,
            create_crypto_cookie_crypto(),
            db.clone(),
            create_test_config_service(db.clone()),
            Arc::new(
                crate::service::ip_access_control_service::IpAccessControlService::new(db.clone()),
            ),
            Arc::new(temps_core::OpenIpGate),
            Arc::new(crate::shield::test_gate()),
            Arc::new(crate::service::cert_host_cache::CertHostCache::new(
                db.clone(),
            )),
            disable_https_redirect,
        );
        Ok(Some(Harness {
            _db: test_db,
            lb,
            route_table,
        }))
    }

    enum Outcome {
        /// `request_filter` let the request through to `upstream_peer`.
        Forwarded {
            path: String,
            status_host: Option<String>,
            cookie: bool,
            peer: Option<String>,
        },
        /// The proxy answered itself.
        Answered { head: String, body: String },
    }

    async fn send(h: &Harness, raw_request: &str) -> Result<Outcome> {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        client.write_all(raw_request.as_bytes()).await?;
        let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
        assert!(session.read_request().await?, "request must parse");
        let mut ctx = h.lb.new_ctx();
        h.lb.early_request_filter(&mut session, &mut ctx).await?;
        let answered = h.lb.request_filter(&mut session, &mut ctx).await?;
        if !answered {
            let header = session.req_header();
            let path = header
                .uri
                .path_and_query()
                .map(|pq| pq.as_str().to_string())
                .unwrap_or_default();
            let status_host = header
                .headers
                .get(STATUS_HOST_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let cookie = header.headers.get("cookie").is_some();
            let peer = if ctx.status_page_host {
                Some(
                    h.lb.upstream_peer(&mut session, &mut ctx)
                        .await?
                        .address()
                        .to_string(),
                )
            } else {
                None
            };
            return Ok(Outcome::Forwarded {
                path,
                status_host,
                cookie,
                peer,
            });
        }
        drop(session);
        let mut raw = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut raw),
        )
        .await??;
        let response = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = response
            .split_once("\r\n\r\n")
            .map(|(h, b)| (h.to_string(), b.to_string()))
            .unwrap_or((response.clone(), String::new()));
        Ok(Outcome::Answered { head, body })
    }

    fn get(path: &str, host: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nCookie: session=secret\r\n\r\n")
    }

    fn assert_not_found(outcome: Outcome, json: bool) {
        match outcome {
            Outcome::Answered { head, body } => {
                assert!(head.starts_with("HTTP/1.1 404"), "{head}");
                assert!(
                    head.to_ascii_lowercase()
                        .contains("cache-control: no-store"),
                    "{head}"
                );
                assert_eq!(body, if json { NOT_FOUND_JSON } else { NOT_FOUND_HTML });
                assert!(!body.contains("<title>Temps</title>"));
            }
            Outcome::Forwarded { path, .. } => panic!("must answer 404, forwarded {path}"),
        }
    }

    #[tokio::test]
    async fn verified_host_serves_only_the_page_and_summary_on_the_ingress_listener() -> Result<()>
    {
        let Some(h) = harness(Some(INGRESS), true).await? else {
            return Ok(());
        };
        h.route_table
            .insert_status_page_host_for_test(HOST, Some(SLUG));

        for (path, expected) in [
            ("/", format!("/api/status/{SLUG}")),
            ("/summary.json", format!("/api/status/{SLUG}/summary.json")),
        ] {
            match send(&h, &get(path, HOST)).await? {
                Outcome::Forwarded {
                    path: forwarded,
                    status_host,
                    cookie,
                    peer,
                } => {
                    assert_eq!(forwarded, expected);
                    assert_eq!(status_host.as_deref(), Some(HOST));
                    assert!(!cookie, "credentials never reach the public listener");
                    assert_eq!(peer.as_deref(), Some(INGRESS));
                }
                Outcome::Answered { head, .. } => panic!("{path} must be forwarded: {head}"),
            }
        }

        // A query string never travels: `/` is the page, whatever follows.
        match send(&h, &get("/?x=1/../../api/projects", HOST)).await? {
            Outcome::Forwarded { path, .. } => assert_eq!(path, format!("/api/status/{SLUG}")),
            Outcome::Answered { head, .. } => panic!("{head}"),
        }

        // Host header spelling does not matter; the binding does.
        assert!(matches!(
            send(&h, &get("/", "Status.Example.COM.:8080")).await?,
            Outcome::Forwarded { .. }
        ));

        for path in [
            "/index.html",
            "/api/projects",
            &format!("/api/status/{SLUG}"),
            "/api/_temps/captcha/verify",
            "/api/otel/v1/traces",
            "/summary.json/",
            "//",
            "/%2e%2e/api/projects",
            "/.well-known/acme-challenge/unknown-token",
        ] {
            assert_not_found(send(&h, &get(path, HOST)).await?, false);
        }
        assert_not_found(
            send(
                &h,
                &format!("POST / HTTP/1.1\r\nHost: {HOST}\r\nContent-Length: 0\r\n\r\n"),
            )
            .await?,
            false,
        );
        Ok(())
    }

    #[tokio::test]
    async fn unverified_or_unpublished_host_answers_404_and_never_the_console() -> Result<()> {
        let Some(h) = harness(Some(INGRESS), true).await? else {
            return Ok(());
        };
        h.route_table
            .insert_status_page_host_for_test("pending.example.com", None);
        assert_not_found(send(&h, &get("/", "pending.example.com")).await?, false);
        assert_not_found(
            send(&h, &get("/summary.json", "pending.example.com")).await?,
            true,
        );
        assert_not_found(
            send(&h, &get("/api/projects", "pending.example.com")).await?,
            false,
        );
        Ok(())
    }

    /// A signed-in address served by this proxy still gets and renews its
    /// certificate (ADR 065 D8): Let's Encrypt never signs in, so a stored
    /// HTTP-01 challenge is answered before the gate. Any other path under
    /// the prefix still meets the gate.
    #[tokio::test]
    async fn a_signed_in_host_still_answers_its_acme_challenge_over_http() -> Result<()> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_core::gate::{GateKind, GatePolicy, TempsGate};

        const GATED: &str = "app.example.com";
        let Some(mut h) = harness(None, false).await? else {
            return Ok(());
        };
        let db = h._db.connection_arc().clone();
        let now = chrono::Utc::now();
        temps_entities::domains::ActiveModel {
            domain: Set(GATED.to_string()),
            status: Set("pending".to_string()),
            http_challenge_token: Set(Some("challenge-token".to_string())),
            http_challenge_key_authorization: Set(Some("challenge-token.thumbprint".to_string())),
            is_wildcard: Set(false),
            verification_method: Set("http-01".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        h.route_table.insert_route_for_test(
            GATED,
            temps_routes::RouteInfo {
                backend: temps_routes::BackendType::Upstream {
                    backends: vec![temps_routes::BackendEntry {
                        address: "127.0.0.1:2".to_string(),
                        container_id: None,
                        container_name: None,
                    }],
                    round_robin_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                },
                redirect_to: None,
                status_code: None,
                project: None,
                environment: None,
                deployment: None,
                cert_eligible: true,
            },
        );
        let key = temps_core::gate::generate_signing_key()?;
        h.route_table.insert_gate_policy_for_test(
            GATED,
            GatePolicy {
                address_id: "custom:7".into(),
                gate: GateKind::Temps,
                temps: Some(TempsGate {
                    public_key: temps_core::gate::encode_public_key(&key.verifying_key()),
                    authorize_url: "https://console.example.com/_temps/gate/authorize".into(),
                    not_before: 0,
                    revoked: vec![],
                }),
                access: None,
            },
        );
        let service = Arc::new(temps_routes::gate::GateService::new(
            db,
            Arc::new(temps_core::EncryptionService::new_from_password(
                "gate-test",
            )),
        ));
        h.lb =
            h.lb.with_gate(Arc::new(crate::handler::gate::ProxyGate::new(
                h.route_table.clone(),
                service,
            )));

        match send(
            &h,
            &get("/.well-known/acme-challenge/challenge-token", GATED),
        )
        .await?
        {
            Outcome::Answered { head, body } => {
                assert!(head.starts_with("HTTP/1.1 200"), "{head}");
                assert_eq!(body, "challenge-token.thumbprint");
            }
            Outcome::Forwarded { path, .. } => panic!("the challenge must be answered: {path}"),
        }
        // No stored challenge: the gate answers, never the app.
        for path in ["/.well-known/acme-challenge/other-token", "/"] {
            match send(&h, &get(path, GATED)).await? {
                Outcome::Answered { head, body } => {
                    assert!(!head.starts_with("HTTP/1.1 200"), "{path}: {head}");
                    assert!(!body.contains("thumbprint"), "{path}: {body}");
                }
                Outcome::Forwarded { path, .. } => panic!("{path} must meet the gate"),
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn without_an_ingress_listener_the_host_fails_closed() -> Result<()> {
        let Some(h) = harness(None, true).await? else {
            return Ok(());
        };
        h.route_table
            .insert_status_page_host_for_test(HOST, Some(SLUG));
        assert_not_found(send(&h, &get("/", HOST)).await?, false);
        Ok(())
    }

    #[tokio::test]
    async fn plain_http_is_upgraded_to_https_on_a_verified_host() -> Result<()> {
        let Some(h) = harness(Some(INGRESS), false).await? else {
            return Ok(());
        };
        h.route_table
            .insert_status_page_host_for_test(HOST, Some(SLUG));
        match send(&h, &get("/summary.json", HOST)).await? {
            Outcome::Answered { head, .. } => {
                assert!(head.starts_with("HTTP/1.1 301"), "{head}");
                assert!(
                    head.contains(&format!("https://{HOST}/summary.json")),
                    "{head}"
                );
            }
            Outcome::Forwarded { .. } => panic!("plain HTTP must be upgraded"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_client_cannot_tag_a_request_with_a_status_host() -> Result<()> {
        let Some(h) = harness(Some(INGRESS), true).await? else {
            return Ok(());
        };
        h.route_table
            .insert_status_page_host_for_test(HOST, Some(SLUG));
        // A request for another host carrying a forged tag reaches normal
        // routing without it.
        let raw = format!(
            "GET /api/status/{SLUG} HTTP/1.1\r\nHost: console.example.net\r\n{STATUS_HOST_HEADER}: {HOST}\r\n\r\n"
        );
        match send(&h, &raw).await? {
            Outcome::Forwarded { status_host, .. } => assert_eq!(status_host, None),
            Outcome::Answered { .. } => {}
        }
        // On the status host itself, a forged tag is replaced by the real one.
        let raw = format!(
            "GET / HTTP/1.1\r\nHost: {HOST}\r\n{STATUS_HOST_HEADER}: other.example.com\r\n\r\n"
        );
        match send(&h, &raw).await? {
            Outcome::Forwarded { status_host, .. } => {
                assert_eq!(status_host.as_deref(), Some(HOST))
            }
            Outcome::Answered { head, .. } => panic!("{head}"),
        }
        Ok(())
    }
}

/// Shield end to end through `request_filter` (ADR 066, phases 1 and 4):
/// each mode and verdict, the challenge page and the 429 and 426 that stand
/// in for it, exemptions and bypass tokens, the verify endpoint the proxy
/// answers itself, and the clearance cookie.
#[cfg(test)]
mod shield_enforcement_tests {
    use super::status_page_host_tests::{harness, Harness};
    use super::{LoadBalancer, ProxyContext};
    use anyhow::Result;
    use pingora_proxy::{ProxyHttp, Session};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};
    use temps_entities::{environments, projects, shield_bypass_tokens};
    use temps_shield::{
        bypass, EventKind, EventSink, ListVerdict, Network, ShieldEvent, ShieldSnapshot, SourceList,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const HOST: &str = "shielded.example.com";
    const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) Firefox/131.0";
    const FINGERPRINT: &str = "t13_0123456789abcdef01234567";
    /// On the test lists: blocked, challenged, and on none.
    const BLOCKED: &str = "185.93.89.7";
    const CHALLENGED: &str = "45.33.32.9";
    const CLEAN: &str = "8.8.8.8";

    #[derive(Default)]
    struct Collected(Mutex<Vec<ShieldEvent>>);

    impl EventSink for Collected {
        fn submit(&self, events: Vec<ShieldEvent>) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend(events);
        }
    }

    impl Collected {
        fn take(&self) -> Vec<ShieldEvent> {
            std::mem::take(
                &mut *self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        }
    }

    struct Shielded {
        h: Harness,
        events: Arc<Collected>,
        tokens: Arc<crate::shield::BypassTokens>,
    }

    /// The status-page harness's proxy, with the test lists, an event
    /// collector, and bypass tokens loaded from its database.
    async fn shielded_harness() -> Result<Option<Shielded>> {
        let Some(h) = harness(None, true).await? else {
            return Ok(None);
        };
        let Harness {
            _db,
            lb,
            route_table,
        } = h;
        let now = temps_shield::unix_now() as i64;
        let list = |source: &str, verdict, network: &str| {
            SourceList::new(source, verdict, now, None, Network::parse(network))
        };
        let lists = crate::shield::ShieldLists::from_snapshot(&ShieldSnapshot {
            revision: 1,
            generated_at: now,
            lists: vec![
                list("spamhaus_drop", ListVerdict::Block, "185.93.89.0/24"),
                list("dshield", ListVerdict::Challenge, "45.33.32.0/24"),
            ],
            ..ShieldSnapshot::default()
        });
        let events = Arc::new(Collected::default());
        let tokens = Arc::new(crate::shield::BypassTokens::default());
        let lb: LoadBalancer = lb
            .with_shield_lists(Arc::new(lists))
            .with_shield_events(events.clone())
            .with_shield_bypass_tokens(tokens.clone());
        Ok(Some(Shielded {
            h: Harness {
                _db,
                lb,
                route_table,
            },
            events,
            tokens,
        }))
    }

    enum Outcome {
        Forwarded,
        Answered { head: String, body: String },
    }

    struct Sent {
        outcome: Outcome,
        ctx: ProxyContext,
    }

    /// Runs the request filters for a client at `ip`, over TLS when `tls`:
    /// the duplex stream has no handshake or peer address, so both are set
    /// where `early_request_filter` would have found them.
    async fn send(h: &Harness, raw_request: String, ip: &str, tls: bool) -> Result<Sent> {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        client.write_all(raw_request.as_bytes()).await?;
        let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
        assert!(session.read_request().await?, "request must parse");
        let mut ctx = h.lb.new_ctx();
        h.lb.early_request_filter(&mut session, &mut ctx).await?;
        ctx.ip_address = Some(ip.to_string());
        ctx.tls_fingerprint = tls.then(|| FINGERPRINT.to_string());
        if !h.lb.request_filter(&mut session, &mut ctx).await? {
            return Ok(Sent {
                outcome: Outcome::Forwarded,
                ctx,
            });
        }
        drop(session);
        let mut raw = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut raw),
        )
        .await??;
        let response = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = response
            .split_once("\r\n\r\n")
            .map(|(h, b)| (h.to_string(), b.to_string()))
            .unwrap_or((response.clone(), String::new()));
        Ok(Sent {
            outcome: Outcome::Answered { head, body },
            ctx,
        })
    }

    fn request(method: &str, path: &str, accept: &str, extra: &str) -> String {
        request_as(method, path, accept, UA, extra)
    }

    fn request_as(method: &str, path: &str, accept: &str, user_agent: &str, extra: &str) -> String {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: {user_agent}\r\nAccept: {accept}\r\nContent-Length: 0\r\n{extra}\r\n"
        )
    }

    fn page(path: &str, cookie: Option<&str>) -> String {
        let cookie = cookie
            .map(|c| format!("Cookie: {c}\r\n"))
            .unwrap_or_default();
        request(
            "GET",
            path,
            "text/html,application/xhtml+xml,*/*;q=0.8",
            &cookie,
        )
    }

    fn post_verify(user_agent: &str, body: &str) -> String {
        format!(
            "POST {} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: {user_agent}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            crate::shield::VERIFY_PATH,
            body.len()
        )
    }

    fn answered(sent: Sent) -> (String, String, ProxyContext) {
        match sent.outcome {
            Outcome::Answered { head, body } => (head, body, sent.ctx),
            Outcome::Forwarded => panic!("the proxy must answer, not forward"),
        }
    }

    fn forwarded(sent: Sent) -> ProxyContext {
        match sent.outcome {
            Outcome::Forwarded => sent.ctx,
            Outcome::Answered { head, .. } => panic!("the request must be forwarded: {head}"),
        }
    }

    fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    /// The `name=value` pair of the `Set-Cookie` header in `head`.
    fn set_cookie_pair(head: &str) -> String {
        header(head, "set-cookie")
            .and_then(|cookie| cookie.split(';').next())
            .unwrap_or_default()
            .to_string()
    }

    /// `data-<name>` of the challenge page.
    fn page_data(body: &str, name: &str) -> String {
        body.split(&format!("data-{name}=\""))
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_string()
    }

    /// The route of a new project created with `domain`, after `configure`
    /// sets its Shield settings.
    async fn route(
        h: &Harness,
        domain: &str,
        configure: impl FnOnce(&mut projects::Model, &mut environments::Model),
    ) -> Result<(temps_routes::RouteInfo, i32, i32)> {
        let ops = crate::test_utils::TestDBMockOperations::new(h._db.connection_arc().clone())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let (mut project, mut environment, deployment) = ops
            .create_test_project_with_domain(domain)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        configure(&mut project, &mut environment);
        let (project_id, env_id) = (project.id, environment.id);
        Ok((
            temps_routes::RouteInfo {
                backend: temps_routes::BackendType::Upstream {
                    backends: vec![temps_routes::BackendEntry {
                        address: "127.0.0.1:9".to_string(),
                        container_id: None,
                        container_name: None,
                    }],
                    round_robin_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                },
                redirect_to: None,
                status_code: None,
                project: Some(Arc::new(project)),
                environment: Some(Arc::new(environment)),
                deployment: Some(Arc::new(deployment)),
                cert_eligible: false,
            },
            env_id,
            project_id,
        ))
    }

    /// `HOST` served by a new project in `mode`; returns its environment
    /// and project ids.
    async fn host_in(h: &Harness, mode: &str) -> Result<(i32, i32)> {
        host_with(h, |project, _| project.shield_mode = mode.to_string()).await
    }

    async fn host_with(
        h: &Harness,
        configure: impl FnOnce(&mut projects::Model, &mut environments::Model),
    ) -> Result<(i32, i32)> {
        let (route, env_id, project_id) = route(h, HOST, configure).await?;
        h.route_table.insert_route_for_test(HOST, route);
        Ok((env_id, project_id))
    }

    fn solve(token: &str, difficulty: u8) -> String {
        let prefix = Sha256::new_with_prefix(token.as_bytes());
        (0u64..)
            .map(|nonce| nonce.to_string())
            .find(|nonce| {
                let mut hash = prefix.clone();
                hash.update(nonce.as_bytes());
                temps_shield::has_leading_zero_bits(&hash.finalize(), u32::from(difficulty))
            })
            .unwrap_or_default()
    }

    /// The verify body for the challenge page in `body`, solved.
    fn solution(body: &str) -> String {
        let token = page_data(body, "token");
        let difficulty: u8 = page_data(body, "difficulty").parse().unwrap_or(0);
        format!(
            r#"{{"token":"{token}","nonce":"{}","solve_ms":700}}"#,
            solve(&token, difficulty)
        )
    }

    #[tokio::test]
    async fn log_mode_records_the_verdict_and_serves_the_request() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        let (env_id, _) = host_in(&s.h, "log").await?;

        let ctx = forwarded(send(&s.h, page("/", None), BLOCKED, true).await?);
        assert_eq!(ctx.shield_verdict().as_deref(), Some("block"));
        assert_eq!(ctx.shield_reasons(), ["spamhaus_drop (185.93.89.0/24)"]);
        assert_eq!(ctx.shield_header, None, "log mode never answers");
        assert_eq!(ctx.shield_env, Some(env_id));
        let events = s.events.take();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EventKind::Verdict);
        assert_eq!(
            (events[0].verdict.as_str(), events[0].mode.as_str()),
            ("block", "log")
        );
        assert_eq!(events[0].environment_id, env_id);
        assert_eq!(events[0].ip, BLOCKED);
        assert_eq!(events[0].request_id, ctx.request_id);

        // A clean visitor is no event.
        let ctx = forwarded(send(&s.h, page("/", None), CLEAN, true).await?);
        assert_eq!(ctx.shield_verdict(), None);
        assert!(s.events.take().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn off_mode_neither_acts_nor_records() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        host_in(&s.h, "off").await?;
        let ctx = forwarded(send(&s.h, page("/", None), BLOCKED, true).await?);
        assert_eq!(ctx.shield_verdict(), None);
        assert_eq!(ctx.shield_env, None);
        assert!(s.events.take().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn selective_blocks_a_listed_address_without_saying_why() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        host_in(&s.h, "selective").await?;

        let (head, body, ctx) = answered(send(&s.h, page("/", None), BLOCKED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(
            header(&head, "x-temps-shield"),
            Some(format!("block; ref={}", ctx.request_id).as_str())
        );
        assert_eq!(body, "Forbidden\n");
        assert!(!head.contains("spamhaus") && !body.contains("spamhaus"));
        assert_eq!(ctx.routing_status, "shield_block");
        assert_eq!(ctx.shield_verdict().as_deref(), Some("block"));
        let events = s.events.take();
        assert_eq!(events.len(), 1);
        assert_eq!(
            (events[0].verdict.as_str(), events[0].mode.as_str()),
            ("block", "selective")
        );

        // A clean visitor passes, with no header and no event.
        let ctx = forwarded(send(&s.h, page("/", None), CLEAN, true).await?);
        assert_eq!(ctx.shield_header, None);
        assert!(s.events.take().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_challenge_is_a_page_for_page_loads_and_a_429_for_the_rest() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        host_in(&s.h, "selective").await?;

        let (head, body, ctx) = answered(send(&s.h, page("/", None), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(header(&head, "cache-control"), Some("no-store"));
        assert_eq!(
            header(&head, "x-temps-shield"),
            Some(format!("challenge; ref={}", ctx.request_id).as_str())
        );
        assert_eq!(page_data(&body, "token").len(), 80, "{body}");
        assert_eq!(page_data(&body, "difficulty"), "18");
        assert!(!body.contains("dshield"));
        let events = s.events.take();
        assert_eq!(events[0].kind, EventKind::ChallengeIssued);
        assert_eq!(events[0].difficulty, Some(18));

        // An XHR and a form POST cannot show a page.
        for raw in [
            request("GET", "/data", "application/json", ""),
            request("POST", "/form", "text/html", ""),
        ] {
            let (head, body, ctx) = answered(send(&s.h, raw, CHALLENGED, true).await?);
            assert!(head.starts_with("HTTP/1.1 429"), "{head}");
            assert_eq!(header(&head, "retry-after"), Some("60"));
            assert_eq!(
                header(&head, "content-type"),
                Some("application/problem+json")
            );
            assert_eq!(
                header(&head, "x-temps-shield"),
                Some(format!("challenge; ref={}", ctx.request_id).as_str())
            );
            assert!(body.contains(r#""status":429"#), "{body}");
            assert!(!body.contains("dshield"));
        }
        assert_eq!(s.events.take().len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_challenge_over_plain_http_asks_for_tls() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        host_in(&s.h, "selective").await?;
        let (head, _, ctx) = answered(send(&s.h, page("/", None), CHALLENGED, false).await?);
        assert!(head.starts_with("HTTP/1.1 426"), "{head}");
        assert_eq!(header(&head, "upgrade"), Some("TLS/1.2, HTTP/1.1"));
        assert_eq!(
            header(&head, "x-temps-shield"),
            Some(format!("challenge; ref={}", ctx.request_id).as_str())
        );
        // Plain HTTP that Shield does not challenge is served.
        forwarded(send(&s.h, page("/", None), CLEAN, false).await?);
        Ok(())
    }

    #[tokio::test]
    async fn an_exempt_path_skips_a_challenge_but_not_a_block() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        host_with(&s.h, |project, environment| {
            project.shield_mode = "selective".to_string();
            environment.shield_exempt_paths =
                environments::ShieldExemptPaths(vec!["/api/*".to_string()]);
        })
        .await?;

        let ctx = forwarded(
            send(
                &s.h,
                request("POST", "/api/users", "application/json", ""),
                CHALLENGED,
                true,
            )
            .await?,
        );
        // Still recorded with the request log.
        assert_eq!(ctx.shield_verdict().as_deref(), Some("challenge"));
        assert_eq!(ctx.shield_header, None);
        let (head, _, _) = answered(send(&s.h, page("/", None), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        let (head, body, _) = answered(
            send(
                &s.h,
                request("GET", "/api/users", "application/json", ""),
                BLOCKED,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(body, "Forbidden\n");
        Ok(())
    }

    /// The token row's id, after storing the hash of a new token for it.
    async fn create_token(
        h: &Harness,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<(i64, String)> {
        let db = h._db.connection();
        let row = shield_bypass_tokens::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            name: Set("uptime monitor".to_string()),
            token_hash: Set(String::new()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await?;
        let (token, hash) = bypass::generate(row.id);
        let mut row: shield_bypass_tokens::ActiveModel = row.into();
        row.token_hash = Set(bypass::to_hex(&hash));
        let row = row.update(db).await?;
        Ok((row.id, token))
    }

    #[tokio::test]
    async fn a_bypass_token_passes_even_a_block_and_records_its_use() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        let (_, project_id) = host_in(&s.h, "attack").await?;
        // A project-wide token applies to the environment.
        let (id, token) = create_token(&s.h, project_id, None).await?;
        s.tokens.spawn(
            s.h._db.connection_arc().clone(),
            &tokio::runtime::Handle::current(),
        );
        s.tokens.refresh(s.h._db.connection()).await?;

        let with_token = |accept: &str, token: &str| {
            request(
                "GET",
                "/",
                accept,
                &format!("X-Temps-Shield-Token: {token}\r\n"),
            )
        };
        let ctx = forwarded(send(&s.h, with_token("text/html", &token), BLOCKED, true).await?);
        assert_eq!(ctx.shield_verdict().as_deref(), Some("block"));
        assert_eq!(ctx.shield_header, None);
        // In attack mode a token also skips the challenge everyone gets.
        forwarded(send(&s.h, with_token("application/json", &token), CLEAN, true).await?);

        // Someone else's token, or a forged one, is just a header.
        let (forged, _) = bypass::generate(id);
        let (head, _, _) =
            answered(send(&s.h, with_token("text/html", &forged), BLOCKED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");

        // The use reaches the row, off the request path.
        let mut last_used = None;
        for _ in 0..50 {
            last_used = shield_bypass_tokens::Entity::find_by_id(id)
                .one(s.h._db.connection())
                .await?
                .and_then(|row| row.last_used_at);
            if last_used.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(last_used.is_some(), "last_used_at was never recorded");
        Ok(())
    }

    #[tokio::test]
    async fn a_solved_challenge_clears_the_visitor_who_solved_it() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        let (env_id, _) = host_in(&s.h, "selective").await?;

        // 1. No clearance: the challenge page.
        let (head, body, _) = answered(send(&s.h, page("/", None), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        let solved = solution(&body);
        s.events.take();

        // 2. Regression: the old body chose its own challenge, environment
        //    and identifier, and any solution of it cleared the visitor.
        let forged = format!(
            r#"{{"challenge":"aa","nonce":"{}","environment_id":{env_id},"identifier":"x","identifier_type":"ja4"}}"#,
            solve("aa", 1)
        );
        let (head, _, _) = answered(send(&s.h, post_verify(UA, &forged), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 400"), "{head}");
        assert!(!head.contains("temps_clr"), "{head}");

        // 3. The served token, unsolved, earns nothing, and is recorded.
        let token = page_data(&body, "token");
        let unsolved = format!(r#"{{"token":"{token}","nonce":"x"}}"#);
        let (head, body_out, _) =
            answered(send(&s.h, post_verify(UA, &unsolved), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert!(body_out.contains("unsolved"), "{body_out}");
        let failed = s.events.take();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].kind, EventKind::ChallengeFailed);
        assert_eq!(failed[0].reasons, ["unsolved"]);

        // 4. The solution, posted by another visitor (here another
        //    User-Agent), clears no one: the visitor comes from the
        //    connection, not from anything in the body.
        let (head, body_out, _) =
            answered(send(&s.h, post_verify("curl/8.5.0", &solved), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert!(body_out.contains("invalid"), "{body_out}");
        s.events.take();

        // 5. Posted by the visitor it was served to, it sets the clearance.
        let (head, _, _) = answered(send(&s.h, post_verify(UA, &solved), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(
            head.contains("Secure") && head.contains("HttpOnly"),
            "{head}"
        );
        let pair = set_cookie_pair(&head);
        assert!(
            pair.starts_with(&format!("__Host-temps_clr_{env_id}=")),
            "{head}"
        );
        let solved_events = s.events.take();
        assert_eq!(solved_events.len(), 1);
        assert_eq!(solved_events[0].kind, EventKind::ChallengeSolved);
        assert_eq!(solved_events[0].solve_ms, Some(700));
        assert_eq!(solved_events[0].environment_id, env_id);

        // 6. The same token cannot be redeemed twice.
        let (head, _, _) = answered(send(&s.h, post_verify(UA, &solved), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");

        // 7. The clearance admits this visitor, and only this visitor.
        let ctx = forwarded(send(&s.h, page("/", Some(&pair)), CHALLENGED, true).await?);
        assert_eq!(ctx.shield_header, None);
        let other = request_as(
            "GET",
            "/",
            "text/html",
            "curl/8.5.0",
            &format!("Cookie: {pair}\r\n"),
        );
        let (head, _, _) = answered(send(&s.h, other, CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        // A clearance never admits a block.
        let (head, body_out, _) =
            answered(send(&s.h, page("/", Some(&pair)), BLOCKED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(body_out, "Forbidden\n");
        Ok(())
    }

    #[tokio::test]
    async fn attack_mode_challenges_every_visitor() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        // The legacy column still means attack.
        host_with(&s.h, |project, _| project.attack_mode = true).await?;
        let (head, body, _) = answered(send(&s.h, page("/", None), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(page_data(&body, "difficulty"), "20");
        let events = s.events.take();
        assert_eq!(events[0].kind, EventKind::ChallengeIssued);
        assert_eq!(
            (events[0].verdict.as_str(), events[0].mode.as_str()),
            ("pass", "attack")
        );
        // The solver and ACME HTTP-01 are never challenged.
        let (head, _, _) = answered(
            send(
                &s.h,
                request("GET", temps_shield::assets::SOLVER_WASM_PATH, "*/*", ""),
                CLEAN,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(header(&head, "content-type"), Some("application/wasm"));
        assert!(header(&head, "etag").is_some(), "{head}");
        // A challenge stored for this host is answered over plain HTTP.
        {
            use sea_orm::{ActiveModelTrait, Set};
            let now = chrono::Utc::now();
            temps_entities::domains::ActiveModel {
                domain: Set(HOST.to_string()),
                status: Set("pending".to_string()),
                http_challenge_token: Set(Some("stored-token".to_string())),
                http_challenge_key_authorization: Set(Some("stored-token.thumbprint".to_string())),
                is_wildcard: Set(false),
                verification_method: Set("http-01".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(s.h._db.connection_arc().as_ref())
            .await?;
        }
        let acme = request("GET", "/.well-known/acme-challenge/stored-token", "*/*", "");
        let (head, body, _) = answered(send(&s.h, acme, CLEAN, false).await?);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body, "stored-token.thumbprint");
        assert!(header(&head, "x-temps-shield").is_none(), "{head}");
        // Any other path under the prefix is no way past Shield.
        let probe = request("GET", "/.well-known/acme-challenge/missing", "*/*", "");
        let (head, _, _) = answered(send(&s.h, probe, CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 429"), "{head}");
        Ok(())
    }

    /// Path routes (ADR 050): the host's environment at `/` and another at
    /// `/docs`. The page under `/docs` posts to the one verify path, which
    /// resolves to the host's environment; the clearance still goes to the
    /// environment the token was issued for, and each environment keeps its
    /// own cookie.
    #[tokio::test]
    async fn path_routed_environments_keep_their_own_clearance() -> Result<()> {
        let Some(s) = shielded_harness().await? else {
            return Ok(());
        };
        let (host_env, _) = host_in(&s.h, "selective").await?;
        let (docs_route, docs_env, _) = route(&s.h, "docs-target.example.com", |project, _| {
            project.shield_mode = "selective".to_string()
        })
        .await?;
        s.h.route_table.insert_path_routes_for_test(
            host_env,
            vec![temps_routes::PathRoute {
                prefix: "/docs".to_string(),
                strip_prefix: false,
                target_service: "docs".to_string(),
                target: Some(docs_route),
                sleeping_environment_id: None,
            }],
        );

        let (_, docs_page, _) = answered(send(&s.h, page("/docs/", None), CHALLENGED, true).await?);
        let (_, host_page, _) = answered(send(&s.h, page("/", None), CHALLENGED, true).await?);

        let (head, _, _) = answered(
            send(
                &s.h,
                post_verify(UA, &solution(&docs_page)),
                CHALLENGED,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let docs_pair = set_cookie_pair(&head);
        assert!(
            docs_pair.starts_with(&format!("__Host-temps_clr_{docs_env}=")),
            "{head}"
        );
        let (head, _, _) = answered(
            send(
                &s.h,
                post_verify(UA, &solution(&host_page)),
                CHALLENGED,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let host_pair = set_cookie_pair(&head);
        assert!(
            host_pair.starts_with(&format!("__Host-temps_clr_{host_env}=")),
            "{head}"
        );

        // Each cookie admits its own environment only; together, both.
        let both = format!("{host_pair}; {docs_pair}");
        forwarded(send(&s.h, page("/docs/", Some(&both)), CHALLENGED, true).await?);
        forwarded(send(&s.h, page("/", Some(&both)), CHALLENGED, true).await?);
        let (head, _, _) =
            answered(send(&s.h, page("/docs/", Some(&host_pair)), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        let (head, _, _) =
            answered(send(&s.h, page("/", Some(&docs_pair)), CHALLENGED, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        Ok(())
    }

    // ---- project edge rules (ADR-089) ---------------------------------

    /// `HOST` served by a project with Shield off and the edge rules in
    /// `rules` (in order), enforced by a gate loaded from the database.
    /// Returns the harness, the project id and the environment id.
    async fn edge_harness(rules: &[serde_json::Value]) -> Result<Option<(Harness, i32, i32)>> {
        edge_harness_with(rules, |_| {}).await
    }

    async fn edge_harness_with(
        rules: &[serde_json::Value],
        configure: impl FnOnce(&mut environments::Model),
    ) -> Result<Option<(Harness, i32, i32)>> {
        let Some(s) = shielded_harness().await? else {
            return Ok(None);
        };
        let (env_id, project_id) = host_with(&s.h, |project, environment| {
            project.shield_mode = "off".to_string();
            configure(environment);
        })
        .await?;
        let db = s.h._db.connection_arc().clone();
        let service = crate::service::edge_rule_service::EdgeRuleService::new(db.clone());
        for rule in rules {
            // The API creates log-only rules by default; these tests enforce
            // unless a rule says otherwise.
            let mut rule = rule.clone();
            if let Some(object) = rule.as_object_mut() {
                object
                    .entry("mode")
                    .or_insert_with(|| serde_json::json!("enforce"));
            }
            let request = serde_json::from_value(rule)?;
            service
                .create(project_id, request, None)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        let gate = Arc::new(crate::edge_rules::EdgeRuleGate::new(db));
        gate.hydrate().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let Harness {
            _db,
            lb,
            route_table,
        } = s.h;
        let lb = lb.with_edge_rules(gate, None);
        Ok(Some((
            Harness {
                _db,
                lb,
                route_table,
            },
            project_id,
            env_id,
        )))
    }

    fn path_rule(op: &str, path: &str, action: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "expression": {"combinator": "all", "children": [
                {"kind": "condition", "field": "path", "op": op, "values": [path], "case_insensitive": true}
            ]},
            "action": action
        })
    }

    #[tokio::test]
    async fn edge_rules_block_redirect_and_allow_in_order() -> Result<()> {
        let Some((h, _, _)) = edge_harness(&[
            path_rule("eq", "/admin/health", serde_json::json!({"type": "allow"})),
            path_rule(
                "starts_with",
                "/admin",
                serde_json::json!({"type": "block", "status": 404, "body": "Nothing here"}),
            ),
            path_rule(
                "eq",
                "/old",
                serde_json::json!({"type": "redirect", "target": "/new?{query}", "status": 308}),
            ),
        ])
        .await?
        else {
            return Ok(());
        };

        // Paths ignore case by default.
        let (head, body, ctx) =
            answered(send(&h, request("GET", "/ADMIN/users", "*/*", ""), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 404"), "{head}");
        assert_eq!(body, "Nothing here");
        assert_eq!(header(&head, "cache-control"), Some("no-store"));
        assert_eq!(ctx.routing_status, "edge_rule_blocked");

        // An earlier allow rule skips the block.
        forwarded(send(&h, request("GET", "/admin/health", "*/*", ""), CLEAN, true).await?);

        let (head, _, ctx) =
            answered(send(&h, request("GET", "/old?page=2", "*/*", ""), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 308"), "{head}");
        assert_eq!(header(&head, "location"), Some("/new?page=2"));
        assert_eq!(ctx.routing_status, "edge_rule_redirected");

        // Unmatched requests continue.
        forwarded(send(&h, request("GET", "/", "*/*", ""), CLEAN, true).await?);
        // An encoded slash cannot dodge a prefix rule.
        let (head, _, _) =
            answered(send(&h, request("GET", "/%61dmin/users", "*/*", ""), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 404"), "{head}");
        Ok(())
    }

    #[tokio::test]
    async fn edge_rules_never_touch_acme_or_the_challenge_endpoints() -> Result<()> {
        let Some((h, _, _)) = edge_harness(&[serde_json::json!({
            "expression": {"combinator": "all", "children": []},
            "action": {"type": "block", "status": 403}
        })])
        .await?
        else {
            return Ok(());
        };
        // Every request matches the rule...
        let (head, _, _) = answered(send(&h, request("GET", "/", "*/*", ""), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        // ...except the solver a challenge page loads and ACME HTTP-01.
        let (head, _, _) = answered(
            send(
                &h,
                request("GET", temps_shield::assets::SOLVER_WASM_PATH, "*/*", ""),
                CLEAN,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let probe = request("GET", "/.well-known/acme-challenge/missing", "*/*", "");
        let outcome = send(&h, probe, CLEAN, false).await?;
        if let Outcome::Answered { head, .. } = &outcome.outcome {
            assert!(!head.starts_with("HTTP/1.1 403"), "{head}");
        }
        // An exempt prefix cannot carry another path past the rules.
        let smuggled = request("GET", "/.well-known/acme-challenge/../../admin", "*/*", "");
        let (head, _, _) = answered(send(&h, smuggled, CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        // Only the solver files under `/api/__temps/`; the rest is the app's.
        let (head, _, _) = answered(
            send(
                &h,
                request("GET", "/api/__temps/private", "*/*", ""),
                CLEAN,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        // Nor is the rest of `/api/_temps/` exempt: a project that serves its
        // own Temps API receives it, and its rules must apply there.
        let (head, _, ctx) = answered(
            send(
                &h,
                request("POST", "/api/_temps/event", "*/*", ""),
                CLEAN,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(ctx.routing_status, "edge_rule_blocked");
        // Without a password wall the password form's path is the app's too.
        let verify = crate::handler::password_wall::PASSWORD_VERIFY_PATH;
        let (head, _, ctx) =
            answered(send(&h, request("POST", verify, "*/*", ""), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(ctx.routing_status, "edge_rule_blocked");
        Ok(())
    }

    #[tokio::test]
    async fn edge_rules_leave_the_password_form_to_the_password_wall() -> Result<()> {
        let Some((h, _, _)) = edge_harness_with(
            &[serde_json::json!({
                "expression": {"combinator": "all", "children": [
                    {"kind": "condition", "field": "method", "op": "eq", "values": ["POST"]}
                ]},
                "action": {"type": "block", "status": 403}
            })],
            |environment| {
                environment.deployment_config =
                    Some(temps_entities::deployment_config::DeploymentConfig {
                        security: Some(temps_entities::deployment_config::SecurityConfig {
                            password_protection: Some(
                                temps_entities::deployment_config::PasswordProtectionConfig {
                                    enabled: true,
                                    password_hash: "not-a-real-hash".to_string(),
                                },
                            ),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
            },
        )
        .await?
        else {
            return Ok(());
        };
        let verify = crate::handler::password_wall::PASSWORD_VERIFY_PATH;
        // The wall answers its own form, wrong password and all.
        let (head, _, ctx) =
            answered(send(&h, request("POST", verify, "*/*", ""), CLEAN, true).await?);
        assert!(!head.starts_with("HTTP/1.1 403"), "{head}");
        assert_ne!(ctx.routing_status, "edge_rule_blocked");
        // Every other POST is still the rule's.
        let (head, _, ctx) =
            answered(send(&h, request("POST", "/form", "*/*", ""), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(ctx.routing_status, "edge_rule_blocked");
        // The exemption is the exact path, not a prefix.
        let (head, _, _) = answered(
            send(
                &h,
                request("POST", &format!("{verify}/x"), "*/*", ""),
                CLEAN,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        Ok(())
    }

    #[tokio::test]
    async fn a_challenge_rule_serves_shields_challenge_until_the_visitor_solves_it() -> Result<()> {
        let Some((h, _, env_id)) = edge_harness(&[path_rule(
            "starts_with",
            "/login",
            serde_json::json!({"type": "challenge"}),
        )])
        .await?
        else {
            return Ok(());
        };
        forwarded(send(&h, page("/", None), CLEAN, true).await?);

        // A page load gets the challenge page; an API call a 429; plain
        // HTTP a 426, since no clearance can be set there.
        let (head, body, ctx) = answered(send(&h, page("/login", None), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert_eq!(ctx.routing_status, "edge_rule_challenged");
        let (head, _, _) = answered(
            send(
                &h,
                request("POST", "/login", "application/json", ""),
                CLEAN,
                true,
            )
            .await?,
        );
        assert!(head.starts_with("HTTP/1.1 429"), "{head}");
        let (head, _, _) = answered(send(&h, page("/login", None), CLEAN, false).await?);
        assert!(head.starts_with("HTTP/1.1 426"), "{head}");

        // Solving it clears this visitor for the environment.
        let (head, _, _) =
            answered(send(&h, post_verify(UA, &solution(&body)), CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let pair = set_cookie_pair(&head);
        assert!(
            pair.starts_with(&format!("__Host-temps_clr_{env_id}=")),
            "{head}"
        );
        forwarded(send(&h, page("/login", Some(&pair)), CLEAN, true).await?);
        // Another visitor with the same cookie is still challenged.
        let other = request_as(
            "GET",
            "/login",
            "text/html",
            "curl/8.5.0",
            &format!("Cookie: {pair}\r\n"),
        );
        let (head, _, _) = answered(send(&h, other, CLEAN, true).await?);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        Ok(())
    }

    #[tokio::test]
    async fn a_log_only_rule_counts_matches_without_acting() -> Result<()> {
        let mut rule = path_rule(
            "starts_with",
            "/admin",
            serde_json::json!({"type": "block", "status": 403}),
        );
        rule["mode"] = serde_json::json!("log_only");
        let Some((h, project_id, _)) = edge_harness(&[rule]).await? else {
            return Ok(());
        };
        forwarded(send(&h, request("GET", "/admin", "*/*", ""), CLEAN, true).await?);
        forwarded(send(&h, request("GET", "/admin/x", "*/*", ""), CLEAN, true).await?);
        let gate =
            h.lb.edge_rules
                .clone()
                .ok_or_else(|| anyhow::anyhow!("gate installed"))?;
        gate.flush_hits()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let service =
            crate::service::edge_rule_service::EdgeRuleService::new(h._db.connection_arc().clone());
        let listed = service
            .list(project_id)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        assert_eq!(listed.rules[0].hits, 2);
        Ok(())
    }
}

#[cfg(test)]
mod client_fingerprint_tests {
    use super::resolve_client_fingerprint;
    use axum::http::{HeaderMap, HeaderName, HeaderValue};
    use std::net::IpAddr;
    use temps_shield::ja4::{Ja4, Ja4Source};

    const SPEC_JA4: &str = "t13d1516h2_8daaf6152771_e5627efa2ab1";
    const CURL_JA4: &str = "t13d3012h2_1d37bd780c83_882d495ac381";

    fn ja4(text: &str) -> Ja4 {
        Ja4::parse_forwarded(text).expect("test JA4 must be valid")
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("test address must be valid")
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn resolve(
        peer: &str,
        pairs: &[(&'static str, &'static str)],
        trust_loopback: bool,
    ) -> (Option<String>, Ja4Source) {
        let local = HeaderName::from_static("x-ja4");
        let fingerprint = resolve_client_fingerprint(
            Some(ip(peer)),
            &headers(pairs),
            trust_loopback,
            Some(&local),
            Some(ja4(CURL_JA4)),
        );
        (fingerprint.ja4.map(|j| j.to_string()), fingerprint.source)
    }

    /// Behind Cloudflare the TLS client is Cloudflare, so its `cf-ja4` is
    /// the visitor's and the computed one is not; a malformed header gives
    /// no JA4 rather than Cloudflare's own.
    #[test]
    fn cloudflare_peers_supply_the_ja4() {
        assert_eq!(
            resolve("173.245.48.1", &[("cf-ja4", SPEC_JA4)], false),
            (Some(SPEC_JA4.into()), Ja4Source::Cloudflare)
        );
        assert_eq!(
            resolve("173.245.48.1", &[("cf-ja4", "not a ja4")], false),
            (None, Ja4Source::Cloudflare)
        );
        assert_eq!(
            resolve("173.245.48.1", &[], false),
            (None, Ja4Source::Cloudflare)
        );
    }

    #[test]
    fn bunny_peers_supply_the_ja4() {
        assert_eq!(
            resolve("185.152.66.1", &[("cdn-ja4", SPEC_JA4)], false),
            (Some(SPEC_JA4.into()), Ja4Source::Bunny)
        );
        // Bunny's peers are not trusted for Cloudflare's header.
        assert_eq!(
            resolve("185.152.66.1", &[("cf-ja4", SPEC_JA4)], false),
            (None, Ja4Source::Bunny)
        );
    }

    /// Anyone else is the TLS client: the computed JA4 counts, and the
    /// headers they wrote do not.
    #[test]
    fn direct_clients_get_the_computed_ja4() {
        assert_eq!(
            resolve(
                "8.8.8.8",
                &[("cf-ja4", SPEC_JA4), ("cdn-ja4", SPEC_JA4)],
                false
            ),
            (Some(CURL_JA4.into()), Ja4Source::Direct)
        );
        assert_eq!(
            resolve_client_fingerprint(None, &HeaderMap::new(), false, None, None).source,
            Ja4Source::Direct
        );
    }

    /// A trusted local proxy terminated the visitor's TLS, so the computed
    /// JA4 is the proxy's: only the configured header counts, and without
    /// it none is known. Untrusted loopback is direct.
    #[test]
    fn trusted_loopback_peers_supply_the_configured_header() {
        assert_eq!(
            resolve(
                "127.0.0.1",
                &[("x-forwarded-for", "8.8.8.8"), ("x-ja4", SPEC_JA4)],
                true
            ),
            (Some(SPEC_JA4.into()), Ja4Source::Local)
        );
        assert_eq!(
            resolve("127.0.0.1", &[("x-forwarded-for", "8.8.8.8")], true),
            (None, Ja4Source::Local)
        );
        // Another proxy's header is not the configured one.
        assert_eq!(
            resolve("127.0.0.1", &[("cf-ja4", SPEC_JA4)], true),
            (None, Ja4Source::Local)
        );
        let unconfigured = resolve_client_fingerprint(
            Some(ip("127.0.0.1")),
            &headers(&[("x-ja4", SPEC_JA4)]),
            true,
            None,
            Some(ja4(CURL_JA4)),
        );
        assert_eq!(
            (unconfigured.ja4, unconfigured.source),
            (None, Ja4Source::Local)
        );
        assert_eq!(
            resolve("127.0.0.1", &[("x-ja4", SPEC_JA4)], false),
            (Some(CURL_JA4.into()), Ja4Source::Direct)
        );
    }
}
