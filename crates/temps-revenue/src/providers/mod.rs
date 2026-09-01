// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Provider-agnostic webhook verification and event normalization.
//!
//! Adding a new provider = implement [`RevenueProvider`] and register it
//! in [`ProviderRegistry::default_registry`]. Nothing else in the crate
//! is provider-specific.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

pub mod stripe;

pub use stripe::StripeProvider;

/// Normalized subscription status used across all providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Trialing,
    Active,
    PastDue,
    Canceled,
    Unpaid,
    Incomplete,
}

impl SubscriptionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trialing => "trialing",
            Self::Active => "active",
            Self::PastDue => "past_due",
            Self::Canceled => "canceled",
            Self::Unpaid => "unpaid",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Normalized event types. Every provider must map its raw events into
/// this vocabulary. Unknown events are dropped, not errored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizedEventType {
    ChargeSucceeded,
    ChargeRefunded,
    SubscriptionCreated,
    SubscriptionUpdated,
    SubscriptionCanceled,
    CustomerCreated,
    InvoicePaid,
    /// Synthetic event emitted per invoice line to drive the invoice-based
    /// MRR timeseries. Carries `mrr_minor` = `amount / period_days * 30`
    /// with `occurred_at = period_start`, so the MRR query can rebuild a
    /// historical curve for metered/tiered/hybrid subscriptions from
    /// invoice history alone. Not surfaced in Stripe; emitted by the
    /// ingestion layer when parsing `invoice.paid`.
    MrrRealized,
}

impl NormalizedEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ChargeSucceeded => "charge.succeeded",
            Self::ChargeRefunded => "charge.refunded",
            Self::SubscriptionCreated => "subscription.created",
            Self::SubscriptionUpdated => "subscription.updated",
            Self::SubscriptionCanceled => "subscription.canceled",
            Self::CustomerCreated => "customer.created",
            Self::InvoicePaid => "invoice.paid",
            Self::MrrRealized => "mrr.realized",
        }
    }
}

/// A provider-normalized event ready for persistence.
///
/// Amounts are always in the currency's minor units (cents for USD/EUR,
/// whole yen for JPY, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NormalizedEvent {
    /// Stable provider-assigned ID for this event (e.g. Stripe's `evt_...`).
    /// Used as the idempotency key for ingestion.
    pub provider_event_id: String,
    pub event_type: NormalizedEventType,
    /// Opaque customer reference scoped to this provider.
    pub customer_ref: Option<String>,
    /// Opaque subscription reference scoped to this provider. Set for
    /// subscription and invoice events.
    pub subscription_ref: Option<String>,
    pub subscription_status: Option<SubscriptionStatus>,
    /// Monthly-normalized MRR for subscription events (minor units).
    pub mrr_minor: Option<i64>,
    /// Charge amount or invoice total for charge/invoice events.
    pub amount_minor: Option<i64>,
    /// ISO-4217 3-letter code (lowercase, per Stripe convention).
    pub currency: Option<String>,
    pub occurred_at: DateTime<Utc>,
    /// Opaque price/SKU reference (Stripe price_id, LemonSqueezy
    /// variant_id, …). Used by the integration allowlist filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_id: Option<String>,
    /// Opaque product reference (Stripe product_id, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    /// Raw payload kept for audit/debug. Never returned over the API.
    pub raw: serde_json::Value,
}

/// How to treat metered-billing subscriptions when computing MRR.
///
/// * `DeriveFromInvoices` (default): ignore the subscription row's
///   `mrr_minor` for metered items and rely on the per-invoice
///   [`NormalizedEventType::MrrRealized`] events instead. Correct for
///   pure-metered, hybrid, tiered, and flat — recommended.
/// * `UseSubscription`: trust whatever MRR the subscription parser
///   returns (0 for metered). Legacy behavior.
/// * `Ignore`: drop metered subscriptions from MRR entirely.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeteredMode {
    #[default]
    DeriveFromInvoices,
    UseSubscription,
    Ignore,
}

