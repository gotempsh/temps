// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 8: assemble the routers each listener serves.

use super::*;

/// What [`build_router`] needs besides the initialised plugins.
pub(super) struct RouterInputs {
    pub(super) db: Arc<DbConnection>,
    pub(super) config: Arc<ServerConfig>,
    pub(super) encryption_service: Arc<EncryptionService>,
    pub(super) route_table: Arc<temps_proxy::CachedPeerTable>,
    pub(super) request_policy_gate_slot: Arc<temps_core::RequestPolicyGateSlot>,
    pub(super) core_handles: CoreServiceHandles,
    pub(super) node_routes: Router,
    pub(super) provided_admin_gate_service:
        Option<crate::commands::serve::admin_gate_service::AdminGateService>,
    pub(super) provided_admin_gate_handle: Option<temps_core::admin_gate::AdminGateHandle>,
    pub(super) ready_flag: Arc<std::sync::atomic::AtomicBool>,
}

/// The routers [`serve`](super::serve::serve) binds or hands to other
/// components.
pub(super) struct ConsoleRouters {
    /// The public-only listener's app (ADR-051 D4), when one is configured.
    pub(super) ingress_app: Option<Router>,
    pub(super) public_app: Router,
    pub(super) admin_app: Router,
    /// The admin API for the platform-console listener, when an embedding
    /// binary overrode the root bundle and configured an address.
    pub(super) platform_router: Option<Router>,
    /// The admin surface without the IP gate, for the console-proxy dispatcher.
    pub(super) console_router: Router,
    /// Public and admin API without the SPA or the IP gate, for plugins.
    pub(super) plugin_api_router: Router,
    pub(super) admin_gate_handle: temps_core::admin_gate::AdminGateHandle,
}

/// The public-only and public listener apps, assembled from routers that
/// already exist.
pub(super) struct PublicListenerApps {
    pub(super) ingress_app: Option<Router>,
    pub(super) public_app: Router,
}

