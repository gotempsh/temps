// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CSV-based backfill for migrating users.
//!
//! A user who migrates to Temps can export their existing subscriptions
//! and invoices from their payment provider's dashboard (no API keys
//! required) and upload the CSVs here. The webhook path remains the
//! source of truth for live updates; CSV imports only fill historical
//! gaps.
//!
//! Guarantees:
//!   * **Idempotent.** Re-uploading the same CSV produces no duplicate
//!     rows, because subscriptions and events are keyed by the provider's
//!     own stable IDs.
//!   * **Non-destructive.** Subscription rows are never downgraded: if
//!     the DB has a row newer than the CSV snapshot (e.g. a webhook
//!     already advanced it), we keep the DB row.
//!   * **Offline.** No network calls. Just CSV parsing + DB writes.
//!
//! See the handler module for the HTTP surface and expected columns.

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use csv::ReaderBuilder;
use sea_orm::{
    sea_query::OnConflict, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    DatabaseTransaction, EntityTrait, QueryFilter, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_entities::{
    revenue_customers_state, revenue_events, revenue_integrations::Model as IntegrationModel,
    revenue_subscriptions_state,
};
use tracing::{debug, info, warn};

use crate::error::RevenueError;
use crate::service::integration::RevenueIntegrationService;

/// Outcome of a single CSV import run, returned to the user so they can
/// see exactly what landed.
#[derive(Debug, Clone, Serialize)]
pub struct ImportOutcome {
    pub rows_read: usize,
    pub inserted: usize,
    pub updated: usize,
    pub skipped_stale: usize,
    pub skipped_invalid: usize,
    /// First 25 row-level errors (1-based row numbers) for UI display.
    /// Truncating keeps responses bounded on large files.
    pub errors: Vec<ImportRowError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportRowError {
    pub row: usize,
    pub reason: String,
}

const MAX_REPORTED_ERRORS: usize = 25;

pub struct RevenueImportService {
    db: Arc<DatabaseConnection>,
    integrations: Arc<RevenueIntegrationService>,
}

impl RevenueImportService {
    pub fn new(db: Arc<DatabaseConnection>, integrations: Arc<RevenueIntegrationService>) -> Self {
        Self { db, integrations }
    }

    /// Ingest a Stripe "Subscriptions" CSV export.
    ///
    /// Expected columns (Stripe dashboard defaults; spaces/underscores
    /// normalized to lowercase_dots):
    ///   * `id` — subscription ID (required)
    ///   * `customer` — customer ID (required)
    ///   * `status` — active / trialing / past_due / canceled / ...
    ///   * `currency` — 3-letter ISO code
    ///   * `plan.amount` — minor units per interval
    ///   * `plan.interval` — day / week / month / year
    ///   * `plan.interval_count` — optional integer, default 1
    ///   * `quantity` — optional integer, default 1
    ///   * `current_period_start` — unix ts or RFC3339, optional
    ///   * `created` — unix ts or RFC3339, optional (used when
    ///     `current_period_start` absent)
    ///   * `canceled_at` — unix ts or RFC3339, optional
    pub async fn import_subscriptions_csv(
        &self,
        project_id: i32,
        integration_id: i32,
        csv_bytes: &[u8],
    ) -> Result<ImportOutcome, RevenueError> {
        let integration = self.integrations.get(project_id, integration_id).await?;

        if integration.provider != "stripe" {
            return Err(RevenueError::Validation {
                message: format!(
                    "CSV import is currently only supported for Stripe (integration {} uses '{}')",
                    integration.id, integration.provider
                ),
            });
        }

        let mut outcome = ImportOutcome {
            rows_read: 0,
            inserted: 0,
            updated: 0,
            skipped_stale: 0,
            skipped_invalid: 0,
            errors: Vec::new(),
        };

        let mut reader = ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(csv_bytes);

        let txn = self.db.begin().await?;

        // Row enumeration starts at 2 (line 1 is the header).
        for (idx, record) in reader.deserialize::<SubscriptionRow>().enumerate() {
            let line = idx + 2;
            outcome.rows_read += 1;
            let row = match record {
                Ok(r) => r,
                Err(e) => {
                    outcome.skipped_invalid += 1;
                    push_error(&mut outcome.errors, line, format!("parse: {}", e));
                    continue;
                }
            };
            match upsert_subscription_from_csv(&txn, &integration, &row).await {
                Ok(UpsertOutcome::Inserted) => outcome.inserted += 1,
                Ok(UpsertOutcome::Updated) => outcome.updated += 1,
                Ok(UpsertOutcome::SkippedStale) => outcome.skipped_stale += 1,
                Ok(UpsertOutcome::SkippedInvalid(reason)) => {
                    outcome.skipped_invalid += 1;
                    push_error(&mut outcome.errors, line, reason);
                }
                Err(e) => {
                    // Abort the whole import on an actual DB failure —
                    // partial commits would confuse later imports.
                    warn!(
                        integration_id = integration.id,
                        line, error = %e, "subscription CSV import aborted"
                    );
                    return Err(e);
                }
            }
        }

        txn.commit().await?;

        info!(
            integration_id = integration.id,
            rows = outcome.rows_read,
            inserted = outcome.inserted,
            updated = outcome.updated,
            skipped_stale = outcome.skipped_stale,
            skipped_invalid = outcome.skipped_invalid,
            "subscriptions CSV import complete"
        );
        Ok(outcome)
    }

    /// Ingest a Stripe "Invoices" CSV export (status=paid recommended).
    ///
    /// Each paid row becomes a synthetic `invoice.paid` event in
    /// `revenue_events`, which is exactly what the MRR timeseries query
    /// reads — so historical charts light up immediately.
    ///
    /// Expected columns:
    ///   * `id` — invoice ID (required, used as the idempotency key)
    ///   * `customer` — customer ID, optional
    ///   * `subscription` — subscription ID, optional
    ///   * `amount_paid` — minor units (required)
    ///   * `currency` — 3-letter ISO code (required)
    ///   * `status` — when present, only `paid` rows are imported
    ///   * `created` or `paid_at` — unix ts or RFC3339 (required)
    pub async fn import_invoices_csv(
        &self,
        project_id: i32,
        integration_id: i32,
        csv_bytes: &[u8],
    ) -> Result<ImportOutcome, RevenueError> {
        let integration = self.integrations.get(project_id, integration_id).await?;

        if integration.provider != "stripe" {
            return Err(RevenueError::Validation {
                message: format!(
                    "CSV import is currently only supported for Stripe (integration {} uses '{}')",
                    integration.id, integration.provider
                ),
            });
        }

        let mut outcome = ImportOutcome {
            rows_read: 0,
            inserted: 0,
            updated: 0,
            skipped_stale: 0,
            skipped_invalid: 0,
            errors: Vec::new(),
        };

        let mut reader = ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(csv_bytes);

        let txn = self.db.begin().await?;

        for (idx, record) in reader.deserialize::<InvoiceRow>().enumerate() {
            let line = idx + 2;
            outcome.rows_read += 1;
            let row = match record {
                Ok(r) => r,
                Err(e) => {
                    outcome.skipped_invalid += 1;
                    push_error(&mut outcome.errors, line, format!("parse: {}", e));
                    continue;
                }
            };
            match insert_invoice_event(&txn, &integration, &row).await {
                Ok(InvoiceInsertOutcome::Inserted) => outcome.inserted += 1,
                Ok(InvoiceInsertOutcome::Duplicate) => outcome.skipped_stale += 1,
                Ok(InvoiceInsertOutcome::SkippedInvalid(reason)) => {
                    outcome.skipped_invalid += 1;
                    push_error(&mut outcome.errors, line, reason);
                }
                Err(e) => {
                    warn!(
                        integration_id = integration.id,
                        line, error = %e, "invoices CSV import aborted"
                    );
                    return Err(e);
                }
            }
        }

        txn.commit().await?;

        info!(
            integration_id = integration.id,
            rows = outcome.rows_read,
            inserted = outcome.inserted,
            skipped_duplicate = outcome.skipped_stale,
            skipped_invalid = outcome.skipped_invalid,
            "invoices CSV import complete"
        );
        Ok(outcome)
    }
}

fn push_error(buf: &mut Vec<ImportRowError>, row: usize, reason: String) {
    if buf.len() < MAX_REPORTED_ERRORS {
        buf.push(ImportRowError { row, reason });
    }
}

// ---------------------------------------------------------------- Rows

#[derive(Debug, Deserialize)]
struct SubscriptionRow {
    #[serde(alias = "id", alias = "subscription_id", alias = "Subscription ID")]
    id: String,
    #[serde(
        default,
        alias = "customer",
        alias = "customer_id",
        alias = "Customer ID"
    )]
    customer: Option<String>,
    #[serde(default, alias = "status", alias = "Status")]
    status: Option<String>,
    #[serde(default, alias = "currency", alias = "Currency")]
    currency: Option<String>,
    #[serde(
        default,
        alias = "plan.amount",
        alias = "plan_amount",
        alias = "Plan amount",
        alias = "amount"
    )]
    plan_amount: Option<f64>,
    #[serde(
        default,
        alias = "plan.interval",
        alias = "plan_interval",
        alias = "Plan interval",
        alias = "interval"
    )]
    plan_interval: Option<String>,
    #[serde(
        default,
        alias = "plan.interval_count",
        alias = "plan_interval_count",
        alias = "interval_count"
    )]
    plan_interval_count: Option<f64>,
    #[serde(default, alias = "quantity", alias = "Quantity")]
    quantity: Option<f64>,
    #[serde(
        default,
        alias = "current_period_start",
        alias = "Current period start"
    )]
    current_period_start: Option<String>,
    #[serde(default, alias = "created", alias = "Created", alias = "start_date")]
    created: Option<String>,
    #[serde(default, alias = "canceled_at", alias = "Canceled at")]
    canceled_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InvoiceRow {
    #[serde(alias = "id", alias = "invoice_id", alias = "Invoice ID")]
    id: String,
    #[serde(default, alias = "customer", alias = "Customer ID")]
    customer: Option<String>,
    #[serde(default, alias = "subscription", alias = "Subscription ID")]
    subscription: Option<String>,
    #[serde(
        alias = "amount_paid",
        alias = "Amount paid",
        alias = "amount",
        alias = "Total"
    )]
    amount_paid: f64,
    #[serde(alias = "currency", alias = "Currency")]
    currency: String,
    #[serde(default, alias = "status", alias = "Status")]
    status: Option<String>,
    #[serde(default, alias = "created", alias = "Created")]
    created: Option<String>,
    #[serde(default, alias = "paid_at", alias = "Paid at")]
    paid_at: Option<String>,
}

