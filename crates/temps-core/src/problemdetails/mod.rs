use std::collections::BTreeMap;

use serde;
use serde_json::Value;

use axum::http::StatusCode;
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
}

/// Create a new `Problem` response to send to the client.
pub fn new<S>(status_code: S) -> Problem
where
    S: Into<StatusCode>,
{
    Problem {
        status_code: status_code.into(),
        body: BTreeMap::new(),
    }
}

impl Problem {
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
        if self.body.is_empty() {
            self.status_code.into_response()
        } else {
            let body = Json(self.body);
            let mut response = (self.status_code, body).into_response();

            response
                .headers_mut()
                .insert(CONTENT_TYPE, "application/problem+json".parse().unwrap());
            response
        }
    }
}
