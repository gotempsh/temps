// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::config::*;
use crate::service::lb_service::LbService;
use crate::traits::*;
use async_trait::async_trait;
use pingora_core::{upstreams::peer::HttpPeer, Error, ErrorType, Result as PingoraResult};
use std::sync::Arc;
use temps_routes::CachedPeerTable;
use tracing::{debug, warn};

const ROUTE_PREFIX_TEMPS: &str = "/api/_temps";
const ROUTE_PREFIX_OTEL: &str = "/api/otel";
/// OTLP ingest. The rest of `/api/otel` is the console's authenticated query
/// API and must keep reaching the full console listener.
const ROUTE_PREFIX_OTEL_INGEST: &str = "/api/otel/v1/";

/// Paths every route under which is a public ingest endpoint: served for any
/// `Host`, never authenticated by session, and mounted on the console's
/// public router. They are the only path-based routing left in the proxy,
/// and they go to the public-only listener when one exists (ADR-051).
pub fn is_public_ingest_path(path: &str) -> bool {
    path.starts_with(ROUTE_PREFIX_TEMPS) || path.starts_with(ROUTE_PREFIX_OTEL_INGEST)
}

/// Match only the two authenticated DNS sync endpoints. Prefix matching would
/// accidentally capture unrelated node APIs or attacker-controlled suffixes.
pub fn is_internal_dns_sync_path(path: &str) -> bool {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let mut segments = path.split('/');
    matches!(
        (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ),
        (Some(""), Some("api"), Some("internal"), Some("nodes"), Some(node_id), Some("dns"), Some("changes" | "ack"), None)
            if node_id.parse::<i32>().is_ok()
    )
}

/// How long a request will wait for the route table's first load to complete
/// before falling back to the console. The proxy now binds its listeners before
/// the initial (DB-heavy) route load finishes, so a request that arrives in that
/// brief startup window would otherwise be sent to the console instead of its
/// real backend. This only applies until the first successful load; afterwards an
/// unmatched host falls through immediately, exactly as before.
const FIRST_LOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Implementation of UpstreamResolver trait
pub struct UpstreamResolverImpl {
    server_config: Arc<ProxyConfig>,
    lb_service: Arc<LbService>,
    route_table: Arc<CachedPeerTable>,
}

impl UpstreamResolverImpl {
    pub fn new(
        server_config: Arc<ProxyConfig>,
        lb_service: Arc<LbService>,
        route_table: Arc<CachedPeerTable>,
    ) -> Self {
        Self {
            server_config,
            lb_service,
            route_table,
        }
    }

    /// Where requests that may only reach the console's public routes go:
    /// the public-only listener when one exists, the console listener
    /// otherwise (a split proxy started without an ingress address).
    fn public_surface_address(&self) -> &str {
        self.server_config
            .console_ingress_address
            .as_deref()
            .unwrap_or(&self.server_config.console_address)
    }
}

#[async_trait]
impl UpstreamResolver for UpstreamResolverImpl {
    async fn resolve_peer(
        &self,
        host: &str,
        path: &str,
        sni_hostname: Option<&str>,
    ) -> PingoraResult<PeerSelection> {
        self.resolve_peer_for_request(host, path, "", sni_hostname)
            .await
    }

