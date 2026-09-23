// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    http::{header::CONTENT_TYPE, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use temps_entities::compose_security::ComposeSecurityCheck;
use temps_presets::ComposeParseError;
use utoipa::ToSchema;

/// Problem Details returned when a Compose preview cannot be rendered.
#[derive(Debug, Serialize, ToSchema)]
pub struct ComposePreviewProblemResponse {
    pub title: String,
    pub detail: String,
    /// The individual project security check an administrator may review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_check: Option<ComposeSecurityCheck>,
}

impl ComposePreviewProblemResponse {
    pub fn new(title: &str, path: &str, error: &ComposeParseError) -> Self {
        let policy_check = match error {
            ComposeParseError::PolicyViolation { check, .. } => Some(*check),
            _ => None,
        };
        Self {
            title: title.to_string(),
            detail: format!("Compose preview for '{path}' could not be rendered: {error}"),
            policy_check,
        }
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

    #[tokio::test]
    async fn policy_error_exposes_typed_check_as_problem_details() {
        let error = ComposeParseError::PolicyViolation {
            check: ComposeSecurityCheck::Extends,
            reason: "extends is blocked".to_string(),
        };
        let response =
            ComposePreviewProblemResponse::new("Compose Preview Failed", "compose.yaml", &error)
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
        assert!(problem["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("compose.yaml")));
    }

    #[tokio::test]
    async fn ordinary_parse_error_omits_policy_check() {
        let error = ComposeParseError::MissingServices;
        let response =
            ComposePreviewProblemResponse::new("Invalid Compose Preview", "compose.yaml", &error)
                .into_response();
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("problem body should be readable");
        let problem: serde_json::Value =
            serde_json::from_slice(&body).expect("problem body should be valid JSON");
        assert!(problem.get("policy_check").is_none());
    }
}
