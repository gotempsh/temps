//! Email validation handlers

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::{bad_request, internal_server_error},
    problemdetails::Problem,
};
use tracing::error;
use utoipa::ToSchema;

use super::types::AppState;
use crate::services::{
    MiscResult as ServiceMiscResult, MxResult as ServiceMxResult,
    ReachabilityStatus as ServiceReachabilityStatus, SmtpResult as ServiceSmtpResult,
    SyntaxResult as ServiceSyntaxResult, ValidateEmailRequest as ServiceValidateEmailRequest,
    ValidateEmailResponse as ServiceValidateEmailResponse,
};

/// Configure validation routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/emails/validate", post(validate_email))
}

// ========================================
// Request/Response Types
// ========================================

/// Request body for validating an email address
#[derive(Debug, Deserialize, ToSchema)]
pub struct ValidateEmailRequest {
    /// Email address to validate
    #[schema(example = "someone@gmail.com")]
    pub email: String,
    /// Optional SOCKS5 proxy configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyRequest>,
}

/// Proxy configuration for email validation
#[derive(Debug, Deserialize, ToSchema)]
pub struct ProxyRequest {
    /// Proxy host
    #[schema(example = "proxy.example.com")]
    pub host: String,
    /// Proxy port
    #[schema(example = 1080)]
    pub port: u16,
    /// Optional proxy username
    pub username: Option<String>,
    /// Optional proxy password
    pub password: Option<String>,
}

/// Email reachability status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReachabilityStatus {
    /// Email is safe to send to
    Safe,
    /// Email might bounce, proceed with caution
    Risky,
    /// Email is invalid and will definitely bounce
    Invalid,
    /// Unable to determine deliverability
    Unknown,
}

impl From<ServiceReachabilityStatus> for ReachabilityStatus {
    fn from(status: ServiceReachabilityStatus) -> Self {
        match status {
            ServiceReachabilityStatus::Safe => ReachabilityStatus::Safe,
            ServiceReachabilityStatus::Risky => ReachabilityStatus::Risky,
            ServiceReachabilityStatus::Invalid => ReachabilityStatus::Invalid,
            ServiceReachabilityStatus::Unknown => ReachabilityStatus::Unknown,
        }
    }
}

/// Syntax validation result
#[derive(Debug, Serialize, ToSchema)]
pub struct SyntaxResult {
    /// Whether the email syntax is valid
    pub is_valid_syntax: bool,
    /// The domain part of the email
    #[schema(example = "gmail.com")]
    pub domain: Option<String>,
    /// The username part of the email
    #[schema(example = "someone")]
    pub username: Option<String>,
    /// Suggested email correction if available
    pub suggestion: Option<String>,
}

impl From<ServiceSyntaxResult> for SyntaxResult {
    fn from(result: ServiceSyntaxResult) -> Self {
        Self {
            is_valid_syntax: result.is_valid_syntax,
            domain: result.domain,
            username: result.username,
            suggestion: result.suggestion,
        }
    }
}

/// MX (Mail Exchange) validation result
#[derive(Debug, Serialize, ToSchema)]
pub struct MxResult {
    /// Whether the domain accepts mail
    pub accepts_mail: bool,
    /// List of MX records for the domain
    #[schema(example = json!(["alt1.gmail-smtp-in.l.google.com.", "gmail-smtp-in.l.google.com."]))]
    pub records: Vec<String>,
    /// Error message if MX lookup failed
    pub error: Option<String>,
}

impl From<ServiceMxResult> for MxResult {
    fn from(result: ServiceMxResult) -> Self {
        Self {
            accepts_mail: result.accepts_mail,
            records: result.records,
            error: result.error,
        }
    }
}

/// Miscellaneous validation result
#[derive(Debug, Serialize, ToSchema)]
pub struct MiscResult {
    /// Whether the email is from a disposable email provider
    pub is_disposable: bool,
    /// Whether the email is a role-based account (e.g., admin@, info@)
    pub is_role_account: bool,
    /// Whether the email provider is a B2C (consumer) email provider
    pub is_b2c: bool,
    /// Gravatar URL if available
    pub gravatar_url: Option<String>,
}

impl From<ServiceMiscResult> for MiscResult {
    fn from(result: ServiceMiscResult) -> Self {
        Self {
            is_disposable: result.is_disposable,
            is_role_account: result.is_role_account,
            is_b2c: result.is_b2c,
            gravatar_url: result.gravatar_url,
        }
    }
}

/// SMTP validation result
#[derive(Debug, Serialize, ToSchema)]
pub struct SmtpResult {
    /// Whether we could connect to the SMTP server
    pub can_connect_smtp: bool,
    /// Whether the mailbox appears to have a full inbox
    pub has_full_inbox: bool,
    /// Whether this is a catch-all domain
    pub is_catch_all: bool,
    /// Whether the email is deliverable
    pub is_deliverable: bool,
    /// Whether the mailbox is disabled
    pub is_disabled: bool,
    /// Error message if SMTP check failed
    pub error: Option<String>,
}

impl From<ServiceSmtpResult> for SmtpResult {
    fn from(result: ServiceSmtpResult) -> Self {
        Self {
            can_connect_smtp: result.can_connect_smtp,
            has_full_inbox: result.has_full_inbox,
            is_catch_all: result.is_catch_all,
            is_deliverable: result.is_deliverable,
            is_disabled: result.is_disabled,
            error: result.error,
        }
    }
}

/// Complete email validation response
#[derive(Debug, Serialize, ToSchema)]
pub struct ValidateEmailResponse {
    /// The email address that was validated
    #[schema(example = "someone@gmail.com")]
    pub email: String,
    /// Overall reachability status: safe, risky, invalid, or unknown
    pub is_reachable: ReachabilityStatus,
    /// Syntax validation result
    pub syntax: SyntaxResult,
    /// MX record validation result
    pub mx: MxResult,
    /// Miscellaneous validation result
    pub misc: MiscResult,
    /// SMTP validation result
    pub smtp: SmtpResult,
}

impl From<ServiceValidateEmailResponse> for ValidateEmailResponse {
    fn from(response: ServiceValidateEmailResponse) -> Self {
        Self {
            email: response.email,
            is_reachable: response.is_reachable.into(),
            syntax: response.syntax.into(),
            mx: response.mx.into(),
            misc: response.misc.into(),
            smtp: response.smtp.into(),
        }
    }
}

/// Validate an email address
#[utoipa::path(
    tag = "Email Validation",
    post,
    path = "/emails/validate",
    request_body = ValidateEmailRequest,
    responses(
        (status = 200, description = "Email validation result", body = ValidateEmailResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn validate_email(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Json(request): Json<ValidateEmailRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsValidate);

    // Validate request
    if request.email.is_empty() {
        return Err(bad_request().detail("Email address is required").build());
    }

    // Basic email format validation
    if !request.email.contains('@') {
        return Err(bad_request()
            .detail("Invalid email format: missing @ symbol")
            .build());
    }

    // Convert proxy config if provided
    let proxy = request.proxy.map(|p| crate::services::ProxyConfig {
        host: p.host,
        port: p.port,
        username: p.username,
        password: p.password,
    });

    let service_request = ServiceValidateEmailRequest {
        email: request.email.clone(),
        proxy,
    };

    let result = state
        .validation_service
        .validate(service_request)
        .await
        .map_err(|e| {
            error!("Failed to validate email: {}", e);
            internal_server_error()
                .detail(format!("Failed to validate email: {}", e))
                .build()
        })?;

    Ok((StatusCode::OK, Json(ValidateEmailResponse::from(result))))
}
