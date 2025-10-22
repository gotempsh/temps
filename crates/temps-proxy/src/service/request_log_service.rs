use chrono::DateTime;
use sea_orm::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::request_logs;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum RequestLogServiceError {
    #[error("Database error")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Invalid filter parameters: {0}")]
    InvalidFilter(String),
}

/// Response model for request logs - includes all fields from request_logs entity
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RequestLogResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub date: String,
    pub host: String,
    pub method: String,
    pub request_path: String,
    pub message: String,
    pub status_code: i32,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub request_id: String,
    pub level: String,
    pub user_agent: String,
    pub started_at: String,
    pub finished_at: String,
    pub elapsed_time: Option<i32>,
    pub is_static_file: Option<bool>,
    pub referrer: Option<String>,
    pub ip_address: Option<String>,
    pub session_id: Option<i32>,
    pub headers: Option<String>,
    pub request_headers: Option<String>,
    pub ip_address_id: Option<i32>,
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub is_mobile: bool,
    pub is_entry_page: bool,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub visitor_id: Option<i32>,
}

impl From<request_logs::Model> for RequestLogResponse {
    fn from(model: request_logs::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            deployment_id: model.deployment_id,
            date: model.date,
            host: model.host,
            method: model.method,
            request_path: model.request_path,
            message: model.message,
            status_code: model.status_code,
            branch: model.branch,
            commit: model.commit,
            request_id: model.request_id,
            level: model.level,
            user_agent: model.user_agent,
            started_at: model.started_at,
            finished_at: model.finished_at,
            elapsed_time: model.elapsed_time,
            is_static_file: model.is_static_file,
            referrer: model.referrer,
            ip_address: model.ip_address,
            session_id: model.session_id,
            headers: model.headers,
            request_headers: model.request_headers,
            ip_address_id: model.ip_address_id,
            browser: model.browser,
            browser_version: model.browser_version,
            operating_system: model.operating_system,
            is_mobile: model.is_mobile,
            is_entry_page: model.is_entry_page,
            is_crawler: model.is_crawler,
            crawler_name: model.crawler_name,
            visitor_id: model.visitor_id,
        }
    }
}

/// Service for querying request logs
pub struct RequestLogService {
    db: Arc<DatabaseConnection>,
}

