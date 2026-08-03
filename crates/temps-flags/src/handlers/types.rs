//! Request/response DTOs and shared handler state for feature flags.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use temps_core::{AuditLogger, ProjectAccessChecker};
use temps_entities::{feature_flag_environments, feature_flags};
use utoipa::ToSchema;

use crate::eval::{FlagSnapshot, FlagValueType};
use crate::services::FlagService;

pub struct FlagsAppState {
    pub flag_service: Arc<FlagService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Team-based project access checker (registered by a plugin; `None` in
    /// plain OSS, where the guard is a no-op). Confines callers to projects
    /// they may reach.
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

// =============================================================================
// Tri-state deserializers
// =============================================================================
//
// Serde's plain `Option<Option<T>>` collapses a present JSON `null` into `None`,
// which makes "clear this field" indistinguishable from "leave it alone". These
// mirror the helpers in `temps-environments`.

/// - field absent → `None` (leave unchanged)
/// - field present as `null` → `Some(None)` (clear the description)
/// - field present as a string → `Some(Some(s))`
fn deserialize_optional_optional_string<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Some(None)),
        v => {
            let s: String = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
            Ok(Some(Some(s)))
        }
    }
}

/// - field absent → `None` (leave unchanged)
/// - field present as `null` → `Some(None)` (clear the override, inherit default)
/// - anything else → `Some(Some(value))`
///
/// `null` is unambiguous here because it is never a legal flag value: a flag
/// must always resolve to something usable, so `null` can only mean "unset".
fn deserialize_optional_optional_value<'de, D>(
    deserializer: D,
) -> Result<Option<Option<serde_json::Value>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Some(None)),
        v => Ok(Some(Some(v))),
    }
}

// =============================================================================
// Requests
// =============================================================================

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateFlagRequest {
    /// Stable key used in application code. Immutable after create.
    #[schema(example = "checkout.v2")]
    pub key: String,
    /// Fixed at create: retyping would invalidate every stored value and every
    /// call site.
    pub value_type: FlagValueType,
    /// Served whenever evaluation cannot do better. Must match `value_type`.
    ///
    /// Left unannotated so utoipa emits a free-form schema: a bool flag's
    /// default is `false`, not an object, and `value_type = Object` would tell
    /// every generated client otherwise.
    #[schema(example = false)]
    pub default_value: serde_json::Value,
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the flag may be exposed on the unauthenticated same-origin
    /// evaluation endpoint. Defaults to `false`: flags are server-only unless
    /// explicitly opted in, because targeting rules can encode business logic.
    #[serde(default)]
    pub client_visible: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateFlagRequest {
    /// Must match the flag's existing `value_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    /// Tri-state: absent leaves it, `null` clears it, a string sets it.
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_visible: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetFlagEnvironmentRequest {
    /// Tri-state: absent leaves the override, `null` clears it (inherit the
    /// flag default), anything else sets it. Must match `value_type`.
    #[serde(default, deserialize_with = "deserialize_optional_optional_value")]
    pub value: Option<Option<serde_json::Value>>,
    /// The kill switch. `false` makes the flag serve its default regardless of
    /// any override — and, once targeting exists, regardless of any rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

// =============================================================================
// Responses
// =============================================================================

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlagEnvironmentResponse {
    pub environment_id: i32,
    pub enabled: bool,
    pub value: Option<serde_json::Value>,
}

impl From<feature_flag_environments::Model> for FlagEnvironmentResponse {
    fn from(model: feature_flag_environments::Model) -> Self {
        Self {
            environment_id: model.environment_id,
            enabled: model.enabled,
            value: model.value,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlagResponse {
    pub id: i32,
    pub key: String,
    pub value_type: String,
    pub default_value: serde_json::Value,
    pub description: Option<String>,
    pub client_visible: bool,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Per-environment overrides. Empty means the flag inherits its default
    /// everywhere.
    pub environments: Vec<FlagEnvironmentResponse>,
}

impl FlagResponse {
    pub fn new(
        flag: feature_flags::Model,
        environments: Vec<feature_flag_environments::Model>,
    ) -> Self {
        Self {
            id: flag.id,
            key: flag.key,
            value_type: flag.value_type,
            default_value: flag.default_value,
            description: flag.description,
            client_visible: flag.client_visible,
            archived_at: flag.archived_at.map(|t| t.to_rfc3339()),
            created_at: flag.created_at.to_rfc3339(),
            updated_at: flag.updated_at.to_rfc3339(),
            environments: environments.into_iter().map(Into::into).collect(),
        }
    }
}

/// Note the absence of `salt`: it is never exposed. Publishing the bucketing
/// salt would let a client predict, and self-select into, a rollout cohort.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlagListResponse {
    pub flags: Vec<FlagResponse>,
    /// Total flags matching the filter, across all pages.
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlagSnapshotResponse {
    pub environment_id: i32,
    /// Flags collapsed to what the evaluator needs, sorted by key so the
    /// serialized form — and therefore the ETag — is stable.
    pub flags: Vec<FlagSnapshot>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArchiveFlagResponse {
    pub key: String,
    pub archived_at: Option<String>,
}
