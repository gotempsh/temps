use crate::services::{AuditService, AuditLogWithDetails};
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use utoipa::ToSchema;
use temps_core::DateTime;
pub struct AppState {
    pub audit_service: Arc<AuditService>,
}


/// Response type for audit log entries
#[derive(Serialize, ToSchema)]
pub struct AuditLogResponse {
    /// Unique identifier for the audit log entry
    pub id: i32,
    /// The type of action that was performed
    #[schema(example = "USER_LOGIN")]
    pub operation_type: String,
    /// The user who performed the action
    pub user_id: i32,
    /// User details who performed the action
    pub user: Option<AuditLogUserInfo>,
    /// IP address details
    pub ip_address: Option<AuditLogIpInfo>,
    /// When the action occurred
    #[schema(example = 11932193)]
    pub audit_date: i64,
    /// Additional context about the action
    pub data: Option<serde_json::Value>,
}

/// User information in audit log
#[derive(Serialize, ToSchema)]
pub struct AuditLogUserInfo {
    /// User ID
    pub id: i32,
    /// User's name
    #[schema(example = "John Doe")]
    pub name: String,
    /// User's email
    #[schema(example = "john.doe@example.com")]
    pub email: String,
}

/// IP address information in audit log
#[derive(Serialize, ToSchema)]
pub struct AuditLogIpInfo {
    /// IP address
    #[schema(example = "192.168.1.1")]
    pub ip: String,
    /// Country code
    #[schema(example = "US")]
    pub country: Option<String>,
    /// City name
    #[schema(example = "San Francisco")]
    pub city: Option<String>,
    /// Latitude
    #[schema(example = 37.7749)]
    pub latitude: Option<f64>,
    /// Longitude
    #[schema(example = 122.4194)]
    pub longitude: Option<f64>,
}

/// Query parameters for listing audit logs
#[derive(Deserialize, Clone, ToSchema)]
pub struct ListAuditLogsQuery {
    /// Filter logs by operation type
    #[schema(example = "user.login")]
    pub operation_type: Option<String>,
    /// Filter logs by user ID
    #[schema(example = 1)]
    pub user_id: Option<i32>,
    /// Start timestamp (milliseconds since epoch)
    #[schema(example = 1)]
    pub from: Option<DateTime>,
    /// End timestamp (milliseconds since epoch)
    #[schema(example = 1)]
    pub to: Option<DateTime>,
    /// Maximum number of logs to return
    #[schema(example = 100)]
    pub limit: Option<i32>,
    /// Number of logs to skip
    #[schema(example = 0)]
    pub offset: Option<i32>,
}

impl From<AuditLogWithDetails> for AuditLogResponse {
    fn from(details: AuditLogWithDetails) -> Self {
        Self {
            id: details.log.id,
            operation_type: details.log.operation_type,
            user_id: details.log.user_id,
            user: details.user.map(|u| AuditLogUserInfo {
                id: u.id,
                name: u.name,
                email: u.email,
            }),
            ip_address: details.ip_address.map(|ip| AuditLogIpInfo {
                ip: ip.ip_address,
                country: Some(ip.country),
                city: ip.city,
                latitude: ip.latitude,
                longitude: ip.longitude,
            }),
            audit_date: details.log.audit_date.timestamp_millis(),
            data: serde_json::from_str(&details.log.data).ok(),
        }
    }
}