    async fn resolve_peer_for_request(
        &self,
        host: &str,
        path: &str,
        method: &str,
        sni_hostname: Option<&str>,
    ) -> PingoraResult<PeerSelection> {
        debug!(
            "Resolving peer for host: {}, path: {}, sni: {:?}",
            host, path, sni_hostname
        );

        // The public ingress hostname (ADR-051) is owned by the platform and
        // may only ever reach the public-only listener: never a project
        // route, and never the full console, whatever the path or client.
        // Checked before the path prefixes below so `/api/otel/...` on this
        // host cannot reach the console's authenticated query API.
        if let Some(ingress_address) = self.server_config.console_ingress_address.as_deref() {
            if self.route_table.is_public_ingress_hostname(host) {
                debug!(
                    "Routing public ingress host {} to the public-only listener: {}",
                    host, ingress_address
                );
                return Ok(console_peer(ingress_address));
            }
        }

        // A status page's custom hostname (#1412) is answered by the proxy's
        // status-host branch in `request_filter`, which pins it to the
        // public-only listener before peer resolution. Reaching this point
        // with one means that branch was bypassed: refuse rather than let any
        // path prefix below send the host to the console or a project.
        if self.route_table.status_page_host(host).is_some()
            || sni_hostname.is_some_and(|sni| self.route_table.status_page_host(sni).is_some())
        {
            warn!(
                host,
                "status-page hostname reached generic peer resolution; refusing"
            );
            return Err(Error::explain(
                ErrorType::ConnectNoRoute,
                format!("status-page hostname '{host}' is only served by the status-page branch"),
            ));
        }

        // Route API and OTLP paths directly to the console (API server),
        // regardless of the Host header — needed for OTLP push from containers
        // that use host.docker.internal as the host. Public ingest paths go to
        // the public-only listener, so "reachable from any host" is a property
        // of what that listener mounts rather than of a path prefix. The OTLP
        // query API shares the `/api/otel` prefix but is an authenticated
        // console API, so it keeps going to the full console listener.
        if path.starts_with(ROUTE_PREFIX_TEMPS) || path.starts_with(ROUTE_PREFIX_OTEL) {
            let address = if is_public_ingest_path(path) {
                self.public_surface_address()
            } else {
                self.server_config.console_address.as_str()
            };
            debug!("Routing temps API request to console: {}", address);
            return Ok(console_peer(address));
        }

        // Try the in-memory route table across all three lookup strategies
        // (TLS/SNI, HTTP host, legacy). Returns the matched peer, if any.
        let sni_or_host = sni_hostname.unwrap_or(host);
        let lookup = |label: &str| -> Option<PeerSelection> {
            // 1. TLS/SNI-based routing
            if let Some(route_info) = self.route_table.get_route_by_sni(sni_or_host) {
                let selection = route_info.select_backend();
                debug!(
                    "{}: found TLS route via SNI/Host {} -> {}",
                    label, sni_or_host, selection.address
                );
                return Some(PeerSelection {
                    peer: Box::new(HttpPeer::new(
                        selection.address.clone(),
                        false,
                        "".to_string(),
                    )),
                    container_id: selection.container_id,
                    container_name: selection.container_name,
                });
            }

            // 2. HTTP Host-based routing (HTTP routes)
            if let Some(route_info) = self.route_table.get_route_by_host(host) {
                let project_id = route_info.project.as_ref().map(|p| p.id);
                let env_id = route_info.environment.as_ref().map(|e| e.id);
                let selection = route_info.select_backend();
                debug!(
                    "{}: found HTTP route for {} -> {} (project_id: {:?}, env_id: {:?})",
                    label, host, selection.address, project_id, env_id
                );
                return Some(PeerSelection {
                    peer: Box::new(HttpPeer::new(
                        selection.address.clone(),
                        false,
                        "".to_string(),
                    )),
                    container_id: selection.container_id,
                    container_name: selection.container_name,
                });
            }

            // 3. Legacy: the old get_route method for backwards compatibility
            if let Some(route_info) = self.route_table.get_route(host) {
                let project_id = route_info.project.as_ref().map(|p| p.id);
                let env_id = route_info.environment.as_ref().map(|e| e.id);
                let selection = route_info.select_backend();
                debug!(
                    "{}: found legacy route for {} -> {} (project_id: {:?}, env_id: {:?})",
                    label, host, selection.address, project_id, env_id
                );
                return Some(PeerSelection {
                    peer: Box::new(HttpPeer::new(
                        selection.address.clone(),
                        false,
                        "".to_string(),
                    )),
                    container_id: selection.container_id,
                    container_name: selection.container_name,
                });
            }

            None
        };

        if let Some(selection) = lookup("route lookup") {
            return Ok(selection);
        }

        // No route matched. If the table has never loaded — i.e. the proxy just
        // bound its listeners and the initial load is still running in the
        // background — hold this request briefly for the first load, then retry
        // the lookup before falling back to the console. After the first load
        // this branch is skipped and an unmatched host falls through immediately.
        if !self.route_table.has_loaded() {
            debug!(
                "Route table not loaded yet; waiting up to {:?} for first load (host: {})",
                FIRST_LOAD_WAIT, host
            );
            self.route_table.wait_until_loaded(FIRST_LOAD_WAIT).await;
            if let Some(selection) = lookup("route lookup (after first load)") {
                return Ok(selection);
            }
        }

        // The host belongs to a deployment whose containers are all down (or
        // that is paused). Fail the request so `fail_to_proxy` answers 503:
        // falling through to the console would serve its SPA with HTTP 200
        // on the application's own hostname, and uptime monitors would
        // report the stopped app as operational (issue #1334). Marked as an
        // upstream failure: the application is down, Temps is not.
        if let Some(unavailable) = self.route_table.get_unavailable_route(host) {
            debug!(
                host,
                project_id = unavailable.project.id,
                environment_id = unavailable.environment.id,
                deployment_id = unavailable.deployment.id,
                reason = unavailable.reason.as_str(),
                "Host has no live upstream; failing the request instead of routing to the console"
            );
            return Err(Error::explain(
                ErrorType::ConnectNoRoute,
                format!(
                    "no live upstream for host '{}' (project {}, environment {}, deployment {}): {}",
                    host,
                    unavailable.project.id,
                    unavailable.environment.id,
                    unavailable.deployment.id,
                    unavailable.reason.as_str()
                ),
            )
            .into_up());
        }

        let endpoint_path = path.split_once('?').map_or(path, |(path, _)| path);
        let dns_method_matches = (method == "GET" && endpoint_path.ends_with("/dns/changes"))
            || (method == "POST" && endpoint_path.ends_with("/dns/ack"));
        if dns_method_matches
            && is_internal_dns_sync_path(path)
            && !self.lb_service.has_route_in_snapshot(host)
        {
            if let Some(address) = self.server_config.internal_dns_sync_address.as_ref() {
                debug!(%address, "Routing authenticated DNS sync request to proxy service");
                return Ok(PeerSelection {
                    peer: Box::new(HttpPeer::new(address.clone(), false, "".to_string())),
                    container_id: None,
                    container_name: None,
                });
            }
        }

        // No route found - route to console address as default
        debug!(
            "No route found in table for host: {}, routing to console (route_count={})",
            host,
            self.route_table.len()
        );
        let peer = Box::new(HttpPeer::new(
            self.server_config.console_address.clone(),
            false,
            "".to_string(),
        ));
        Ok(PeerSelection {
            peer,
            container_id: None,
            container_name: None,
        })
    }

