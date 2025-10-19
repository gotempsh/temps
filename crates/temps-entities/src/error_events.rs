use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

// ============= STRUCTURED DATA TYPES =============

/// Complete error event data - all context grouped in one structure
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ErrorEventData {
    /// Source of the error event (e.g., "sentry", "custom", "bugsnag", etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    /// User context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserContext>,

    /// Device/Browser context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceContext>,

    /// Request context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestContext>,

    /// Stack trace frames
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<Vec<StackFrame>>,

    /// Environment/SDK context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentContext>,

    /// Trace/Debug context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,

    /// Sentry-specific data (complete SDK payload, contexts, raw event data)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sentry: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct UserContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Additional custom user context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct DeviceContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_kernel_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_width: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_height: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_width: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_height: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processor_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processor_frequency: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_memory: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_time: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct RequestContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookies: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_string: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_data: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct StackFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineno: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colno: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abs_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_context: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_context: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct EnvironmentContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_integrations: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_memory: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct TraceContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breadcrumbs: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contexts: Option<serde_json::Value>,
}

// ============= MAIN ENTITY =============

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "error_events")]
pub struct Model {
    // ===== Primary Key =====
    #[sea_orm(primary_key)]
    pub id: i64,

    // ===== Foreign Keys (ACID - referential integrity) =====
    pub error_group_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub ip_geolocation_id: Option<i32>,

    // ===== Indexed fields (ACID - fast queries) =====
    pub fingerprint_hash: String,
    pub timestamp: DBDateTime,

    // ===== Core error data (frequently displayed) =====
    pub exception_type: String,
    pub exception_value: Option<String>,

    // ===== Source tracking (for multi-SDK support) =====
    /// Source of the error event (e.g., "sentry", "custom", "bugsnag")
    pub source: Option<String>,

    // ===== ALL STRUCTURED DATA IN ONE JSONB COLUMN =====
    /// Structured error event data (use ErrorEventData::from/to for type safety)
    #[sea_orm(column_type = "JsonBinary")]
    pub data: Option<serde_json::Value>,

    // ===== Metadata =====
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::error_groups::Entity",
        from = "Column::ErrorGroupId",
        to = "super::error_groups::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    ErrorGroups,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Projects,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Environments,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Deployments,
    #[sea_orm(
        belongs_to = "super::visitor::Entity",
        from = "Column::VisitorId",
        to = "super::visitor::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Visitor,
    #[sea_orm(
        belongs_to = "super::ip_geolocations::Entity",
        from = "Column::IpGeolocationId",
        to = "super::ip_geolocations::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    IpGeolocations,
}

impl Related<super::error_groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ErrorGroups.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environments.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployments.def()
    }
}

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl Related<super::ip_geolocations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IpGeolocations.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

// ============= HELPER METHODS =============

impl Model {
    /// Get typed data from JSONB column
    pub fn get_data(&self) -> Option<ErrorEventData> {
        self.data.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Set data from typed struct
    pub fn set_data(data: ErrorEventData) -> Option<serde_json::Value> {
        serde_json::to_value(data).ok()
    }
}

impl ErrorEventData {
    /// Convert to JSONB value for database storage
    pub fn to_json_value(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }

    /// Parse from JSONB value
    pub fn from_json_value(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}
