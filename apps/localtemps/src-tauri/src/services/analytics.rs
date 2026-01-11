//! Analytics Service
//!
//! Handles ingestion and querying of analytics events from @temps-sdk/react-analytics.

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

use crate::entities::analytics_event::{self, ActiveModel, Entity as AnalyticsEvent, EventType};

#[derive(Error, Debug)]
pub enum AnalyticsError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AnalyticsError>;

/// Service for managing analytics events
pub struct AnalyticsService {
    db: Arc<DatabaseConnection>,
}

impl AnalyticsService {
    /// Create a new AnalyticsService
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Ingest a regular analytics event (page_view, page_leave, heartbeat, custom)
    pub async fn ingest_event(&self, payload: Value) -> Result<i32> {
        let event_name = payload
            .get("event_name")
            .and_then(|v| v.as_str())
            .map(String::from);
        let request_path = payload
            .get("request_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let request_query = payload
            .get("request_query")
            .and_then(|v| v.as_str())
            .map(String::from);
        let domain = payload
            .get("domain")
            .and_then(|v| v.as_str())
            .map(String::from);
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let request_id = payload
            .get("request_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        self.insert_event(
            EventType::Event,
            event_name,
            request_path,
            request_query,
            domain,
            session_id,
            request_id,
            payload,
        )
        .await
    }

    /// Ingest speed analytics (web vitals)
    pub async fn ingest_speed(&self, payload: Value) -> Result<i32> {
        let path = payload
            .get("path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .map(String::from);
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let request_id = payload
            .get("request_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        self.insert_event(
            EventType::Speed,
            Some("speed_metrics".to_string()),
            path,
            query,
            None,
            session_id,
            request_id,
            payload,
        )
        .await
    }

    /// Ingest session replay initialization
    pub async fn ingest_session_init(&self, payload: Value) -> Result<i32> {
        let session_id = payload
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(String::from);
        let url = payload
            .get("url")
            .and_then(|v| v.as_str())
            .map(String::from);

        self.insert_event(
            EventType::SessionInit,
            Some("session_init".to_string()),
            url,
            None,
            None,
            session_id,
            None,
            payload,
        )
        .await
    }

    /// Ingest session replay events (rrweb data)
    pub async fn ingest_session_events(&self, payload: Value) -> Result<i32> {
        let session_id = payload
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(String::from);

        self.insert_event(
            EventType::SessionEvents,
            Some("session_events".to_string()),
            None,
            None,
            None,
            session_id,
            None,
            payload,
        )
        .await
    }

    /// Insert an event into the database
    async fn insert_event(
        &self,
        event_type: EventType,
        event_name: Option<String>,
        request_path: Option<String>,
        request_query: Option<String>,
        domain: Option<String>,
        session_id: Option<String>,
        request_id: Option<String>,
        payload: Value,
    ) -> Result<i32> {
        let now = Utc::now().to_rfc3339();
        let payload_str = serde_json::to_string(&payload)?;

        let model = ActiveModel {
            event_type: Set(event_type.to_string()),
            event_name: Set(event_name),
            request_path: Set(request_path),
            request_query: Set(request_query),
            domain: Set(domain),
            session_id: Set(session_id),
            request_id: Set(request_id),
            payload: Set(payload_str),
            received_at: Set(now),
            ..Default::default()
        };

        let result = model.insert(self.db.as_ref()).await?;
        Ok(result.id)
    }

    /// List events with pagination, ordered by received_at DESC
    pub async fn list_events(
        &self,
        limit: u64,
        offset: u64,
        event_type: Option<&str>,
        event_name: Option<&str>,
    ) -> Result<Vec<analytics_event::Model>> {
        let mut query = AnalyticsEvent::find().order_by_desc(analytics_event::Column::ReceivedAt);

        if let Some(et) = event_type {
            query = query.filter(analytics_event::Column::EventType.eq(et));
        }

        if let Some(en) = event_name {
            query = query.filter(analytics_event::Column::EventName.eq(en));
        }

        let events = query
            .offset(offset)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;

        Ok(events)
    }

    /// Get a single event by ID
    pub async fn get_event(&self, id: i32) -> Result<Option<analytics_event::Model>> {
        let event = AnalyticsEvent::find_by_id(id).one(self.db.as_ref()).await?;
        Ok(event)
    }

    /// Count all events
    pub async fn count_events(&self) -> Result<u64> {
        let count = AnalyticsEvent::find().count(self.db.as_ref()).await?;
        Ok(count)
    }

    /// Clear all events
    pub async fn clear_events(&self) -> Result<u64> {
        let result = AnalyticsEvent::delete_many().exec(self.db.as_ref()).await?;
        Ok(result.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Database;
    use tempfile::TempDir;

    async fn setup_test_db() -> Arc<DatabaseConnection> {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
        let db = Database::connect(&database_url).await.unwrap();

        // Run migrations
        use crate::db::migrations::Migrator;
        use sea_orm_migration::MigratorTrait;
        Migrator::up(&db, None).await.unwrap();

        Arc::new(db)
    }

    #[tokio::test]
    async fn test_ingest_and_list_events() {
        let db = setup_test_db().await;
        let service = AnalyticsService::new(db);

        // Ingest an event
        let payload = serde_json::json!({
            "event_name": "page_view",
            "request_path": "/dashboard",
            "domain": "localhost:3000"
        });

        let id = service.ingest_event(payload).await.unwrap();
        assert!(id > 0);

        // List events
        let events = service.list_events(10, 0, None, None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name, Some("page_view".to_string()));
    }
}
