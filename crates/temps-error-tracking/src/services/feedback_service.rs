// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Native project feedback inbox. Protocol adapters only construct submissions;
//! persistence, retry identity, ownership and triage belong to this service.

use super::types::ErrorTrackingError;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use utoipa::ToSchema;

pub const MAX_FEEDBACK_BATCH: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackSubmission {
    pub source: String,
    pub source_id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub message: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
    pub associated_event_id: Option<String>,
    pub replay_id: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub context: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, FromQueryResult)]
pub struct FeedbackView {
    pub id: i64,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub source: String,
    pub source_id: String,
    pub timestamp: String,
    pub received_at: String,
    pub status: String,
    pub message: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
    pub associated_event_id: Option<String>,
    pub error_group_id: Option<i32>,
    pub replay_id: Option<String>,
    pub replay_session_id: Option<i32>,
    pub replay_visitor_id: Option<i32>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub context: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FeedbackPage {
    pub items: Vec<FeedbackView>,
    pub total: i64,
}

#[derive(Default)]
pub struct FeedbackReceipt {
    pub stored: u64,
    pub duplicates: u64,
}

pub struct FeedbackService {
    db: Arc<DatabaseConnection>,
}

const SELECT: &str = "SELECT f.id,f.project_id,f.environment_id,f.deployment_id,f.source,f.source_id,to_char(f.timestamp AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS timestamp,to_char(f.received_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS received_at,f.status,f.message,f.name,f.email,f.url,f.associated_event_id,f.replay_id,f.release,f.environment,f.context,r.id AS replay_session_id,r.visitor_id AS replay_visitor_id,(SELECT k.error_group_id FROM error_events k WHERE k.project_id=f.project_id AND (k.data #>> '{sentry,event_id}')=f.associated_event_id AND k.environment_id IS NOT DISTINCT FROM f.environment_id AND k.deployment_id IS NOT DISTINCT FROM f.deployment_id ORDER BY k.timestamp DESC,k.id DESC LIMIT 1) AS error_group_id FROM project_feedback f LEFT JOIN session_replay_sessions r ON r.project_id=f.project_id AND r.environment_id IS NOT DISTINCT FROM f.environment_id AND r.deployment_id IS NOT DISTINCT FROM f.deployment_id AND r.session_replay_id=('sentry-'||f.project_id||'-'||COALESCE(f.environment_id,0)||'-'||COALESCE(f.deployment_id,0)||'-'||f.replay_id) AND r.is_active=true";

impl FeedbackService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn ingest(
        &self,
        project: i32,
        environment: Option<i32>,
        deployment: Option<i32>,
        submissions: Vec<FeedbackSubmission>,
    ) -> Result<FeedbackReceipt, ErrorTrackingError> {
        if submissions.len() > MAX_FEEDBACK_BATCH {
            return Err(invalid());
        }
        if submissions.is_empty() {
            return Ok(FeedbackReceipt::default());
        }
        for f in &submissions {
            validate(f)?;
        }
        let count = submissions.len() as u64;
        let json = serde_json::to_value(submissions).map_err(|_| invalid())?;
        let tx = self.db.begin().await?;
        // Feedback is low-volume control data. Serialize identity checks per
        // project in this transaction, then write the entire batch once.
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(17303,$1)",
            [project.into()],
        ))
        .await?;
        let conflict = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT 1 AS conflict FROM project_feedback f JOIN jsonb_to_recordset($2::jsonb) AS s(source text,source_id text) ON f.source=s.source AND f.source_id=s.source_id WHERE f.project_id=$1 AND (f.environment_id IS DISTINCT FROM $3 OR f.deployment_id IS DISTINCT FROM $4) LIMIT 1",
            [project.into(),json.clone().into(),environment.into(),deployment.into()])).await?;
        if conflict.is_some() {
            return Err(ErrorTrackingError::Validation(
                "Feedback identity belongs to another DSN scope".into(),
            ));
        }
        let stored = tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO project_feedback(project_id,environment_id,deployment_id,source,source_id,timestamp,message,name,email,url,associated_event_id,replay_id,release,environment,context) SELECT $1,$3,$4,s.source,s.source_id,s.timestamp,s.message,s.name,s.email,s.url,s.associated_event_id,s.replay_id,s.release,s.environment,s.context FROM jsonb_to_recordset($2::jsonb) AS s(source text,source_id text,timestamp timestamptz,message text,name text,email text,url text,associated_event_id text,replay_id text,release text,environment text,context jsonb) ON CONFLICT(project_id,source,source_id) DO NOTHING",
            [project.into(),json.into(),environment.into(),deployment.into()])).await?.rows_affected();
        tx.commit().await?;
        Ok(FeedbackReceipt {
            stored,
            duplicates: count - stored,
        })
    }

    pub async fn list(
        &self,
        project: i32,
        status: Option<String>,
        page: u64,
        size: u64,
    ) -> Result<FeedbackPage, ErrorTrackingError> {
        if status.as_deref().is_some_and(|s| !valid_status(s)) {
            return Err(invalid());
        }
        let size = size.clamp(1, 100);
        let offset = page.clamp(1, 1_000_000).saturating_sub(1) * size;
        let values = vec![project.into(), status.into()];
        let tx = self
            .db
            .begin_with_config(None, Some(sea_orm::AccessMode::ReadOnly))
            .await?;
        let total = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT COUNT(*) AS total FROM project_feedback WHERE project_id=$1 AND ($2::text IS NULL OR status=$2)", values.clone())).await?.ok_or(invalid())?.try_get("","total")?;
        let mut values = values;
        values.push(size.into());
        values.push(offset.into());
        let items = FeedbackView::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            format!("{SELECT} WHERE f.project_id=$1 AND ($2::text IS NULL OR f.status=$2) ORDER BY f.received_at DESC,f.id DESC LIMIT $3 OFFSET $4"), values)).all(&tx).await?;
        tx.commit().await?;
        Ok(FeedbackPage { items, total })
    }

    pub async fn get(&self, project: i32, id: i64) -> Result<FeedbackView, ErrorTrackingError> {
        FeedbackView::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("{SELECT} WHERE f.project_id=$1 AND f.id=$2"),
            [project.into(), id.into()],
        ))
        .one(self.db.as_ref())
        .await?
        .ok_or(ErrorTrackingError::EventNotFound)
    }

    pub async fn delete(&self, project: i32, id: i64) -> Result<(), ErrorTrackingError> {
        let count = self
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM project_feedback WHERE project_id=$1 AND id=$2",
                [project.into(), id.into()],
            ))
            .await?
            .rows_affected();
        if count == 0 {
            return Err(ErrorTrackingError::EventNotFound);
        }
        Ok(())
    }

    pub async fn set_status(
        &self,
        project: i32,
        id: i64,
        status: &str,
    ) -> Result<(), ErrorTrackingError> {
        if !valid_status(status) {
            return Err(invalid());
        }
        let n = self.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE project_feedback SET status=$3,updated_at=NOW() WHERE project_id=$1 AND id=$2", [project.into(),id.into(),status.into()])).await?.rows_affected();
        if n == 0 {
            return Err(ErrorTrackingError::EventNotFound);
        }
        Ok(())
    }
}

