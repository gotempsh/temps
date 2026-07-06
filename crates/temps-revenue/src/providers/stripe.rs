//! Stripe webhook verification and event normalization.
//!
//! We verify the `Stripe-Signature` header (documented format:
//! `t=<unix_ts>,v1=<hex_sig>[,v1=<hex_sig>...]`) per Stripe's published
//! algorithm: HMAC-SHA256 over `"{t}.{raw_body}"` using the endpoint's
//! signing secret, with any `v1` entry accepted (Stripe sends multiple
//! during secret rotation).
//!
//! No network calls are made here — the whole crate is offline-only.

use chrono::{DateTime, Duration, TimeZone, Utc};
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use super::{
    NormalizedEvent, NormalizedEventType, ProviderError, RevenueProvider, SubscriptionStatus,
};

type HmacSha256 = Hmac<Sha256>;

/// Tolerance window for timestamp replay protection. Matches Stripe's
/// recommended 5-minute default.
const DEFAULT_TOLERANCE_SECONDS: i64 = 300;

const STRIPE_SIGNATURE_HEADER: &str = "stripe-signature";

/// Events we ask users to subscribe to. Unknown events are tolerated
/// but noisy — keeping the list tight reduces ingestion volume.
const STRIPE_RECOMMENDED_EVENTS: &[&str] = &[
    "charge.succeeded",
    "charge.refunded",
    "customer.created",
    "customer.subscription.created",
    "customer.subscription.updated",
    "customer.subscription.deleted",
    "invoice.paid",
];

pub struct StripeProvider {
    tolerance_seconds: i64,
    /// Injectable clock — tests override to exercise the replay window
    /// without touching the real wall clock.
    now: fn() -> i64,
}

impl Default for StripeProvider {
    fn default() -> Self {
        Self {
            tolerance_seconds: DEFAULT_TOLERANCE_SECONDS,
            now: default_now,
        }
    }
}

fn default_now() -> i64 {
    Utc::now().timestamp()
}

impl StripeProvider {
    #[cfg(test)]
    fn with_clock(now: fn() -> i64) -> Self {
        Self {
            tolerance_seconds: DEFAULT_TOLERANCE_SECONDS,
            now,
        }
    }

    fn verify_signature(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        signing_secret: &str,
    ) -> Result<i64, ProviderError> {
        let header_val = headers
            .get(STRIPE_SIGNATURE_HEADER)
            .ok_or(ProviderError::MissingHeader {
                header: "Stripe-Signature",
            })?
            .to_str()
            .map_err(|_| ProviderError::MalformedHeader)?;

        let (timestamp, signatures) = parse_signature_header(header_val)?;

        // Enforce tolerance before doing any crypto — cheap rejection of
        // stale replays, and prevents attackers from using the HMAC check
        // as a timing oracle on signatures.
        let now = (self.now)();
        if (now - timestamp).abs() > self.tolerance_seconds {
            return Err(ProviderError::ReplayExpired);
        }

        let signed_payload = format!("{}.", timestamp);
        let mut mac = HmacSha256::new_from_slice(signing_secret.as_bytes())
            .map_err(|_| ProviderError::InvalidSignature)?;
        mac.update(signed_payload.as_bytes());
        mac.update(body);
        let expected = mac.finalize().into_bytes();
        let expected_bytes: &[u8] = &expected;

        // Constant-time compare against any v1 signature Stripe sent.
        let mut matched = false;
        for sig_hex in &signatures {
            let Ok(sig_bytes) = hex::decode(sig_hex) else {
                continue;
            };
            if sig_bytes.len() != expected_bytes.len() {
                continue;
            }
            if sig_bytes.ct_eq(expected_bytes).into() {
                matched = true;
            }
        }

        if matched {
            Ok(timestamp)
        } else {
            Err(ProviderError::InvalidSignature)
        }
    }
}

