// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    http::{header::CONTENT_TYPE, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::collections::BTreeMap;
use temps_core::problemdetails::ProblemDetails;
use temps_entities::compose_security::ComposeSecurityCheck;
use temps_presets::compose_check::{ComposeCheckErrorClass, ComposeCheckReport};
use temps_presets::ComposeParseError;
use utoipa::ToSchema;

/// Problem Details returned when a Compose preview cannot be rendered.
#[derive(Debug, Serialize, ToSchema)]
pub struct ComposePreviewProblemResponse {
    #[serde(flatten)]
    pub problem: ProblemDetails,
    /// The individual project security check an administrator may review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_check: Option<ComposeSecurityCheck>,
    /// The class a deployment of the same configuration would fail with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<ComposeCheckErrorClass>,
    /// The deploy-time Compose check that rejected the configuration, when
    /// it ran: the `docker compose config` command, its sanitized output and
    /// the referenced variable names. Variable values are never included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_check: Option<ComposeCheckReport>,
}

/// The deploy-time class of a preview rendering error, so a preview that
/// fails for the same reason as a deployment reports the same class.
pub fn preview_error_class(error: &ComposeParseError) -> ComposeCheckErrorClass {
    match error {
        ComposeParseError::InvalidYaml { .. } | ComposeParseError::MissingServices => {
            ComposeCheckErrorClass::InvalidYaml
        }
        ComposeParseError::InvalidOverride { .. } => ComposeCheckErrorClass::InvalidOverride,
        ComposeParseError::PolicyViolation { .. } => ComposeCheckErrorClass::PolicyViolation,
        ComposeParseError::Serialization { .. } => ComposeCheckErrorClass::Internal,
    }
}

impl ComposePreviewProblemResponse {
    pub fn new(title: &str, path: &str, uri: &Uri, error: &ComposeParseError) -> Self {
        let policy_check = match error {
            ComposeParseError::PolicyViolation { check, .. } => Some(*check),
            _ => None,
        };
        Self {
            problem: preview_problem(
                title,
                format!("Compose preview for '{path}' could not be rendered: {error}"),
                uri,
            ),
            policy_check,
            error_class: Some(preview_error_class(error)),
            compose_check: None,
        }
    }

    /// The deploy-time Compose check rejected the configuration. The detail
    /// is the message the deployment would fail with.
    pub fn from_check(path: &str, uri: &Uri, report: ComposeCheckReport) -> Self {
        let error = report
            .error
            .clone()
            .unwrap_or_else(|| "the Compose configuration check failed".to_string());
        Self {
            problem: preview_problem(
                "Compose Check Failed",
                format!("Compose configuration '{path}' would fail to deploy: {error}"),
                uri,
            ),
            policy_check: None,
            error_class: report.error_class,
            compose_check: Some(report),
        }
    }
}

fn preview_problem(title: &str, detail: String, uri: &Uri) -> ProblemDetails {
    ProblemDetails {
        type_url: Some("about:blank".to_string()),
        title: title.to_string(),
        status: Some(StatusCode::BAD_REQUEST.as_u16()),
        detail: Some(detail),
        instance: Some(uri.to_string()),
        extensions: BTreeMap::new(),
    }
}

