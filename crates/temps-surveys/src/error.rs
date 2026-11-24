// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed errors for the surveys domain.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SurveyError {
    #[error("Survey {survey_id} not found in project {project_id}")]
    NotFound { project_id: i32, survey_id: i32 },

    #[error("Invalid survey '{name}': {message}")]
    Validation { name: String, message: String },

    #[error("Invalid results query for survey {survey_id}: {message}")]
    InvalidQuery { survey_id: i32, message: String },

    #[error("Survey {survey_id} in project {project_id} changed since it was read; reload it and apply the change again")]
    StaleWrite { project_id: i32, survey_id: i32 },

    #[error("Stored survey {survey_id} is unreadable ({field}): {reason}")]
    Corrupt {
        survey_id: i32,
        field: &'static str,
        reason: String,
    },

    #[error("Failed to serialize {field} of survey '{name}': {reason}")]
    Serialization {
        name: String,
        field: &'static str,
        reason: String,
    },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}
