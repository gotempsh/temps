// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::ErrorTrackingError;
use crate::sentry::client_reports::{ClientDiscard, ClientReport};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClientOutcome {
    pub id: i64,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub bucket: String,
    pub reason: String,
    pub category: String,
    pub unit: String,
    /// Decimal string avoids rounding large integer totals in JavaScript.
    pub quantity: String,
    pub first_reported_at: String,
    pub last_reported_at: String,
    pub last_received_at: String,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClientOutcomePage {
    pub items: Vec<ClientOutcome>,
    pub total: i64,
    pub retention_days: u32,
    pub approximate: bool,
}

pub struct ClientOutcomeService {
    db: Arc<DatabaseConnection>,
}

#[derive(Debug, Default)]
pub struct ClientOutcomeReceipt {
    /// Discard entries added to counters; quantities keep their original units.
    pub accepted: u64,
    /// Discard entries already acknowledged for an exact identified retry.
    pub duplicates: u64,
}
impl ClientOutcomeService {
    /// The named CHECK aborts the entire statement, including receipt claims.
    /// Classify this specific resource limit without treating arbitrary database
    /// constraint failures as retryable or exposing their payload-bearing detail.
    pub fn is_receipt_capacity_error(error: &sea_orm::DbErr) -> bool {
        match error {
            sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(error))
            | sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(error)) => {
                error.as_database_error().and_then(|db| db.constraint())
                    == Some("client_outcome_receipt_capacity")
            }
            _ => false,
        }
    }
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// One aggregation/upsert per envelope; never one DB operation per discarded
    /// item. Receipt time controls retention and buckets, regardless of SDK clock.
    pub async fn ingest(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        entries: Vec<ClientDiscard>,
    ) -> Result<u64, ErrorTrackingError> {
        Ok(self
            .ingest_reports(
                project_id,
                environment_id,
                deployment_id,
                vec![ClientReport {
                    source_key: None,
                    entries,
                }],
            )
            .await?
            .accepted)
    }

    /// One atomic SQL statement claims exact report retries and updates all
    /// counters. Concurrent requests cannot claim the same scoped receipt twice.
    /// Receipts contain only hashes, expire after 24 hours, and never extend on
    /// a duplicate. Unidentified reports always count independently.
    pub async fn ingest_reports(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        reports: Vec<ClientReport>,
    ) -> Result<ClientOutcomeReceipt, ErrorTrackingError> {
        if reports.len() > 1000 || reports.iter().map(|r| r.entries.len()).sum::<usize>() > 1000 {
            return Err(ErrorTrackingError::Validation(
                "At most 1000 client outcomes per envelope".into(),
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for key in reports.iter().filter_map(|r| r.source_key.as_ref()) {
            if key.len() != 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !keys.insert(key)
            {
                return Err(ErrorTrackingError::Validation(
                    "Invalid or repeated client outcome receipt key".into(),
                ));
            }
        }
        let reports = reports
            .into_iter()
            .filter(|r| !r.entries.is_empty())
            .collect::<Vec<_>>();
        if reports.is_empty() {
            return Ok(ClientOutcomeReceipt::default());
        }
        let rows = serde_json::to_value(reports).map_err(|_| {
            ErrorTrackingError::Validation("Client outcome serialization failed".into())
        })?;
        let row = self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,r#"
            WITH reports AS MATERIALIZED (
                SELECT source_key,entries FROM jsonb_to_recordset($4)
                AS r(source_key TEXT,entries JSONB)
            ), claims AS (
                INSERT INTO telemetry_client_outcome_receipts(project_id,environment_id,deployment_id,source_key,expires_at)
                SELECT $1,$2,$3,source_key,NOW()+INTERVAL '24 hours'
                FROM reports WHERE source_key IS NOT NULL ORDER BY source_key
                ON CONFLICT(project_id,environment_id,deployment_id,source_key) DO UPDATE SET
                expires_at=EXCLUDED.expires_at WHERE telemetry_client_outcome_receipts.expires_at<=NOW()
                RETURNING source_key
            ), accepted AS MATERIALIZED (
                SELECT entries FROM reports r WHERE source_key IS NULL
                OR EXISTS(SELECT 1 FROM claims c WHERE c.source_key=r.source_key)
            ), budgets AS (
                INSERT INTO telemetry_client_outcome_receipt_budgets(project_id,bucket,used)
                SELECT s.project_id,date_trunc('hour',NOW()),n.used::integer
                FROM (SELECT NULL::integer AS project_id UNION ALL SELECT $1) s
                CROSS JOIN (SELECT count(*) AS used FROM claims) n WHERE n.used>0
                ORDER BY s.project_id NULLS FIRST
                ON CONFLICT(project_id,bucket) DO UPDATE SET
                used=telemetry_client_outcome_receipt_budgets.used+EXCLUDED.used
                RETURNING id
            ), updated AS (
            INSERT INTO telemetry_client_outcomes(project_id,environment_id,deployment_id,bucket,reason,category,unit,quantity,first_reported_at,last_reported_at)
            SELECT $1,$2,$3,date_trunc('hour',NOW()),reason,category,min(unit),sum(quantity)::bigint,min(reported_at),max(reported_at)
            FROM accepted CROSS JOIN LATERAL jsonb_to_recordset(entries) AS e(reason TEXT,category TEXT,unit TEXT,quantity BIGINT,reported_at TIMESTAMPTZ)
            WHERE (SELECT count(*) FROM budgets)>=0
            GROUP BY reason,category ORDER BY reason,category
            ON CONFLICT(project_id,environment_id,deployment_id,bucket,reason,category) DO UPDATE SET
            quantity=LEAST(9223372036854775807::numeric,telemetry_client_outcomes.quantity::numeric+EXCLUDED.quantity)::bigint,
            first_reported_at=LEAST(telemetry_client_outcomes.first_reported_at,EXCLUDED.first_reported_at),
            last_reported_at=GREATEST(telemetry_client_outcomes.last_reported_at,EXCLUDED.last_reported_at),
            last_received_at=NOW() RETURNING id
            ) SELECT COALESCE((SELECT sum(jsonb_array_length(entries)) FROM accepted),0)::bigint AS accepted,
                (COALESCE((SELECT sum(jsonb_array_length(entries)) FROM reports),0)
                 -COALESCE((SELECT sum(jsonb_array_length(entries)) FROM accepted),0))::bigint AS duplicates,
                (SELECT count(*) FROM updated) AS updated
        "#,[project_id.into(),environment_id.into(),deployment_id.into(),rows.into()])).await?
            .ok_or_else(|| ErrorTrackingError::Validation("Client outcome write returned no receipt".into()))?;
        Ok(ClientOutcomeReceipt {
            accepted: row.try_get::<i64>("", "accepted")? as u64,
            duplicates: row.try_get::<i64>("", "duplicates")? as u64,
        })
    }
    pub async fn list(
        &self,
        project_id: i32,
        days: u32,
        page: u64,
        page_size: u64,
    ) -> Result<ClientOutcomePage, ErrorTrackingError> {
        if !(1..=31).contains(&days)
            || page == 0
            || !(1..=100).contains(&page_size)
            || page
                .checked_sub(1)
                .and_then(|n| n.checked_mul(page_size))
                .is_none_or(|n| n > 100_000)
        {
            return Err(ErrorTrackingError::Validation(
                "Client outcome queries require 1–31 days, positive page, and page size 1–100"
                    .into(),
            ));
        }
        let tx = self.db.begin().await?;
        tx.execute_unprepared("SET LOCAL statement_timeout='5s'; SET LOCAL work_mem='4MB'")
            .await?;
        let row=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,r#"
            WITH recent AS MATERIALIZED (
                SELECT * FROM telemetry_client_outcomes WHERE project_id=$1 AND bucket>=NOW()-make_interval(days=>$2)
            ), page AS (
                SELECT id,environment_id,deployment_id,to_char(bucket AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS bucket,
                reason,category,unit,quantity::text,
                to_char(first_reported_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS first_reported_at,
                to_char(last_reported_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS last_reported_at,
                to_char(last_received_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS last_received_at
                FROM recent ORDER BY bucket DESC,id DESC LIMIT $3 OFFSET $4
            ) SELECT COALESCE(jsonb_agg(to_jsonb(page) ORDER BY bucket DESC,id DESC),'[]'::jsonb) AS items,
                (SELECT count(*) FROM recent) AS total FROM page
        "#,[project_id.into(),(days as i32).into(),(page_size as i64).into(),(((page-1)*page_size) as i64).into()])).await?.ok_or_else(||ErrorTrackingError::Validation("Client outcome query returned no result".into()))?;
        let items = serde_json::from_value(row.try_get("", "items")?).map_err(|_| {
            ErrorTrackingError::Validation("Invalid stored client outcome schema".into())
        })?;
        let total = row.try_get("", "total")?;
        tx.commit().await?;
        Ok(ClientOutcomePage {
            items,
            total,
            retention_days: 31,
            approximate: true,
        })
    }
    pub async fn cleanup(&self) -> Result<u64, ErrorTrackingError> {
        let outcomes = self.db.execute_unprepared("DELETE FROM telemetry_client_outcomes WHERE id IN (SELECT id FROM telemetry_client_outcomes WHERE bucket<NOW()-INTERVAL '31 days' ORDER BY bucket LIMIT 2000)").await?.rows_affected();
        // Recheck expiry on the locked row: a concurrent claim may have renewed
        // an expired receipt after the candidate subquery read its old version.
        let receipts = self.db.execute_unprepared("DELETE FROM telemetry_client_outcome_receipts WHERE expires_at<NOW() AND id IN (SELECT id FROM telemetry_client_outcome_receipts WHERE expires_at<NOW() ORDER BY expires_at LIMIT 2000)").await?.rows_affected();
        self.db.execute_unprepared("DELETE FROM telemetry_client_outcome_receipt_budgets WHERE id IN (SELECT id FROM telemetry_client_outcome_receipt_budgets WHERE bucket<date_trunc('hour',NOW())-INTERVAL '24 hours' ORDER BY bucket LIMIT 2000)").await?;
        Ok(outcomes + receipts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sentry::client_reports::decode;
    use sea_orm::{ActiveModelTrait, Set};
    #[tokio::test]
    #[serial_test::serial]
    async fn client_outcomes_batch_concurrency_units_scope_and_retention() {
        let test_db = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test_db.connection_arc();
        let project = temps_entities::projects::ActiveModel {
            name: Set("Synthetic outcomes".into()),
            slug: Set(format!("synthetic-outcomes-{}", uuid::Uuid::new_v4())),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap()
        .id;
        let service = Arc::new(ClientOutcomeService::new(db.clone()));
        let payload=br#"{"discarded_events":[{"reason":"before_send","category":"error","quantity":3},{"reason":"buffer_overflow","category":"log_byte","quantity":100},{"reason":"before_send","category":"error","quantity":4}]}"#;
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let service = service.clone();
            tasks.push(tokio::spawn(async move {
                service
                    .ingest(project, None, None, decode(payload).unwrap())
                    .await
                    .unwrap()
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), 3);
        }
        let page = service.list(project, 1, 1, 20).await.unwrap();
        assert_eq!(page.total, 2);
        assert!(page.approximate);
        let errors = page.items.iter().find(|r| r.category == "error").unwrap();
        assert_eq!(errors.quantity, "112");
        assert_eq!(errors.unit, "items");
        assert!(errors.bucket.ends_with('Z'));
        let bytes = page
            .items
            .iter()
            .find(|r| r.category == "log_byte")
            .unwrap();
        assert_eq!(bytes.quantity, "1600");
        assert_eq!(bytes.unit, "bytes");
        assert_eq!(service.list(project + 1, 1, 1, 20).await.unwrap().total, 0);
        let beyond = service.list(project, 1, 3, 1).await.unwrap();
        assert!(beyond.items.is_empty());
        assert_eq!(beyond.total, 2);
        assert!(service.list(project, 32, 1, 20).await.is_err());
        assert!(service.list(project, 1, u64::MAX, 100).await.is_err());
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE telemetry_client_outcomes SET quantity=9223372036854775807 WHERE project_id=$1 AND category='error'",[project.into()])).await.unwrap();
        service
            .ingest(project, None, None, decode(payload).unwrap())
            .await
            .unwrap();
        assert_eq!(
            service
                .list(project, 1, 1, 20)
                .await
                .unwrap()
                .items
                .iter()
                .find(|r| r.category == "error")
                .unwrap()
                .quantity,
            "9223372036854775807"
        );
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE telemetry_client_outcomes SET bucket=NOW()-INTERVAL '32 days' WHERE project_id=$1",[project.into()])).await.unwrap();
        assert_eq!(service.cleanup().await.unwrap(), 2);
        assert_eq!(service.list(project, 31, 1, 20).await.unwrap().total, 0);
        db.execute_unprepared("ALTER TABLE telemetry_client_outcomes ADD CONSTRAINT synthetic_outcome_failure CHECK (quantity=0) NOT VALID").await.unwrap();
        assert!(service
            .ingest(project, None, None, decode(payload).unwrap())
            .await
            .is_err());
        assert_eq!(
            service.list(project, 1, 1, 20).await.unwrap().total,
            0,
            "failed batch must not be acknowledged or partially stored"
        );
    }
}
