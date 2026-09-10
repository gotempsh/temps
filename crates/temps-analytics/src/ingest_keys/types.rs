// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Types for analytics ingest keys (ADR-040).
//!
//! # The key is public, deliberately
//!
//! `public_key` is **not a secret**. It is minted to be pasted into a browser
//! bundle and into `?temps_key=` query strings, exactly like a Sentry DSN
//! public key or a PostHog project key. Consequently it is stored in the clear,
//! returned in the clear by every API response, and never masked with `***`.
//! Masking it would imply a confidentiality property this credential does not
//! have, and would send an operator hunting for a "reveal" affordance that must
//! not exist.
//!
//! The `pa_` prefix ("public analytics") is chosen to be visually distinct from
//! `tk_` (API keys), `dt_` (deployment tokens) and `si_` (service ingest
//! tokens), all of which *are* secrets and *are* hashed at rest.

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use thiserror::Error;
use utoipa::ToSchema;

/// Prefix on every analytics ingest key. See the module docs: this marks the
/// value as public, non-secret, write-only and analytics-only.
pub const ANALYTICS_INGEST_KEY_PREFIX: &str = "pa_";

/// Number of random bytes behind a key. Hex-encoded this yields 64 characters,
/// so a full key is `pa_` + 64 = 67 characters, inside the column's
/// `varchar(80)`. Matches the entropy of `DSNService::generate_key(32)`.
pub const ANALYTICS_INGEST_KEY_BYTES: usize = 32;

/// Label applied when an operator does not supply one. Mirrors the column
/// default in `m20260831_000001_create_analytics_ingest_keys`.
pub const DEFAULT_INGEST_KEY_NAME: &str = "Default ingest key";

/// Longest accepted operator-facing label (matches `varchar(128)`).
pub const MAX_INGEST_KEY_NAME_LEN: usize = 128;

/// Upper bound on `allowed_origins` entries. Bounded so a single row cannot be
/// grown into an unbounded JSON blob that every hot-path resolve must parse.
pub const MAX_ALLOWED_ORIGINS: usize = 50;

/// Longest accepted single origin entry.
pub const MAX_ALLOWED_ORIGIN_LEN: usize = 253;

/// Ceiling on a key's `rate_limit_per_minute`.
///
/// 100k requests/minute is ~1,667/s from a single project — comfortably above
/// any legitimate site's ingest volume, and far below "may as well be
/// unlimited". The bound exists so a stored number that *reads* as a limit
/// actually constrains something: `i32::MAX` per minute is unlimited wearing a
/// limit's clothes, and an operator reviewing the row would never notice.
/// Genuinely unlimited has its own explicit, honest encoding — `null` or a
/// non-positive value.
pub const MAX_RATE_LIMIT_PER_MINUTE: i32 = 100_000;

/// An analytics ingest key as returned by the admin API.
///
/// `public_key` is present in full on every response — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsIngestKey {
    pub id: i32,
    pub project_id: i32,
    /// `None` means the key is scoped to the whole project. An
    /// environment-scoped key additionally attributes ingested data to that
    /// environment's current deployment, when it has one.
    pub environment_id: Option<i32>,
    /// Operator-facing label.
    pub name: String,
    /// The ingest key itself, `pa_` + 64 hex characters.
    ///
    /// **Not a secret.** It is designed to ship in client-side JavaScript and
    /// is returned unmasked so operators can copy it. See the module docs.
    pub public_key: String,
    pub is_active: bool,
    #[schema(value_type = Option<String>, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub revoked_at: Option<UtcDateTime>,
    /// `None` or `<= 0` means unlimited.
    pub rate_limit_per_minute: Option<i32>,
    /// Exact origins (`scheme://host[:port]`) permitted to use this key from a
    /// browser. `None` or `[]` means any origin. This is a browser-enforced
    /// convenience control, not authentication — a non-browser client ignores
    /// `Origin` entirely.
    pub allowed_origins: Option<Vec<String>>,
    pub event_count: i64,
    #[schema(value_type = Option<String>, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub last_used_at: Option<UtcDateTime>,
    pub created_by_user_id: Option<i32>,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub updated_at: UtcDateTime,
}

/// Request body for minting a new ingest key.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CreateAnalyticsIngestKeyRequest {
    /// Scope the key to a single environment. Recommended: it lets Temps
    /// attribute ingested data to that environment's current deployment.
    /// Omit for a project-wide key.
    pub environment_id: Option<i32>,
    /// Operator-facing label. Defaults to "Default ingest key".
    pub name: Option<String>,
    /// Exact origins permitted to use this key from a browser. Omit or send an
    /// empty array to allow any origin.
    pub allowed_origins: Option<Vec<String>>,
    /// Requests per minute for this key. Omit for the 600/min default; send a
    /// non-positive value for unlimited.
    pub rate_limit_per_minute: Option<i32>,
}