impl RequestLogService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Get request logs with optional filters
    #[allow(clippy::too_many_arguments)]
    pub async fn get_logs(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        status_code: Option<i32>,
        method: Option<&str>,
        start_date: Option<i64>,
        end_date: Option<i64>,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<RequestLogResponse>, u64), RequestLogServiceError> {
        let limit = std::cmp::min(limit, 100);

        let mut query = request_logs::Entity::find();

        // Apply optional project filter
        if let Some(proj_id) = project_id {
            query = query.filter(request_logs::Column::ProjectId.eq(proj_id));
        }

        // Apply optional filters
        if let Some(env_id) = environment_id {
            query = query.filter(request_logs::Column::EnvironmentId.eq(env_id));
        }

        if let Some(dep_id) = deployment_id {
            query = query.filter(request_logs::Column::DeploymentId.eq(dep_id));
        }

        if let Some(status) = status_code {
            query = query.filter(request_logs::Column::StatusCode.eq(status));
        }

        if let Some(http_method) = method {
            query = query.filter(request_logs::Column::Method.eq(http_method));
        }

        // Date range filtering - convert milliseconds to datetime string
        if let Some(start_ms) = start_date {
            if let Some(start_dt) = DateTime::from_timestamp_millis(start_ms) {
                let start_str = start_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
                query = query.filter(request_logs::Column::StartedAt.gte(start_str));
            }
        }

        if let Some(end_ms) = end_date {
            if let Some(end_dt) = DateTime::from_timestamp_millis(end_ms) {
                let end_str = end_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
                query = query.filter(request_logs::Column::StartedAt.lte(end_str));
            }
        }

        // Order by most recent first
        query = query.order_by_desc(request_logs::Column::Id);

        // Get total count before applying limit/offset
        let total = query.clone().count(self.db.as_ref()).await?;

        // Apply limit and offset
        let items = query
            .limit(limit)
            .offset(offset)
            .all(self.db.as_ref())
            .await?;

        let responses = items.into_iter().map(RequestLogResponse::from).collect();

        Ok((responses, total))
    }

    /// Get a single request log by ID
    pub async fn get_log_by_id(
        &self,
        id: i32,
        project_id: Option<i32>,
    ) -> Result<Option<RequestLogResponse>, RequestLogServiceError> {
        let mut query = request_logs::Entity::find_by_id(id);

        if let Some(proj_id) = project_id {
            query = query.filter(request_logs::Column::ProjectId.eq(proj_id));
        }

        let log = query.one(self.db.as_ref()).await?;

        Ok(log.map(RequestLogResponse::from))
    }

    /// Get request logs by request_id
    pub async fn get_logs_by_request_id(
        &self,
        request_id: &str,
        project_id: i32,
    ) -> Result<Vec<RequestLogResponse>, RequestLogServiceError> {
        let logs = request_logs::Entity::find()
            .filter(request_logs::Column::RequestId.eq(request_id))
            .filter(request_logs::Column::ProjectId.eq(project_id))
            .order_by_desc(request_logs::Column::Id)
            .all(self.db.as_ref())
            .await?;

        Ok(logs.into_iter().map(RequestLogResponse::from).collect())
    }

    /// Get request logs by session
    pub async fn get_logs_by_session(
        &self,
        session_id: i32,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<RequestLogResponse>, u64), RequestLogServiceError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let query = request_logs::Entity::find()
            .filter(request_logs::Column::SessionId.eq(session_id))
            .filter(request_logs::Column::ProjectId.eq(project_id))
            .order_by_desc(request_logs::Column::Id);

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;

        let responses = items.into_iter().map(RequestLogResponse::from).collect();

        Ok((responses, total))
    }

    /// Get request logs by visitor
    pub async fn get_logs_by_visitor(
        &self,
        visitor_id: i32,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<RequestLogResponse>, u64), RequestLogServiceError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let query = request_logs::Entity::find()
            .filter(request_logs::Column::VisitorId.eq(visitor_id))
            .filter(request_logs::Column::ProjectId.eq(project_id))
            .order_by_desc(request_logs::Column::Id);

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;

        let responses = items.into_iter().map(RequestLogResponse::from).collect();

        Ok((responses, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;

    #[tokio::test]
    async fn test_get_logs_pagination() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let service = RequestLogService::new(test_db.connection_arc().clone());

        // Test with default pagination
        let result = service
            .get_logs(Some(1), None, None, None, None, None, None, 20, 0)
            .await;
        assert!(result.is_ok());

        let (logs, total) = result.unwrap();
        assert_eq!(total, 0); // Should be 0 in empty test database
        assert_eq!(logs.len(), 0);
    }

    #[tokio::test]
    async fn test_get_logs_with_filters() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let service = RequestLogService::new(test_db.connection_arc().clone());

        // Test with environment filter
        let result = service
            .get_logs(Some(1), Some(1), None, None, None, None, None, 20, 0)
            .await;
        assert!(result.is_ok());

        // Test with deployment filter
        let result = service
            .get_logs(Some(1), None, Some(1), None, None, None, None, 20, 0)
            .await;
        assert!(result.is_ok());

        // Test with status code and method filters
        let result = service
            .get_logs(
                Some(1),
                None,
                None,
                Some(200),
                Some("GET"),
                None,
                None,
                20,
                0,
            )
            .await;
        assert!(result.is_ok());

        // Test with date range filters
        let start = chrono::Utc::now().timestamp_millis() - 86400000; // 24 hours ago
        let end = chrono::Utc::now().timestamp_millis();
        let result = service
            .get_logs(
                Some(1),
                None,
                None,
                None,
                None,
                Some(start),
                Some(end),
                20,
                0,
            )
            .await;
        assert!(result.is_ok());
    }
}
