// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::config::*;
use crate::service::lb_service::LbService;
use crate::traits::*;
use async_trait::async_trait;
use pingora_core::{upstreams::peer::HttpPeer, Result as PingoraResult};
use std::sync::Arc;
use temps_routes::CachedPeerTable;
use tracing::debug;

const ROUTE_PREFIX_TEMPS: &str = "/api/_temps";
const ROUTE_PREFIX_OTEL: &str = "/api/otel";

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

        // Route API and OTLP ingest paths directly to the console (API server),
        // regardless of the Host header — needed for OTLP push from containers
        // that use host.docker.internal as the host.
        if path.starts_with(ROUTE_PREFIX_TEMPS) || path.starts_with(ROUTE_PREFIX_OTEL) {
            debug!(
                "Routing temps API request to console: {}",
                self.server_config.console_address
            );
            let peer = Box::new(HttpPeer::new(
                self.server_config.console_address.clone(),
                false,
                "".to_string(),
            ));
            return Ok(PeerSelection {
                peer,
                container_id: None,
                container_name: None,
            });
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
        // Lock-free snapshot lookup for operator-defined custom routes.
        self.lb_service.has_route_in_snapshot(host)
    }

    async fn get_lb_strategy(&self, _host: &str) -> Option<String> {
        Some("round_robin".to_string())
    }

    fn console_address(&self) -> &str {
        &self.server_config.console_address
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
        let route_info = self.route_table.get_route(host)?;
        Some(ProjectContext {
            project: route_info.project?,
            environment: route_info.environment?,
            deployment: route_info.deployment?,
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
