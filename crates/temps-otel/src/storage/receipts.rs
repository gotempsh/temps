// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional adapter identities, claimed in the SAME transaction as native rows.
//! No content-based guesses for anonymous observations. One bounded batch claim,
//! not per-record I/O; receipts contain hashes only. OTLP keeps its old behavior.

use crate::error::{OtelError, StorageErrorKind};
use crate::types::IngestIdentity;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use std::collections::BTreeSet;

pub(crate) async fn claim(
    tx: &DatabaseTransaction,
    signal: &str,
    records: &[(i32, Option<&IngestIdentity>)],
) -> Result<Vec<usize>, OtelError> {
    let mut unique = BTreeSet::new();
    let mut rows = Vec::new();
    let mut accepted = Vec::new();
    for (index, (project, identity)) in records.iter().enumerate() {
        match identity {
            None => accepted.push(index),
            Some(identity) => {
                let key = identity.as_str();
                if unique.insert((*project, key)) {
                    rows.push(serde_json::json!({"project":project,"key":key,"index":index}));
                }
            }
        }
    }
    if rows.is_empty() {
        return Ok(accepted);
    }
    if rows.len() > 1000 {
        return Err(OtelError::Validation {
            message: "Adapter identity batch exceeds 1000 records".into(),
        });
    }
    let result = tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres, r#"
        WITH input AS (
            SELECT * FROM jsonb_to_recordset($2) AS r(project integer,key text,index integer)
        ), claims AS (
            INSERT INTO otel_ingest_receipts(project_id,signal,source_key,expires_at)
            SELECT project,$1,key,NOW()+INTERVAL '1 hour' FROM input ORDER BY project,key
            ON CONFLICT(project_id,signal,source_key) DO UPDATE SET expires_at=EXCLUDED.expires_at
            WHERE otel_ingest_receipts.expires_at<NOW()
            RETURNING project_id,source_key
        ), budgets AS (
            INSERT INTO otel_ingest_receipt_budgets(project_id,bucket,used)
            SELECT project_id,date_trunc('hour',NOW()),count(*)::integer FROM (
                SELECT project_id FROM claims UNION ALL SELECT NULL::integer FROM claims
            ) q GROUP BY project_id ORDER BY project_id NULLS FIRST
            ON CONFLICT(project_id,bucket) DO UPDATE SET used=otel_ingest_receipt_budgets.used+EXCLUDED.used
            RETURNING id
        ) SELECT i.index FROM input i JOIN claims c ON c.project_id=i.project AND c.source_key=i.key
          WHERE (SELECT count(*) FROM budgets)>=0
    "#, [signal.into(), serde_json::Value::Array(rows).into()])).await.map_err(|error| {
        // Capacity is backpressure, not a permanently rejected observation.
        let capacity = match &error {
            sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(error)) | sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(error)) => error.as_database_error().and_then(|db| db.constraint()) == Some("otel_ingest_receipt_capacity"),
            _ => false,
        };
        if capacity {
            OtelError::Storage { message: "Adapter retry identity capacity reached".into(), kind: StorageErrorKind::AdapterCapacity }
        } else { error.into() }
    })?;
    for row in result {
        accepted.push(row.try_get::<i32>("", "index")? as usize);
    }
    accepted.sort_unstable();
    Ok(accepted)
}