impl RevenueProvider for StripeProvider {
    fn name(&self) -> &'static str {
        "stripe"
    }

    fn display_name(&self) -> &'static str {
        "Stripe"
    }

    fn recommended_event_filter(&self) -> &[&'static str] {
        STRIPE_RECOMMENDED_EVENTS
    }

    fn verify_and_parse(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        signing_secret: &str,
    ) -> Result<Vec<NormalizedEvent>, ProviderError> {
        self.verify_signature(headers, body, signing_secret)?;

        let raw: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| ProviderError::MalformedPayload {
                reason: format!("invalid JSON: {}", e),
            })?;

        let events = parse_stripe_event(&raw)?;
        // Unknown type returns an empty list — forward-compatible with
        // Stripe shipping new event types.
        Ok(events)
    }
}

/// Parses the `Stripe-Signature` header into `(timestamp, [v1 sig hex strings])`.
///
/// Stripe format: `t=1492774577,v1=abc...,v1=def...` (additional `v0`
/// entries are ignored).
fn parse_signature_header(header: &str) -> Result<(i64, Vec<String>), ProviderError> {
    let mut timestamp: Option<i64> = None;
    let mut signatures: Vec<String> = Vec::new();

    for part in header.split(',') {
        let mut kv = part.splitn(2, '=');
        let key = kv.next().ok_or(ProviderError::MalformedHeader)?.trim();
        let value = kv.next().ok_or(ProviderError::MalformedHeader)?.trim();
        match key {
            "t" => {
                timestamp = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| ProviderError::MalformedHeader)?,
                );
            }
            "v1" => signatures.push(value.to_string()),
            _ => {} // ignore v0 and future schemes
        }
    }

    let timestamp = timestamp.ok_or(ProviderError::MalformedHeader)?;
    if signatures.is_empty() {
        return Err(ProviderError::MalformedHeader);
    }
    Ok((timestamp, signatures))
}

/// Converts a raw Stripe event JSON into one or more normalized events.
///
/// Most Stripe events map 1:1 to a single normalized event. `invoice.paid`
/// is special: it emits the original invoice event **plus** one
/// [`NormalizedEventType::MrrRealized`] event per invoice line so the MRR
/// timeseries can be rebuilt from invoice history alone (the trick that
/// makes metered / tiered / hybrid subscriptions work correctly).
///
/// Returns an empty list for unknown event types (forward-compatibility).
/// Returns `Err` only when the payload is structurally broken.
fn parse_stripe_event(raw: &serde_json::Value) -> Result<Vec<NormalizedEvent>, ProviderError> {
    let id =
        raw.get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::MalformedPayload {
                reason: "missing event id".into(),
            })?;
    let stripe_type = raw.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
        ProviderError::MalformedPayload {
            reason: "missing event type".into(),
        }
    })?;
    let created = raw.get("created").and_then(|v| v.as_i64()).ok_or_else(|| {
        ProviderError::MalformedPayload {
            reason: "missing event created timestamp".into(),
        }
    })?;
    let occurred_at =
        Utc.timestamp_opt(created, 0)
            .single()
            .ok_or_else(|| ProviderError::MalformedPayload {
                reason: format!("invalid created timestamp {}", created),
            })?;

    let object = raw
        .pointer("/data/object")
        .ok_or_else(|| ProviderError::MalformedPayload {
            reason: "missing data.object".into(),
        })?;

    let (event_type, specifics) = match stripe_type {
        "charge.succeeded" => (NormalizedEventType::ChargeSucceeded, parse_charge(object)),
        "charge.refunded" => (NormalizedEventType::ChargeRefunded, parse_charge(object)),
        "customer.created" => (NormalizedEventType::CustomerCreated, parse_customer(object)),
        "customer.subscription.created" => (
            NormalizedEventType::SubscriptionCreated,
            parse_subscription(object),
        ),
        "customer.subscription.updated" => (
            NormalizedEventType::SubscriptionUpdated,
            parse_subscription(object),
        ),
        "customer.subscription.deleted" => (
            NormalizedEventType::SubscriptionCanceled,
            parse_subscription(object),
        ),
        "invoice.paid" => (NormalizedEventType::InvoicePaid, parse_invoice(object)),
        _ => return Ok(Vec::new()),
    };

    let primary = NormalizedEvent {
        provider_event_id: id.to_string(),
        event_type,
        customer_ref: specifics.customer_ref.clone(),
        subscription_ref: specifics.subscription_ref.clone(),
        subscription_status: specifics.subscription_status,
        mrr_minor: specifics.mrr_minor,
        amount_minor: specifics.amount_minor,
        currency: specifics.currency.clone(),
        occurred_at,
        price_id: specifics.price_id.clone(),
        product_id: specifics.product_id.clone(),
        raw: raw.clone(),
    };

    let mut out = Vec::with_capacity(1);
    out.push(primary);

    // Fan out invoice.paid into per-line mrr.realized events. Uses the
    // line's own price/product for attribution so the allowlist filter
    // works line-by-line even when the invoice bundles multiple SKUs.
    if matches!(event_type, NormalizedEventType::InvoicePaid) {
        for (idx, realized) in invoice_mrr_realized_events(id, object)
            .into_iter()
            .enumerate()
        {
            let _ = idx;
            out.push(realized);
        }
    }

    Ok(out)
}