// ---------------------------------------------------------------- Upsert

enum UpsertOutcome {
    Inserted,
    Updated,
    SkippedStale,
    SkippedInvalid(String),
}

enum InvoiceInsertOutcome {
    Inserted,
    Duplicate,
    SkippedInvalid(String),
}

async fn upsert_subscription_from_csv(
    txn: &DatabaseTransaction,
    integration: &IntegrationModel,
    row: &SubscriptionRow,
) -> Result<UpsertOutcome, RevenueError> {
    if row.id.trim().is_empty() {
        return Ok(UpsertOutcome::SkippedInvalid(
            "missing subscription id".into(),
        ));
    }

    let status = normalize_status(row.status.as_deref());
    let currency = row.currency.as_ref().map(|c| c.trim().to_lowercase());
    let mrr_minor = compute_row_mrr(row);

    let started_at =
        parse_ts(row.created.as_deref()).or_else(|| parse_ts(row.current_period_start.as_deref()));
    let canceled_at = parse_ts(row.canceled_at.as_deref());
    // Snapshot age: CSV rows don't carry an explicit updated_at, so we
    // use the newest of the interesting timestamps as a proxy.
    let snapshot_at = [started_at, canceled_at]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or_else(Utc::now);

    if let Some(ref cust) = row.customer {
        upsert_customer_row(txn, integration, cust, snapshot_at).await?;
    }

    let existing = revenue_subscriptions_state::Entity::find()
        .filter(revenue_subscriptions_state::Column::IntegrationId.eq(integration.id))
        .filter(revenue_subscriptions_state::Column::ProviderSubscriptionId.eq(&row.id))
        .one(txn)
        .await?;

    match existing {
        Some(existing_row) => {
            // Don't let a stale CSV snapshot clobber fresher webhook
            // state. The webhook sets updated_at = Utc::now() on every
            // event, so comparing against snapshot_at is a conservative
            // but correct "is the CSV newer?" check.
            if existing_row.updated_at > snapshot_at {
                debug!(
                    integration_id = integration.id,
                    subscription = %row.id,
                    "skipping subscription CSV row: DB copy is newer"
                );
                return Ok(UpsertOutcome::SkippedStale);
            }
            let mut active: revenue_subscriptions_state::ActiveModel = existing_row.into();
            active.status = Set(status);
            active.mrr_minor = Set(mrr_minor);
            active.currency = Set(currency);
            if let Some(c) = canceled_at {
                active.canceled_at = Set(Some(c));
            }
            if let Some(s) = started_at {
                active.started_at = Set(Some(s));
            }
            active.update(txn).await?;
            Ok(UpsertOutcome::Updated)
        }
        None => {
            let new = revenue_subscriptions_state::ActiveModel {
                project_id: Set(integration.project_id),
                integration_id: Set(integration.id),
                provider: Set(integration.provider.clone()),
                provider_subscription_id: Set(row.id.clone()),
                customer_ref: Set(row.customer.clone()),
                status: Set(status),
                mrr_minor: Set(mrr_minor),
                currency: Set(currency),
                started_at: Set(started_at.or(Some(snapshot_at))),
                canceled_at: Set(canceled_at),
                updated_at: Set(snapshot_at),
                ..Default::default()
            };
            new.insert(txn).await?;
            Ok(UpsertOutcome::Inserted)
        }
    }
}