    async fn has_custom_route(&self, host: &str) -> bool {
        // Lock-free snapshot lookup — never queries the database.
        // The snapshot is kept current by `LbService::run_refresh_loop` (60s).
        // Write-through on LbService only updates the instance that processed the
        // write; this resolver holds the hot-path instance, which never receives
        // admin-API writes directly. The 60-second periodic loop is the sole
        // propagation mechanism here (worst-case staleness: 60 seconds).
        self.lb_service.has_route_in_snapshot(host)
    }

    async fn has_route_for_host(&self, host: &str) -> bool {
        // Project deployment hosts live in the in-memory route table
        // (HTTP host map, SNI/TLS map, legacy lookup, wildcards). Operator
        // overrides live in the `custom_routes` snapshot. The admin gate must
        // recognize a host as "known" if any of these match — otherwise it
        // 404s real project traffic the moment an admin host is configured.
        // Neither branch queries the database on the request hot path.
        if self.route_table.get_route_by_host(host).is_some()
            || self.route_table.get_route_by_sni(host).is_some()
            || self.route_table.get_route(host).is_some()
        {
            return true;
        }
        // A project host whose containers are down is still a project host:
        // it must reach the 503, not the admin gate's 404.
        if self.route_table.get_unavailable_route(host).is_some() {
            return true;
        }
        // Lock-free snapshot lookup for operator-defined custom routes.
        self.lb_service.has_route_in_snapshot(host)
    }