impl IntoResponse for ComposePreviewProblemResponse {
    fn into_response(self) -> Response {
        let mut response = (StatusCode::BAD_REQUEST, Json(self)).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use utoipa::PartialSchema;

    #[test]
    fn openapi_schema_includes_problem_details_and_policy_check() {
        let schema = serde_json::to_value(ComposePreviewProblemResponse::schema())
            .expect("preview problem schema should serialize");
        assert_eq!(
            schema["allOf"][0]["$ref"],
            "#/components/schemas/ProblemDetails"
        );
        assert!(schema["allOf"][1]["properties"]["policy_check"].is_object());
    }

    #[tokio::test]
    async fn policy_error_exposes_typed_check_as_problem_details() {
        let error = ComposeParseError::PolicyViolation {
            check: ComposeSecurityCheck::Extends,
            reason: "extends is blocked".to_string(),
        };
        let response = ComposePreviewProblemResponse::new(
            "Compose Preview Failed",
            "compose.yaml",
            &Uri::from_static("/compose-preview"),
            &error,
        )
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/problem+json"))
        );
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("problem body should be readable");
        let problem: serde_json::Value =
            serde_json::from_slice(&body).expect("problem body should be valid JSON");
        assert_eq!(problem["policy_check"], "extends");
        assert_eq!(problem["error_class"], "policy_violation");
        assert!(problem.get("compose_check").is_none());
        assert_eq!(problem["type"], "about:blank");
        assert_eq!(problem["title"], "Compose Preview Failed");
        assert_eq!(problem["status"], 400);
        assert_eq!(problem["instance"], "/compose-preview");
        assert_eq!(problem["extensions"], serde_json::json!({}));
        assert!(problem["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("compose.yaml")));
    }

    #[tokio::test]
    async fn ordinary_parse_error_omits_policy_check() {
        let error = ComposeParseError::MissingServices;
        let response = ComposePreviewProblemResponse::new(
            "Invalid Compose Preview",
            "compose.yaml",
            &Uri::from_static("/compose-preview"),
            &error,
        )
        .into_response();
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("problem body should be readable");
        let problem: serde_json::Value =
            serde_json::from_slice(&body).expect("problem body should be valid JSON");
        assert!(problem.get("policy_check").is_none());
        assert_eq!(problem["error_class"], "invalid_yaml");
        assert_eq!(problem["status"], 400);
        assert_eq!(problem["instance"], "/compose-preview");
    }

    #[tokio::test]
    async fn failed_check_reports_the_deploy_error_and_its_class() {
        let mut report = ComposeCheckReport::failure(
            ComposeCheckErrorClass::PolicyViolation,
            "Compose security policy rejected privileged for service 'web': privileged containers are not allowed",
        );
        report.referenced_variables = vec!["APP_PORT".to_string()];
        let response = ComposePreviewProblemResponse::from_check(
            "compose.yaml",
            &Uri::from_static("/compose-preview"),
            report,
        )
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 8192)
            .await
            .expect("problem body should be readable");
        let problem: serde_json::Value =
            serde_json::from_slice(&body).expect("problem body should be valid JSON");
        assert_eq!(problem["title"], "Compose Check Failed");
        assert_eq!(problem["error_class"], "policy_violation");
        assert_eq!(problem["compose_check"]["passed"], false);
        assert_eq!(
            problem["compose_check"]["referenced_variables"],
            serde_json::json!(["APP_PORT"])
        );
        assert!(problem["detail"].as_str().is_some_and(|detail| {
            detail.contains("compose.yaml") && detail.contains("privileged")
        }));
    }

    #[test]
    fn every_preview_error_maps_to_a_deploy_class() {
        let cases = [
            (
                ComposeParseError::InvalidYaml {
                    reason: "bad".to_string(),
                },
                ComposeCheckErrorClass::InvalidYaml,
            ),
            (
                ComposeParseError::MissingServices,
                ComposeCheckErrorClass::InvalidYaml,
            ),
            (
                ComposeParseError::InvalidOverride {
                    reason: "bad".to_string(),
                },
                ComposeCheckErrorClass::InvalidOverride,
            ),
            (
                ComposeParseError::Serialization {
                    reason: "bad".to_string(),
                },
                ComposeCheckErrorClass::Internal,
            ),
        ];
        for (error, class) in cases {
            assert_eq!(preview_error_class(&error), class, "{error}");
        }
    }

    #[tokio::test]
    async fn problem_instance_preserves_public_preview_base_url_query() {
        let uri = Uri::from_static(
            "/git/public/gitlab/owner/repo/compose-file?base_url=https%3A%2F%2Fgitlab.example.com",
        );
        let response = ComposePreviewProblemResponse::new(
            "Invalid Compose Preview",
            "compose.yaml",
            &uri,
            &ComposeParseError::MissingServices,
        )
        .into_response();
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("problem body should be readable");
        let problem: serde_json::Value =
            serde_json::from_slice(&body).expect("problem body should be valid JSON");

        assert_eq!(
            problem["instance"],
            "/git/public/gitlab/owner/repo/compose-file?base_url=https%3A%2F%2Fgitlab.example.com"
        );
    }
}
