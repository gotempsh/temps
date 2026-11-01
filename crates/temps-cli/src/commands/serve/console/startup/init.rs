// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 4: run the plugins' two-phase start (every plugin registers its
//! services, then every plugin initialises), then release the proxy.

use super::*;

/// Initialise every registered plugin and fire `ready_signal` once the
/// project IP gate slot is resolved.
pub(super) async fn initialize_plugins(
    plugin_manager: &mut PluginManager,
    ready_signal: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    // Initialize all plugins
    debug!("Initializing plugins");
    if let Err(e) = plugin_manager.initialize_plugins().await {
        let error_msg = format!("{}", e);
        tracing::error!("❌ Plugin initialization FAILED");
        tracing::error!("Error: {}", error_msg);
        tracing::error!("Error details: {:?}", e);
        tracing::error!("");
        tracing::error!("Most common causes:");
        tracing::error!("  • Missing GeoLite2-City.mmdb file");
        tracing::error!("  • Database connection failed");
        tracing::error!("  • Service initialization error");
        tracing::error!("");
        tracing::error!("Check the error message above for details.");
        return Err(
            crate::commands::serve::startup_failure::ConsoleStartupError::PluginInitialization {
                reason: error_msg,
            }
            .into(),
        );
    }
    debug!("All plugins initialized successfully");

    // `project_ip_gate_slot` (see the security guardrail comment on
    // `ConsoleApiParams::project_ip_gate_slot`) is fully resolved at this
    // exact point: `ProxyPlugin::initialize` -- which claims the slot from
    // whatever `Arc<dyn ProjectIpGate>` an EE plugin registered in phase 1,
    // if any did -- already ran as part of `initialize_plugins()` above
    // (two-phase: ALL plugins register, THEN ALL plugins initialize). There
    // is nothing left between here and the end of this function that could
    // change that outcome, so signal it now rather than after routers,
    // middleware, the admin gate, and the TCP listener are also built.
    //
    // `commands/serve/mod.rs` holds the proxy's startup on this signal
    // specifically to close a P1 security race (PR #725): before this fix,
    // the wait was tied to the FULL console being ready (whatever that
    // happened to take), so every IP-restricted project was reachable by
    // any client for however long console startup took, on every boot.
    // Firing here means the wait now resolves as soon as the one thing it
    // actually depends on is done, deterministically, in the overwhelming
    // majority of cases -- the bounded timeout on the other end becomes a
    // backstop against a genuinely hung plugin `initialize()`, not the
    // expected path.
    if let Some(signal) = ready_signal {
        let _ = signal.send(());
        debug!("Project IP gate slot resolved; signaled ready_signal early");
    }

    Ok(())
}