#[derive(Default)]
struct EventSpecifics {
    customer_ref: Option<String>,
    subscription_ref: Option<String>,
    subscription_status: Option<SubscriptionStatus>,
    mrr_minor: Option<i64>,
    amount_minor: Option<i64>,
    currency: Option<String>,
    price_id: Option<String>,
    product_id: Option<String>,
}

fn parse_charge(object: &serde_json::Value) -> EventSpecifics {
    EventSpecifics {
        customer_ref: object
            .get("customer")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        amount_minor: object.get("amount").and_then(|v| v.as_i64()),
        currency: object
            .get("currency")
            .and_then(|v| v.as_str())
            .map(str::to_lowercase),
        ..Default::default()
    }
}

fn parse_customer(object: &serde_json::Value) -> EventSpecifics {
    EventSpecifics {
        customer_ref: object
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ..Default::default()
    }
}

fn parse_invoice(object: &serde_json::Value) -> EventSpecifics {
    // Pick the first line's price/product for top-level attribution.
    // Multi-line invoices get exploded into MrrRealized events downstream
    // which carry per-line SKUs, so losing detail here is fine.
    let (price_id, product_id) = object
        .pointer("/lines/data")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .map(|line| extract_price_and_product(line.get("price")))
        .unwrap_or((None, None));

    EventSpecifics {
        customer_ref: object
            .get("customer")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        subscription_ref: object
            .get("subscription")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        amount_minor: object.get("amount_paid").and_then(|v| v.as_i64()),
        currency: object
            .get("currency")
            .and_then(|v| v.as_str())
            .map(str::to_lowercase),
        price_id,
        product_id,
        ..Default::default()
    }
}

fn parse_subscription(object: &serde_json::Value) -> EventSpecifics {
    let status = object
        .get("status")
        .and_then(|v| v.as_str())
        .and_then(map_subscription_status);

    let currency = object
        .get("currency")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);

    let mrr_minor = compute_subscription_mrr(object);

    // First item's price/product as the subscription's "primary" SKU. This
    // is a reasonable summary for the allowlist filter; a subscription
    // that mixes allowlisted and non-allowlisted prices is an edge case
    // users should split into separate subscriptions anyway.
    let (price_id, product_id) = object
        .pointer("/items/data")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .map(|item| extract_price_and_product(item.get("price")))
        .unwrap_or((None, None));

    EventSpecifics {
        customer_ref: object
            .get("customer")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        subscription_ref: object
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        subscription_status: status,
        mrr_minor,
        currency,
        amount_minor: None,
        price_id,
        product_id,
    }
}

/// Extract `(price_id, product_id)` from a Stripe price object. Handles
/// both expanded (`price.product = { "id": "prod_..." }`) and unexpanded
/// (`price.product = "prod_..."`) forms.
fn extract_price_and_product(
    price: Option<&serde_json::Value>,
) -> (Option<String>, Option<String>) {
    let Some(price) = price else {
        return (None, None);
    };
    let price_id = price.get("id").and_then(|v| v.as_str()).map(str::to_string);
    let product_id = price.get("product").and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(_) => v.get("id").and_then(|id| id.as_str()).map(str::to_string),
        _ => None,
    });
    (price_id, product_id)
}

