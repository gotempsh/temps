// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod analytics;
pub mod api_traffic;
pub mod channel;
pub mod handler;
pub mod plugin;
pub mod traits;
pub mod types;

#[cfg(test)]
pub mod testing;

// Re-export main types, service, and plugin
pub use analytics::AnalyticsService;
pub use channel::{extract_referrer_hostname, get_channel, parse_utm_params, Channel, UtmParams};
pub use plugin::AnalyticsPlugin;
pub use traits::Analytics;
pub use types::*;
