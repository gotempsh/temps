use temps_core::UtcDateTime;

use sea_orm::{DbErr, FromQueryResult};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Represents a single exception with its stack trace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionData {
    /// The type/class of the exception (e.g., "TypeError", "ValueError")
    pub exception_type: String,
    /// The exception message/value
    pub exception_value: Option<String>,
    /// The stack trace for this specific exception
    pub stack_trace: Option<serde_json::Value>,
    /// Mechanism information (how the exception was captured)
    pub mechanism: Option<serde_json::Value>,
    /// Module where the exception occurred
    pub module: Option<String>,
    /// Thread ID if available
    pub thread_id: Option<String>,
}

#[derive(Error, Debug)]
pub enum ErrorTrackingError {
    #[error("Database error: {0}")]
    Database(#[from] DbErr),

    #[error("Error group not found")]
    GroupNotFound,

    #[error("Error event not found")]
    EventNotFound,

    #[error("Invalid fingerprint")]
    InvalidFingerprint,

    #[error("Vector embedding service error: {0}")]
    EmbeddingService(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Project not found")]
    ProjectNotFound,
}

/// Input data for creating a new error event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateErrorEventData {
    // Source of the error event (e.g., "sentry", "custom", "bugsnag")
    pub source: Option<String>,

    // Raw Sentry event (full event payload from Sentry SDK)
    pub raw_sentry_event: Option<serde_json::Value>,

    // Core error information - list of exceptions (Sentry can have multiple)
    pub exceptions: Vec<ExceptionData>,

    // Legacy fields for backward compatibility (derived from first exception if present)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<serde_json::Value>,

    // Context
    pub url: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub method: Option<String>,
    pub headers: Option<serde_json::Value>,

    // User information
    pub user_id: Option<String>,
    pub user_email: Option<String>,
    pub user_username: Option<String>,
    pub user_ip_address: Option<String>,
    pub user_segment: Option<String>,
    pub session_id: Option<String>,
    pub user_context: Option<serde_json::Value>,

    // Device/Browser context
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,

    // Additional context
    pub request_context: Option<serde_json::Value>,
    pub extra_context: Option<serde_json::Value>,
    pub release_version: Option<String>,
    pub build_number: Option<String>,

    // Server information
    pub server_name: Option<String>,
    pub environment: Option<String>, // Sentry environment field

    // SDK information
    pub sdk_name: Option<String>,
    pub sdk_version: Option<String>,
    pub sdk_integrations: Option<serde_json::Value>, // JSONB - array of SDK integrations

    // Platform information
    pub platform: Option<String>, // node, javascript, python, etc.

    // Transaction information
    pub transaction_name: Option<String>,

    // Breadcrumbs
    pub breadcrumbs: Option<serde_json::Value>, // JSONB - array of breadcrumb objects

    // Request details (expanded)
    pub request_cookies: Option<serde_json::Value>, // JSONB
    pub request_query_string: Option<serde_json::Value>, // JSONB
    pub request_data: Option<serde_json::Value>,    // JSONB - POST data

    // Contexts (expanded device/OS info)
    pub contexts: Option<serde_json::Value>, // JSONB - full contexts object
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub os_build: Option<String>,
    pub os_kernel_version: Option<String>,
    pub device_arch: Option<String>,
    pub device_processor_count: Option<i32>,
    pub device_processor_frequency: Option<i32>,
    pub device_memory_size: Option<i64>,
    pub device_free_memory: Option<i64>,
    pub device_boot_time: Option<UtcDateTime>,
    pub runtime_name: Option<String>,
    pub runtime_version: Option<String>,
    pub app_start_time: Option<UtcDateTime>,
    pub app_memory: Option<i64>,
    pub locale: Option<String>,
    pub timezone: Option<String>,

    // Relations
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub ip_geolocation_id: Option<i32>,
}

impl Default for CreateErrorEventData {
    fn default() -> Self {
        Self {
            source: None,
            raw_sentry_event: None,
            exceptions: Vec::new(),
            exception_type: None,
            exception_value: None,
            stack_trace: None,
            url: None,
            user_agent: None,
            referrer: None,
            method: None,
            headers: None,
            user_id: None,
            user_email: None,
            user_username: None,
            user_ip_address: None,
            user_segment: None,
            session_id: None,
            user_context: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            operating_system_version: None,
            device_type: None,
            screen_width: None,
            screen_height: None,
            viewport_width: None,
            viewport_height: None,
            request_context: None,
            extra_context: None,
            release_version: None,
            build_number: None,
            server_name: None,
            environment: None,
            sdk_name: None,
            sdk_version: None,
            sdk_integrations: None,
            platform: None,
            transaction_name: None,
            breadcrumbs: None,
            request_cookies: None,
            request_query_string: None,
            request_data: None,
            contexts: None,
            os_name: None,
            os_version: None,
            os_build: None,
            os_kernel_version: None,
            device_arch: None,
            device_processor_count: None,
            device_processor_frequency: None,
            device_memory_size: None,
            device_free_memory: None,
            device_boot_time: None,
            runtime_name: None,
            runtime_version: None,
            app_start_time: None,
            app_memory: None,
            locale: None,
            timezone: None,
            project_id: 0,
            environment_id: None,
            deployment_id: None,
            visitor_id: None,
            ip_geolocation_id: None,
        }
    }
}

/// Domain model for error groups
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorGroupDomain {
    pub id: i32,
    pub title: String,
    pub error_type: String,
    pub message_template: Option<String>,
    pub first_seen: UtcDateTime,
    pub last_seen: UtcDateTime,
    pub total_count: i32,
    pub status: String,
    pub assigned_to: Option<String>,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

/// Domain model for error events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEventDomain {
    pub id: i64,
    pub error_group_id: i32,
    pub fingerprint_hash: String,
    pub timestamp: UtcDateTime,

    // Source of the error event
    pub source: Option<String>,

    // Core error data
    pub exception_type: String,
    pub exception_value: Option<String>,
    pub stack_trace: Option<serde_json::Value>,

    // Request context
    pub url: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub method: Option<String>,
    pub headers: Option<serde_json::Value>,

    // User context
    pub user_id: Option<String>,
    pub user_email: Option<String>,
    pub user_username: Option<String>,
    pub user_ip_address: Option<String>,
    pub user_segment: Option<String>,
    pub session_id: Option<String>,
    pub user_context: Option<serde_json::Value>,

    // Device/Browser context
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,

    // Request context
    pub request_context: Option<serde_json::Value>,

    // Additional context
    pub extra_context: Option<serde_json::Value>,

    // Release/Build information
    pub release_version: Option<String>,
    pub build_number: Option<String>,

    // Server information
    pub server_name: Option<String>,
    pub environment: Option<String>,

    // SDK information
    pub sdk_name: Option<String>,
    pub sdk_version: Option<String>,
    pub sdk_integrations: Option<serde_json::Value>,

    // Platform information
    pub platform: Option<String>,

    // Transaction information
    pub transaction_name: Option<String>,

    // Breadcrumbs
    pub breadcrumbs: Option<serde_json::Value>,

    // Request details (expanded)
    pub request_cookies: Option<serde_json::Value>,
    pub request_query_string: Option<serde_json::Value>,
    pub request_data: Option<serde_json::Value>,

    // Contexts (expanded device/OS info)
    pub contexts: Option<serde_json::Value>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub os_build: Option<String>,
    pub os_kernel_version: Option<String>,
    pub device_arch: Option<String>,
    pub device_processor_count: Option<i32>,
    pub device_processor_frequency: Option<i32>,
    pub device_memory_size: Option<i64>,
    pub device_free_memory: Option<i64>,
    pub device_boot_time: Option<UtcDateTime>,
    pub runtime_name: Option<String>,
    pub runtime_version: Option<String>,
    pub app_start_time: Option<UtcDateTime>,
    pub app_memory: Option<i64>,
    pub locale: Option<String>,
    pub timezone: Option<String>,

    // Relations
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,

    // Geography
    pub ip_geolocation_id: Option<i32>,

    // Raw JSONB data (full transparency - contains complete error event)
    pub data: Option<serde_json::Value>,

    // Metadata
    pub created_at: UtcDateTime,
}

/// Statistics for error groups
#[derive(Debug, Serialize, Deserialize, FromQueryResult)]
pub struct ErrorGroupStats {
    pub total_groups: i64,
    pub unresolved_groups: i64,
    pub resolved_groups: i64,
    pub ignored_groups: i64,
}

/// Time series data point for error trends
#[derive(Debug, Serialize, Deserialize, FromQueryResult)]
pub struct ErrorTimeSeriesPoint {
    pub timestamp: UtcDateTime,
    pub count: i64,
}
