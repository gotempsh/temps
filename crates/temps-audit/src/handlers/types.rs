use crate::services::{AuditLogWithDetails, AuditService};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::DateTime;
use utoipa::{IntoParams, ToSchema};
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
    /// The user who performed the action (`null` when that account has
    /// since been deleted; `data` retains the original actor context)
    pub user_id: Option<i32>,
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

/// Query parameters for listing audit logs.
///
/// Every field is optional — omitting one means "don't filter on it". Deriving
/// `IntoParams` makes utoipa render them as optional query params with the
/// correct types; the previous hand-written `params(("operation_type", Query,
/// …))` tuples defaulted every param to `required: true, type: string`, which
/// misled both API clients and the AI `describe_api`/`call_api` tools into
/// thinking all filters were mandatory.
#[derive(Deserialize, Clone, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListAuditLogsQuery {
    /// Filter logs by operation type (omit for all)
    #[param(example = "user.login")]
    pub operation_type: Option<String>,
    /// Filter logs by user ID (omit for all users)
    #[param(example = 1)]
    pub user_id: Option<i32>,
    /// Start timestamp (milliseconds since epoch)
    #[param(example = 1)]
    pub from: Option<DateTime>,
    /// End timestamp (milliseconds since epoch)
    #[param(example = 1)]
    pub to: Option<DateTime>,
    /// Maximum number of logs to return
    #[param(example = 100)]
    pub limit: Option<i32>,
    /// Number of logs to skip
    #[param(example = 0)]
    pub offset: Option<i32>,
}

/// Request to redact a deleted user's identifier values from audit `data`
/// payloads. At least one field must be provided; values shorter than 3
/// characters are rejected.
#[derive(Deserialize, Clone, ToSchema)]
pub struct ScrubAuditDataRequest {
    /// Email address to redact wherever it appears as a payload value
    #[schema(example = "jane@example.com")]
    pub email: Option<String>,
    /// Username to redact wherever it appears as a payload value
    #[schema(example = "jane.doe")]
    pub username: Option<String>,
    /// Display name to redact wherever it appears as a payload value
    #[schema(example = "Jane Doe")]
    pub name: Option<String>,
}

/// Outcome of a PII scrub pass over audit log payloads.
#[derive(Serialize, ToSchema)]
pub struct ScrubAuditDataResponse {
    /// Number of audit rows whose payload was inspected
    pub rows_scanned: u64,
    /// Number of audit rows that had at least one value redacted
    pub rows_scrubbed: u64,
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
