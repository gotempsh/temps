// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `POST /api/_temps/surveys`: the surveys a visitor may see, for the
//! browser SDK.
//!
//! Same mechanism as `/api/_temps/flags` (ADR 110 D1 and D3): served on the
//! customer's own origin, unauthenticated, the tenant resolved from `Host`
//! through the route table, rate limited per project and per address, read
//! from memory, `Cache-Control: no-store`. A separate path rather than a
//! field on the flags response, so a page that reads flags does not pay for
//! surveys and the reverse.
//!
//! Only what a widget needs leaves the server: questions and the conditions
//! the browser applies (path, triggering event, once per visitor). Sampling
//! and the flag condition are decided here, so a visitor outside them never
//! learns the survey exists.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Extension, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_core::problemdetails;
use temps_core::RequestMetadata;
use temps_flags::handlers::public::{check_context, implicit_context, merge};
use temps_flags::handlers::rate_limit::SlotRateLimiter;
use temps_flags::{AttributeValue, EvalContext, FlagService};
use temps_geo::GeoIpService;
use temps_routes::CachedPeerTable;
use tracing::{debug, warn};
use utoipa::ToSchema;

use crate::model::SurveyQuestion;
use crate::selection::{needs_flags, select, DeliveredSurvey};
use crate::services::SurveyService;

pub const BODY_LIMIT: usize = 16 * 1024;
/// A browser asks once per page load on sites that enable surveys; the same
/// ceilings as the flags endpoint, counted separately.
pub const PROJECT_LIMIT_PER_MINUTE: u32 = 30_000;
pub const ADDRESS_LIMIT_PER_MINUTE: u32 = 600;

pub struct PublicSurveysState {
    pub survey_service: Arc<SurveyService>,
    pub flag_service: Arc<FlagService>,
    pub route_table: Arc<CachedPeerTable>,
    pub geoip: Option<Arc<GeoIpService>>,
    pub project_limiter: SlotRateLimiter,
    pub address_limiter: SlotRateLimiter,
}

pub fn configure_public_routes() -> Router<Arc<PublicSurveysState>> {
    Router::new()
        .route("/_temps/surveys", post(public_surveys))
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
}

/// Body of `POST /api/_temps/surveys`. Both fields are optional.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct PublicSurveysRequest {
    /// Stable subject for sampling and flag conditions, e.g. a signed-in
    /// user id. Defaults to the Temps visitor cookie.
    #[serde(default)]
    pub key: Option<String>,
    /// Attributes for the flag conditions. Override the implicit ones.
    #[serde(default)]
    pub attributes: HashMap<String, AttributeValue>,
}

/// The conditions the browser applies before showing a survey.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicSurveyDisplay {
    /// Page path pattern (`*` matches any run of characters); absent for
    /// every page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_pattern: Option<String>,
    /// Show after the app tracks this event; absent to show on page load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_event: Option<String>,
    pub once_per_visitor: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicSurvey {
    pub id: i32,
    pub name: String,
    pub questions: Vec<SurveyQuestion>,
    pub display: PublicSurveyDisplay,
}

