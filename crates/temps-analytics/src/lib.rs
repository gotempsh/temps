// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod analytics;
pub mod api_traffic;
pub mod channel;
pub mod handler;
pub mod ingest_keys;
pub mod plugin;
pub mod traits;
pub mod types;

#[cfg(test)]
pub mod testing;

// Re-export main types, service, and plugin
pub use analytics::AnalyticsService;
pub use channel::{extract_referrer_hostname, get_channel, parse_utm_params, Channel, UtmParams};
pub use ingest_keys::{
    extract_analytics_key, is_origin_allowed, resolve_keyed_ingest_scope, AnalyticsIngestKey,
    AnalyticsIngestKeyError, AnalyticsIngestKeyService, AnalyticsIngestRateLimiter,
    ResolvedIngestScope, ANALYTICS_INGEST_KEY_HEADER, ANALYTICS_INGEST_KEY_PREFIX,
    ANALYTICS_INGEST_KEY_QUERY_PARAM,
};
pub use plugin::AnalyticsPlugin;
pub use traits::Analytics;
pub use types::*;
