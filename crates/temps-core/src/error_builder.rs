// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::problemdetails;
use axum::http::StatusCode;
use serde::Serialize;
use std::collections::HashMap;

use crate::problemdetails::PermissionDenialKind;

pub struct ErrorBuilder {
    status: StatusCode,
    type_: String,
    title: String,
    detail: String,
    instance: String,
    values: HashMap<String, serde_json::Value>,
    permission_denial: Option<(PermissionDenialKind, Option<String>)>,
}

impl ErrorBuilder {
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            type_: String::new(),
            title: String::new(),
            detail: String::new(),
            instance: String::new(),
            values: HashMap::new(),
            permission_denial: None,
        }
    }

    pub fn type_(mut self, type_: impl Into<String>) -> Self {
        self.type_ = type_.into();
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = instance.into();
        self
    }

    pub fn value<T: Serialize>(mut self, key: &str, value: T) -> Self {
        if let Ok(value) = serde_json::to_value(value) {
            self.values.insert(key.to_string(), value);
        }
        self
    }

    /// Attach internal authorization-denial metadata for response middleware.
    /// This metadata is never serialized into the problem body.
    #[doc(hidden)]
    pub fn permission_denial(
        mut self,
        kind: PermissionDenialKind,
        required_permission: Option<String>,
    ) -> Self {
        self.permission_denial = Some((kind, required_permission));
        self
    }

    pub fn build(self) -> problemdetails::Problem {
        let mut problem = problemdetails::new(self.status)
            .with_type(self.type_)
            .with_title(self.title)
            .with_detail(self.detail)
            .with_instance(self.instance)
            .with_value("timestamp", chrono::Utc::now().to_rfc3339());

        for (key, value) in self.values {
            problem = problem.with_value(&key, value);
        }

        if let Some((kind, required_permission)) = self.permission_denial {
            problem = problem.with_permission_denial(kind, required_permission);
        }

        problem
    }
}

// Common error builders
pub fn internal_server_error() -> ErrorBuilder {
    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
        .type_("https://temps.sh/probs/internal-server-error")
        .title("Internal Server Error")
        .detail("An unexpected error occurred while processing your request")
        .instance("/error/internal-server-error")
        .value("error_code", "INTERNAL_SERVER_ERROR")
}

pub fn not_found() -> ErrorBuilder {
    ErrorBuilder::new(StatusCode::NOT_FOUND)
        .type_("https://temps.sh/probs/not-found")
        .title("Resource Not Found")
        .instance("/error/not-found")
        .value("error_code", "NOT_FOUND")
}

pub fn unauthorized() -> ErrorBuilder {
    ErrorBuilder::new(StatusCode::UNAUTHORIZED)
        .type_("https://temps.sh/probs/unauthorized")
        .title("Unauthorized")
        .detail("Authentication is required to access this resource")
        .instance("/error/unauthorized")
        .value("error_code", "UNAUTHORIZED")
}

pub fn bad_request() -> ErrorBuilder {
    ErrorBuilder::new(StatusCode::BAD_REQUEST)
        .type_("https://temps.sh/probs/bad-request")
        .title("Bad Request")
        .detail("The request was malformed or invalid")
        .instance("/error/bad-request")
}

pub fn forbidden() -> ErrorBuilder {
    ErrorBuilder::new(StatusCode::FORBIDDEN)
        .type_("https://temps.sh/probs/forbidden")
        .title("Forbidden")
        .detail("You do not have permission to access this resource")
        .instance("/error/forbidden")
        .value("error_code", "FORBIDDEN")
}

pub fn conflict() -> ErrorBuilder {
    ErrorBuilder::new(StatusCode::CONFLICT)
        .type_("https://temps.sh/probs/conflict")
        .title("Conflict")
        .instance("/error/conflict")
        .detail("The request could not be completed due to a conflict with the current state of the resource")
        .value("error_code", "CONFLICT")
}

pub fn too_many_requests() -> ErrorBuilder {
    ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
        .type_("https://temps.sh/probs/too-many-requests")
        .title("Too Many Requests")
        .instance("/error/too-many-requests")
        .detail("Rate limit exceeded, please retry later")
        .value("error_code", "TOO_MANY_REQUESTS")
}
