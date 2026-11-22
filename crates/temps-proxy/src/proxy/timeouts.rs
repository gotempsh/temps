// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Upstream I/O timeouts: the console's fixed timeout and the configurable customer-traffic timeouts.

/// Selects the upstream read/write/idle timeout for a proxied request.
/// `default_timeout` is the caller's already-computed websocket-aware value
/// (3600s for websocket upgrades, 60s otherwise); this only widens it
/// further, to [`CONSOLE_IO_TIMEOUT_SECS`], for non-websocket traffic bound
/// for the console address — long-running admin operations (e.g.
/// triggering an import) routinely exceed the 60s hot-path bound tuned for
/// customer-app traffic. See the call site in `LoadBalancer::upstream_peer`
/// for the full rationale.
///
/// [`CONSOLE_IO_TIMEOUT_SECS`] must cover the real worst case of the
/// slowest known console operation (import execute), not just look
/// generous: `POST /imports/execute` runs service creation, then every
/// created service's data transfer concurrently (each individually bounded
/// by `temps_import::resource_executor::TRANSFER_TIMEOUT` = 1800s), then
/// deploy-and-verify (`temps_import::deployment_verifier`'s
/// `TRIGGER_GRACE`(15s) + `DEPLOY_TIMEOUT`(600s) + `HTTP_TIMEOUT`(90s) =
/// 705s) — all inside the one HTTP request the handler awaits directly. A
/// timeout shorter than `1800 + 705` would reintroduce, at a longer time
/// constant, the exact "import succeeds server-side, browser sees a dead
/// connection" bug this timeout extension exists to fix.
pub(super) const CONSOLE_IO_TIMEOUT_SECS: u64 = 3600;

/// The control-plane listener `peer_addr` belongs to, for timeout purposes.
///
/// The public-only listener (ADR-051) is the same console process as
/// `console_addr`, so a request routed there gets the console's fixed
/// timeout rather than whatever customer traffic is configured to use.
pub(super) fn control_plane_address<'a>(
    peer_addr: &str,
    console_addr: &'a str,
    ingress_addr: Option<&'a str>,
) -> &'a str {
    match ingress_addr {
        Some(ingress) if !ingress.is_empty() && peer_addr == ingress => ingress,
        _ => console_addr,
    }
}

/// Console/control-plane traffic always gets a fixed timeout, regardless of
/// what customer app traffic is configured to use — a `None` (no timeout)
/// resolved for customer traffic must never leak into the console path.
pub(super) fn upstream_io_timeout(
    peer_addr: &str,
    console_addr: &str,
    is_websocket: bool,
    default_timeout: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    let is_console = !console_addr.is_empty() && peer_addr == console_addr;
    if is_console && !is_websocket {
        Some(std::time::Duration::from_secs(CONSOLE_IO_TIMEOUT_SECS))
    } else {
        default_timeout
    }
}

/// Which of the three configurable timeout classes a request falls under.
/// SSE and WebSocket are long-lived by design and get their own idle-timeout
/// class distinct from regular HTTP — see `resolve_customer_io_timeout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeoutTrafficKind {
    Http,
    Sse,
    WebSocket,
}

/// Resolve the upstream I/O timeout for customer app traffic: pick the
/// override for this traffic kind from the merged project/environment
/// `DeploymentConfig` (falling back to the matching global default), then
/// clamp to the global hard ceiling — unless the resolved value is `0`
/// ("no timeout"), in which case `None` is returned and the ceiling never
/// applies. Timeouts are opt-in: an app with no override and a zero global
/// default (the platform default) gets an unbounded connection, exactly as
/// it did before this setting existed. `0` can also be set explicitly as a
/// project/environment override, to force "no timeout" even when the
/// operator has configured a nonzero global default.
///
/// Pure function so the classify+clamp logic is unit-testable without a full
/// Pingora session — mirrors `upstream_io_timeout` above, which stays
/// separate and untouched: it governs the fixed console/control-plane
/// timeout, not customer app traffic.
pub(super) fn resolve_customer_io_timeout(
    kind: TimeoutTrafficKind,
    effective_config: &temps_entities::deployment_config::DeploymentConfig,
    request_timeouts: &temps_config::RequestTimeoutSettings,
) -> Option<std::time::Duration> {
    let (override_seconds, default_seconds) = match kind {
        TimeoutTrafficKind::Http => (
            effective_config.request_timeout_seconds,
            request_timeouts.default_http_timeout_seconds,
        ),
        TimeoutTrafficKind::Sse => (
            effective_config.sse_idle_timeout_seconds,
            request_timeouts.default_sse_idle_timeout_seconds,
        ),
        TimeoutTrafficKind::WebSocket => (
            effective_config.websocket_idle_timeout_seconds,
            request_timeouts.default_websocket_idle_timeout_seconds,
        ),
    };
    let resolved = override_seconds
        .and_then(|secs| u32::try_from(secs).ok())
        .unwrap_or(default_seconds);
    if resolved == 0 {
        return None;
    }
    Some(std::time::Duration::from_secs(
        request_timeouts.clamp_to_ceiling(resolved) as u64,
    ))
}