/// Request body for a partial update.
///
/// `allowed_origins` and `rate_limit_per_minute` use the three-state
/// double-`Option` encoding: field absent = leave unchanged, explicit `null` =
/// clear, value = set. Plain `Option<Option<T>>` alone cannot express this
/// because serde collapses an explicit JSON `null` into the outer `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateAnalyticsIngestKeyRequest {
    /// New operator-facing label. Absent leaves it unchanged. The column is
    /// `NOT NULL`, so there is no "clear" state — send a new label instead.
    pub name: Option<String>,
    /// Absent = unchanged, `null` = clear (any origin allowed), array = replace.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    #[schema(value_type = Option<Vec<String>>)]
    pub allowed_origins: Option<Option<Vec<String>>>,
    /// Absent = unchanged, `null` = clear (unlimited), value = replace.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    #[schema(value_type = Option<i32>)]
    pub rate_limit_per_minute: Option<Option<i32>>,
}

/// Deserialize a PATCH field that distinguishes three states: absent (`None`),
/// present-null (`Some(None)` → clear), and present-value (`Some(Some(v))` →
/// set). Pair with `#[serde(default, deserialize_with = "...")]`.
fn deserialize_optional_field<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    // serde only invokes this when the key is present, so wrap whatever we
    // parse — including an explicit `null` — in an outer `Some`.
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

/// The ingest scope a presented key resolves to.
///
/// This is the contract between the key service and the five public analytics
/// ingest handlers: once a key resolves, the `Host` header is no longer
/// consulted for resolution (ADR-040 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIngestScope {
    pub project_id: i32,
    /// `None` for a project-scoped key.
    pub environment_id: Option<i32>,
    /// **Derived, never stored.** Read from `environments.current_deployment_id`
    /// at resolve time when the key is environment-scoped, so a key never has
    /// to be re-minted on deploy. `None` in the no-deployment case this feature
    /// exists for.
    pub deployment_id: Option<i32>,
    /// Row id of the key, used as the rate-limiter bucket and the usage-counter
    /// target.
    pub key_id: i32,
    /// `None` or empty means any origin.
    pub allowed_origins: Option<Vec<String>>,
    /// `None` or `<= 0` means unlimited.
    pub rate_limit_per_minute: Option<i32>,
}

/// Errors produced by [`crate::ingest_keys::AnalyticsIngestKeyService`].
#[derive(Debug, Error)]
pub enum AnalyticsIngestKeyError {
    #[error("Analytics ingest key {key_id} not found in project {project_id}")]
    KeyNotFound { key_id: i32, project_id: i32 },

    #[error("Project {project_id} not found while managing analytics ingest keys")]
    ProjectNotFound { project_id: i32 },

    #[error("Environment {environment_id} not found while scoping an analytics ingest key for project {project_id}")]
    EnvironmentNotFound {
        environment_id: i32,
        project_id: i32,
    },

    #[error("Environment {environment_id} belongs to project {environment_project_id}, not project {project_id}; refusing to scope an analytics ingest key across projects")]
    EnvironmentProjectMismatch {
        environment_id: i32,
        environment_project_id: i32,
        project_id: i32,
    },

    #[error("Validation error on analytics ingest key {field}: {message}")]
    Validation { field: String, message: String },

    #[error("Analytics ingest key {key_id} has a malformed allowed_origins column: {reason}")]
    MalformedAllowedOrigins { key_id: i32, reason: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_request_distinguishes_absent_from_explicit_null() {
        let absent: UpdateAnalyticsIngestKeyRequest =
            serde_json::from_str(r#"{}"#).expect("empty object should deserialize");
        assert_eq!(absent.allowed_origins, None);
        assert_eq!(absent.rate_limit_per_minute, None);

        let cleared: UpdateAnalyticsIngestKeyRequest =
            serde_json::from_str(r#"{"allowed_origins": null, "rate_limit_per_minute": null}"#)
                .expect("explicit nulls should deserialize");
        assert_eq!(cleared.allowed_origins, Some(None));
        assert_eq!(cleared.rate_limit_per_minute, Some(None));

        let set: UpdateAnalyticsIngestKeyRequest = serde_json::from_str(
            r#"{"allowed_origins": ["https://app.example.com"], "rate_limit_per_minute": 10}"#,
        )
        .expect("values should deserialize");
        assert_eq!(
            set.allowed_origins,
            Some(Some(vec!["https://app.example.com".to_string()]))
        );
        assert_eq!(set.rate_limit_per_minute, Some(Some(10)));
    }

    #[test]
    fn key_prefix_and_length_match_the_column_width() {
        // `pa_` + hex(32 bytes) = 3 + 64 = 67, inside varchar(80).
        let rendered = ANALYTICS_INGEST_KEY_PREFIX.len() + ANALYTICS_INGEST_KEY_BYTES * 2;
        assert_eq!(rendered, 67);
        assert!(rendered <= 80);
    }

    #[test]
    fn errors_carry_identifiers() {
        let err = AnalyticsIngestKeyError::KeyNotFound {
            key_id: 42,
            project_id: 7,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("42"), "{rendered}");
        assert!(rendered.contains('7'), "{rendered}");
    }
}