/// Provider-specific integration settings persisted in
/// `revenue_integrations.config`.
///
/// The tag is the lowercase provider name, so adding a new provider
/// means adding a new variant and the existing rows are untouched.
/// Old rows (pre-config) and rows with `NULL` config are treated as
/// "accept all events, no filtering" via [`ProviderConfig::default_for`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ProviderConfig {
    Stripe(StripeConfig),
    LemonSqueezy(LemonSqueezyConfig),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StripeConfig {
    /// Only events tagged with one of these Stripe price IDs are ingested.
    /// Empty = accept all prices.
    #[serde(default)]
    pub price_allowlist: Vec<String>,
    /// Only events tagged with one of these Stripe product IDs are
    /// ingested. Empty = accept all products. Combined with
    /// `price_allowlist` via OR — if either list has a match, accept.
    #[serde(default)]
    pub product_allowlist: Vec<String>,
    /// When an allowlist is set, should we still ingest charges that
    /// lack a price reference (e.g. standalone `charge.succeeded` without
    /// a subscription)?  Default true — charges don't belong to a SKU.
    #[serde(default = "default_true")]
    pub include_unpriced_charges: bool,
    /// How to compute MRR for metered / tiered / hybrid subscriptions.
    #[serde(default)]
    pub metered_mode: MeteredMode,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LemonSqueezyConfig {
    #[serde(default)]
    pub variant_allowlist: Vec<String>,
    #[serde(default)]
    pub product_allowlist: Vec<String>,
}

impl ProviderConfig {
    /// Deserialize from the JSONB column. `None` + malformed JSON both
    /// yield `None`, which the filter layer treats as "accept everything"
    /// for forward/backward compatibility.
    pub fn from_value(value: Option<&serde_json::Value>) -> Option<Self> {
        let raw = value?;
        serde_json::from_value::<Self>(raw.clone()).ok()
    }

    /// Does this event pass the configured allowlist?
    ///
    /// Returns `true` when:
    ///   * config is absent (accept-all default),
    ///   * provider doesn't match the event's provider (shouldn't happen
    ///     in practice since one integration = one provider — fail open),
    ///   * allowlists are empty (no filtering requested),
    ///   * the event carries a matching price_id OR product_id,
    ///   * the event has no price/product AND the config allows unpriced
    ///     charges for that event type.
    pub fn accepts(&self, event: &NormalizedEvent) -> bool {
        match self {
            ProviderConfig::Stripe(cfg) => cfg.accepts(event),
            ProviderConfig::LemonSqueezy(cfg) => cfg.accepts(event),
        }
    }

    /// Metered-mode resolution — only Stripe expresses this today.
    pub fn metered_mode(&self) -> MeteredMode {
        match self {
            ProviderConfig::Stripe(cfg) => cfg.metered_mode,
            ProviderConfig::LemonSqueezy(_) => MeteredMode::default(),
        }
    }
}

impl StripeConfig {
    fn accepts(&self, event: &NormalizedEvent) -> bool {
        if self.price_allowlist.is_empty() && self.product_allowlist.is_empty() {
            return true;
        }

        let price_match = event
            .price_id
            .as_deref()
            .map(|p| self.price_allowlist.iter().any(|allowed| allowed == p))
            .unwrap_or(false);
        let product_match = event
            .product_id
            .as_deref()
            .map(|p| self.product_allowlist.iter().any(|allowed| allowed == p))
            .unwrap_or(false);

        if price_match || product_match {
            return true;
        }

        // Event has no SKU attribution. Decide by event type:
        //   * customer.created has no price — always accept, it's a
        //     dimension not a fact.
        //   * charge.succeeded / charge.refunded only if the operator
        //     opted into unpriced charges.
        //   * subscription.* / invoice.* without a price: rare (Stripe
        //     always emits items[]) but reject — the allowlist is the
        //     whole point of this config.
        match event.event_type {
            NormalizedEventType::CustomerCreated => true,
            NormalizedEventType::ChargeSucceeded | NormalizedEventType::ChargeRefunded => {
                event.price_id.is_none()
                    && event.product_id.is_none()
                    && self.include_unpriced_charges
            }
            _ => false,
        }
    }
}

impl LemonSqueezyConfig {
    fn accepts(&self, event: &NormalizedEvent) -> bool {
        if self.variant_allowlist.is_empty() && self.product_allowlist.is_empty() {
            return true;
        }
        let variant_match = event
            .price_id
            .as_deref()
            .map(|p| self.variant_allowlist.iter().any(|allowed| allowed == p))
            .unwrap_or(false);
        let product_match = event
            .product_id
            .as_deref()
            .map(|p| self.product_allowlist.iter().any(|allowed| allowed == p))
            .unwrap_or(false);
        variant_match
            || product_match
            || matches!(event.event_type, NormalizedEventType::CustomerCreated)
    }
}

/// Errors raised while verifying and parsing an inbound webhook.
///
/// All variants map to HTTP 400 at the ingestion handler; the specific
/// reason is logged but not leaked in the response body.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Missing required signature header: {header}")]
    MissingHeader { header: &'static str },

    #[error("Malformed signature header")]
    MalformedHeader,

    #[error("Signature verification failed")]
    InvalidSignature,

    #[error("Webhook timestamp outside the allowed tolerance")]
    ReplayExpired,

    #[error("Malformed webhook payload: {reason}")]
    MalformedPayload { reason: String },
}

/// Each concrete provider implements this.
pub trait RevenueProvider: Send + Sync {
    /// Short machine name used in URLs (e.g. "stripe"). Must be a
    /// URL-safe ASCII slug and unique across registered providers.
    fn name(&self) -> &'static str;

    /// Human-readable display name for the UI.
    fn display_name(&self) -> &'static str;

    /// Events we ask the user to subscribe to in their dashboard.
    fn recommended_event_filter(&self) -> &[&'static str];

    /// Verifies the signature and parses the raw body into zero or more
    /// normalized events. Unknown event types are filtered out (returned
    /// as an empty list, not an error).
    fn verify_and_parse(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        signing_secret: &str,
    ) -> Result<Vec<NormalizedEvent>, ProviderError>;
}

/// Lookup table for all registered providers.
#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Arc<HashMap<&'static str, Arc<dyn RevenueProvider>>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn RevenueProvider>>) -> Self {
        let mut map: HashMap<&'static str, Arc<dyn RevenueProvider>> = HashMap::new();
        for p in providers {
            map.insert(p.name(), p);
        }
        Self {
            providers: Arc::new(map),
        }
    }

    /// Default registry containing every provider built into Temps.
    pub fn default_registry() -> Self {
        Self::new(vec![Arc::new(StripeProvider::default())])
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn RevenueProvider>> {
        self.providers.get(name).cloned()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.providers.keys().copied().collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}
