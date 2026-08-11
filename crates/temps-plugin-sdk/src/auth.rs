//! Verified authentication context for external-plugin handlers.
//!
//! Temps authenticates the original request and forwards a resolved caller
//! over the plugin's private Unix socket. The SDK runtime verifies that the
//! request carries the per-process handshake secret before inserting the
//! caller into request extensions. Handler extractors read only that
//! extension; they never trust caller-controlled headers directly.

use std::collections::BTreeSet;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::protocol::headers;

/// SDK-owned representation of a Temps role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginRole {
    Admin,
    PlatformAdmin,
    User,
    Reader,
    ApiReader,
    Custom,
    MetricsIngest,
}

impl PluginRole {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "platform_admin" => Some(Self::PlatformAdmin),
            "user" => Some(Self::User),
            "reader" => Some(Self::Reader),
            "api_reader" => Some(Self::ApiReader),
            "custom" => Some(Self::Custom),
            "metrics_ingest" => Some(Self::MetricsIngest),
            _ => None,
        }
    }
}

/// A platform permission in its stable wire form (`projects:read`, etc.).
///
/// Kept as a validated opaque value so a newer Temps can add a permission
/// without making older plugin binaries reject the entire caller context.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginPermission(String);

impl PluginPermission {
    pub const PROJECTS_READ: &'static str = "projects:read";
    pub const PROJECTS_WRITE: &'static str = "projects:write";
    pub const PROJECTS_CREATE: &'static str = "projects:create";
    pub const DEPLOYMENTS_READ: &'static str = "deployments:read";
    pub const DEPLOYMENTS_CREATE: &'static str = "deployments:create";

    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_' || byte == b':'))
        .then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A caller authenticated by Temps and verified by the SDK runtime.
#[derive(Clone, Serialize)]
pub struct AuthenticatedCaller {
    pub user_id: i32,
    pub email: String,
    pub role: PluginRole,
    pub permissions: BTreeSet<PluginPermission>,
    pub request_id: String,
    #[serde(skip)]
    actor_token: Option<String>,
}

impl std::fmt::Debug for AuthenticatedCaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticatedCaller")
            .field("user_id", &self.user_id)
            .field("email", &self.email)
            .field("role", &self.role)
            .field("permissions", &self.permissions)
            .field("request_id", &self.request_id)
            .field(
                "actor_token",
                &self.actor_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl AuthenticatedCaller {
    /// Construct a caller for an embedded host adapter or a test.
    ///
    /// The resulting caller has no actor token and therefore cannot call the
    /// platform API through [`crate::PluginContext::api_as_caller`]. External
    /// plugin requests should always use the runtime extractor instead.
    pub fn without_platform_delegation<I, S>(
        user_id: i32,
        email: impl Into<String>,
        role: PluginRole,
        permissions: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            user_id,
            email: email.into(),
            role,
            permissions: permissions
                .into_iter()
                .filter_map(|permission| PluginPermission::parse(permission.as_ref()))
                .collect(),
            request_id: String::new(),
            actor_token: None,
        }
    }

    pub fn is_admin(&self) -> bool {
        self.role == PluginRole::Admin
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions
            .iter()
            .any(|candidate| candidate.as_str() == permission)
    }

    pub(crate) fn actor_token(&self) -> Option<&str> {
        self.actor_token.as_deref()
    }
}

/// Extractor for routes which may be called anonymously.
#[derive(Debug, Clone)]
pub struct OptionalCaller(pub Option<AuthenticatedCaller>);

/// Rejection returned when a protected plugin handler has no verified user.
#[derive(Debug, Clone)]
pub struct AuthenticationRequired;

impl IntoResponse for AuthenticationRequired {
    fn into_response(self) -> Response {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "type": "https://temps.sh/problems/plugin-authentication-required",
                "title": "Authentication Required",
                "status": StatusCode::UNAUTHORIZED.as_u16(),
                "detail": "This plugin route requires a caller authenticated by Temps."
            })),
        )
            .into_response()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedProxyRequest(pub Option<AuthenticatedCaller>);

