// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keeps a [`SettingsRetentionResolver`] in step with the `settings` row.

use std::sync::Arc;
use std::time::Duration;

use temps_core::SettingsRetentionResolver;
use tracing::warn;

use crate::ConfigService;

/// How often the resolver re-reads `observability_retention`. A change made
/// in the settings page reaches newly ingested ClickHouse rows within this
/// window, without a restart.
pub const RETENTION_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Build a [`SettingsRetentionResolver`] loaded from the current settings, and
/// spawn a task that refreshes it every [`RETENTION_REFRESH_INTERVAL`].
///
/// The initial load happens before this returns, so the first ClickHouse
/// insert is already stamped with the configured window. If the settings
/// cannot be read, the resolver starts from the table defaults (which are also
/// the settings defaults) and the refresh task picks up the real values once
/// the database answers; a failed refresh keeps the last known values.
pub async fn settings_retention_resolver(
    config: Arc<ConfigService>,
) -> Arc<SettingsRetentionResolver> {
    let resolver = Arc::new(SettingsRetentionResolver::new());
    refresh(&config, &resolver).await;

    let task_resolver = resolver.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(RETENTION_REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await; // the first tick fires immediately; already loaded
        loop {
            interval.tick().await;
            refresh(&config, &task_resolver).await;
        }
    });

    resolver
}

async fn refresh(config: &ConfigService, resolver: &SettingsRetentionResolver) {
    match config.get_settings().await {
        Ok(settings) => resolver.apply(&settings.observability_retention),
        Err(error) => warn!(
            %error,
            "Could not read observability_retention; ClickHouse rows keep the last known \
             retention until the next refresh"
        ),
    }
}
