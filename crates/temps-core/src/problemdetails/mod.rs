// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;

use serde;
use serde_json::Value;

use axum::http::{HeaderValue, StatusCode};
use axum::{http::header::CONTENT_TYPE, response::IntoResponse, Json};
use serde::Serialize;

use utoipa::ToSchema;

/// Representation of a Problem error to return to the client.
/// Follows RFC 7807 - Problem Details for HTTP APIs
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(example = json!({
    "type": "https://example.com/probs/out-of-memory",
    "title": "Internal Server Error",
    "detail": "The server encountered an unexpected condition",
    "instance": "/account/12345/msgs/abc",
    "additional_info": "Custom field with additional details"
}))]
pub struct ProblemDetails {
    /// A URI reference that identifies the problem type
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[schema(example = "https://example.com/probs/out-of-memory")]
    pub type_url: Option<String>,
    /// A short, human-readable summary of the problem type
    #[schema(example = "Internal Server Error")]
    pub title: String,
    /// A human-readable explanation specific to this occurrence of the problem
    #[schema(example = "The server encountered an unexpected condition")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A URI reference that identifies the specific occurrence of the problem
    #[schema(example = "/account/12345/msgs/abc")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Additional properties of the problem
    #[schema(additional_properties = true)]
    pub extensions: BTreeMap<String, Value>,
}

/// Representation of a Problem error to return to the client.
#[allow(dead_code)] // These fields are used by the various features.
#[derive(Debug, Clone)]
pub struct Problem {
    /// The status code of the problem.
    pub status_code: StatusCode,
    /// The actual body of the problem.
    pub body: BTreeMap<String, Value>,
    /// Internal-only metadata carried to response middleware. This is never
    /// serialized into the RFC 7807 response body.
    permission_denial: Option<PermissionDenialMarker>,
}

/// Stable authorization guard denial categories used by security auditing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDenialKind {
    InsufficientPermission,
    CrossProjectScope,
    DeploymentTokenNotAllowed,
    ProjectAccess,
    ProjectPermission,
    MissingPrincipal,
}

impl PermissionDenialKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InsufficientPermission => "insufficient_permission",
            Self::CrossProjectScope => "cross_project_scope",
            Self::DeploymentTokenNotAllowed => "deployment_token_not_allowed",
            Self::ProjectAccess => "project_access",
            Self::ProjectPermission => "project_permission",
            Self::MissingPrincipal => "missing_principal",
        }
    }
}

/// A server-generated response extension proving that a 403 came from an
/// authorization guard. Its fields are intentionally private so callers can
/// inspect, but cannot forge or mutate, the marker.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDenialMarker {
    kind: PermissionDenialKind,
    required_permission: Option<String>,
}

impl PermissionDenialMarker {
    pub fn kind(&self) -> PermissionDenialKind {
        self.kind
    }

    pub fn required_permission(&self) -> Option<&str> {
        self.required_permission.as_deref()
    }
}

/// Create a new `Problem` response to send to the client.
pub fn new<S>(status_code: S) -> Problem
where
    S: Into<StatusCode>,
{
    Problem {
        status_code: status_code.into(),
        body: BTreeMap::new(),
        permission_denial: None,
    }
}

impl Problem {
    /// Mark this problem as a genuine authorization-guard denial. The marker
    /// is attached only to the resulting HTTP response extensions.
    #[doc(hidden)]
    pub fn with_permission_denial(
        mut self,
        kind: PermissionDenialKind,
        required_permission: Option<String>,
    ) -> Self {
        self.permission_denial = Some(PermissionDenialMarker {
            kind,
            required_permission,
        });
        self
    }

    /// Specify the "type" to use for the problem.
    pub fn with_type<S>(self, value: S) -> Self
    where
        S: Into<String>,
    {
        self.with_value("type", value.into())
    }

    /// Specify the "title" to use for the problem.
    pub fn with_title<S>(self, value: S) -> Self
    where
        S: Into<String>,
    {
        self.with_value("title", value.into())
    }

    /// Specify the "detail" to use for the problem.
    pub fn with_detail<S>(self, value: S) -> Self
    where
        S: Into<String>,
    {
        self.with_value("detail", value.into())
    }

    /// Specify the "instance" to use for the problem.
    pub fn with_instance<S>(self, value: S) -> Self
    where
        S: Into<String>,
    {
        self.with_value("instance", value.into())
    }

    /// Specify an arbitrary value to include in the problem.
    ///
    /// # Parameters
    /// - `key` - The key for the value.
    /// - `value` - The value itself.
    pub fn with_value<V>(mut self, key: &str, value: V) -> Self
    where
        V: Into<Value>,
    {
        self.body.insert(key.to_owned(), value.into());

        self
    }
}

impl<S> From<S> for Problem
where
    S: Into<StatusCode>,
{
    fn from(status_code: S) -> Self {
        new(status_code.into())
    }
}
/// Result type where the error is always a `Problem`.
pub type Result<T> = std::result::Result<T, Problem>;

impl IntoResponse for Problem {
    fn into_response(self) -> axum::response::Response {
        let mut response = if self.body.is_empty() {
            self.status_code.into_response()
        } else {
            let body = Json(self.body);
            let mut response = (self.status_code, body).into_response();

            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/problem+json"),
            );
            response
        };

        if let Some(marker) = self.permission_denial {
            response.extensions_mut().insert(marker);
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn permission_denial_marker_is_internal_and_preserves_body() {
        let problem = new(StatusCode::FORBIDDEN)
            .with_title("Forbidden")
            .with_value("required_permission", "users:write")
            .with_permission_denial(
                PermissionDenialKind::InsufficientPermission,
                Some("users:write".to_string()),
            );

        let response = problem.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let marker = response
            .extensions()
            .get::<PermissionDenialMarker>()
            .expect("guard marker should propagate through IntoResponse");
        assert_eq!(marker.kind(), PermissionDenialKind::InsufficientPermission);
        assert_eq!(marker.required_permission(), Some("users:write"));

        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("problem body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("problem body should remain JSON");
        assert_eq!(json["title"], "Forbidden");
        assert_eq!(json["required_permission"], "users:write");
        assert!(json.get("permission_denial").is_none());
    }

    #[test]
    fn ordinary_problem_has_no_permission_denial_marker() {
        let response = new(StatusCode::FORBIDDEN).into_response();
        assert!(response
            .extensions()
            .get::<PermissionDenialMarker>()
            .is_none());
    }
}
