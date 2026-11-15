// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 9: install the assembled routers where other components reach them,
//! bind the listeners and serve until shutdown.

use super::*;

/// What [`serve`] needs besides the plugins and the assembled routers.
pub(super) struct ServeInputs {
    pub(super) db: Arc<DbConnection>,
    pub(super) config: Arc<ServerConfig>,
    pub(super) cookie_crypto: Arc<CookieCrypto>,
    pub(super) ready_flag: Arc<std::sync::atomic::AtomicBool>,
    pub(super) post_init: PostInit,
    pub(super) upgrade_probe: Option<crate::commands::serve::upgrade_telemetry::UpgradeProbe>,
    pub(super) startup_state: Arc<temps_core::console_startup::ConsoleStartupState>,
}

/// Start the platform-console listener when configured, hand the console
/// router to the console-proxy dispatcher and the API to external plugins,
/// then bind the public-only, public and admin listeners, flip `/readyz` and
/// serve until the shutdown signal.
pub(super) async fn serve(
    plugin_manager: &PluginManager,
    routers: ConsoleRouters,
    inputs: ServeInputs,
) -> anyhow::Result<()> {
    let ConsoleRouters {
        ingress_app,
        public_app,
        admin_app,
        platform_router,
        console_router,
        plugin_api_router,
        admin_gate_handle,
    } = routers;
    let ServeInputs {
        db,
        config,
        cookie_crypto,
        ready_flag,
        post_init:
            PostInit {
                startup_reporter,
                enrollment_bootstrap,
            },
        upgrade_probe,
        startup_state,
    } = inputs;

    if let (Some(router), Some(addr)) = (platform_router, PLATFORM_CONSOLE_ADDR.get()) {
        let platform_app = Router::new()
            .nest("/api", router)
            .fallback(serve_original_console)
            .layer(axum::middleware::from_fn(track_server_errors))
            .layer(axum::middleware::from_fn_with_state(
                admin_gate_handle.clone(),
                crate::commands::serve::admin_gate::admin_gate,
            ));
        let listener = bind_console_listener(addr).await?;
        info!("Platform console (original temps UI) listening on {addr}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                platform_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            {
                tracing::error!("Platform console listener failed: {e}");
            }
        });
    }

    info!("Plugin system initialized successfully with static file serving");

    // ADR-045 §3: install the just-assembled `console_router` into a shared
    // slot for the console-proxy dispatcher — the same "shared slot, filled
    // post-construction" shape `RouterHostApi`'s bridge uses just below, and
    // for the same reason: the router does not exist until this point in
    // startup. The Cloud plugin registers the slot (and starts
    // `ConsoleProxyWorker` reading from it) during service registration;
    // this fills it. The fallback only exists for builds without that
    // plugin, so the console still starts — nothing reads the slot then.
    let console_dispatch_slot = match plugin_manager
        .service_context()
        .get_service::<temps_cloud_client::ConsoleDispatchSlot>()
    {
        Some(slot) => slot.as_ref().clone(),
        None => {
            let slot = temps_cloud_client::ConsoleDispatchSlot::new();
            plugin_manager
                .service_context()
                .register_service(Arc::new(slot.clone()));
            slot
        }
    };
    console_dispatch_slot
        .set(Arc::new(temps_cloud_client::ConsoleRouterHandle::new(
            console_router,
        )))
        .await;

    let external_plugins_service = plugin_manager
        .service_context()
        .get_service::<temps_external_plugins::ExternalPluginsService>();
    let cloud_service = plugin_manager
        .service_context()
        .get_service::<CloudService>();

    // Join the channel to the router now that both exist. Plugins connect
    // during startup, before this router could possibly be assembled (it
    // contains the external-plugin routes), so the bridge is installed into
    // a shared slot the channel reads per call rather than passed at
    // connect time.
    if let Some(service) = external_plugins_service.clone() {
        match plugin_manager
            .service_context()
            .get_service::<temps_auth::UserService>()
        {
            Some(user_service) => {
                let bridge = Arc::new(temps_external_plugins::host_api::RouterHostApi::new(
                    plugin_api_router,
                    db.clone(),
                    cookie_crypto.clone(),
                    user_service,
                ));
                service.set_host_api(bridge).await;
                // Same key the bridge verifies with, so a token this proxy
                // mints is one the channel will accept.
                service.set_actor_crypto(cookie_crypto.clone()).await;
                info!("Plugin platform API bridge installed");
            }
            None => {
                // Say so rather than leaving the slot empty and letting every
                // plugin API call fail with a generic error later.
                tracing::warn!(
                    "UserService is not registered, so plugins cannot call the platform \
                     API over the channel; plugin API calls will be refused"
                );
            }
        }
    }

    let shutdown_signal = {
        let svc = external_plugins_service.clone();
        let cloud = cloud_service.clone();
        // Seal every unsealed log head so a restart never loses the last
        // minutes of container logs (ADR-046 §1). The WAL covers crashes;
        // this covers the ordinary upgrade restart.
        let log_writer = plugin_manager
            .service_context()
            .get_service::<temps_log_aggregator::ChunkWriterService>();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Console API received shutdown signal, stopping background services...");
            join_cloud_enrollment_bootstrap(enrollment_bootstrap).await;
            if let Some(writer) = log_writer {
                match tokio::time::timeout(std::time::Duration::from_secs(20), writer.flush_all())
                    .await
                {
                    Ok(()) => info!("Log heads sealed"),
                    Err(_) => {
                        warn!("Sealing log heads exceeded 20s; unsealed lines stay in the WAL")
                    }
                }
            }
            if let Some(service) = cloud {
                service.shutdown().await;
                info!("Managed telemetry mirror shut down");
            }
            if let Some(service) = svc {
                service.shutdown_all().await;
                info!("External plugins shut down");
            }
        }
        .shared()
    };

    // Bound before `/readyz` flips, so a proxy that is told the console is
    // ready can already reach the public-only surface.
    let ingress = match (config.public_surface_address(), ingress_app) {
        (Some(address), Some(app)) => {
            let listener = bind_console_listener(address).await?;
            info!(
                "Console PUBLIC-ONLY API server listening on {} (third-party callbacks, ADR-051)",
                address
            );
            Some((listener, app))
        }
        _ => None,
    };
    let ingress_fut = serve_public_only(ingress, shutdown_signal.clone());

    match config.console_admin_address.as_deref() {
        Some(admin_addr) if !admin_addr.is_empty() => {
            // Two-listener mode: public + admin on separate addresses.
            let public_listener = bind_console_listener(&config.console_address).await?;
            info!(
                "Console PUBLIC API server listening on {}",
                config.console_address
            );
            let admin_listener = bind_console_listener(admin_addr).await?;
            info!("Console ADMIN API server listening on {}", admin_addr);

            // Routers, middleware, and both listeners are ready; flip
            // `/readyz` to 200. `ready_signal` already fired earlier, right
            // after plugin init -- see that call site for why the two are
            // deliberately decoupled.
            ready_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            startup_state.mark_running();
            crate::commands::serve::upgrade_telemetry::complete_startup(
                upgrade_probe.as_ref(),
                startup_reporter.as_ref(),
                &config.data_dir,
            );

            let public_fut = axum::serve(
                public_listener,
                public_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal.clone());

            let admin_fut = axum::serve(
                admin_listener,
                admin_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal);

            tokio::try_join!(public_fut, admin_fut, ingress_fut)?;
        }
        _ => {
            // Single-listener mode (backwards compatible): merge public + admin
            // and serve from `console_address`. Admin gate still applies if
            // configured, but it now gates the merged surface — operators who
            // want network-layer isolation should set TEMPS_CONSOLE_ADMIN_ADDRESS.
            let merged = Router::new().merge(public_app).merge(admin_app);

            let listener = bind_console_listener(&config.console_address).await?;
            info!("Console API server listening on {}", config.console_address);

            // Routers, middleware, and the listener are ready; flip `/readyz`
            // to 200. `ready_signal` already fired earlier, right after
            // plugin init -- see that call site for why the two are
            // deliberately decoupled.
            ready_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            startup_state.mark_running();
            crate::commands::serve::upgrade_telemetry::complete_startup(
                upgrade_probe.as_ref(),
                startup_reporter.as_ref(),
                &config.data_dir,
            );

            let console_fut = axum::serve(
                listener,
                merged.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal);

            tokio::try_join!(console_fut, ingress_fut)?;
        }
    }

    info!("Console API server exited");
    Ok(())
}
