// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod handlers;
pub mod plugin;
pub mod providers;
pub mod sentry;
pub mod services;

// Re-export main types but not the types modules to avoid ambiguity
pub use handlers::handler;
pub use providers::*;
pub use sentry::{
    DSNService, Envelope, EnvelopeError, EnvelopeItem, SentryIngestionService,
    SENTRY_TUNNEL_ROUTE_PATH,
};
pub use services::*;

// Export plugin
pub use plugin::ErrorTrackingPlugin;