#[cfg(test)]
mod resolve_customer_io_timeout_tests {
    use super::*;
    use temps_config::RequestTimeoutSettings;
    use temps_entities::deployment_config::DeploymentConfig;

    #[test]
    fn no_timeout_by_default_when_nothing_is_configured() {
        // The platform default: an app with no project/environment override
        // and no operator-configured global default gets an unbounded
        // connection for every traffic kind, exactly as it did before this
        // setting existed.
        let config = DeploymentConfig::default();
        let settings = RequestTimeoutSettings::default();

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Http, &config, &settings),
            None
        );
        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Sse, &config, &settings),
            None
        );
        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::WebSocket, &config, &settings),
            None
        );
    }

    #[test]
    fn falls_back_to_a_nonzero_global_default_per_kind_when_unconfigured() {
        // Once an operator opts the platform into default timeouts, SSE must
        // get its own idle-timeout class rather than silently falling
        // through to the HTTP default (the RST-on-idle bug this feature
        // fixes).
        let config = DeploymentConfig::default();
        let settings = RequestTimeoutSettings {
            max_request_timeout_seconds: 3600,
            default_http_timeout_seconds: 60,
            default_sse_idle_timeout_seconds: 3600,
            default_websocket_idle_timeout_seconds: 1800,
        };

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Http, &config, &settings),
            Some(std::time::Duration::from_secs(60))
        );
        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Sse, &config, &settings),
            Some(std::time::Duration::from_secs(3600))
        );
        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::WebSocket, &config, &settings),
            Some(std::time::Duration::from_secs(1800))
        );
    }

    #[test]
    fn project_or_environment_override_wins_when_below_ceiling() {
        let config = DeploymentConfig {
            request_timeout_seconds: Some(10),
            sse_idle_timeout_seconds: Some(120),
            websocket_idle_timeout_seconds: Some(90),
            ..Default::default()
        };
        let settings = RequestTimeoutSettings::default();

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Http, &config, &settings),
            Some(std::time::Duration::from_secs(10))
        );
        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Sse, &config, &settings),
            Some(std::time::Duration::from_secs(120))
        );
        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::WebSocket, &config, &settings),
            Some(std::time::Duration::from_secs(90))
        );
    }

    #[test]
    fn explicit_zero_override_forces_no_timeout_even_with_a_nonzero_global_default() {
        // An operator can opt the whole platform into a default timeout, and
        // a specific project can still opt back out with an explicit 0.
        let config = DeploymentConfig {
            request_timeout_seconds: Some(0),
            ..Default::default()
        };
        let settings = RequestTimeoutSettings {
            default_http_timeout_seconds: 60,
            ..RequestTimeoutSettings::default()
        };

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Http, &config, &settings),
            None
        );
    }

    #[test]
    fn global_hard_ceiling_always_wins_even_over_an_explicit_override() {
        let config = DeploymentConfig {
            request_timeout_seconds: Some(9000),
            ..Default::default()
        };
        let settings = RequestTimeoutSettings {
            max_request_timeout_seconds: 120,
            ..Default::default()
        };

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Http, &config, &settings),
            Some(std::time::Duration::from_secs(120)),
            "an operator-lowered ceiling must win even over a project's explicit override"
        );
    }

    /// Same guarantee as above, but for the SSE and WebSocket arms — each
    /// traffic kind reads a different `DeploymentConfig` field, so the ceiling
    /// clamp needs to be proven for all three, not just HTTP.
    #[test]
    fn global_hard_ceiling_wins_over_an_explicit_override_for_sse_and_websocket() {
        let config = DeploymentConfig {
            sse_idle_timeout_seconds: Some(9000),
            websocket_idle_timeout_seconds: Some(9000),
            ..Default::default()
        };
        let settings = RequestTimeoutSettings {
            max_request_timeout_seconds: 120,
            ..Default::default()
        };

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Sse, &config, &settings),
            Some(std::time::Duration::from_secs(120))
        );
        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::WebSocket, &config, &settings),
            Some(std::time::Duration::from_secs(120))
        );
    }

    #[test]
    fn global_hard_ceiling_also_clamps_a_nonzero_unconfigured_default() {
        let config = DeploymentConfig::default();
        let settings = RequestTimeoutSettings {
            max_request_timeout_seconds: 30,
            default_websocket_idle_timeout_seconds: 9000,
            ..RequestTimeoutSettings::default()
        };

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::WebSocket, &config, &settings),
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn ceiling_has_no_effect_when_nothing_resolves_to_a_timeout() {
        // The ceiling only constrains a timeout that's actually configured —
        // it must never *create* one for traffic that has none.
        let config = DeploymentConfig::default();
        let settings = RequestTimeoutSettings {
            max_request_timeout_seconds: 30,
            ..RequestTimeoutSettings::default()
        };

        assert_eq!(
            resolve_customer_io_timeout(TimeoutTrafficKind::Http, &config, &settings),
            None
        );
    }
}

