// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! # temps-revenue
//!
//! Per-project revenue tracking for Temps.
//!
//! Users connect a payment provider (Stripe at launch) by:
//! 1. Creating an integration (we generate a unique webhook URL).
//! 2. Pasting the webhook URL into the provider's dashboard.
//! 3. Pasting the provider's signing secret back into Temps.
//!
//! We never call the provider's API. All data flows inbound via verified
//! webhook events and is normalized into a provider-agnostic model so
//! additional providers can be added with a new `RevenueProvider` impl.

pub mod error;
pub mod handlers;
pub mod plugin;
pub mod providers;
pub mod service;

pub use error::RevenueError;
pub use plugin::RevenuePlugin;
pub use providers::{
    LemonSqueezyConfig, MeteredMode, NormalizedEvent, NormalizedEventType, ProviderConfig,
    ProviderError, ProviderRegistry, RevenueProvider, StripeConfig, SubscriptionStatus,
};
pub use service::{
    AnalyticsError, CreateIntegrationInput, CustomerMovement, ImportOutcome, ImportRowError,
    IngestOutcome, IntegrationView, MetricsSummary, MrrBucket, RecentEvent,
    RevenueAnalyticsService, RevenueImportService, RevenueIngestionService,
    RevenueIntegrationService,
};