impl<S> FromRequestParts<S> for AuthenticatedCaller
where
    S: Send + Sync,
{
    type Rejection = AuthenticationRequired;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<VerifiedProxyRequest>()
            .and_then(|verified| verified.0.clone())
            .ok_or(AuthenticationRequired)
    }
}

impl<S> FromRequestParts<S> for OptionalCaller
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<VerifiedProxyRequest>()
                .and_then(|verified| verified.0.clone()),
        ))
    }
}

pub(crate) fn verify_proxy_headers(
    request: &mut axum::extract::Request,
    expected_secret: &str,
) -> Result<Option<AuthenticatedCaller>, AuthenticationRequired> {
    let signature = request
        .headers()
        .get(headers::AUTH_SIGNATURE)
        .and_then(|value| value.to_str().ok());
    if !signature.is_some_and(|provided| secure_secret_matches(provided, expected_secret)) {
        return Err(AuthenticationRequired);
    }

    let user_id = request
        .headers()
        .get(headers::USER_ID)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i32>().ok());
    let email = request
        .headers()
        .get(headers::USER_EMAIL)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let role = request
        .headers()
        .get(headers::USER_ROLE)
        .and_then(|value| value.to_str().ok())
        .and_then(PluginRole::parse);

    let caller = match (user_id, email, role) {
        (Some(user_id), Some(email), Some(role)) => {
            let permissions = request
                .headers()
                .get(headers::USER_PERMISSIONS)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .split(',')
                .filter_map(PluginPermission::parse)
                .collect();
            let request_id = request
                .headers()
                .get(headers::REQUEST_ID)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let actor_token = request
                .headers()
                .get(headers::ACTOR_TOKEN)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            Some(AuthenticatedCaller {
                user_id,
                email,
                role,
                permissions,
                request_id,
                actor_token,
            })
        }
        (None, None, None) => None,
        _ => return Err(AuthenticationRequired),
    };

    request
        .extensions_mut()
        .insert(VerifiedProxyRequest(caller.clone()));
    Ok(caller)
}

pub(crate) fn secure_secret_matches(provided: &str, expected: &str) -> bool {
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[test]
    fn verified_headers_produce_a_typed_caller() {
        let mut request = Request::builder()
            .header(headers::AUTH_SIGNATURE, "secret")
            .header(headers::USER_ID, "42")
            .header(headers::USER_EMAIL, "dev@temps.sh")
            .header(headers::USER_ROLE, "custom")
            .header(headers::USER_PERMISSIONS, "projects:read")
            .header(headers::REQUEST_ID, "req-1")
            .body(axum::body::Body::empty())
            .expect("test request");

        let caller = verify_proxy_headers(&mut request, "secret")
            .expect("verified request")
            .expect("authenticated caller");
        assert_eq!(caller.user_id, 42);
        assert!(caller.has_permission(PluginPermission::PROJECTS_READ));
        assert!(!caller.has_permission(PluginPermission::PROJECTS_WRITE));
    }

    #[test]
    fn a_forged_or_missing_signature_is_rejected() {
        let mut request = Request::builder()
            .header(headers::USER_ID, "1")
            .header(headers::USER_EMAIL, "attacker@example.com")
            .header(headers::USER_ROLE, "admin")
            .body(axum::body::Body::empty())
            .expect("test request");
        assert!(verify_proxy_headers(&mut request, "secret").is_err());
        assert!(!secure_secret_matches("secreu", "secret"));
        assert!(!secure_secret_matches("secret-extra", "secret"));
    }

    #[test]
    fn an_api_key_permission_set_is_not_widened_from_its_role() {
        let mut request = Request::builder()
            .header(headers::AUTH_SIGNATURE, "secret")
            .header(headers::USER_ID, "7")
            .header(headers::USER_EMAIL, "reader@temps.sh")
            .header(headers::USER_ROLE, "admin")
            .header(headers::USER_PERMISSIONS, "projects:read")
            .body(axum::body::Body::empty())
            .expect("test request");
        let caller = verify_proxy_headers(&mut request, "secret")
            .expect("verified request")
            .expect("authenticated caller");
        assert!(caller.is_admin());
        assert!(!caller.has_permission(PluginPermission::PROJECTS_WRITE));
    }
}
