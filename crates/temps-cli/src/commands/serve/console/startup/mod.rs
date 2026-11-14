// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `start_console_api`, as the ordered steps it runs.
//!
//! Startup order is load-bearing, so each step is its own function and
//! [`start_console_api`] calls them in sequence:
//!
//! 1. [`validate::validate_deps`]: Docker, GeoLite2 and the logs directory.
//! 2. [`core_services::register_core_services`]: what every plugin resolves.
//! 3. [`plugins::register_plugins`]: every plugin, in dependency order.
//! 4. [`init::initialize_plugins`]: all plugins register their services, then
//!    all initialise; `ready_signal` fires.
//! 5. [`bootstrap::bootstrap_after_init`]: telemetry, initial admin, SSO and
//!    Cloud enrollment.
//! 6. [`background::start_background_services`] and
//!    [`nodes::start_node_control`]: background loops and node routes.
//! 7. [`router::build_router`]: the routers each listener serves.
//! 8. [`serve::serve`]: bind the listeners and serve until shutdown.

use super::*;

mod ai_tools;
mod background;
mod bootstrap;
mod core_services;
mod init;
mod nodes;
mod plugins;
mod router;
mod serve;
mod traefik_adapters;
mod validate;

use ai_tools::install_ai_api_tools;
use background::start_background_services;
use bootstrap::{bootstrap_after_init, PostInit};
use core_services::{register_core_services, CoreServiceHandles, CoreServices};
use init::initialize_plugins;
use nodes::start_node_control;
use plugins::{register_plugins, PluginInputs};
use router::{build_router, ConsoleRouters, RouterInputs};
use serve::{serve, ServeInputs};
use traefik_adapters::{AlarmServiceDriftSink, TraefikTlsProvisioner};
use validate::validate_deps;

/// Initialize and start the console API server
pub async fn start_console_api(params: ConsoleApiParams) -> anyhow::Result<()> {
    let ConsoleApiParams {
        upgrade_probe,
        db,
        config,
        cookie_crypto,
        encryption_service,
        route_table,
        runtime_context,
        queue,
        ready_signal,
        additional_templates,
        on_demand_waker,
        activator,
        local_drivers,
        extra_plugins,
        admin_gate_service: provided_admin_gate_service,
        admin_gate_handle: provided_admin_gate_handle,
        retention_resolver_slot,
        project_ip_gate_slot,
        request_policy_gate_slot,
        overlay_dns_slot,
        dns_policy_slot,
        update_status,
        self_updater,
        traefik_discovery,
        external_plugin_registry,
        profile,
        startup_state,
    } = params;

    // Count panics for the anonymous `error_summary` telemetry event. Only
    // the sanitized source location (crate-relative file:line) is recorded —
    // never the panic message, which can embed user data. Chains to the
    // previous hook so normal backtrace printing is unaffected. Task panics
    // don't kill the process, so they are flushed by the summary task below;
    // a fatal main-thread panic may be lost, which is acceptable for v1.
    {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            temps_core::error_metrics::record_panic(panic_info.location());
            previous_hook(panic_info);
        }));
    }

    // Readiness flag for the `/readyz` probe. Starts `false` (not ready) and is
    // flipped to `true` immediately before the listeners begin serving — after
    // routers, middleware, and the admin gate are all built, not merely after
    // plugin init. The health router (mounted on the public surface below)
    // reads this so a supervisor or the split-topology upgrade gate can tell
    // "warming up" (503) from "serving" (200) for the process's lifetime.
    //
    // This is deliberately a *later* point than `ready_signal` below now fires
    // at (see that call site) — `/readyz` needs "actually able to serve a
    // request", `ready_signal` needs only "plugin two-phase init is done".
    let ready_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (docker_handle, docker_available) =
        validate_deps(profile, &config, &db, &encryption_service).await?;

    // Initialize plugin manager
    let mut plugin_manager = PluginManager::new();

    let core_handles = register_core_services(
        plugin_manager.service_context(),
        CoreServices {
            db: db.clone(),
            config: config.clone(),
            encryption_service: encryption_service.clone(),
            cookie_crypto: cookie_crypto.clone(),
            docker_handle: docker_handle.clone(),
            docker_available,
            profile,
            route_table: route_table.clone(),
            runtime_context,
            retention_resolver_slot,
            project_ip_gate_slot,
            request_policy_gate_slot: request_policy_gate_slot.clone(),
            overlay_dns_slot,
            dns_policy_slot,
            update_status,
            self_updater,
            traefik_discovery: traefik_discovery.clone(),
            additional_templates,
            on_demand_waker,
            activator,
            local_drivers,
        },
    )
    .await?;

    register_plugins(
        &mut plugin_manager,
        PluginInputs {
            config: config.clone(),
            db: db.clone(),
            queue,
            profile,
            docker_available,
            external_plugin_registry,
            extra_plugins,
        },
    )
    .await?;

    initialize_plugins(&mut plugin_manager, ready_signal).await?;

    let service_context = plugin_manager.service_context();
    let post_init = bootstrap_after_init(service_context, &db, &config).await?;
    start_background_services(
        service_context,
        &db,
        &config,
        &docker_handle,
        &traefik_discovery,
    )
    .await;
    let node_routes = start_node_control(service_context, &db);

    let routers = build_router(
        &plugin_manager,
        RouterInputs {
            db: db.clone(),
            config: config.clone(),
            encryption_service,
            route_table,
            request_policy_gate_slot,
            core_handles,
            node_routes,
            provided_admin_gate_service,
            provided_admin_gate_handle,
            ready_flag: ready_flag.clone(),
        },
    )
    .await?;

    serve(
        &plugin_manager,
        routers,
        ServeInputs {
            db,
            config,
            cookie_crypto,
            ready_flag,
            post_init,
            upgrade_probe,
            startup_state,
        },
    )
    .await
}