#[cfg(test)]
mod upstream_io_timeout_tests {
    use super::*;
    use std::time::Duration;

    /// Regression test: POST /api/imports/execute (and any other
    /// long-running console/control-plane call) used to be RST'd at the
    /// 60s hot-path default before the handler finished — the import would
    /// complete successfully server-side while the browser saw a 503 with
    /// no way to tell the user it actually worked.
    #[test]
    fn console_traffic_gets_the_extended_timeout() {
        let console = "10.0.0.5:8081";
        let timeout = upstream_io_timeout(console, console, false, Some(Duration::from_secs(60)));
        assert_eq!(timeout, Some(Duration::from_secs(CONSOLE_IO_TIMEOUT_SECS)));
    }

    /// Console traffic must always get a concrete timeout even when customer
    /// traffic elsewhere is resolving to "no timeout" — a `None` default
    /// must never leak the unbounded state onto the control plane.
    #[test]
    fn console_traffic_gets_the_extended_timeout_even_when_default_is_none() {
        let console = "10.0.0.5:8081";
        let timeout = upstream_io_timeout(console, console, false, None);
        assert_eq!(timeout, Some(Duration::from_secs(CONSOLE_IO_TIMEOUT_SECS)));
    }

    /// The console timeout must actually cover the real worst case of the
    /// slowest console operation (import execute), not just be "generous".
    ///
    /// This crate can't depend on `temps-import` (wrong direction --
    /// `temps-proxy` sits below it), so the four constants below are
    /// necessarily hardcoded copies, not references to the real ones. The
    /// authoritative check lives in
    /// `temps_import::services::resource_executor::tests::worst_case_execute_duration_fits_under_the_documented_console_timeout`,
    /// which owns all four real constants and fails at the source if they
    /// drift. If you change any of the four numbers below, update that test
    /// (and this one) too.
    #[test]
    fn console_timeout_covers_the_worst_case_import_execute_duration() {
        const TRIGGER_GRACE_SECS: u64 = 15;
        const DEPLOY_TIMEOUT_SECS: u64 = 600;
        const HTTP_TIMEOUT_SECS: u64 = 90;
        const TRANSFER_TIMEOUT_SECS: u64 = 30 * 60;

        let worst_case_execute_duration =
            TRANSFER_TIMEOUT_SECS + TRIGGER_GRACE_SECS + DEPLOY_TIMEOUT_SECS + HTTP_TIMEOUT_SECS;

        assert!(
            CONSOLE_IO_TIMEOUT_SECS > worst_case_execute_duration,
            "console timeout ({CONSOLE_IO_TIMEOUT_SECS}s) must exceed the worst-case import \
             execute duration ({worst_case_execute_duration}s) — service data transfers run \
             concurrently (see populate_services), so the worst case no longer scales with the \
             number of services, but it must still fit inside one timeout window"
        );
    }

    #[test]
    fn customer_app_traffic_keeps_the_hot_path_default() {
        let timeout = upstream_io_timeout(
            "10.0.0.9:9000",
            "10.0.0.5:8081",
            false,
            Some(Duration::from_secs(60)),
        );
        assert_eq!(timeout, Some(Duration::from_secs(60)));
    }

    /// The whole point of the opt-in default: customer traffic resolving to
    /// "no timeout" (`None`) must pass straight through unchanged.
    #[test]
    fn customer_app_traffic_with_no_timeout_configured_stays_unbounded() {
        let timeout = upstream_io_timeout("10.0.0.9:9000", "10.0.0.5:8081", false, None);
        assert_eq!(timeout, None);
    }

    #[test]
    fn websocket_upgrade_to_the_console_keeps_the_websocket_timeout() {
        // Console traffic never upgrades to websocket today, but the
        // extended console bound must never override the caller's own
        // websocket-specific timeout if that combination ever occurs. Uses
        // a value distinct from CONSOLE_IO_TIMEOUT_SECS so the assertion
        // can't pass by coincidence.
        let console = "10.0.0.5:8081";
        let timeout = upstream_io_timeout(console, console, true, Some(Duration::from_secs(7200)));
        assert_eq!(timeout, Some(Duration::from_secs(7200)));
    }

    #[test]
    fn empty_console_address_never_matches() {
        // The trait's default console_address() is "" for resolvers that
        // don't override it (test mocks) — must never accidentally match a
        // peer address and grant an unintended extended timeout.
        let timeout =
            upstream_io_timeout("10.0.0.9:9000", "", false, Some(Duration::from_secs(60)));
        assert_eq!(timeout, Some(Duration::from_secs(60)));
    }
}
