// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod analytics;
pub mod api_traffic;
pub mod channel;
/// Cookieless visitor ids (ADR 109 D3).
pub mod cookieless;
pub mod datacenter;
pub mod global;
pub mod global_handler;
pub mod handler;
pub mod identity_settings;
pub mod ingest_keys;
pub mod plugin;
pub mod retention;
pub mod traits;
pub mod types;
pub mod visitor_audit;
pub mod visitor_identity;

#[cfg(test)]
pub mod testing;

// Re-export main types, service, and plugin
pub use analytics::{validate_event_property_key, AnalyticsService, MAX_EVENT_PROPERTY_KEY_LEN};
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

pub mod activity;