fn invalid() -> ErrorTrackingError {
    ErrorTrackingError::Validation("Invalid feedback submission or filter".into())
}
pub fn valid_status(s: &str) -> bool {
    matches!(s, "open" | "resolved" | "ignored")
}
fn validate(f: &FeedbackSubmission) -> Result<(), ErrorTrackingError> {
    if f.message.trim().is_empty()
        || f.source != "sentry"
        || f.source_id.len() != 32
        || !f.source_id.bytes().all(|b| b.is_ascii_hexdigit())
        || f.source_id.bytes().all(|b| b == b'0')
    {
        return Err(invalid());
    }
    for (v, limit) in [
        (Some(f.message.as_str()), 16384),
        (f.name.as_deref(), 1024),
        (f.email.as_deref(), 1024),
        (f.url.as_deref(), 8192),
        (f.release.as_deref(), 256),
        (f.environment.as_deref(), 256),
        (f.associated_event_id.as_deref(), 32),
        (f.replay_id.as_deref(), 32),
    ] {
        if v.is_some_and(|v| v.len() > limit || v.contains('\0')) {
            return Err(invalid());
        }
    }
    for id in [&f.associated_event_id, &f.replay_id].into_iter().flatten() {
        if id.len() != 32
            || !id.bytes().all(|b| b.is_ascii_hexdigit())
            || id.bytes().all(|b| b == b'0')
        {
            return Err(invalid());
        }
    }
    let context = serde_json::to_string(&f.context).map_err(|_| invalid())?;
    if context.len() > 65536 || contains_nul(&f.context) {
        return Err(invalid());
    }
    Ok(())
}

fn contains_nul(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains('\0'),
        Value::Array(values) => values.iter().any(contains_nul),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| key.contains('\0') || contains_nul(value)),
        _ => false,
    }
}

