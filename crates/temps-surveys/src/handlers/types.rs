// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request/response DTOs and shared handler state for surveys.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use temps_core::{AuditLogger, ProjectAccessChecker};
use utoipa::{IntoParams, ToSchema};

use crate::model::{SurveyQuestion, SurveyStatus, SurveyTargeting};
use crate::services::results::{AnswerValue, QuestionResults};
use crate::services::{SubmittedResponse, Survey, SurveyResults, SurveyService};

pub struct SurveysAppState {
    pub survey_service: Arc<SurveyService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Team-based project access checker (`None` in plain OSS, where the
    /// guard is a no-op).
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

fn default_status() -> SurveyStatus {
    SurveyStatus::Draft
}

/// `null` clears the field, absence leaves it as it is.
fn deserialize_optional_optional_string<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<String> = Option::deserialize(deserializer)?;
    Ok(Some(value))
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateSurveyRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Defaults to `draft`: nothing is shown until the survey is activated.
    #[serde(default = "default_status")]
    pub status: SurveyStatus,
    #[serde(default)]
    pub questions: Vec<SurveyQuestion>,
    #[serde(default)]
    pub targeting: SurveyTargeting,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateSurveyRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// `null` clears the description.
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<SurveyStatus>,
    /// Replaces every question. Keep a question's `id` to keep its answers
    /// together.
    #[serde(default)]
    pub questions: Option<Vec<SurveyQuestion>>,
    #[serde(default)]
    pub targeting: Option<SurveyTargeting>,
    /// `updated_at` as last read. When sent, the update is refused with 409
    /// if the survey changed since.
    #[serde(default)]
    pub expected_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SurveyResponse {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub description: Option<String>,
    pub status: SurveyStatus,
    pub questions: Vec<SurveyQuestion>,
    pub targeting: SurveyTargeting,
    pub created_at: String,
    /// Send back as `expected_updated_at` to guard an update.
    pub updated_at: String,
}

impl From<Survey> for SurveyResponse {
    fn from(survey: Survey) -> Self {
        Self {
            id: survey.id,
            project_id: survey.project_id,
            name: survey.name,
            description: survey.description,
            status: survey.status,
            questions: survey.questions,
            targeting: survey.targeting,
            created_at: survey.created_at.to_rfc3339(),
            updated_at: survey.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SurveyListResponse {
    pub surveys: Vec<SurveyResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListSurveysQuery {
    /// Only surveys in this status.
    pub status: Option<SurveyStatus>,
    /// Page number, from 1. Default 1.
    pub page: Option<u64>,
    /// Surveys per page. Default 20, maximum 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ResultsQuery {
    /// Start of the window, RFC 3339. Default: 30 days before `end_date`.
    pub start_date: Option<String>,
    /// End of the window (exclusive), RFC 3339. Default: now.
    pub end_date: Option<String>,
    /// Only answers recorded in this environment.
    pub environment_id: Option<i32>,
    /// Responses list only: page number, from 1.
    pub page: Option<u64>,
    /// Responses list only: responses per page, default 20, maximum 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SurveyResultsResponse {
    pub survey_id: i32,
    pub start_date: String,
    pub end_date: String,
    /// `survey_shown` events in the window.
    pub shown: i64,
    /// `survey_dismissed` events in the window.
    pub dismissed: i64,
    /// `survey_sent` events (submitted responses) in the window.
    pub sent: i64,
    /// `sent / shown`; absent before the survey was shown.
    pub response_rate: Option<f64>,
    pub questions: Vec<QuestionResults>,
}

impl SurveyResultsResponse {
    pub fn new(survey_id: i32, results: SurveyResults) -> Self {
        let response_rate = results.response_rate();
        Self {
            survey_id,
            start_date: results.window.start.to_rfc3339(),
            end_date: results.window.end.to_rfc3339(),
            shown: results.shown,
            dismissed: results.dismissed,
            sent: results.sent,
            response_rate,
            questions: results.questions,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SurveyAnswerSet {
    /// Analytics event id of the `survey_sent` event.
    pub event_id: i64,
    pub timestamp: String,
    pub environment_id: Option<i32>,
    /// Visitor who answered; open their profile from the console.
    pub visitor_id: Option<i32>,
    /// Session the answer was given in; links to its replay when recorded.
    pub session_id: Option<String>,
    /// Page the survey was answered on.
    pub pathname: String,
    /// Answers by question id. Only declared questions, in the shape their
    /// kind allows.
    pub answers: HashMap<String, AnswerValue>,
}

impl From<SubmittedResponse> for SurveyAnswerSet {
    fn from(response: SubmittedResponse) -> Self {
        Self {
            event_id: response.event_id,
            timestamp: response.timestamp.to_rfc3339(),
            environment_id: response.environment_id,
            visitor_id: response.visitor_id,
            session_id: response.session_id,
            pathname: response.pathname,
            answers: response.answers,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SurveyAnswerListResponse {
    pub responses: Vec<SurveyAnswerSet>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeleteSurveyResponse {
    pub id: i32,
    pub deleted: bool,
}

pub fn total_pages(total: u64, page_size: u64) -> u64 {
    if page_size == 0 {
        0
    } else {
        total.div_ceil(page_size)
    }
}