impl From<&DeliveredSurvey> for PublicSurvey {
    fn from(survey: &DeliveredSurvey) -> Self {
        Self {
            id: survey.id,
            name: survey.name.clone(),
            questions: survey.questions.clone(),
            display: PublicSurveyDisplay {
                path_pattern: survey.targeting.path_pattern.clone(),
                trigger_event: survey.targeting.trigger_event.clone(),
                once_per_visitor: survey.targeting.once_per_visitor,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicSurveysResponse {
    pub surveys: Vec<PublicSurvey>,
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn problem(status: StatusCode, title: &str, detail: String) -> Response {
    no_store(
        problemdetails::new(status)
            .with_title(title)
            .with_detail(detail)
            .into_response(),
    )
}

fn parse_body(body: &Bytes) -> Result<PublicSurveysRequest, String> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(PublicSurveysRequest::default());
    }
    serde_json::from_slice(body).map_err(|e| e.to_string())
}

/// The active surveys the visitor making this request may see.
///
/// Unauthenticated: the project comes from the `Host` the browser called.
/// Answers are recorded through the analytics event endpoint as
/// `survey_shown`, `survey_dismissed` and `survey_sent`.
#[utoipa::path(
    tag = "Surveys",
    post,
    path = "/_temps/surveys",
    request_body = PublicSurveysRequest,
    responses(
        (status = 200, description = "Surveys this visitor may see", body = PublicSurveysResponse),
        (status = 400, description = "Malformed or oversized context"),
        (status = 404, description = "No environment serves this host"),
        (status = 429, description = "Rate limit exceeded"),
        (status = 503, description = "Surveys could not be loaded")
    )
)]
pub async fn public_surveys(
    State(state): State<Arc<PublicSurveysState>>,
    Extension(metadata): Extension<RequestMetadata>,
    body: Bytes,
) -> Response {
    let request = match parse_body(&body) {
        Ok(request) => request,
        Err(reason) => {
            return problem(
                StatusCode::BAD_REQUEST,
                "Malformed Survey Request",
                format!("The survey request body is not valid: {reason}"),
            )
        }
    };
    let explicit = EvalContext {
        key: request.key,
        attributes: request.attributes,
    };
    if let Err(reason) = check_context(&explicit) {
        return problem(StatusCode::BAD_REQUEST, "Invalid Survey Context", reason);
    }

    let host = metadata.host.as_str();
    let route = state.route_table.get_route(host);
    let (Some(project), Some(environment)) = (
        route.as_ref().and_then(|r| r.project.clone()),
        route.as_ref().and_then(|r| r.environment.clone()),
    ) else {
        debug!(host = %host, "Survey request for a host no environment serves");
        return problem(
            StatusCode::NOT_FOUND,
            "Unknown Host",
            format!("No Temps environment serves host '{host}'."),
        );
    };
    let deployment_id = route
        .as_ref()
        .and_then(|r| r.deployment.as_ref())
        .map(|d| d.id);

    if !state.address_limiter.check(
        SlotRateLimiter::owner_for_address(&metadata.ip_address),
        ADDRESS_LIMIT_PER_MINUTE,
    ) || !state
        .project_limiter
        .check(project.id as u32, PROJECT_LIMIT_PER_MINUTE)
    {
        warn!(project_id = project.id, host = %host, "Survey request rate limit exceeded");
        let mut response = problem(
            StatusCode::TOO_MANY_REQUESTS,
            "Survey Rate Limit Exceeded",
            format!(
                "Survey requests for host '{host}' exceeded {ADDRESS_LIMIT_PER_MINUTE} per minute \
                 per address or {PROJECT_LIMIT_PER_MINUTE} per project."
            ),
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return response;
    }

    let surveys = match state.survey_service.active_surveys(project.id).await {
        Ok(surveys) => surveys,
        Err(error) => {
            warn!(project_id = project.id, %error, "Failed to load surveys for a browser");
            return problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "Surveys Unavailable",
                format!("Surveys for host '{host}' could not be loaded; try again later."),
            );
        }
    };
    if surveys.surveys.is_empty() {
        return no_store(
            Json(PublicSurveysResponse {
                surveys: Vec::new(),
            })
            .into_response(),
        );
    }

    let implicit = implicit_context(
        &metadata,
        host,
        environment.id,
        &environment.slug,
        deployment_id,
        state.geoip.as_deref(),
    )
    .await;
    let ctx = merge(implicit, explicit);

    // Only loaded when a survey has a flag condition. If it cannot be loaded,
    // those surveys fail closed and the rest are still served.
    let flags = if needs_flags(&surveys.surveys) {
        match state
            .flag_service
            .environment_flags(project.id, environment.id)
            .await
        {
            Ok(flags) => Some(flags),
            Err(error) => {
                warn!(
                    project_id = project.id,
                    environment_id = environment.id,
                    %error,
                    "Failed to load flags for survey conditions; flag-conditioned surveys are hidden"
                );
                None
            }
        }
    } else {
        None
    };

    let visible = select(&surveys.surveys, flags.as_deref(), &ctx)
        .into_iter()
        .map(PublicSurvey::from)
        .collect();
    no_store(Json(PublicSurveysResponse { surveys: visible }).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{QuestionKind, SurveyTargeting};

    #[test]
    fn an_empty_body_is_an_empty_request() {
        let request = parse_body(&Bytes::from_static(b"  ")).unwrap();
        assert!(request.key.is_none() && request.attributes.is_empty());
        assert!(parse_body(&Bytes::from_static(b"{\"key\": 3}")).is_err());
    }

    #[test]
    fn only_display_conditions_reach_the_browser() {
        let survey = DeliveredSurvey {
            id: 4,
            name: "Checkout".into(),
            questions: vec![SurveyQuestion {
                id: "nps".into(),
                kind: QuestionKind::Nps,
                prompt: "How likely?".into(),
                optional: false,
                choices: Vec::new(),
                scale: None,
                lower_label: None,
                upper_label: None,
            }],
            targeting: SurveyTargeting {
                path_pattern: Some("/checkout/*".into()),
                trigger_event: None,
                sample_rate: 0.25,
                once_per_visitor: true,
                flag: Some(crate::model::SurveyFlagCondition {
                    key: "internal-beta".into(),
                    value: serde_json::json!(true),
                }),
            },
        };
        let json = serde_json::to_string(&PublicSurvey::from(&survey)).unwrap();
        assert!(json.contains("/checkout/*"), "{json}");
        // The audience rules stay on the server.
        assert!(!json.contains("internal-beta"), "{json}");
        assert!(!json.contains("sample_rate"), "{json}");
    }

    #[test]
    fn responses_are_never_cached() {
        let response = problem(StatusCode::NOT_FOUND, "Unknown Host", "x".into());
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
}