pub(crate) fn validate_submission(f: &FeedbackSubmission) -> Result<(), ErrorTrackingError> {
    validate(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_entities::projects;

    async fn project(db: &DatabaseConnection) -> i32 {
        projects::ActiveModel {
            name: Set("Synthetic feedback project".into()),
            slug: Set(format!("synthetic-feedback-{}", uuid::Uuid::new_v4())),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap()
        .id
    }
    fn submission() -> FeedbackSubmission {
        FeedbackSubmission {
            source: "sentry".into(),
            source_id: "11111111111111111111111111111111".into(),
            timestamp: chrono::Utc::now(),
            message: "Synthetic feedback".into(),
            name: None,
            email: None,
            url: None,
            associated_event_id: None,
            replay_id: None,
            release: Some("synthetic-release".into()),
            environment: None,
            context: serde_json::json!({"tags":{"synthetic":true}}),
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn native_feedback_batches_retries_ownership_triage_and_rollback() {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                eprintln!("Skipping native database test: Docker-backed TestDatabase unavailable");
                return;
            }
        };
        let db = test_db.connection_arc();
        let a = project(&db).await;
        let b = project(&db).await;
        let service = Arc::new(FeedbackService::new(db.clone()));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let service = service.clone();
            tasks.push(tokio::spawn(async move {
                service
                    .ingest(a, None, None, vec![submission()])
                    .await
                    .unwrap()
            }));
        }
        let mut stored = 0;
        let mut duplicates = 0;
        for task in tasks {
            let receipt = task.await.unwrap();
            stored += receipt.stored;
            duplicates += receipt.duplicates;
        }
        assert_eq!((stored, duplicates), (1, 15));
        let list = service.list(a, None, 1, 20).await.unwrap();
        assert_eq!(list.total, 1);
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].message, "Synthetic feedback");
        assert!(list.items[0].timestamp.ends_with('Z'));
        assert!(list.items[0].received_at.ends_with('Z'));
        assert_eq!(list.items[0].context["tags"]["synthetic"], true);
        let id = list.items[0].id;
        assert!(service.set_status(b, id, "resolved").await.is_err());
        assert!(service.set_status(a, id, "invalid").await.is_err());
        service.set_status(a, id, "resolved").await.unwrap();
        assert_eq!(
            service
                .list(a, Some("open".into()), 1, 20)
                .await
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            service
                .list(a, Some("resolved".into()), 1, 20)
                .await
                .unwrap()
                .items[0]
                .id,
            id
        );
        assert_eq!(
            service
                .ingest(b, None, None, vec![submission()])
                .await
                .unwrap()
                .stored,
            1
        );
        assert!(service
            .ingest(a, Some(987654), None, vec![submission()])
            .await
            .is_err());
        let mut invalid = submission();
        invalid.source_id = "22222222222222222222222222222222".into();
        invalid.message = "invalid\0feedback".into();
        let mut valid = submission();
        valid.source_id = "33333333333333333333333333333333".into();
        assert!(service
            .ingest(a, None, None, vec![valid.clone(), invalid])
            .await
            .is_err());
        assert_eq!(service.list(a, None, 1, 20).await.unwrap().total, 1);
        db.execute_unprepared("CREATE FUNCTION synthetic_feedback_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic write failure'; END $$; CREATE TRIGGER synthetic_feedback_fail BEFORE INSERT ON project_feedback FOR EACH ROW EXECUTE FUNCTION synthetic_feedback_fail()").await.unwrap();
        assert!(service
            .ingest(a, None, None, vec![valid.clone()])
            .await
            .is_err());
        db.execute_unprepared("DROP TRIGGER synthetic_feedback_fail ON project_feedback; DROP FUNCTION synthetic_feedback_fail()").await.unwrap();
        assert_eq!(
            service
                .ingest(a, None, None, vec![valid])
                .await
                .unwrap()
                .stored,
            1
        );
        let count = db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT COUNT(*) AS count FROM error_groups",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "count")
            .unwrap();
        assert_eq!(count, 0, "feedback must not inflate native error groups");
        // Error linkage follows retained native events, independently of the
        // short-lived retry identity table.
        let sdk_id = "44444444444444444444444444444444";
        let error = crate::services::CreateErrorEventData {
            project_id: a,
            source: Some("sentry".into()),
            exception_type: Some("Message".into()),
            exception_value: Some("Synthetic linked feedback error".into()),
            raw_sentry_event: Some(
                serde_json::json!({"event_id":sdk_id,"message":"Synthetic linked feedback error"}),
            ),
            ..Default::default()
        };
        let group = crate::services::ErrorIngestionService::new(db.clone())
            .process_error_event(error)
            .await
            .unwrap();
        let mut linked = submission();
        linked.source_id = "55555555555555555555555555555555".into();
        linked.associated_event_id = Some(sdk_id.into());
        linked.context = serde_json::json!({"literal_escape":r"\u0000"});
        service.ingest(a, None, None, vec![linked]).await.unwrap();
        let row = service
            .list(a, None, 1, 20)
            .await
            .unwrap()
            .items
            .into_iter()
            .find(|r| r.associated_event_id.is_some())
            .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM error_event_source_keys WHERE project_id=$1",
            [a.into()],
        ))
        .await
        .unwrap();
        assert_eq!(
            service.get(a, row.id).await.unwrap().error_group_id,
            Some(group)
        );
        assert!(service.delete(b, row.id).await.is_err());
        service.delete(a, row.id).await.unwrap();
        assert!(service.get(a, row.id).await.is_err());
    }
}