    async fn get_lb_strategy(&self, _host: &str) -> Option<String> {
        Some("round_robin".to_string())
    }

    fn console_address(&self) -> &str {
        &self.server_config.console_address
    }

    fn console_ingress_address(&self) -> Option<&str> {
        self.server_config.console_ingress_address.as_deref()
    }

    fn is_public_ingress_host(&self, host: &str) -> bool {
        self.route_table.is_public_ingress_hostname(host)
    }

    fn status_page_host(&self, host: &str) -> Option<temps_routes::StatusPageHostRoute> {
        self.route_table.status_page_host(host)
    }
}

/// A plain-HTTP peer for one of the console's loopback listeners.
fn console_peer(address: &str) -> PeerSelection {
    PeerSelection {
        peer: Box::new(HttpPeer::new(address.to_string(), false, "".to_string())),
        container_id: None,
        container_name: None,
    }
}

/// Implementation of ProjectContextResolver trait
pub struct ProjectContextResolverImpl {
    route_table: Arc<CachedPeerTable>,
}

impl ProjectContextResolverImpl {
    pub fn new(route_table: Arc<CachedPeerTable>) -> Self {
        Self { route_table }
    }

    fn project_context_from_route(&self, host: &str) -> Option<ProjectContext> {
        if let Some(route_info) = self.route_table.get_route(host) {
            return Some(ProjectContext {
                project: route_info.project?,
                environment: route_info.environment?,
                deployment: route_info.deployment?,
                upstream_unavailable: false,
            });
        }
        // A host whose deployment has no live upstream still belongs to that
        // project: attribute the failed request to it and apply its request
        // policy before the resolver fails it with a 503.
        let unavailable = self.route_table.get_unavailable_route(host)?;
        Some(ProjectContext {
            project: unavailable.project,
            environment: unavailable.environment,
            deployment: unavailable.deployment,
            upstream_unavailable: true,
        })
    }
}

#[async_trait]
impl ProjectContextResolver for ProjectContextResolverImpl {
    async fn resolve_context(&self, host: &str) -> Option<ProjectContext> {
        if let Some(context) = self.project_context_from_route(host) {
            return Some(context);
        }

        // Upstream selection waits for the first route load before retrying.
        // Context resolution must do the same or request filters can observe no
        // project and skip project-scoped protections for a route that becomes
        // available moments later in the same request.
        if !self.route_table.has_loaded() {
            debug!(
                "Route table not loaded yet; waiting up to {:?} before resolving project context (host: {})",
                FIRST_LOAD_WAIT, host
            );
            self.route_table.wait_until_loaded(FIRST_LOAD_WAIT).await;
            return self.project_context_from_route(host);
        }

        None
    }

    async fn is_static_deployment(&self, host: &str) -> bool {
        // Use route_info.is_static() to check if backend is static directory
        if let Some(route_info) = self.route_table.get_route(host) {
            return route_info.is_static();
        }
        false
    }

    async fn get_redirect_info(&self, host: &str) -> Option<(String, u16)> {
        // Use cached redirect info from route table
        let route_info = self.route_table.get_route(host)?;
        let redirect_to = route_info.redirect_to?;
        let status_code = route_info.status_code? as u16;
        Some((redirect_to, status_code))
    }

    async fn get_static_path(&self, host: &str) -> Option<String> {
        // Use route_info.static_dir() to get static directory path
        let route_info = self.route_table.get_route(host)?;
        route_info.static_dir().map(|s| s.to_string())
    }

    fn match_path_route(&self, host: &str, path: &str) -> Option<temps_routes::PathRoute> {
        self.route_table.match_path_route(host, path)
    }
}

#[cfg(test)]
mod public_ingress_tests {
    use super::*;
    use pingora_core::upstreams::peer::Peer;

    const CONSOLE: &str = "127.0.0.1:18085";
    const INGRESS: &str = "127.0.0.1:18086";
    const INGRESS_HOST: &str = "hooks.example.com";