fn map_subscription_status(status: &str) -> Option<SubscriptionStatus> {
    match status {
        "trialing" => Some(SubscriptionStatus::Trialing),
        "active" => Some(SubscriptionStatus::Active),
        "past_due" => Some(SubscriptionStatus::PastDue),
        "canceled" => Some(SubscriptionStatus::Canceled),
        "unpaid" => Some(SubscriptionStatus::Unpaid),
        "incomplete" | "incomplete_expired" => Some(SubscriptionStatus::Incomplete),
        _ => None,
    }
}

/// Sum each subscription item normalized to a per-month figure.
///
/// Stripe pricing intervals: day / week / month / year. We project each
/// into monthly dollars using:
///   - month -> ×1/interval_count
///   - year -> ÷12 (×1/interval_count)
///   - week -> ×(52/12) ≈ 4.333
///   - day  -> ×(365/12) ≈ 30.417
///
/// Canceled subs contribute 0 so state-table MRR reflects the drop.
///
/// Metered / tiered lines (no `unit_amount` on the price) are **skipped
/// per-item** rather than aborting the whole subscription — so hybrid
/// subs (flat base + metered add-on) still report the known flat MRR.
/// The skipped portion is recovered via invoice-based `mrr.realized`
/// events when `MeteredMode::DeriveFromInvoices` is set.
fn compute_subscription_mrr(subscription: &serde_json::Value) -> Option<i64> {
    let status = subscription.get("status").and_then(|v| v.as_str());
    if matches!(status, Some("canceled") | Some("incomplete_expired")) {
        return Some(0);
    }

    let items = subscription.pointer("/items/data")?.as_array()?;

    let mut total: f64 = 0.0;
    for item in items {
        let quantity = item.get("quantity").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let Some(price) = item.get("price") else {
            continue;
        };
        let Some(unit_amount) = price.get("unit_amount").and_then(|v| v.as_f64()) else {
            // Metered / tiered — no fixed unit_amount. Invoice-based MRR
            // handles these.
            continue;
        };
        let Some(recurring) = price.get("recurring") else {
            continue;
        };
        let Some(interval) = recurring.get("interval").and_then(|v| v.as_str()) else {
            continue;
        };
        let interval_count = recurring
            .get("interval_count")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0)
            .max(1.0);

        let monthly = match interval {
            "month" => unit_amount / interval_count,
            "year" => unit_amount / (12.0 * interval_count),
            "week" => unit_amount * (52.0 / 12.0) / interval_count,
            "day" => unit_amount * (365.0 / 12.0) / interval_count,
            _ => continue,
        };
        total += monthly * quantity;
    }

    Some(total.round() as i64)
}

impl NormalizedEvent {
    /// Exposed for other crates (ingestion service) that need the
    /// occurred_at timestamp as a chrono DateTime.
    pub fn occurred_at_utc(&self) -> DateTime<Utc> {
        self.occurred_at
    }
}

/// 30-day month constant (in seconds) — matches the day×30 projection
/// used in the MRR recipe. Intentionally not 365/12 to keep arithmetic
/// tidy and consistent across billing cycles.
const SECONDS_PER_MRR_MONTH: f64 = 30.0 * 86_400.0;