pub async fn cleanup(db: &impl ConnectionTrait) -> Result<u64, OtelError> {
    let n = db.execute_unprepared("DELETE FROM otel_ingest_receipts WHERE expires_at<NOW() AND id IN (SELECT id FROM otel_ingest_receipts WHERE expires_at<NOW() ORDER BY expires_at LIMIT 2000)").await?.rows_affected();
    db.execute_unprepared("DELETE FROM otel_ingest_receipt_budgets WHERE id IN (SELECT id FROM otel_ingest_receipt_budgets WHERE bucket<date_trunc('hour',NOW())-INTERVAL '1 hour' ORDER BY bucket LIMIT 2000)").await?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        storage::{timescaledb::TimescaleDbStorage, OtelStorage},
        types::*,
    };
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Set};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    // Required acceptance: fail when the isolated test database is unavailable,
    // rather than reporting a skipped storage test as evidence of success.
    #[tokio::test]
    async fn adapter_receipts_atomic_concurrent_rollback_capacity_expiry_and_anonymous() {
        let test_db = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test_db.connection_arc();
        let project = temps_entities::projects::ActiveModel {
            name: Set("Synthetic adapter acceptance".into()),
            slug: Set(format!("synthetic-adapter-{}", uuid::Uuid::new_v4())),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let storage = Arc::new(TimescaleDbStorage::new(db.clone(), None));
        let now = Utc::now();
        let attrs = BTreeMap::new();
        let resource = ResourceInfo {
            service_name: "synthetic".into(),
            ..Default::default()
        };
        let span = SpanRecord {
            ingest_identity: Some(IngestIdentity::from_digest([0x11; 32])),
            project_id: project.id,
            deployment_id: None,
            resource: resource.clone(),
            trace_id: "1".repeat(32),
            span_id: "2".repeat(16),
            parent_span_id: None,
            name: "Synthetic root".into(),
            kind: SpanKind::Server,
            start_time: now,
            end_time: now,
            duration_ms: 0.0,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: attrs.clone(),
            events: vec![],
        };
        let log = LogRecord {
            ingest_identity: Some(IngestIdentity::from_digest([0x11; 32])),
            project_id: project.id,
            deployment_id: None,
            resource: resource.clone(),
            timestamp: now,
            observed_timestamp: now,
            severity: LogSeverity::Info,
            severity_text: "info".into(),
            body: "Synthetic log".into(),
            trace_id: Some(span.trace_id.clone()),
            span_id: Some(span.span_id.clone()),
            attributes: attrs.clone(),
            attribute_types: Default::default(),
        };
        let mut metric = MetricPoint::skeleton(
            project.id,
            None,
            resource,
            "synthetic.counter".into(),
            MetricType::Sum,
            "none".into(),
            now,
            attrs,
        );
        metric.ingest_identity = Some(IngestIdentity::from_digest([0x11; 32]));
        metric.value = Some(3.0);
        metric.unit = "millisecond".into();
        metric
            .attributes
            .insert("temps.sentry.value".into(), "3.000000000000000001".into());
        metric.exemplars.push(Exemplar {
            timestamp: now,
            value: 3.0,
            trace_id: Some(span.trace_id.clone()),
            span_id: Some(span.span_id.clone()),
            attributes: Default::default(),
        });
        metric.temporality = Some(AggregationTemporality::Delta);
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let (storage, span, log, metric) =
                (storage.clone(), span.clone(), log.clone(), metric.clone());
            tasks.push(tokio::spawn(async move {
                // Duplicate identities within the same batch must also collapse.
                (
                    storage.store_spans(vec![span.clone(), span]).await.unwrap(),
                    storage.store_logs(vec![log]).await.unwrap(),
                    storage.store_metrics(vec![metric]).await.unwrap(),
                )
            }));
        }
        let mut totals = (0, 0, 0);
        for task in tasks {
            let n = task.await.unwrap();
            totals.0 += n.0;
            totals.1 += n.1;
            totals.2 += n.2;
        }
        assert_eq!(totals, (1, 1, 1));
        let summary = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT span_count FROM otel_trace_summaries WHERE project_id=$1 AND trace_id=$2",
                [project.id.into(), span.trace_id.clone().into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.try_get::<i64>("", "span_count").unwrap(), 1);

        let point=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT unit,temporality,attributes,exemplars FROM otel_metrics WHERE project_id=$1",[project.id.into()])).await.unwrap().unwrap();
        assert_eq!(point.try_get::<String>("", "unit").unwrap(), "millisecond");
        assert_eq!(point.try_get::<String>("", "temporality").unwrap(), "delta");
        assert_eq!(
            point
                .try_get::<serde_json::Value>("", "attributes")
                .unwrap()["temps.sentry.value"],
            "3.000000000000000001"
        );
        let exemplars = point.try_get::<serde_json::Value>("", "exemplars").unwrap();
        assert_eq!(exemplars[0]["trace_id"], span.trace_id);
        assert_eq!(exemplars[0]["span_id"], span.span_id);

        // A rejected native row rolls back the claim and its capacity reservation.
        db.execute_unprepared("CREATE FUNCTION synthetic_reject_log() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.body='synthetic reject' THEN RAISE EXCEPTION 'synthetic abort' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER synthetic_reject_log BEFORE INSERT ON otel_log_events FOR EACH ROW EXECUTE FUNCTION synthetic_reject_log()").await.unwrap();
        let mut failed = log.clone();
        failed.ingest_identity = Some(IngestIdentity::from_digest([0x33; 32]));
        failed.body = "synthetic reject".into();
        assert!(storage
            .store_logs(vec![failed.clone()])
            .await
            .unwrap_err()
            .is_transient());
        failed.body = "Synthetic recovered".into();
        assert_eq!(storage.store_logs(vec![failed.clone()]).await.unwrap(), 1);
        assert_eq!(storage.store_logs(vec![failed]).await.unwrap(), 0);
        db.execute_unprepared("DROP TRIGGER synthetic_reject_log ON otel_log_events; DROP FUNCTION synthetic_reject_log()").await.unwrap();

        // Scope is project-local even when two adapters produce the same key.
        let other = temps_entities::projects::ActiveModel {
            name: Set("Synthetic other".into()),
            slug: Set(format!("synthetic-other-{}", uuid::Uuid::new_v4())),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let mut isolated = log.clone();
        isolated.project_id = other.id;
        assert_eq!(storage.store_logs(vec![isolated]).await.unwrap(), 1);

        // Saturation rolls back the entire native batch. Known retries still ACK.
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE otel_ingest_receipt_budgets SET used=16384 WHERE project_id=$1",
            [project.id.into()],
        ))
        .await
        .unwrap();
        let mut fresh = log.clone();
        fresh.ingest_identity = Some(IngestIdentity::from_digest([0x44; 32]));
        let error = storage.store_logs(vec![fresh.clone()]).await.unwrap_err();
        assert!(matches!(
            error,
            OtelError::Storage {
                kind: StorageErrorKind::AdapterCapacity,
                ..
            }
        ));
        assert_eq!(storage.store_logs(vec![log.clone()]).await.unwrap(), 0);
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE otel_ingest_receipt_budgets SET used=0 WHERE project_id=$1",
            [project.id.into()],
        ))
        .await
        .unwrap();
        assert_eq!(storage.store_logs(vec![fresh]).await.unwrap(), 1);

        let mut anonymous = log.clone();
        anonymous.ingest_identity = None;
        assert_eq!(
            storage
                .store_logs(vec![anonymous.clone(), anonymous])
                .await
                .unwrap(),
            2
        );
        // A regular application attribute is data, even when its name used to
        // be an internal key. Invalid/repeated values cannot alter delivery.
        for key in ["not-a-hash".to_string(), "a".repeat(64)] {
            let mut native = log.clone();
            native.ingest_identity = None;
            native
                .attributes
                .insert("temps.ingest.source_key".into(), key);
            assert_eq!(
                storage
                    .store_logs(vec![native.clone(), native.clone()])
                    .await
                    .unwrap(),
                2
            );
            let serialized = serde_json::to_value(&native).unwrap();
            assert!(serialized.get("ingest_identity").is_none());
            let mut injected = serialized;
            injected["ingest_identity"] = serde_json::json!("1".repeat(64));
            let decoded: LogRecord = serde_json::from_value(injected).unwrap();
            assert!(decoded.ingest_identity.is_none());
            assert_eq!(storage.store_logs(vec![decoded]).await.unwrap(), 1);
        }
        let mut native_span = span.clone();
        native_span.ingest_identity = None;
        native_span.span_id = "e".repeat(16);
        native_span
            .attributes
            .insert("temps.ingest.source_key".into(), "not-a-hash".into());
        let mut next_span = native_span.clone();
        next_span.span_id = "f".repeat(16);
        assert_eq!(
            storage
                .store_spans(vec![native_span, next_span])
                .await
                .unwrap(),
            2
        );
        let mut native_metric = metric.clone();
        native_metric.ingest_identity = None;
        native_metric
            .attributes
            .insert("temps.ingest.source_key".into(), "not-a-hash".into());
        let mut next_metric = native_metric.clone();
        next_metric.timestamp += chrono::Duration::milliseconds(1);
        assert_eq!(
            storage
                .store_metrics(vec![native_metric, next_metric])
                .await
                .unwrap(),
            2
        );
        // Timestamps can share the DB's microsecond, or even a frozen SDK
        // clock tick. Order by original precision, then SDK sequence, not arrival.
        let mut ordered = Vec::new();
        for (suffix, sequence) in [(2, 0), (1, 1), (1, 9)] {
            let mut value = log.clone();
            value.ingest_identity = None;
            value.body = format!("Synthetic ordering {suffix}/{sequence}");
            value.attributes.insert(
                "temps.sentry.timestamp".into(),
                format!(
                    "{}.{:06}{suffix:03}",
                    now.timestamp(),
                    now.timestamp_subsec_micros()
                ),
            );
            value
                .attributes
                .insert("sentry.timestamp.sequence".into(), sequence.to_string());
            ordered.push(value);
        }
        assert_eq!(storage.store_logs(ordered).await.unwrap(), 3);
        let ordered = storage
            .query_logs(LogQuery {
                project_id: project.id,
                search: Some("Synthetic ordering".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            ordered.iter().map(|v| v.body.as_str()).collect::<Vec<_>>(),
            vec![
                "Synthetic ordering 2/0",
                "Synthetic ordering 1/9",
                "Synthetic ordering 1/1"
            ]
        );

        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE otel_ingest_receipts SET expires_at=NOW()-INTERVAL '1 second' WHERE project_id=$1 AND signal='logs' AND source_key=$2",[project.id.into(),"1".repeat(64).into()])).await.unwrap();
        assert_eq!(storage.store_logs(vec![log.clone()]).await.unwrap(), 1);
        cleanup(db.as_ref()).await.unwrap();
        assert_eq!(storage.store_logs(vec![log]).await.unwrap(), 0);
    }
}