async fn upsert_customer_row(
    txn: &DatabaseTransaction,
    integration: &IntegrationModel,
    customer_ref: &str,
    first_seen_at: DateTime<Utc>,
) -> Result<(), RevenueError> {
    let new_row = revenue_customers_state::ActiveModel {
        project_id: Set(integration.project_id),
        integration_id: Set(integration.id),
        provider: Set(integration.provider.clone()),
        provider_customer_ref: Set(customer_ref.to_string()),
        first_seen_at: Set(first_seen_at),
        churned_at: Set(None),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let _ = revenue_customers_state::Entity::insert(new_row)
        .on_conflict(
            OnConflict::columns([
                revenue_customers_state::Column::IntegrationId,
                revenue_customers_state::Column::ProviderCustomerRef,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(txn)
        .await?;
    Ok(())
}

async fn insert_invoice_event(
    txn: &DatabaseTransaction,
    integration: &IntegrationModel,
    row: &InvoiceRow,
) -> Result<InvoiceInsertOutcome, RevenueError> {
    if row.id.trim().is_empty() {
        return Ok(InvoiceInsertOutcome::SkippedInvalid(
            "missing invoice id".into(),
        ));
    }
    if let Some(ref status) = row.status {
        if !status.eq_ignore_ascii_case("paid") {
            return Ok(InvoiceInsertOutcome::SkippedInvalid(format!(
                "ignoring invoice status '{}' (only 'paid' imported)",
                status
            )));
        }
    }
    let occurred_at = parse_ts(row.paid_at.as_deref())
        .or_else(|| parse_ts(row.created.as_deref()))
        .unwrap_or_else(Utc::now);

    let amount_minor = row.amount_paid.round() as i64;
    if amount_minor <= 0 {
        return Ok(InvoiceInsertOutcome::SkippedInvalid(
            "amount_paid is zero or negative".into(),
        ));
    }

    let event_id = format!("csv:invoice:{}", row.id);
    // Stripe's invoice CSV export carries no per-line price/product data,
    // so both SKU references are always NULL for CSV-imported rows. The
    // ingestion allowlist (if the operator set one) treats NULL-SKU
    // invoice rows like unpriced charges — accepted when
    // `include_unpriced_charges` is true, rejected otherwise. See
    // `ProviderConfig::accepts` for the policy.
    let row_model = revenue_events::ActiveModel {
        project_id: Set(integration.project_id),
        integration_id: Set(integration.id),
        provider: Set(integration.provider.clone()),
        provider_event_id: Set(event_id),
        event_type: Set("invoice.paid".to_string()),
        customer_ref: Set(row.customer.clone()),
        subscription_ref: Set(row.subscription.clone()),
        subscription_status: Set(None),
        mrr_minor: Set(None),
        amount_minor: Set(Some(amount_minor)),
        currency: Set(Some(row.currency.to_lowercase())),
        occurred_at: Set(occurred_at),
        payload: Set(serde_json::json!({ "source": "csv_import", "invoice_id": row.id })),
        created_at: Set(Utc::now()),
        price_id: Set(None),
        product_id: Set(None),
        ..Default::default()
    };

    match row_model.insert(txn).await {
        Ok(_) => Ok(InvoiceInsertOutcome::Inserted),
        Err(sea_orm::DbErr::RecordNotInserted) => Ok(InvoiceInsertOutcome::Duplicate),
        Err(sea_orm::DbErr::Exec(r)) if r.to_string().contains("duplicate key") => {
            Ok(InvoiceInsertOutcome::Duplicate)
        }
        Err(sea_orm::DbErr::Query(r)) if r.to_string().contains("duplicate key") => {
            Ok(InvoiceInsertOutcome::Duplicate)
        }
        Err(other) => Err(other.into()),
    }
}

// ---------------------------------------------------------------- Helpers

fn normalize_status(raw: Option<&str>) -> String {
    match raw.map(str::trim).map(str::to_lowercase).as_deref() {
        Some("trialing") => "trialing".into(),
        Some("active") => "active".into(),
        Some("past_due") => "past_due".into(),
        Some("canceled") | Some("cancelled") => "canceled".into(),
        Some("unpaid") => "unpaid".into(),
        Some("incomplete") | Some("incomplete_expired") => "incomplete".into(),
        Some(other) if !other.is_empty() => other.to_string(),
        _ => "active".into(),
    }
}

/// Project a Stripe CSV row's plan into per-month minor units using the
/// same conversion the webhook path uses.
fn compute_row_mrr(row: &SubscriptionRow) -> i64 {
    let amount = row.plan_amount.unwrap_or(0.0);
    if amount <= 0.0 {
        return 0;
    }
    let qty = row.quantity.unwrap_or(1.0).max(1.0);
    let count = row.plan_interval_count.unwrap_or(1.0).max(1.0);
    let interval = row
        .plan_interval
        .as_deref()
        .map(str::to_lowercase)
        .unwrap_or_else(|| "month".into());
    let monthly = match interval.as_str() {
        "month" => amount / count,
        "year" => amount / (12.0 * count),
        "week" => amount * (52.0 / 12.0) / count,
        "day" => amount * (365.0 / 12.0) / count,
        _ => return 0,
    };
    (monthly * qty).round() as i64
}

/// Accept unix-seconds integers or RFC3339 strings — Stripe's CSV export
/// uses unix epoch, but some tools reformat it.
fn parse_ts(value: Option<&str>) -> Option<DateTime<Utc>> {
    let v = value?.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(secs) = v.parse::<i64>() {
        return Utc.timestamp_opt(secs, 0).single();
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
        return Some(dt.with_timezone(&Utc));
    }
    None
}

// ---------------------------------------------------------------- Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mrr_monthly_simple() {
        let row = SubscriptionRow {
            id: "sub_1".into(),
            customer: None,
            status: None,
            currency: Some("usd".into()),
            plan_amount: Some(900.0),
            plan_interval: Some("month".into()),
            plan_interval_count: None,
            quantity: Some(3.0),
            current_period_start: None,
            created: None,
            canceled_at: None,
        };
        assert_eq!(compute_row_mrr(&row), 2700);
    }

    #[test]
    fn mrr_yearly_divides_by_twelve() {
        let row = SubscriptionRow {
            id: "sub_1".into(),
            customer: None,
            status: None,
            currency: Some("usd".into()),
            plan_amount: Some(12000.0),
            plan_interval: Some("year".into()),
            plan_interval_count: Some(1.0),
            quantity: Some(1.0),
            current_period_start: None,
            created: None,
            canceled_at: None,
        };
        assert_eq!(compute_row_mrr(&row), 1000);
    }

    #[test]
    fn mrr_unknown_interval_is_zero() {
        let row = SubscriptionRow {
            id: "sub_1".into(),
            customer: None,
            status: None,
            currency: None,
            plan_amount: Some(500.0),
            plan_interval: Some("fortnight".into()),
            plan_interval_count: None,
            quantity: None,
            current_period_start: None,
            created: None,
            canceled_at: None,
        };
        assert_eq!(compute_row_mrr(&row), 0);
    }

    #[test]
    fn ts_parses_epoch_and_rfc3339() {
        assert_eq!(
            parse_ts(Some("1700000000")).unwrap().timestamp(),
            1700000000
        );
        assert!(parse_ts(Some("2026-04-20T12:34:56Z")).is_some());
        assert!(parse_ts(Some("")).is_none());
        assert!(parse_ts(None).is_none());
    }

    #[test]
    fn status_normalization_maps_british_spelling() {
        assert_eq!(normalize_status(Some("Cancelled")), "canceled");
        assert_eq!(normalize_status(Some("Active")), "active");
        assert_eq!(normalize_status(None), "active");
    }
}