/// Fan an `invoice.paid` out into per-line `mrr.realized` events.
///
/// For each billing line, compute the realized MRR as:
///     `mrr_minor = amount_minor / period_days * 30`
/// and emit one synthetic event with `occurred_at = period.start`.
///
/// The analytics MRR query reads these events so historical MRR curves
/// light up from invoice backfill alone (the webhook path adds new ones
/// as `invoice.paid` arrives live).
///
/// Skipped lines:
///   * proration lines (`proration: true`) — tiny periods spike the chart.
///   * lines with no period (`period_start == period_end`) — one-time.
///   * lines with zero or negative amount.
fn invoice_mrr_realized_events(
    invoice_id: &str,
    invoice: &serde_json::Value,
) -> Vec<NormalizedEvent> {
    let Some(lines) = invoice.pointer("/lines/data").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let currency = invoice
        .get("currency")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);
    let customer = invoice
        .get("customer")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let invoice_sub = invoice
        .get("subscription")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line
            .get("proration")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }

        let amount = line.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
        if amount <= 0 {
            continue;
        }

        let period_start = line
            .pointer("/period/start")
            .and_then(|v| v.as_i64())
            .and_then(|s| Utc.timestamp_opt(s, 0).single());
        let period_end = line
            .pointer("/period/end")
            .and_then(|v| v.as_i64())
            .and_then(|s| Utc.timestamp_opt(s, 0).single());

        let (Some(start), Some(end)) = (period_start, period_end) else {
            continue;
        };
        let delta = end - start;
        if delta <= Duration::zero() {
            continue;
        }

        let seconds = delta.num_seconds() as f64;
        let mrr = ((amount as f64) * SECONDS_PER_MRR_MONTH / seconds).round() as i64;

        let (price_id, product_id) = extract_price_and_product(line.get("price"));
        let subscription_ref = line
            .get("subscription")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| invoice_sub.clone());

        out.push(NormalizedEvent {
            provider_event_id: format!("{}:mrr:{}", invoice_id, idx),
            event_type: NormalizedEventType::MrrRealized,
            customer_ref: customer.clone(),
            subscription_ref,
            subscription_status: None,
            mrr_minor: Some(mrr),
            amount_minor: Some(amount),
            currency: currency.clone(),
            occurred_at: start,
            price_id,
            product_id,
            raw: serde_json::json!({
                "source": "invoice_line",
                "invoice_id": invoice_id,
                "line_index": idx,
                "period": { "start": start, "end": end },
            }),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{KeyInit, Mac};

    const SECRET: &str = "whsec_test_secret_value";

    fn sign(ts: i64, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(format!("{}.", ts).as_bytes());
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn headers(sig: String) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("stripe-signature", sig.parse().unwrap());
        h
    }

    fn fixed_clock() -> i64 {
        1_700_000_000
    }

    fn provider() -> StripeProvider {
        StripeProvider::with_clock(fixed_clock)
    }

    #[test]
    fn rejects_missing_header() {
        let err = provider()
            .verify_and_parse(&HeaderMap::new(), b"{}", SECRET)
            .unwrap_err();
        assert!(matches!(err, ProviderError::MissingHeader { .. }));
    }

    #[test]
    fn rejects_invalid_signature() {
        let ts = fixed_clock();
        let body = br#"{"id":"evt_1","type":"charge.succeeded","created":1,"data":{"object":{}}}"#;
        let bogus = format!("t={},v1={}", ts, hex::encode([0u8; 32]));
        let err = provider()
            .verify_and_parse(&headers(bogus), body, SECRET)
            .unwrap_err();
        assert!(matches!(err, ProviderError::InvalidSignature));
    }

    #[test]
    fn rejects_expired_timestamp() {
        let ts = fixed_clock() - 10_000;
        let body = br#"{"id":"evt_1","type":"charge.succeeded","created":1,"data":{"object":{}}}"#;
        let sig = format!("t={},v1={}", ts, sign(ts, body));
        let err = provider()
            .verify_and_parse(&headers(sig), body, SECRET)
            .unwrap_err();
        assert!(matches!(err, ProviderError::ReplayExpired));
    }

    #[test]
    fn accepts_valid_charge_succeeded() {
        let ts = fixed_clock();
        let body = br#"{
            "id":"evt_123",
            "type":"charge.succeeded",
            "created":1700000000,
            "data":{"object":{"customer":"cus_abc","amount":4200,"currency":"USD"}}
        }"#;
        let sig = format!("t={},v1={}", ts, sign(ts, body));
        let events = provider()
            .verify_and_parse(&headers(sig), body, SECRET)
            .unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.provider_event_id, "evt_123");
        assert_eq!(e.event_type, NormalizedEventType::ChargeSucceeded);
        assert_eq!(e.customer_ref.as_deref(), Some("cus_abc"));
        assert_eq!(e.amount_minor, Some(4200));
        assert_eq!(e.currency.as_deref(), Some("usd"));
    }

    #[test]
    fn accepts_multiple_v1_signatures() {
        let ts = fixed_clock();
        let body = br#"{"id":"evt_1","type":"customer.created","created":1700000000,"data":{"object":{"id":"cus_1"}}}"#;
        let good = sign(ts, body);
        let sig = format!("t={},v1={},v1={}", ts, hex::encode([0u8; 32]), good);
        let events = provider()
            .verify_and_parse(&headers(sig), body, SECRET)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, NormalizedEventType::CustomerCreated);
    }

    #[test]
    fn ignores_unknown_event_types() {
        let ts = fixed_clock();
        let body = br#"{"id":"evt_1","type":"something.obscure","created":1700000000,"data":{"object":{}}}"#;
        let sig = format!("t={},v1={}", ts, sign(ts, body));
        let events = provider()
            .verify_and_parse(&headers(sig), body, SECRET)
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn normalizes_yearly_mrr() {
        let ts = fixed_clock();
        // $120/year subscription -> $10/month = 1000 minor.
        let body = br#"{
            "id":"evt_sub",
            "type":"customer.subscription.created",
            "created":1700000000,
            "data":{"object":{
                "id":"sub_1",
                "customer":"cus_1",
                "status":"active",
                "currency":"usd",
                "items":{"data":[{
                    "quantity":1,
                    "price":{"unit_amount":12000,"recurring":{"interval":"year","interval_count":1}}
                }]}
            }}
        }"#;
        let sig = format!("t={},v1={}", ts, sign(ts, body));
        let events = provider()
            .verify_and_parse(&headers(sig), body, SECRET)
            .unwrap();
        assert_eq!(events[0].mrr_minor, Some(1000));
    }

    #[test]
    fn normalizes_monthly_mrr_with_quantity() {
        let ts = fixed_clock();
        // 3 seats × $9 monthly = $27/mo = 2700 minor
        let body = br#"{
            "id":"evt_sub",
            "type":"customer.subscription.updated",
            "created":1700000000,
            "data":{"object":{
                "id":"sub_1","customer":"cus_1","status":"active","currency":"usd",
                "items":{"data":[{
                    "quantity":3,
                    "price":{"unit_amount":900,"recurring":{"interval":"month","interval_count":1}}
                }]}
            }}
        }"#;
        let sig = format!("t={},v1={}", ts, sign(ts, body));
        let events = provider()
            .verify_and_parse(&headers(sig), body, SECRET)
            .unwrap();
        assert_eq!(events[0].mrr_minor, Some(2700));
        assert_eq!(
            events[0].subscription_status,
            Some(SubscriptionStatus::Active)
        );
    }

    #[test]
    fn canceled_subscription_zeros_mrr() {
        let ts = fixed_clock();
        let body = br#"{
            "id":"evt_sub","type":"customer.subscription.deleted","created":1700000000,
            "data":{"object":{
                "id":"sub_1","customer":"cus_1","status":"canceled","currency":"usd",
                "items":{"data":[{"quantity":1,"price":{"unit_amount":1000,"recurring":{"interval":"month"}}}]}
            }}
        }"#;
        let sig = format!("t={},v1={}", ts, sign(ts, body));
        let events = provider()
            .verify_and_parse(&headers(sig), body, SECRET)
            .unwrap();
        assert_eq!(
            events[0].event_type,
            NormalizedEventType::SubscriptionCanceled
        );
        assert_eq!(events[0].mrr_minor, Some(0));
    }

    #[test]
    fn tampered_body_rejected() {
        let ts = fixed_clock();
        let body = br#"{"id":"evt_1","type":"customer.created","created":1700000000,"data":{"object":{"id":"cus_1"}}}"#;
        let tampered = br#"{"id":"evt_1","type":"customer.created","created":1700000000,"data":{"object":{"id":"cus_X"}}}"#;
        let sig = format!("t={},v1={}", ts, sign(ts, body));
        let err = provider()
            .verify_and_parse(&headers(sig), tampered, SECRET)
            .unwrap_err();
        assert!(matches!(err, ProviderError::InvalidSignature));
    }
}
