use chrono::Utc;
use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum StatusPageError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Not found")]
    NotFound,
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Validate a status-monitor `check_path` to prevent URL/header injection
/// when constructing the probe URL (Fix #25).
///
/// Rules:
/// - Must start with `/`
/// - Must not contain `@` (prevents userinfo injection turning the probe
///   into a request against a different host)
/// - Must not contain `://` (prevents scheme injection)
/// - Must not contain CR, LF, NUL, or tab (prevents request smuggling)
/// - Capped at 2048 bytes
pub fn validate_check_path(path: &str) -> Result<(), StatusPageError> {
    if path.len() > 2048 {
        return Err(StatusPageError::Validation(format!(
            "check_path length {} exceeds 2048 byte limit",
            path.len()
        )));
    }
    if !path.starts_with('/') {
        return Err(StatusPageError::Validation(
            "check_path must start with '/'".to_string(),
        ));
    }
    if path.contains('@') {
        return Err(StatusPageError::Validation(
            "check_path must not contain '@' (userinfo injection)".to_string(),
        ));
    }
    if path.contains("://") {
        return Err(StatusPageError::Validation(
            "check_path must not contain '://' (scheme injection)".to_string(),
        ));
    }
    if path
        .chars()
        .any(|c| c == '\r' || c == '\n' || c == '\0' || c == '\t')
    {
        return Err(StatusPageError::Validation(
            "check_path must not contain control characters".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod check_path_tests {
    use super::*;

    #[test]
    fn rejects_no_leading_slash() {
        assert!(validate_check_path("health").is_err());
    }

    #[test]
    fn rejects_userinfo_injection() {
        assert!(validate_check_path("/foo@evil.com/bar").is_err());
    }

    #[test]
    fn rejects_scheme_injection() {
        assert!(validate_check_path("/x://attacker.com/y").is_err());
    }

    #[test]
    fn rejects_crlf() {
        assert!(validate_check_path("/foo\r\nHost: evil").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let long = format!("/{}", "a".repeat(2048));
        assert!(validate_check_path(&long).is_err());
    }

    #[test]
    fn accepts_simple_path() {
        assert!(validate_check_path("/health").is_ok());
    }

    #[test]
    fn accepts_path_with_query_chars() {
        assert!(validate_check_path("/api/v1/health?check=1&deep=true").is_ok());
    }
}

// Request/Response DTOs
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct CreateMonitorRequest {
    pub name: String,
    pub monitor_type: String,
    pub environment_id: i32, // Required: monitors must be associated with an environment
    #[serde(default)]
    pub check_interval_seconds: Option<i32>,
    #[serde(default)]
    pub check_path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MonitorResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub name: String,
    pub monitor_type: String,
    pub check_path: Option<String>,
    pub monitor_url: String, // The URL being monitored (constructed from environment)
    pub check_interval_seconds: i32,
    pub is_active: bool,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StatusCheckResponse {
    pub id: i32,
    pub monitor_id: i32,
    pub status: String,
    pub response_time_ms: Option<i32>,
    #[schema(value_type = String, format = "date-time")]
    pub checked_at: UtcDateTime,
    pub error_message: Option<String>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateIncidentRequest {
    pub title: String,
    pub description: Option<String>,
    pub severity: String,
    pub environment_id: Option<i32>,
    pub monitor_id: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IncidentResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub monitor_id: Option<i32>,
    pub title: String,
    pub description: Option<String>,
    pub severity: String,
    pub status: String,
    #[schema(value_type = String, format = "date-time")]
    pub started_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub resolved_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateIncidentStatusRequest {
    pub status: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IncidentUpdateResponse {
    pub id: i32,
    pub incident_id: i32,
    pub status: String,
    pub message: String,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StatusPageOverview {
    pub status: String,
    pub monitors: Vec<MonitorStatus>,
    pub recent_incidents: Vec<IncidentResponse>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MonitorStatus {
    pub monitor: MonitorResponse,
    pub current_status: String,
    pub uptime_percentage: f64,
    pub avg_response_time_ms: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UptimeHistoryResponse {
    pub monitor_id: i32,
    pub uptime_data: Vec<UptimeDataPoint>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UptimeDataPoint {
    #[schema(value_type = String, format = "date-time")]
    pub timestamp: UtcDateTime,
    pub status: String,
    pub response_time_ms: Option<i32>,
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CurrentStatusResponse {
    pub monitor_id: i32,
    pub current_status: String, // "operational", "degraded", "down", or "unknown"
    pub uptime_percentage: f64, // Uptime for the requested timeframe
    pub avg_response_time_ms: Option<f64>,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub last_check_at: Option<UtcDateTime>,
}

/// Health summary for a single project based on its production monitors
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectMonitorHealth {
    pub project_id: i32,
    /// Overall status: "operational", "degraded", "down", or "no_monitors"
    pub status: String,
}

// Time-bucketed aggregated data
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StatusBucketedResponse {
    pub monitor_id: i32,
    pub interval: String,
    pub buckets: Vec<StatusBucket>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StatusBucket {
    #[schema(value_type = String, format = "date-time")]
    pub bucket_start: chrono::DateTime<Utc>,
    pub status: String, // "operational", "degraded", or "down"
    pub total_checks: i64,
    pub operational_count: i64,
    pub degraded_count: i64,
    pub down_count: i64,
    pub uptime_percentage: f64,
    pub avg_response_time_ms: Option<f64>,
    pub min_response_time_ms: Option<f64>,
    pub max_response_time_ms: Option<f64>,
    pub p50_response_time_ms: Option<f64>,
    pub p95_response_time_ms: Option<f64>,
    pub p99_response_time_ms: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IncidentBucketedResponse {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub interval: String,
    pub buckets: Vec<IncidentBucket>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IncidentBucket {
    #[schema(value_type = String, format = "date-time")]
    pub bucket_start: chrono::DateTime<Utc>,
    pub total_incidents: i64,
    pub minor_incidents: i64,
    pub major_incidents: i64,
    pub critical_incidents: i64,
    pub resolved_incidents: i64,
    pub active_incidents: i64,
    pub avg_resolution_time_minutes: Option<f64>,
}

// From implementations
impl From<temps_entities::status_monitors::Model> for MonitorResponse {
    fn from(model: temps_entities::status_monitors::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            name: model.name,
            monitor_type: model.monitor_type,
            check_path: model.check_path,
            monitor_url: String::new(), // Will be populated by service layer
            check_interval_seconds: model.check_interval_seconds,
            is_active: model.is_active,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl MonitorResponse {
    /// Create MonitorResponse with monitor URL populated from environment subdomain
    pub fn with_url(mut self, url: String) -> Self {
        self.monitor_url = url;
        self
    }
}

impl From<temps_entities::status_checks::Model> for StatusCheckResponse {
    fn from(model: temps_entities::status_checks::Model) -> Self {
        Self {
            id: model.id,
            monitor_id: model.monitor_id,
            status: model.status,
            response_time_ms: model.response_time_ms,
            checked_at: model.checked_at,
            error_message: model.error_message,
            created_at: model.created_at,
        }
    }
}

impl From<temps_entities::status_incidents::Model> for IncidentResponse {
    fn from(model: temps_entities::status_incidents::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            monitor_id: model.monitor_id,
            title: model.title,
            description: model.description,
            severity: model.severity,
            status: model.status,
            started_at: model.started_at,
            resolved_at: model.resolved_at,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl From<temps_entities::status_incident_updates::Model> for IncidentUpdateResponse {
    fn from(model: temps_entities::status_incident_updates::Model) -> Self {
        Self {
            id: model.id,
            incident_id: model.incident_id,
            status: model.status,
            message: model.message,
            created_at: model.created_at,
        }
    }
}
