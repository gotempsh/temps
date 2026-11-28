// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request/response DTOs and shared handler state for feature flags.

use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use temps_core::{AuditLogger, ProjectAccessChecker};
use temps_entities::{feature_flag_environments, feature_flags};
use utoipa::ToSchema;

use crate::eval::{AttributeValue, FlagRule, FlagSnapshot, FlagValueType, FlagVariant};
use crate::services::{parse_rules, parse_variants, FlagService};

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
    /// Named values targeting rules may serve. Each must match `value_type`.
    #[serde(default)]
    pub variants: Vec<FlagVariant>,
    /// Share of subjects (`0..=1`) whose browser records a `flag_exposure`
    /// event when it reads the flag. Defaults to 0: exposure is opt-in.
    #[serde(default)]
    pub exposure_sample_rate: f64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateFlagRequest {
    /// Must match the flag's existing `value_type`.
    #[serde(default)]
    pub default_value: Option<serde_json::Value>,
    /// Tri-state: absent leaves it, `null` clears it, a string sets it.
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub client_visible: Option<bool>,
    /// Replaces the variant list. A variant still served by a rule cannot be
    /// removed.
    #[serde(default)]
    pub variants: Option<Vec<FlagVariant>>,
    #[serde(default)]
    pub exposure_sample_rate: Option<f64>,
    /// The flag's `updated_at` as last read. When sent, the update is refused
    /// with 409 if the flag changed since.
    #[serde(default)]
    pub expected_updated_at: Option<String>,
}

/// Replace the ordered targeting rules of a flag in one environment.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetFlagRulesRequest {
    /// First match wins. An empty list removes all targeting.
    pub rules: Vec<FlagRule>,
    /// The environment row's `updated_at` as last read, or absent when the
    /// flag had no row for this environment. A 409 means someone else changed
    /// the rules in between: reload and apply the change again.
    #[serde(default)]
    pub expected_updated_at: Option<String>,
}

/// Evaluate one flag as an app in an environment would.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EvaluateFlagRequest {
    pub environment_id: i32,
    /// Bucketing subject. Required for rules with a rollout or a split to
    /// apply.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, AttributeValue>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetFlagEnvironmentRequest {
    /// Tri-state: absent leaves the override, `null` clears it (inherit the
    /// flag default), anything else sets it. Must match `value_type`.
    #[serde(default, deserialize_with = "deserialize_optional_optional_value")]
    pub value: Option<Option<serde_json::Value>>,
    /// The kill switch. `false` makes the flag serve its default regardless of
    /// any override — and, once targeting exists, regardless of any rule.
    #[serde(default)]
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
    /// Ordered targeting rules; first match wins.
    pub rules: Vec<FlagRule>,
    /// `false` when the stored rules could not be decoded. The flag then
    /// serves its default with reason `ERROR` until they are replaced.
    pub rules_valid: bool,
    /// Send back as `expected_updated_at` when replacing the rules.
    pub updated_at: String,
}

/// ISO 8601 with a `Z` suffix and the stored precision, so a revision sent
/// back as `expected_updated_at` compares equal.
pub(crate) fn iso(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

impl From<feature_flag_environments::Model> for FlagEnvironmentResponse {
    fn from(model: feature_flag_environments::Model) -> Self {
        let parsed = parse_rules(&model);
        Self {
            environment_id: model.environment_id,
            enabled: model.enabled,
            value: model.value,
            rules_valid: parsed.is_some(),
            rules: parsed.unwrap_or_default(),
            updated_at: iso(model.updated_at),
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
    /// Named values targeting rules may serve.
    pub variants: Vec<FlagVariant>,
    /// Share of subjects whose browser records exposure events. 0 = off.
    pub exposure_sample_rate: f64,
    pub archived_at: Option<String>,
    /// When an app last actually evaluated this flag. `None` means never seen,
    /// which is a real answer rather than missing data.
    pub last_evaluated_at: Option<String>,
    pub created_at: String,
    /// Send back as `expected_updated_at` to make an update conditional.
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
        let variants = parse_variants(&flag);
        Self {
            id: flag.id,
            variants,
            exposure_sample_rate: flag.exposure_sample_rate,
            key: flag.key,
            value_type: flag.value_type,
            default_value: flag.default_value,
            description: flag.description,
            client_visible: flag.client_visible,
            archived_at: flag.archived_at.map(iso),
            last_evaluated_at: flag.last_evaluated_at.map(iso),
            created_at: iso(flag.created_at),
            updated_at: iso(flag.updated_at),
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

/// Keys a running app actually evaluated since its last report.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RecordExposureRequest {
    /// Flag keys evaluated since the last report. Unknown keys are ignored.
    #[schema(example = json!(["checkout.v2", "api.rate_limit"]))]
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordExposureResponse {
    /// How many keys were accepted for processing.
    ///
    /// Deliberately not the number of rows updated: echoing that back would
    /// let a caller post a single candidate key and read the result as "this
    /// flag exists", turning the endpoint into an existence oracle.
    pub recorded: u64,
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
    /// True when the flag was deleted for good (`purge=true`) and its key is
    /// free again; false when it was archived and can be restored.
    pub purged: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_use_the_z_suffix_and_keep_their_precision() {
        use chrono::TimeZone;
        let timestamp = Utc
            .timestamp_opt(1_763_640_000, 123_456_000)
            .single()
            .expect("a valid timestamp");
        assert_eq!(iso(timestamp), "2025-11-20T12:00:00.123456Z");
        let parsed = DateTime::parse_from_rfc3339(&iso(timestamp)).expect("parses back");
        assert_eq!(parsed.with_timezone(&Utc), timestamp);
    }

    // The tri-state distinction (absent vs explicit null vs value) is the
    // mechanism that has silently regressed in this codebase before: serde's
    // plain Option<Option<T>> collapses a present `null` into `None`, making
    // "clear this field" indistinguishable from "leave it alone".

    #[test]
    fn absent_description_means_leave_unchanged() {
        let parsed: UpdateFlagRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.description, None);
    }

    #[test]
    fn null_description_means_clear_it() {
        let parsed: UpdateFlagRequest = serde_json::from_str(r#"{"description":null}"#).unwrap();
        assert_eq!(
            parsed.description,
            Some(None),
            "an explicit null must survive as the clear signal"
        );
    }

    #[test]
    fn present_description_means_set_it() {
        let parsed: UpdateFlagRequest = serde_json::from_str(r#"{"description":"hi"}"#).unwrap();
        assert_eq!(parsed.description, Some(Some("hi".to_string())));
    }

    #[test]
    fn absent_environment_value_means_leave_the_override() {
        let parsed: SetFlagEnvironmentRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.value, None);
        assert_eq!(parsed.enabled, None);
    }

    #[test]
    fn null_environment_value_means_inherit_the_default() {
        let parsed: SetFlagEnvironmentRequest = serde_json::from_str(r#"{"value":null}"#).unwrap();
        assert_eq!(
            parsed.value,
            Some(None),
            "null clears the override so the flag inherits its default"
        );
    }

    #[test]
    fn false_is_a_value_not_a_clear() {
        let parsed: SetFlagEnvironmentRequest = serde_json::from_str(r#"{"value":false}"#).unwrap();
        assert_eq!(
            parsed.value,
            Some(Some(serde_json::json!(false))),
            "false must not be confused with null — it is a legitimate flag value"
        );
    }

    #[test]
    fn kill_switch_and_value_are_independent() {
        let parsed: SetFlagEnvironmentRequest =
            serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert_eq!(parsed.enabled, Some(false));
        assert_eq!(parsed.value, None, "disabling must not clear the override");
    }
}