    fn resolver(ingress_listener: bool, ingress_host: Option<&str>) -> UpstreamResolverImpl {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let route_table = Arc::new(CachedPeerTable::new(db.clone()));
        // Loaded, so an unmatched host falls through without the startup wait.
        route_table.insert_route_for_test(
            "seed.example.com",
            temps_routes::RouteInfo {
                backend: temps_routes::BackendType::Upstream {
                    backends: vec![temps_routes::BackendEntry {
                        address: "127.0.0.1:18080".into(),
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
                cert_eligible: false,
            },
        );
        if let Some(host) = ingress_host {
            route_table.reserve_public_ingress_hostname_for_test(host);
        }
        UpstreamResolverImpl::new(
            Arc::new(ProxyConfig {
                console_address: CONSOLE.into(),
                console_ingress_address: ingress_listener.then(|| INGRESS.into()),
                ..ProxyConfig::default()
            }),
            Arc::new(LbService::new(db)),
            route_table,
        )
    }

    async fn peer(resolver: &UpstreamResolverImpl, host: &str, path: &str) -> String {
        match resolver
            .resolve_peer_for_request(host, path, "POST", None)
            .await
        {
            Ok(selection) => selection.peer.address().to_string(),
            Err(error) => panic!("{host}{path} must resolve to a peer: {error}"),
        }
    }

    #[test]
    fn only_ingest_paths_count_as_public() {
        for path in [
            "/api/_temps/event",
            "/api/_temps/captcha/verify",
            "/api/otel/v1/traces",
            "/api/otel/v1/1/2/3/logs",
        ] {
            assert!(is_public_ingest_path(path), "{path}");
        }
        for path in [
            "/api/otel/traces",
            "/api/otel/metrics",
            "/api/otel/v1",
            "/api/settings",
            "/",
        ] {
            assert!(!is_public_ingest_path(path), "{path}");
        }
    }

    // Paused clock: an unmatched host waits out the first-load grace period
    // on a table that was never loaded from a database.
    #[tokio::test(start_paused = true)]
    async fn ingress_host_reaches_only_the_public_listener_whatever_the_path() {
        let resolver = resolver(true, Some(INGRESS_HOST));
        for path in [
            "/",
            "/api/settings",
            "/api/webhook/git/gitlab/events",
            "/api/emails/5b1f8f0e-6d0e-4a55-9f0c-2f6a3f4f7c11/track/open",
            // Shares a prefix with the console's authenticated query API.
            "/api/otel/traces",
            "/api/_temps/event",
        ] {
            assert_eq!(
                peer(&resolver, INGRESS_HOST, path).await,
                INGRESS,
                "{path} on the ingress host must never reach the console listener"
            );
        }
        // A raw Host header with a port and different case is the same host.
        assert_eq!(
            peer(&resolver, "Hooks.Example.com:443", "/api/settings").await,
            INGRESS
        );
        assert!(resolver.is_public_ingress_host("hooks.example.com."));
    }

    // Paused clock: an unmatched host waits out the first-load grace period
    // on a table that was never loaded from a database.
    #[tokio::test(start_paused = true)]
    async fn other_hosts_keep_falling_through_to_the_console() {
        let resolver = resolver(true, Some(INGRESS_HOST));
        assert_eq!(peer(&resolver, "console.example.com", "/").await, CONSOLE);
        assert_eq!(
            peer(&resolver, "console.example.com", "/api/settings").await,
            CONSOLE
        );
        assert!(!resolver.is_public_ingress_host("console.example.com"));
        // A project route is untouched by the ingress host existing.
        assert_eq!(
            peer(&resolver, "seed.example.com", "/").await,
            "127.0.0.1:18080"
        );
    }

    // Paused clock: an unmatched host waits out the first-load grace period
    // on a table that was never loaded from a database.
    #[tokio::test(start_paused = true)]
    async fn public_ingest_paths_use_the_public_listener_on_any_host() {
        let resolver = resolver(true, None);
        for host in [
            "console.example.com",
            "seed.example.com",
            "host.docker.internal",
        ] {
            assert_eq!(peer(&resolver, host, "/api/_temps/event").await, INGRESS);
            assert_eq!(peer(&resolver, host, "/api/otel/v1/traces").await, INGRESS);
        }
        // The OTLP query API is an authenticated console API: it must keep
        // reaching the full console, or the dashboard's own requests 404.
        assert_eq!(
            peer(&resolver, "console.example.com", "/api/otel/traces").await,
            CONSOLE
        );
    }

    // Paused clock: an unmatched host waits out the first-load grace period
    // on a table that was never loaded from a database.
    #[tokio::test(start_paused = true)]
    async fn without_an_ingress_listener_routing_is_unchanged() {
        let resolver = resolver(false, Some(INGRESS_HOST));
        assert_eq!(resolver.console_ingress_address(), None);
        assert_eq!(peer(&resolver, INGRESS_HOST, "/").await, CONSOLE);
        assert_eq!(
            peer(&resolver, "seed.example.com", "/api/_temps/event").await,
            CONSOLE
        );
        assert_eq!(
            peer(&resolver, "seed.example.com", "/api/otel/v1/traces").await,
            CONSOLE
        );
    }
}

#[cfg(test)]
mod dns_sync_tests {
    use super::*;
    use pingora_core::upstreams::peer::Peer;
    use std::sync::atomic::AtomicUsize;
    use temps_routes::{BackendEntry, BackendType, RouteInfo};

    fn resolver(enabled: bool) -> UpstreamResolverImpl {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        UpstreamResolverImpl::new(
            Arc::new(ProxyConfig {
                console_address: "127.0.0.1:18085".into(),
                internal_dns_sync_address: enabled.then(|| "127.0.0.1:19053".into()),
                ..ProxyConfig::default()
            }),
            Arc::new(LbService::new(db.clone())),
            Arc::new(CachedPeerTable::new(db)),
        )
    }

    fn app_route() -> RouteInfo {
        RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![BackendEntry {
                    address: "127.0.0.1:18080".into(),
                    container_id: Some("application-container".into()),
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
    fn dns_sync_path_is_exact() {
        for path in [
            "/api/internal/nodes/1/dns/changes",
            "/api/internal/nodes/1/dns/changes?since=2",
            "/api/internal/nodes/1/dns/ack",
        ] {
            assert!(is_internal_dns_sync_path(path), "{path}");
        }
        for path in [
            "/api/internal/nodes/1/dns/changes/extra",
            "/api/internal/nodes/1/dns/ack/",
            "/api/internal/nodes/1/network/peers",
            "/api/internal/nodes/abc/dns/changes",
            "/api/internal/nodes/2147483648/dns/ack",
            "/api/internal/nodes/1/dns/changes-extra",
        ] {
            assert!(!is_internal_dns_sync_path(path), "{path}");
        }
    }

    #[tokio::test]
    async fn application_and_sni_routes_own_dns_shaped_paths() {
        let resolver = resolver(true);
        resolver
            .route_table
            .insert_route_for_test("app.example.com", app_route());
        resolver
            .route_table
            .insert_tls_route_for_test("tls.example.com", app_route());
        for (host, sni) in [
            ("app.example.com", None),
            ("different.example.com", Some("tls.example.com")),
        ] {
            let peer = resolver
                .resolve_peer_for_request(host, "/api/internal/nodes/1/dns/changes", "GET", sni)
                .await
                .unwrap();
            assert_eq!(peer.container_id.as_deref(), Some("application-container"));
        }
    }

    #[tokio::test]
    async fn application_loaded_during_startup_wait_keeps_its_path() {
        let resolver = resolver(true);
        let table = resolver.route_table.clone();
        let load = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            table.insert_route_for_test("app.example.com", app_route());
        });
        let peer = resolver
            .resolve_peer_for_request(
                "app.example.com",
                "/api/internal/nodes/1/dns/changes",
                "GET",
                None,
            )
            .await
            .unwrap();
        load.await.unwrap();
        assert_eq!(peer.container_id.as_deref(), Some("application-container"));
    }

    #[tokio::test]
    async fn only_enabled_dns_methods_use_the_local_service() {
        let enabled = resolver(true);
        let disabled = resolver(false);
        // Requests run concurrently so the deliberate initial-route wait is
        // exercised once, even when the database is unavailable.
        let cases = [
            (
                &enabled,
                "GET",
                "/api/internal/nodes/1/dns/changes?since=2",
                "127.0.0.1:19053",
            ),
            (
                &enabled,
                "POST",
                "/api/internal/nodes/1/dns/ack",
                "127.0.0.1:19053",
            ),
            (
                &enabled,
                "POST",
                "/api/internal/nodes/1/dns/changes",
                "127.0.0.1:18085",
            ),
            (
                &enabled,
                "GET",
                "/api/internal/nodes/1/dns/ack",
                "127.0.0.1:18085",
            ),
            (
                &enabled,
                "GET",
                "/api/internal/nodes/1/network/peers",
                "127.0.0.1:18085",
            ),
            (
                &disabled,
                "GET",
                "/api/internal/nodes/1/dns/changes",
                "127.0.0.1:18085",
            ),
        ];
        futures::future::join_all(cases.into_iter().map(
            |(resolver, method, path, expected)| async move {
                let peer = resolver
                    .resolve_peer_for_request("console.example.com", path, method, None)
                    .await
                    .unwrap();
                assert_eq!(peer.peer.address().to_string(), expected, "{method} {path}");
            },
        ))
        .await;
    }
}

#[cfg(test)]
mod path_route_tests {
    use super::*;
    use temps_routes::{BackendType, PathRoute, RouteInfo};

    fn environment(id: i32) -> Arc<temps_entities::environments::Model> {
        Arc::new(
            serde_json::from_value(serde_json::json!({
                "id": id, "name": "production", "slug": "production", "subdomain": "production",
                "last_deployment": null, "host": "", "upstreams": [],
                "created_at": "2026-09-28T00:00:00Z", "updated_at": "2026-09-28T00:00:00Z",
                "project_id": 1, "current_deployment_id": null, "branch": null,
                "deleted_at": null, "deployment_config": null, "is_preview": false,
                "protected": false, "sleeping": false, "attack_mode": null,
                "force_https": null, "last_activity_at": null
            }))
            .expect("environment fixture"),
        )
    }

    /// The proxy resolves path routes through the context resolver, which
    /// every proxy has; the load balancer's own route-table handle is only
    /// wired when on-demand TLS is configured.
    #[test]
    fn context_resolver_matches_path_routes_in_memory() {
        let table = Arc::new(CachedPeerTable::new(Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        )));
        table.insert_route_for_test(
            "web.example.com",
            RouteInfo {
                backend: BackendType::StaticDir {
                    path: "web".to_string(),
                },
                redirect_to: None,
                status_code: None,
                project: None,
                environment: Some(environment(10)),
                deployment: None,
                cert_eligible: true,
            },
        );
        table.insert_path_routes_for_test(
            10,
            vec![PathRoute {
                prefix: "/api".to_string(),
                strip_prefix: false,
                target_service: "api".to_string(),
                target: None,
                sleeping_environment_id: None,
            }],
        );
        let resolver = ProjectContextResolverImpl::new(table);

        let matched = resolver.match_path_route("web.example.com", "/api/status");
        assert_eq!(matched.map(|r| r.prefix), Some("/api".to_string()));
        assert!(resolver
            .match_path_route("web.example.com", "/about")
            .is_none());
        assert!(resolver
            .match_path_route("other.example.com", "/api/status")
            .is_none());
    }
}

#[cfg(test)]
mod unavailable_host_tests {
    //! Issue #1334: a host whose deployment has no live upstream must fail
    //! (and be answered 503 by `fail_to_proxy`), never fall back to the
    //! console, while every other console fallback keeps working.
    use super::*;
    use crate::test_utils::TestDBMockOperations;
    use pingora_core::upstreams::peer::Peer;
    use pingora_core::ErrorSource;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;

    const CONSOLE: &str = "127.0.0.1:18085";
    const STOPPED_HOST: &str = "stopped-app.example.com";

    struct StoppedApp {
        _database: TestDatabase,
        route_table: Arc<CachedPeerTable>,
        lb_service: Arc<LbService>,
        ids: (i32, i32, i32),
    }

    /// A project whose custom domain [`STOPPED_HOST`] points at a deployment
    /// whose only container exited, loaded into a real route table. `None`
    /// when Docker is unavailable.
    async fn stopped_app() -> Option<StoppedApp> {
        let database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping unavailable-host resolver test: Docker runtime unavailable");
                return None;
            }
            Err(error) => panic!("Could not create isolated test database: {error}"),
        };
        let db = database.connection_arc();
        let test_db = TestDBMockOperations::new(db.clone()).await.unwrap();
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain(STOPPED_HOST)
            .await
            .unwrap();
        temps_entities::deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("stopped-app-container".to_string()),
            container_name: Set("stopped-app-container".to_string()),
            container_port: Set(Some(3000)),
            host_port: Set(Some(9800)),
            status: Set(Some("exited".to_string())),
            deployed_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let route_table = Arc::new(CachedPeerTable::new(db.clone()));
        route_table.load_routes().await.unwrap();
        Some(StoppedApp {
            _database: database,
            route_table,
            lb_service: Arc::new(LbService::new(db)),
            ids: (project.id, environment.id, deployment.id),
        })
    }

    fn resolver(app: &StoppedApp) -> UpstreamResolverImpl {
        UpstreamResolverImpl::new(
            Arc::new(ProxyConfig {
                console_address: CONSOLE.into(),
                ..ProxyConfig::default()
            }),
            app.lb_service.clone(),
            app.route_table.clone(),
        )
    }

    async fn peer_address(resolver: &UpstreamResolverImpl, host: &str, path: &str) -> String {
        match resolver
            .resolve_peer_for_request(host, path, "GET", None)
            .await
        {
            Ok(selection) => selection.peer.address().to_string(),
            Err(error) => panic!("{host}{path} must resolve to a peer: {error}"),
        }
    }

    #[tokio::test]
    async fn stopped_app_host_fails_instead_of_routing_to_the_console() {
        let Some(app) = stopped_app().await else {
            return;
        };
        let resolver = resolver(&app);

        let error = match resolver
            .resolve_peer_for_request(STOPPED_HOST, "/", "GET", None)
            .await
        {
            Ok(selection) => panic!(
                "a stopped app must not be proxied anywhere, got {}",
                selection.peer.address()
            ),
            Err(error) => error,
        };
        assert_eq!(error.etype(), &ErrorType::ConnectNoRoute);
        assert_eq!(
            error.esource(),
            &ErrorSource::Upstream,
            "the application is down, not Temps: log it as an upstream failure"
        );

        // The admin gate must treat it as a project host (503, not 404).
        assert!(resolver.has_route_for_host(STOPPED_HOST).await);
        assert!(!resolver.has_route_for_host("unknown.example.com").await);

        // Every other console fallback is unchanged.
        assert_eq!(
            peer_address(&resolver, "unknown.example.com", "/").await,
            CONSOLE
        );
        assert_eq!(
            peer_address(&resolver, STOPPED_HOST, "/api/_temps/health").await,
            CONSOLE,
            "the /api/_temps prefix still reaches the console from any host"
        );
        assert_eq!(
            peer_address(&resolver, STOPPED_HOST, "/api/otel/v1/traces").await,
            CONSOLE
        );
    }

    #[tokio::test]
    async fn stopped_app_requests_are_attributed_to_their_project() {
        let Some(app) = stopped_app().await else {
            return;
        };
        let resolver = ProjectContextResolverImpl::new(app.route_table.clone());

        let context = resolver
            .resolve_context(STOPPED_HOST)
            .await
            .expect("a stopped app's request must still be attributed");
        assert_eq!(
            (
                context.project.id,
                context.environment.id,
                context.deployment.id
            ),
            app.ids
        );
        assert!(context.upstream_unavailable);
        assert!(!resolver.is_static_deployment(STOPPED_HOST).await);
        assert!(resolver.get_redirect_info(STOPPED_HOST).await.is_none());
        assert!(resolver
            .resolve_context("unknown.example.com")
            .await
            .is_none());
    }
}
