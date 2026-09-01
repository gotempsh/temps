// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Analytics service: read-only projections for the dashboard.
//!
//! Everything here is project-scoped — `project_id` is always the first
//! predicate so a tenant can never read another tenant's numbers.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbBackend, EntityTrait, FromQueryResult, QueryFilter,
    QueryOrder, QuerySelect, Statement,
};
use serde::Serialize;
use thiserror::Error;

use temps_entities::{revenue_customers_state, revenue_events, revenue_subscriptions_state};

#[derive(Debug, Error)]
pub enum AnalyticsError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Invalid bucket size: {0}")]
    InvalidBucket(String),
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum Bucket {
    Day,
    Week,
    Month,
}

impl Bucket {
    fn pg_interval(&self) -> &'static str {
        match self {
            Bucket::Day => "1 day",
            Bucket::Week => "1 week",
            Bucket::Month => "1 month",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AnalyticsError> {
        match value {
            "day" | "daily" => Ok(Bucket::Day),
            "week" | "weekly" => Ok(Bucket::Week),
            "month" | "monthly" => Ok(Bucket::Month),
            other => Err(AnalyticsError::InvalidBucket(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSummary {
    pub currency: String,
    pub current_mrr_minor: i64,
    pub current_arr_minor: i64,
    pub active_subscriptions: i64,
    pub active_customers: i64,
    pub churned_last_30d: i64,
    pub arpu_minor: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MrrBucket {
    pub bucket: DateTime<Utc>,
    pub mrr_minor: i64,
    pub charge_total_minor: i64,
    pub refund_total_minor: i64,
    pub charge_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomerMovement {
    pub bucket: DateTime<Utc>,
    pub new_customers: i64,
    pub churned_customers: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentEvent {
    pub occurred_at: DateTime<Utc>,
    pub event_type: String,
    pub customer_ref: Option<String>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub mrr_minor: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalRecentEvent {
    pub id: i64,
    pub project_id: i32,
    pub project_name: String,
    pub occurred_at: DateTime<Utc>,
    pub event_type: String,
    pub customer_ref: Option<String>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub mrr_minor: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalRevenueSummary {
    pub currency: String,
    pub current_mrr_minor: i64,
    pub paid_last_30d_minor: i64,
    pub refunded_last_30d_minor: i64,
    pub paid_all_time_minor: i64,
    pub refunded_all_time_minor: i64,
    pub active_subscriptions: i64,
    pub active_customers: i64,
    pub transactions_last_30d: i64,
}

#[derive(Debug, Clone, Default)]
pub struct GlobalEventsFilter {
    pub project_id: Option<i32>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub event_types: Option<Vec<String>>,
    pub limit: u64,
}

pub struct RevenueAnalyticsService {
    db: Arc<DatabaseConnection>,
}

impl RevenueAnalyticsService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Sum of active-subscription MRR across *every* project in the
    /// install. Single-tenant today, so this is the org-wide number the
    /// main dashboard shows next to Visitors / Page views.
    pub async fn global_mrr(&self, currency: &str) -> Result<i64, AnalyticsError> {
        let subs = revenue_subscriptions_state::Entity::find()
            .filter(revenue_subscriptions_state::Column::Currency.eq(currency.to_lowercase()))
            .all(self.db.as_ref())
            .await?;

        let mrr = subs
            .iter()
            .filter(|s| matches!(s.status.as_str(), "active" | "trialing" | "past_due"))
            .map(|s| s.mrr_minor)
            .sum();
        Ok(mrr)
    }

    /// Org-wide MRR at a historical point, reconstructed from the event
    /// stream. Sums the same `mrr_minor` deltas (`subscription.*` and
    /// `mrr.realized`) that `mrr_timeseries` uses, up to and including
    /// `as_of`. Matches the semantics of the chart so the "vs yesterday"
    /// delta lines up with what the user sees on the curve.
    pub async fn global_mrr_at(
        &self,
        currency: &str,
        as_of: DateTime<Utc>,
    ) -> Result<i64, AnalyticsError> {
        #[derive(FromQueryResult)]
        struct Row {
            total: Option<i64>,
        }

        let sql = r#"
            SELECT SUM(mrr_minor)::bigint AS total
            FROM revenue_events
            WHERE currency = $1
              AND occurred_at <= $2
              AND event_type IN (
                  'subscription.created',
                  'subscription.updated',
                  'subscription.canceled',
                  'mrr.realized'
              )
        "#;

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [currency.to_lowercase().into(), as_of.into()],
        );
        let row = Row::find_by_statement(stmt).one(self.db.as_ref()).await?;
        Ok(row.and_then(|r| r.total).unwrap_or(0))
    }

    /// Org-wide revenue summary. Cash figures come from `charge.succeeded`
    /// (paid) and `charge.refunded` (refunds). MRR and customer counts
    /// come from the denormalized `revenue_subscriptions_state` and
    /// `revenue_customers_state` projections.
    pub async fn global_summary(
        &self,
        currency: &str,
    ) -> Result<GlobalRevenueSummary, AnalyticsError> {
        let currency = currency.to_lowercase();

        #[derive(FromQueryResult)]
        struct CashRow {
            paid_last_30d: Option<i64>,
            refunded_last_30d: Option<i64>,
            paid_all_time: Option<i64>,
            refunded_all_time: Option<i64>,
            transactions_last_30d: Option<i64>,
        }

        let cash_sql = r#"
            SELECT
                SUM(amount_minor) FILTER (
                    WHERE event_type = 'charge.succeeded' AND occurred_at >= $2
                )::bigint AS paid_last_30d,
                SUM(amount_minor) FILTER (
                    WHERE event_type = 'charge.refunded' AND occurred_at >= $2
                )::bigint AS refunded_last_30d,
                SUM(amount_minor) FILTER (
                    WHERE event_type = 'charge.succeeded'
                )::bigint AS paid_all_time,
                SUM(amount_minor) FILTER (
                    WHERE event_type = 'charge.refunded'
                )::bigint AS refunded_all_time,
                COUNT(*) FILTER (
                    WHERE event_type = 'charge.succeeded' AND occurred_at >= $2
                )::bigint AS transactions_last_30d
            FROM revenue_events
            WHERE currency = $1
        "#;
        let thirty_days_ago = Utc::now() - chrono::Duration::days(30);
        let cash_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            cash_sql,
            [currency.clone().into(), thirty_days_ago.into()],
        );
        let cash = CashRow::find_by_statement(cash_stmt)
            .one(self.db.as_ref())
            .await?;

        let current_mrr_minor = self.global_mrr(&currency).await?;

        let subs = revenue_subscriptions_state::Entity::find()
            .filter(revenue_subscriptions_state::Column::Currency.eq(currency.clone()))
            .all(self.db.as_ref())
            .await?;
        let active_subscriptions = subs
            .iter()
            .filter(|s| matches!(s.status.as_str(), "active" | "trialing" | "past_due"))
            .count() as i64;

        let customers = revenue_customers_state::Entity::find()
            .all(self.db.as_ref())
            .await?;
        let active_customers = customers.iter().filter(|c| c.churned_at.is_none()).count() as i64;

        Ok(GlobalRevenueSummary {
            currency,
            current_mrr_minor,
            paid_last_30d_minor: cash.as_ref().and_then(|r| r.paid_last_30d).unwrap_or(0),
            refunded_last_30d_minor: cash.as_ref().and_then(|r| r.refunded_last_30d).unwrap_or(0),
            paid_all_time_minor: cash.as_ref().and_then(|r| r.paid_all_time).unwrap_or(0),
            refunded_all_time_minor: cash.as_ref().and_then(|r| r.refunded_all_time).unwrap_or(0),
            active_subscriptions,
            active_customers,
            transactions_last_30d: cash
                .as_ref()
                .and_then(|r| r.transactions_last_30d)
                .unwrap_or(0),
        })
    }

    pub async fn summary(
        &self,
        project_id: i32,
        currency: &str,
    ) -> Result<MetricsSummary, AnalyticsError> {
        let currency = currency.to_lowercase();

        let subs = revenue_subscriptions_state::Entity::find()
            .filter(revenue_subscriptions_state::Column::ProjectId.eq(project_id))
            .filter(revenue_subscriptions_state::Column::Currency.eq(&currency))
            .all(self.db.as_ref())
            .await?;

        let mut mrr: i64 = 0;
        let mut active_subs: i64 = 0;
        for s in &subs {
            if matches!(s.status.as_str(), "active" | "trialing" | "past_due") {
                mrr += s.mrr_minor;
                active_subs += 1;
            }
        }

        // Customers with at least one active sub in this currency.
        let customers = revenue_customers_state::Entity::find()
            .filter(revenue_customers_state::Column::ProjectId.eq(project_id))
            .filter(revenue_customers_state::Column::ChurnedAt.is_null())
            .all(self.db.as_ref())
            .await?;
        let active_customers = customers.len() as i64;

        // Last-30d churn (customers with churned_at in the window)
        let thirty_days_ago = Utc::now() - chrono::Duration::days(30);
        let churned_last_30d = revenue_customers_state::Entity::find()
            .filter(revenue_customers_state::Column::ProjectId.eq(project_id))
            .filter(revenue_customers_state::Column::ChurnedAt.gte(thirty_days_ago))
            .all(self.db.as_ref())
            .await?
            .len() as i64;

        let arpu = if active_customers > 0 {
            mrr / active_customers
        } else {
            0
        };

        Ok(MetricsSummary {
            currency,
            current_mrr_minor: mrr,
            current_arr_minor: mrr.saturating_mul(12),
            active_subscriptions: active_subs,
            active_customers,
            churned_last_30d,
            arpu_minor: arpu,
        })
    }

    /// Time-bucketed MRR + charge/refund totals.
    ///
    /// We bucket from the raw `revenue_events` table rather than the
    /// continuous aggregate so vanilla Postgres installs work too. On
    /// TimescaleDB the planner rewrites time_bucket to the hypertable
    /// chunks automatically.
    pub async fn mrr_timeseries(
        &self,
        project_id: i32,
        currency: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        bucket: Bucket,
    ) -> Result<Vec<MrrBucket>, AnalyticsError> {
        #[derive(FromQueryResult)]
        struct Row {
            bucket: DateTime<Utc>,
            mrr_minor: Option<i64>,
            charge_total_minor: Option<i64>,
            refund_total_minor: Option<i64>,
            charge_count: Option<i64>,
        }

        // MRR stacks two independent streams so metered/tiered/hybrid
        // subscriptions show up correctly:
        //
        //   1. `subscription.*` events — flat-fee subs whose MRR lives in
        //      the subscription object itself. `filter_events` zeroes the
        //      mrr_minor for metered subs when `metered_mode =
        //      derive_from_invoices`, so there's no double counting.
        //
        //   2. `mrr.realized` events — synthetic per-invoice-line events
        //      with `occurred_at = period.start`. The mrr_minor on these
        //      is the line's realized MRR (`amount / period_days * 30`).
        //      Summed into the same bucket as their period_start, they
        //      paint the invoice-backed MRR curve.
        //
        // Both streams carry mrr_minor in the same currency; summing them
        // together is safe because the `filter_events` step guarantees
        // no row contributes to both streams for the same subscription.
        let sql = format!(
            r#"
            SELECT
                date_trunc('{granularity}', occurred_at) AS bucket,
                SUM(mrr_minor) FILTER (
                    WHERE event_type IN (
                        'subscription.created',
                        'subscription.updated',
                        'subscription.canceled',
                        'mrr.realized'
                    )
                )::bigint AS mrr_minor,
                SUM(amount_minor) FILTER (WHERE event_type = 'charge.succeeded')::bigint AS charge_total_minor,
                SUM(amount_minor) FILTER (WHERE event_type = 'charge.refunded')::bigint AS refund_total_minor,
                COUNT(*) FILTER (WHERE event_type = 'charge.succeeded')::bigint AS charge_count
            FROM revenue_events
            WHERE project_id = $1
              AND currency = $2
              AND occurred_at >= $3
              AND occurred_at <= $4
            GROUP BY bucket
            ORDER BY bucket ASC
            "#,
            granularity = match bucket {
                Bucket::Day => "day",
                Bucket::Week => "week",
                Bucket::Month => "month",
            }
        );
        // interval usage lives in Bucket::pg_interval for the continuous
        // aggregate; date_trunc is what we use against raw events.
        let _ = bucket.pg_interval();

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                project_id.into(),
                currency.to_lowercase().into(),
                from.into(),
                to.into(),
            ],
        );

        let rows = Row::find_by_statement(stmt).all(self.db.as_ref()).await?;
        Ok(rows
            .into_iter()
            .map(|r| MrrBucket {
                bucket: r.bucket,
                mrr_minor: r.mrr_minor.unwrap_or(0),
                charge_total_minor: r.charge_total_minor.unwrap_or(0),
                refund_total_minor: r.refund_total_minor.unwrap_or(0),
                charge_count: r.charge_count.unwrap_or(0),
            })
            .collect())
    }

    pub async fn customer_movement(
        &self,
        project_id: i32,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        bucket: Bucket,
    ) -> Result<Vec<CustomerMovement>, AnalyticsError> {
        #[derive(FromQueryResult)]
        struct Row {
            bucket: DateTime<Utc>,
            new_customers: Option<i64>,
            churned_customers: Option<i64>,
        }

        let granularity = match bucket {
            Bucket::Day => "day",
            Bucket::Week => "week",
            Bucket::Month => "month",
        };

        let sql = format!(
            r#"
            WITH events AS (
                SELECT date_trunc('{granularity}', first_seen_at) AS bucket, 1 AS n, 0 AS c
                FROM revenue_customers_state
                WHERE project_id = $1 AND first_seen_at BETWEEN $2 AND $3
                UNION ALL
                SELECT date_trunc('{granularity}', churned_at) AS bucket, 0, 1
                FROM revenue_customers_state
                WHERE project_id = $1 AND churned_at IS NOT NULL
                  AND churned_at BETWEEN $2 AND $3
            )
            SELECT bucket,
                   SUM(n)::bigint AS new_customers,
                   SUM(c)::bigint AS churned_customers
            FROM events
            GROUP BY bucket
            ORDER BY bucket ASC
            "#
        );

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [project_id.into(), from.into(), to.into()],
        );

        let rows = Row::find_by_statement(stmt).all(self.db.as_ref()).await?;
        Ok(rows
            .into_iter()
            .map(|r| CustomerMovement {
                bucket: r.bucket,
                new_customers: r.new_customers.unwrap_or(0),
                churned_customers: r.churned_customers.unwrap_or(0),
            })
            .collect())
    }

    pub async fn recent_events(
        &self,
        project_id: i32,
        limit: u64,
    ) -> Result<Vec<RecentEvent>, AnalyticsError> {
        let rows = revenue_events::Entity::find()
            .filter(revenue_events::Column::ProjectId.eq(project_id))
            .order_by_desc(revenue_events::Column::OccurredAt)
            .limit(limit.clamp(1, 200))
            .all(self.db.as_ref())
            .await?;

        Ok(rows
            .into_iter()
            .map(|r| RecentEvent {
                occurred_at: r.occurred_at,
                event_type: r.event_type,
                customer_ref: r.customer_ref,
                amount_minor: r.amount_minor,
                currency: r.currency,
                mrr_minor: r.mrr_minor,
            })
            .collect())
    }

    pub async fn global_recent_events(
        &self,
        filter: GlobalEventsFilter,
    ) -> Result<Vec<GlobalRecentEvent>, AnalyticsError> {
        #[derive(FromQueryResult)]
        struct Row {
            id: i64,
            project_id: i32,
            project_name: String,
            occurred_at: DateTime<Utc>,
            event_type: String,
            customer_ref: Option<String>,
            amount_minor: Option<i64>,
            currency: Option<String>,
            mrr_minor: Option<i64>,
        }

        let mut sql = String::from(
            "SELECT e.id, e.project_id, p.name AS project_name, e.occurred_at, e.event_type, \
             e.customer_ref, e.amount_minor, e.currency, e.mrr_minor \
             FROM revenue_events e \
             JOIN projects p ON p.id = e.project_id \
             WHERE 1 = 1",
        );
        let mut values: Vec<sea_orm::Value> = Vec::new();
        let push = |sql: &mut String, values: &mut Vec<sea_orm::Value>, v: sea_orm::Value| {
            values.push(v);
            sql.push_str(&format!(" ${}", values.len()));
        };

        if let Some(project_id) = filter.project_id {
            sql.push_str(" AND e.project_id =");
            push(&mut sql, &mut values, project_id.into());
        }
        if let Some(from) = filter.from {
            sql.push_str(" AND e.occurred_at >=");
            push(&mut sql, &mut values, from.into());
        }
        if let Some(to) = filter.to {
            sql.push_str(" AND e.occurred_at <=");
            push(&mut sql, &mut values, to.into());
        }
        if let Some(types) = filter.event_types.as_ref() {
            if !types.is_empty() {
                let placeholders: Vec<String> = types
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("${}", values.len() + i + 1))
                    .collect();
                sql.push_str(&format!(
                    " AND e.event_type IN ({})",
                    placeholders.join(", ")
                ));
                for t in types {
                    values.push(t.clone().into());
                }
            }
        }

        sql.push_str(" ORDER BY e.occurred_at DESC LIMIT");
        let limit = filter.limit.clamp(1, 500);
        push(&mut sql, &mut values, (limit as i64).into());

        let stmt = Statement::from_sql_and_values(DbBackend::Postgres, &sql, values);
        let rows = Row::find_by_statement(stmt).all(self.db.as_ref()).await?;

        Ok(rows
            .into_iter()
            .map(|r| GlobalRecentEvent {
                id: r.id,
                project_id: r.project_id,
                project_name: r.project_name,
                occurred_at: r.occurred_at,
                event_type: r.event_type,
                customer_ref: r.customer_ref,
                amount_minor: r.amount_minor,
                currency: r.currency,
                mrr_minor: r.mrr_minor,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn sub(
        id: i32,
        project_id: i32,
        status: &str,
        mrr_minor: i64,
    ) -> revenue_subscriptions_state::Model {
        revenue_subscriptions_state::Model {
            id,
            project_id,
            integration_id: 1,
            provider: "stripe".into(),
            provider_subscription_id: format!("sub_{}", id),
            customer_ref: None,
            status: status.into(),
            mrr_minor,
            currency: Some("usd".into()),
            started_at: None,
            canceled_at: None,
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn global_mrr_sums_active_across_projects_only() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![
                    sub(1, 10, "active", 1500),
                    sub(2, 11, "trialing", 500),
                    sub(3, 12, "past_due", 200),
                    sub(4, 13, "canceled", 9999),
                    sub(5, 14, "incomplete", 9999),
                ]])
                .into_connection(),
        );
        let svc = RevenueAnalyticsService::new(db);

        let total = svc.global_mrr("usd").await.expect("global_mrr succeeds");
        assert_eq!(total, 1500 + 500 + 200);
    }

    #[tokio::test]
    async fn global_mrr_zero_when_no_subs() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<revenue_subscriptions_state::Model>::new()])
                .into_connection(),
        );
        let svc = RevenueAnalyticsService::new(db);

        let total = svc.global_mrr("usd").await.expect("global_mrr succeeds");
        assert_eq!(total, 0);
    }
}