/// Build every router the console serves from the initialised plugins: the
/// agent-facing routes, the split public/admin application, the admin gate,
/// Swagger, MCP and the signed-in gate's authorize page. Also populates the
/// AI API tool handles and starts the node API listener, which need the
/// assembled routers.
pub(super) async fn build_router(
    plugin_manager: &PluginManager,
    inputs: RouterInputs,
) -> anyhow::Result<ConsoleRouters> {
    let RouterInputs {
        db,
        config,
        encryption_service,
        route_table,
        request_policy_gate_slot,
        core_handles:
            CoreServiceHandles {
                gate_service,
                access_key_service,
            },
        node_routes,
        provided_admin_gate_service,
        provided_admin_gate_handle,
        ready_flag,
    } = inputs;
    let service_context = plugin_manager.service_context();

    // Build the application with all plugin routes and OpenAPI schemas
    debug!("Building application with plugin routes");

    // Internal route-sync endpoint for the worker-side internal edge
    // proxy (Option 1 in the route-sync design). Workers long-poll
    // here to mirror the CP's `*.temps.local` route table without
    // needing direct DB access.
    let route_sync_state = Arc::new(temps_routes::route_sync::RouteSyncAppState {
        db: db.clone(),
        peer_table: route_table.clone(),
        encryption_service: encryption_service.clone(),
        request_policy_gate: request_policy_gate_slot.clone(),
        gate: gate_service.clone(),
        access_keys: access_key_service.clone(),
        // ADR 066, I8: workers' Shield events go to the sink the proxy's
        // event store registers; looked up on first upload.
        shield_events: {
            let services = plugin_manager.service_context().create_plugin_context();
            Arc::new(temps_routes::shield_sync::ShieldEventSinkSlot::new(
                move || services.get_service::<dyn temps_routes::shield_sync::EventSink>(),
            ))
        },
    });
    let route_sync_routes =
        temps_routes::route_sync::configure_routes().with_state(route_sync_state);
    // Node-token endpoints for ingress connectors (ADR 058): the agent pulls
    // what to run and reports how it is doing.
    let ingress_node_routes =
        temps_ingress::node_routes(Arc::new(temps_ingress::IngressNodeState {
            db: db.clone(),
            service: plugin_manager
                .service_context()
                .require_service::<temps_ingress::IngressConnectorService>(),
        }));

    // Node-token endpoint for worker network policy enforcers.
    let network_policy_node_routes =
        temps_network_policy::node_routes(Arc::new(temps_network_policy::NetworkPolicyNodeState {
            db: db.clone(),
            service: plugin_manager
                .service_context()
                .require_service::<temps_network_policy::NetworkPolicyService>(),
        }));

    // Build the split application: public routes (event ingest, AI gateway,
    // session replay ingest, etc.) and admin routes (auth, dashboard, CRUD).
    let split = plugin_manager
        .build_split_application()
        .map_err(|e| anyhow::anyhow!("Failed to build application: {}", e))?;

    // ADR-024: Populate the ApiToolsHandle now that the Axum router is fully
    // assembled.  InternalApiCaller holds a clone of the merged admin router
    // (which carries the auth middleware layers) and the unified OpenAPI doc
    // — both are only available at this point, after plugin initialisation and
    // build_split_application().  The handle was registered as a service by
    // AiChatPlugin::register_services(); all adapters (OSS ChatTool, EE rig
    // Tool) retrieved it at that time and hold clones that share the same
    // OnceLock.
    install_ai_api_tools(plugin_manager, &split.admin);

    // Agent-facing node + route-sync routes are public (workers anywhere on
    // the internet POST to them with bearer tokens).
    let public_router = split
        .public
        .merge(node_routes)
        .merge(route_sync_routes)
        .merge(ingress_node_routes)
        .merge(network_policy_node_routes);

    // Use the caller-supplied admin-gate when present (so the proxy and the
    // console share one source of truth) and otherwise build a fresh one.
    let (admin_gate_service, admin_gate_handle) =
        match (provided_admin_gate_service, provided_admin_gate_handle) {
            (Some(svc), Some(handle)) => (svc, handle),
            _ => crate::commands::serve::admin_gate_service::AdminGateService::new(
                db.clone(),
                &config.admin_allowed_ips,
                &config.admin_allowed_hosts,
                config.admin_trust_forwarded_for,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize admin gate: {}", e))?,
        };
    let admin_gate_state = Arc::new(
        crate::commands::serve::admin_gate_handler::AdminGateAppState {
            service: admin_gate_service,
        },
    );
    // Re-apply the plugin middleware stack (auth, request metadata, audit)
    // to our standalone admin-gate routes. Without this, RequireAuth finds
    // no AuthContext injected and the route 401s for logged-in users —
    // `Router::merge` does not propagate parent layers to merged routes.
    let admin_gate_routes =
        crate::commands::serve::admin_gate_handler::configure_routes(admin_gate_state);
    let admin_gate_routes = plugin_manager
        .apply_middleware_to_router(admin_gate_routes, plugin_manager.get_middleware());

    // Swagger UI + the embedded SPA only live on the admin surface. So do
    // the admin-gate management routes.
    let admin_router = split
        .admin
        .merge(create_swagger_router(plugin_manager)?)
        .merge(admin_gate_routes);

    // Wrap each surface in /api like the original single-router did, except
    // for the SPA fallback which serves the dashboard at the document root.
    // Health probes (`/healthz`, `/readyz`) live at the document root on the
    // PUBLIC surface so they are reachable without auth and without the admin
    // gate — a supervisor or load balancer must be able to poll them even when
    // the admin IP allowlist is active. The public surface is also the one that
    // exists in every topology (single- and dual-listener), so probes work
    // regardless of `console_admin_address`.
    // The router plugins reach over the platform channel.
    //
    // Built from the same public + admin routes the console serves, but
    // deliberately without the SPA fallback (a plugin wants the API, not
    // index.html) and without the admin IP gate (that gate exists to keep
    // browsers off the admin listener from untrusted networks; a channel
    // call arrives in-process from a plugin the operator installed, and is
    // authorised by an actor token plus the handler's own permission check).
    let plugin_api_router =
        Router::new().nest("/api", public_router.clone().merge(admin_router.clone()));

    // ADR-045 §3: a second clone of `admin_router`, shaped exactly like
    // `admin_app` below (`nest("/api", ..)` + the static-file fallback) but
    // taken *before* that router is wrapped in the admin IP-allowlist gate —
    // for the same reason `plugin_api_router` above omits it: authorization
    // for a console-proxied request comes from Cloud's own auth plus the
    // browser's session cookie, not from network topology. Installed into
    // the shared `ConsoleDispatchSlot` near the `RouterHostApi` wiring below,
    // for `temps-cloud-client::console_proxy::ConsoleProxyWorker` (started
    // elsewhere, once the `cloud.console_access_enabled` setting exists) to
    // drive in-process.
    let console_router = Router::new()
        .nest("/api", admin_router.clone())
        .fallback(serve_static_file);
    // Nodes paired from the control plane reach it over the WireGuard mesh
    // (ADR 048 D3); that listener serves only the routes nodes call.
    crate::commands::serve::node_api::spawn(
        db.clone(),
        service_context.require_service::<temps_config::ConfigService>(),
        service_context.require_service::<temps_core::EncryptionService>(),
        Router::new().nest("/api", public_router.clone().merge(admin_router.clone())),
    );

    // Build root-level MCP routes (ADR-039). These live outside /api so the
    // CLI wizard's unauthenticated probe (GET /mcp/tools) works without a key.
    // The authenticated sub-router gets the full plugin middleware stack (auth,
    // request metadata) applied so RequireAuth works just like any /api handler.
    let mcp_root_router = {
        let service_context = plugin_manager.service_context();
        if let Some(mcp_state) = service_context.get_service::<McpHandlerState>() {
            let mcp_routers = temps_mcp_server::build_mcp_routers(mcp_state);
            let auth_mcp = plugin_manager.apply_middleware_to_router(
                mcp_routers.authenticated,
                plugin_manager.get_middleware(),
            );
            Router::new().merge(mcp_routers.public).merge(auth_mcp)
        } else {
            debug!("McpHandlerState not registered; MCP routes skipped");
            Router::new()
        }
    };

    let PublicListenerApps {
        ingress_app,
        public_app,
    } = assemble_public_apps(&config, &ready_flag, public_router, mcp_root_router);

    // Platform-console listener: when an embedding binary overrode the root
    // bundle AND configured an address, serve the ORIGINAL console (same
    // admin API + admin gate) on its own listener/origin.
    let platform_router =
        if WEBSITE_OVERRIDE.get().is_some() && PLATFORM_CONSOLE_ADDR.get().is_some() {
            Some(admin_router.clone())
        } else {
            None
        };

    // The signed-in gate's authorize step (ADR 065 D8) is a page a browser
    // opens on the console, so it lives at the root of the admin surface
    // with the auth middleware in front of it.
    let gate_authorize_router = {
        let gate_state = Arc::new(
            temps_ingress::reachability::gate_handlers::GateConsoleState {
                gate: gate_service.clone(),
                audit_service: service_context.require_service::<dyn temps_core::AuditLogger>(),
                project_access_checker: service_context
                    .get_service::<dyn temps_core::ProjectAccessChecker>(),
            },
        );
        let router = Router::new().route(
            temps_core::gate::AUTHORIZE_PATH,
            axum::routing::get(move |req: Request| {
                let gate_state = gate_state.clone();
                async move { gate_authorize(gate_state, req).await }
            }),
        );
        plugin_manager.apply_middleware_to_router(router, plugin_manager.get_middleware())
    };

    let admin_app = assemble_admin_app(admin_router, gate_authorize_router, &admin_gate_handle);

    Ok(ConsoleRouters {
        ingress_app,
        public_app,
        admin_app,
        platform_router,
        console_router,
        plugin_api_router,
        admin_gate_handle,
    })
}

/// Wrap the public routers into the apps the public listeners serve: the
/// public-only app (only when a public surface address is configured) and the
/// public app (health probes, MCP, `/api` public routes).
pub(super) fn assemble_public_apps(
    config: &ServerConfig,
    ready_flag: &Arc<std::sync::atomic::AtomicBool>,
    public_router: Router,
    mcp_root_router: Router,
) -> PublicListenerApps {
    // ADR-051 D4: the same public routes a second time, with nothing else
    // around them. Bound in both listener modes: in dual-listener mode
    // `console_address` has no admin routes, but `public_app` below still
    // mounts the authenticated MCP endpoint, so it cannot stand in for it.
    let ingress_app = config.public_surface_address().map(|_| {
        // Request spans for OpenTelemetry export; a no-op unless `--otel-export`.
        // This is the listener the proxy forwards public ingest traffic to when
        // `console_ingress_address` is set, so it needs the same layer as
        // `public_app` below or that traffic would never be traced.
        crate::otel_export::layer_requests(public_only_app(
            health_router(ready_flag.clone()),
            public_router.clone(),
        ))
    });

    let public_app = Router::new()
        .merge(health_router(ready_flag.clone()))
        .merge(mcp_root_router)
        .nest("/api", public_router)
        .layer(axum::middleware::from_fn(track_server_errors));
    // Request spans for OpenTelemetry export; a no-op unless `--otel-export`.
    let public_app = crate::otel_export::layer_requests(public_app);

    PublicListenerApps {
        ingress_app,
        public_app,
    }
}

/// Wrap the admin routers into the admin listener's app: the authorize page,
/// `/api` admin routes and the SPA fallback, behind the admin gate.
pub(super) fn assemble_admin_app(
    admin_router: Router,
    gate_authorize_router: Router,
    admin_gate_handle: &temps_core::admin_gate::AdminGateHandle,
) -> Router {
    let admin_app = Router::new()
        .merge(gate_authorize_router)
        .nest("/api", admin_router)
        .fallback(serve_static_file)
        .layer(axum::middleware::from_fn(track_server_errors));
    let admin_app = crate::otel_export::layer_requests(admin_app);

    // Defense-in-depth: the Pingora proxy is now the primary enforcer (it
    // 404s gated requests before they ever reach this listener). The axum
    // middleware below only matters when something connects to the console
    // listener directly — e.g. loopback debugging, or a deployment where
    // the operator points an external reverse-proxy at console_address
    // instead of going through Pingora. The middleware short-circuits when
    // the active config is a noop, so the perf cost is negligible.
    admin_app.layer(axum::middleware::from_fn_with_state(
        admin_gate_handle.clone(),
        crate::commands::serve::admin_gate::admin_gate,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use axum::routing::get;
    use std::sync::atomic::AtomicBool;
    use temps_core::admin_gate::{AdminGateConfig, AdminGateHandle, AdminGateSource};
    use tower::ServiceExt; // for `oneshot`

    fn config(ingress_address: Option<&str>) -> ServerConfig {
        let mut config = ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://localhost:5432/test".to_string(),
            None,
            Some("127.0.0.1:3001".to_string()),
        )
        .expect("server config builds");
        config.console_ingress_address = ingress_address.map(str::to_string);
        config
    }

    fn public_router() -> Router {
        Router::new().route("/_temps/event", get(|| async { "event" }))
    }

    fn admin_router() -> Router {
        Router::new().route("/settings", get(|| async { "settings" }))
    }

    fn mcp_root_router() -> Router {
        Router::new().route("/mcp/tools", get(|| async { "tools" }))
    }

    fn gate_authorize_router() -> Router {
        Router::new().route(
            temps_core::gate::AUTHORIZE_PATH,
            get(|| async { "authorize" }),
        )
    }

    /// A request as the listener would see it, from `peer`.
    fn request(path: &str, peer: &str) -> Request {
        let mut request = Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("request builds");
        request.extensions_mut().insert(ConnectInfo(
            peer.parse::<std::net::SocketAddr>().expect("peer address"),
        ));
        request
    }

    async fn status(app: &Router, path: &str) -> StatusCode {
        app.clone()
            .oneshot(request(path, "127.0.0.1:50000"))
            .await
            .expect("router responds")
            .status()
    }

    fn admin_gate(allowed_ips: &[String]) -> AdminGateHandle {
        AdminGateHandle::new(
            AdminGateConfig::from_parts(allowed_ips, &[], false, AdminGateSource::Db)
                .expect("admin gate config builds"),
        )
    }

    #[tokio::test]
    async fn the_public_only_app_exists_only_with_a_public_surface_address() {
        let ready = Arc::new(AtomicBool::new(true));
        let without =
            assemble_public_apps(&config(None), &ready, public_router(), mcp_root_router());
        assert!(without.ingress_app.is_none());

        let with = assemble_public_apps(
            &config(Some("127.0.0.1:3002")),
            &ready,
            public_router(),
            mcp_root_router(),
        );
        let ingress_app = with.ingress_app.expect("a public-only app is built");
        assert_eq!(
            status(&ingress_app, "/api/_temps/event").await,
            StatusCode::OK
        );
        assert_eq!(status(&ingress_app, "/healthz").await, StatusCode::OK);
        assert_eq!(
            status(&ingress_app, "/mcp/tools").await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn the_public_app_serves_probes_mcp_and_public_api_routes() {
        let ready = Arc::new(AtomicBool::new(false));
        let apps = assemble_public_apps(&config(None), &ready, public_router(), mcp_root_router());

        assert_eq!(status(&apps.public_app, "/healthz").await, StatusCode::OK);
        assert_eq!(
            status(&apps.public_app, "/readyz").await,
            StatusCode::SERVICE_UNAVAILABLE,
            "/readyz follows the shared ready flag"
        );
        ready.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(status(&apps.public_app, "/readyz").await, StatusCode::OK);
        assert_eq!(status(&apps.public_app, "/mcp/tools").await, StatusCode::OK);
        assert_eq!(
            status(&apps.public_app, "/api/_temps/event").await,
            StatusCode::OK
        );
        assert_eq!(
            status(&apps.public_app, "/api/settings").await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn the_admin_app_serves_the_admin_api_and_authorize_page() {
        let admin_app =
            assemble_admin_app(admin_router(), gate_authorize_router(), &admin_gate(&[]));

        assert_eq!(status(&admin_app, "/api/settings").await, StatusCode::OK);
        assert_eq!(
            status(&admin_app, temps_core::gate::AUTHORIZE_PATH).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn the_admin_gate_hides_the_admin_app_from_disallowed_peers() {
        let admin_app = assemble_admin_app(
            admin_router(),
            gate_authorize_router(),
            &admin_gate(&["10.0.0.0/8".to_string()]),
        );

        assert_eq!(
            status(&admin_app, "/api/settings").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(&admin_app, temps_core::gate::AUTHORIZE_PATH).await,
            StatusCode::NOT_FOUND
        );

        let allowed = admin_app
            .oneshot(request("/api/settings", "10.1.2.3:50000"))
            .await
            .expect("router responds");
        assert_eq!(allowed.status(), StatusCode::OK);
    }
}
