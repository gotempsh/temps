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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::serve::startup_failure::ConsoleStartupError;
    use temps_core::plugin::{PluginError, ServiceRegistrationContext};

    /// A plugin whose service registration always fails.
    struct FailingPlugin;

    impl TempsPlugin for FailingPlugin {
        fn name(&self) -> &'static str {
            "failing"
        }

        fn register_services<'a>(
            &'a self,
            _context: &'a ServiceRegistrationContext,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PluginError>> + Send + 'a>>
        {
            Box::pin(async {
                Err(PluginError::PluginRegistrationFailed {
                    plugin_name: "failing".to_string(),
                    error: "the settings table is missing".to_string(),
                })
            })
        }
    }

    #[tokio::test]
    async fn fires_ready_signal_once_every_plugin_has_initialised() {
        let mut plugin_manager = PluginManager::new();
        let (sender, receiver) = tokio::sync::oneshot::channel();

        initialize_plugins(&mut plugin_manager, Some(sender))
            .await
            .expect("an empty plugin set initialises");

        receiver
            .await
            .expect("ready_signal fires after a successful init");
    }

    #[tokio::test]
    async fn without_a_ready_signal_initialisation_still_succeeds() {
        let mut plugin_manager = PluginManager::new();
        initialize_plugins(&mut plugin_manager, None)
            .await
            .expect("ready_signal is optional");
    }

    #[tokio::test]
    async fn a_failed_plugin_is_a_typed_startup_error_and_the_proxy_stays_held() {
        let mut plugin_manager = PluginManager::new();
        plugin_manager.register_plugin(Box::new(FailingPlugin));
        let (sender, receiver) = tokio::sync::oneshot::channel();

        let error = initialize_plugins(&mut plugin_manager, Some(sender))
            .await
            .expect_err("a failing plugin fails initialisation");

        match error.downcast_ref::<ConsoleStartupError>() {
            Some(ConsoleStartupError::PluginInitialization { reason }) => {
                assert!(
                    reason.contains("failing") && reason.contains("the settings table is missing"),
                    "the reason names the plugin and the cause: {reason}"
                );
            }
            other => panic!("expected PluginInitialization, got {other:?}"),
        }
        // The signal is dropped unsent: the proxy must not be released by a
        // console that failed to start.
        assert!(receiver.await.is_err());
    }
}
