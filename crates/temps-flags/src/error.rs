// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed errors for the feature-flag domain.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum FlagError {
    #[error("Feature flag '{key}' not found in project {project_id}")]
    NotFound { project_id: i32, key: String },

    #[error("Feature flag '{key}' already exists in project {project_id}")]
    DuplicateKey { project_id: i32, key: String },

    #[error("Environment {environment_id} does not belong to project {project_id}")]
    EnvironmentNotInProject {
        project_id: i32,
        environment_id: i32,
    },

    #[error("Environment {environment_id} not found")]
    EnvironmentNotFound { environment_id: i32 },

    #[error(
        "Invalid flag key '{key}': {reason}. Keys must match [a-z0-9][a-z0-9._-]* and be at most 128 characters"
    )]
    InvalidKey { key: String, reason: String },

    #[error("Invalid value type '{value_type}' for flag '{key}': must be one of bool, string, number, json")]
    InvalidValueType { key: String, value_type: String },

    #[error("Value {value} is not valid for flag '{key}' of type '{value_type}' (field: {field})")]
    ValueTypeMismatch {
        key: String,
        value_type: String,
        value: String,
        field: String,
    },

    #[error("Validation error for flag '{key}': {message}")]
    Validation { key: String, message: String },

    #[error("Deployment token for project {project_id} is not scoped to a single environment; feature-flag snapshots require an environment-scoped token")]
    TokenNotEnvironmentScoped { project_id: i32 },

    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("Failed to serialize flag value for flag '{key}': {reason}")]
    Serialization { key: String, reason: String },
}

/// Semantic mapping from Sea-ORM errors, per CLAUDE.md.
///
/// The one that matters is the unique-index violation on
/// `idx_feature_flags_project_key`. `create()` probes for a duplicate before
/// inserting, but the probe and the insert are not atomic — two concurrent
/// creates of the same key both pass the probe and the index rejects the
/// loser. Without this mapping that surfaces as a 500, while a request that
/// loses the race by a millisecond earlier gets a clean 409. Same conflict,
/// two different answers.
impl From<sea_orm::DbErr> for FlagError {
    fn from(error: sea_orm::DbErr) -> Self {
        if is_unique_violation(&error) {
            return FlagError::DuplicateKey {
                // The IDs are not recoverable from the driver error; the
                // handler's 409 is what the caller acts on.
                project_id: 0,
                key: "<already exists>".to_string(),
            };
        }
        FlagError::Database(error)
    }
}

/// Detect a Postgres unique-constraint violation (SQLSTATE 23505).
///
/// Sea-ORM does not surface SQLSTATE as a typed field, so this inspects the
/// rendered driver error — the same approach the rest of the codebase takes.
fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    match error {
        sea_orm::DbErr::RecordNotInserted => true,
        other => {
            let rendered = other.to_string();
            // Require the SQLSTATE to sit alongside the word "unique" rather
            // than matching "23505" on its own. Flag keys permit digits, so a
            // flag named `23505` could otherwise make an unrelated query error
            // report itself as a duplicate — a 409 hiding a real 500.
            rendered.contains("duplicate key value violates unique constraint")
                || (rendered.contains("23505") && rendered.contains("unique"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_not_inserted_maps_to_duplicate_key() {
        let mapped: FlagError = sea_orm::DbErr::RecordNotInserted.into();
        assert!(matches!(mapped, FlagError::DuplicateKey { .. }));
    }

    /// The concurrent-create race: the unique index rejects the loser, and the
    /// caller must see the same 409 it would have got from the pre-check.
    #[test]
    fn postgres_unique_violation_maps_to_duplicate_key() {
        let raw = sea_orm::DbErr::Custom(
            "error returned from database: (23505) duplicate key value violates unique \
             constraint \"idx_feature_flags_project_key\""
                .to_string(),
        );
        assert!(matches!(
            FlagError::from(raw),
            FlagError::DuplicateKey { .. }
        ));
    }

    #[test]
    fn unrelated_database_errors_stay_database_errors() {
        let raw = sea_orm::DbErr::Custom("connection reset by peer".to_string());
        assert!(matches!(FlagError::from(raw), FlagError::Database(_)));
    }

    /// Flag keys permit digits, so a flag named `23505` must not turn an
    /// unrelated failure into a 409 that hides a real 500.
    #[test]
    fn a_flag_key_that_looks_like_a_sqlstate_does_not_fake_a_conflict() {
        let raw = sea_orm::DbErr::Custom(
            "query failed for flag '23505': connection reset by peer".to_string(),
        );
        assert!(
            matches!(FlagError::from(raw), FlagError::Database(_)),
            "the bare SQLSTATE digits must not be enough on their own"
        );
    }
}
