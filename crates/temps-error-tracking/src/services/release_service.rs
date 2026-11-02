// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Release correlation over retained native signals. No release registry or
//! duplicate telemetry store is created. This is a bounded control-plane read.
use super::ErrorTrackingError;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TelemetryRelease {
    pub release: String,
    #[schema(value_type = String, format = DateTime)]
    pub first_seen: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub last_seen: DateTime<Utc>,
    pub errors: i64,
    pub logs: i64,
    pub spans: i64,
    pub metric_points: i64,
    pub feedback: i64,
    pub check_ins: i64,
    pub example_error_group_id: Option<i32>,
    pub example_trace_id: Option<String>,
    pub example_log_trace_id: Option<String>,
    pub example_metric_name: Option<String>,
    pub example_feedback_id: Option<i64>,
    pub example_monitor_id: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TelemetryReleasePage {
    pub items: Vec<TelemetryRelease>,
    pub total: i64,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
}

pub struct ReleaseService {
    db: Arc<DatabaseConnection>,
}
impl ReleaseService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn list(
        &self,
        project_id: i32,
        release: Option<String>,
        days: u32,
        page: u64,
        page_size: u64,
    ) -> Result<TelemetryReleasePage, ErrorTrackingError> {
        if !(1..=31).contains(&days)
            || page == 0
            || !(1..=100).contains(&page_size)
            || page
                .checked_sub(1)
                .and_then(|n| n.checked_mul(page_size))
                .is_none_or(|n| n > 100_000)
            || release
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 16_384 || s.contains('\0'))
        {
            return Err(ErrorTrackingError::Validation("Release queries require 1–31 days, a positive page, page size 1–100 and a bounded release name".into()));
        }
        let end_time = Utc::now();
        let start_time = end_time - Duration::days(i64::from(days));
        let tx = self.db.begin().await?;
        // Constrain work as well as response cardinality. Time predicates prune
        // native hypertable chunks; PostgreSQL can spill aggregation to disk.
        tx.execute_unprepared("SET LOCAL statement_timeout='5s'; SET LOCAL work_mem='4MB'")
            .await?;
        let row = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
WITH signals AS (
 SELECT data #>> '{environment,release}' AS release, timestamp AS time, 'errors' AS signal,
        error_group_id AS error_group_id, NULL::text AS trace_id, NULL::text AS log_trace_id,
        NULL::text AS metric_name, NULL::bigint AS feedback_id, NULL::int AS monitor_id
 FROM error_events WHERE project_id=$1 AND timestamp BETWEEN $2 AND $3
 UNION ALL
 SELECT service_version,timestamp,'logs',NULL,NULL,trace_id,NULL,NULL,NULL
 FROM otel_log_events WHERE project_id=$1 AND timestamp BETWEEN $2 AND $3
 UNION ALL
 SELECT service_version,start_time,'spans',NULL,trace_id,NULL,NULL,NULL,NULL
 FROM otel_spans WHERE project_id=$1 AND start_time BETWEEN $2 AND $3
 UNION ALL
 SELECT service_version,timestamp,'metric_points',NULL,NULL,NULL,metric_name,NULL,NULL
 FROM otel_metrics WHERE project_id=$1 AND timestamp BETWEEN $2 AND $3
 UNION ALL
 SELECT release,timestamp,'feedback',NULL,NULL,NULL,NULL,id,NULL
 FROM project_feedback WHERE project_id=$1 AND timestamp BETWEEN $2 AND $3
 UNION ALL
 SELECT r.release,r.started_at,'check_ins',NULL,NULL,NULL,NULL,NULL,r.monitor_id
 FROM cron_monitors m JOIN cron_monitor_runs r ON r.monitor_id=m.id
 WHERE m.project_id=$1 AND r.started_at BETWEEN $2 AND $3
), releases AS MATERIALIZED (
 SELECT release,min(time) AS first_seen,max(time) AS last_seen,
 count(*) FILTER(WHERE signal='errors') AS errors,
 count(*) FILTER(WHERE signal='logs') AS logs,
 count(*) FILTER(WHERE signal='spans') AS spans,
 count(*) FILTER(WHERE signal='metric_points') AS metric_points,
 count(*) FILTER(WHERE signal='feedback') AS feedback,
 count(*) FILTER(WHERE signal='check_ins') AS check_ins,
 min(error_group_id) AS example_error_group_id,min(trace_id) AS example_trace_id,
 min(log_trace_id) AS example_log_trace_id,min(metric_name) AS example_metric_name,
 min(feedback_id) AS example_feedback_id,min(monitor_id) AS example_monitor_id
 FROM signals WHERE release IS NOT NULL AND release<>'' AND ($4::text IS NULL OR release=$4)
 GROUP BY release
), page AS (
 SELECT * FROM releases ORDER BY last_seen DESC,release LIMIT $5 OFFSET $6
)
SELECT COALESCE(jsonb_agg(to_jsonb(page) ORDER BY last_seen DESC,release),'[]'::jsonb) AS items,
       (SELECT count(*) FROM releases) AS total FROM page
"#,
                [
                    project_id.into(),
                    start_time.into(),
                    end_time.into(),
                    release.into(),
                    (page_size as i64).into(),
                    (((page - 1) * page_size) as i64).into(),
                ],
            ))
            .await?
            .ok_or_else(|| {
                ErrorTrackingError::Validation("Release correlation returned no aggregate".into())
            })?;
        let items: serde_json::Value = row.try_get("", "items")?;
        let items = serde_json::from_value(items).map_err(|_| {
            ErrorTrackingError::Validation(
                "Release correlation returned invalid native records".into(),
            )
        })?;
        let total = row.try_get("", "total")?;
        tx.commit().await?;
        Ok(TelemetryReleasePage {
            items,
            total,
            start_time,
            end_time,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::feedback_service::{FeedbackService, FeedbackSubmission};
    use sea_orm::{ActiveModelTrait, Set};
    use temps_entities::projects;

    #[tokio::test]
    async fn native_release_queries_bound_work_keep_pagination_and_scope() {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                eprintln!("Skipping native database test: Docker-backed TestDatabase unavailable");
                return;
            }
        };
        let db = test_db.connection_arc();
        let mut ids = vec![];
        for _ in 0..2 {
            ids.push(
                projects::ActiveModel {
                    name: Set("Synthetic release project".into()),
                    slug: Set(format!("synthetic-release-{}", uuid::Uuid::new_v4())),
                    repo_name: Set("synthetic".into()),
                    repo_owner: Set("synthetic".into()),
                    directory: Set("/synthetic".into()),
                    main_branch: Set("main".into()),
                    preset: Set(temps_entities::preset::Preset::NextJs),
                    created_at: Set(Utc::now()),
                    updated_at: Set(Utc::now()),
                    ..Default::default()
                }
                .insert(db.as_ref())
                .await
                .unwrap()
                .id,
            );
        }
        let ingestion = crate::services::ErrorIngestionService::new(db.clone());
        for project in &ids {
            ingestion.process_error_event(crate::services::CreateErrorEventData {
                project_id:*project,source:Some("sentry".into()),
                exception_type:Some("Message".into()),exception_value:Some("Synthetic release error".into()),
                release_version:Some("synthetic-release".into()),
                raw_sentry_event:Some(serde_json::json!({"event_id":uuid::Uuid::new_v4().simple().to_string(),"release":"synthetic-release"})),
                ..Default::default()
            }).await.unwrap();
        }
        let feedback = FeedbackService::new(db.clone());
        for (n, project) in [ids[0], ids[0], ids[1]].into_iter().enumerate() {
            feedback
                .ingest(
                    project,
                    None,
                    None,
                    vec![FeedbackSubmission {
                        source: "sentry".into(),
                        source_id: format!("{:032x}", n + 1),
                        timestamp: Utc::now() - Duration::days(if n == 1 { 2 } else { 0 }),
                        message: "Synthetic release feedback".into(),
                        name: None,
                        email: None,
                        url: None,
                        associated_event_id: None,
                        replay_id: None,
                        release: Some("synthetic-release".into()),
                        environment: None,
                        context: serde_json::json!({}),
                    }],
                )
                .await
                .unwrap();
        }
        let service = ReleaseService::new(db);
        let page = service.list(ids[0], None, 7, 1, 20).await.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].feedback, 2);
        assert_eq!(page.items[0].errors, 1);
        assert_eq!(
            service.list(ids[0], None, 1, 1, 20).await.unwrap().items[0].feedback,
            1
        );
        assert_eq!(
            service.list(ids[1], None, 7, 1, 20).await.unwrap().items[0].feedback,
            1
        );
        let beyond = service.list(ids[0], None, 7, 2, 20).await.unwrap();
        assert_eq!(beyond.total, 1);
        assert!(beyond.items.is_empty());
        assert_eq!(
            service
                .list(ids[0], Some("' OR '1'='1".into()), 7, 1, 20)
                .await
                .unwrap()
                .total,
            0
        );
        // Native retention/deletion immediately changes correlation; there is
        // no duplicate release index that can retain a stale signal count.
        let observed = service.list(ids[0], None, 7, 1, 20).await.unwrap();
        feedback
            .delete(ids[0], observed.items[0].example_feedback_id.unwrap())
            .await
            .unwrap();
        assert_eq!(
            service.list(ids[0], None, 7, 1, 20).await.unwrap().items[0].feedback,
            1
        );
        for (days, page, size) in [
            (0, 1, 20),
            (32, 1, 20),
            (7, 0, 20),
            (7, u64::MAX, 20),
            (7, 1, 101),
        ] {
            assert!(service.list(ids[0], None, days, page, size).await.is_err());
        }
    }
}
