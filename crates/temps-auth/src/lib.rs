// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
mod apikey_handler;
mod apikey_handler_types;
mod apikey_plugin;
mod apikey_service;
mod apikey_types;
mod audit;
mod auth_cookies;
mod auth_service;
mod avatar;
pub mod cli_auth_handler;
pub mod cli_device_handler;
mod deployment_token_service;
mod email_templates;
pub mod handlers;
#[cfg(test)]
mod http_test_support;
pub mod ingress_grants;
mod last_used_throttle;
mod login_guard;
mod mfa_guard;
mod middleware;
/// Public so other plugins (e.g. `temps-cloud`'s managed console-access
/// provider, ADR-045 §4) can hold an `Arc<OidcService>` and reuse the same
/// provider provisioning/role-resolution logic instead of a parallel SSO
/// stack.
pub mod oidc_errors;
mod oidc_handler;
pub mod oidc_service;
mod oidc_types;
mod permission_denial_recorder;
mod plugin;
pub mod rate_limit;
pub mod sensitive_action;
pub mod state;
mod temps_middleware;
mod types;
mod user_service;

pub use middleware::*;
pub use temps_core::resolve_client_ip;

pub use sensitive_action::{
    require_sensitive_action, DefaultSensitiveActionAuthorizer, StepUpError, StepUpService,
    STEP_UP_TTL_MINUTES,
};
pub use state::*;

// Authorization primitives live in `temps-authz`; re-exported so existing
// `temps_auth::...` paths keep working.
pub use temps_authz::*;

// Export plugins
pub use apikey_plugin::ApiKeyPlugin;
pub use plugin::AuthPlugin;

// Export services
pub use apikey_service::{
    cap_api_key_grant, current_owner_role, ApiKeyService, CreateApiKeyRequest, CreateApiKeyResponse,
};
pub use auth_service::{validate_password_complexity, AuthService, VerifiedSession};
pub use deployment_token_service::{
    DeploymentTokenValidationError, DeploymentTokenValidationService, ValidatedDeploymentToken,
};
pub use user_service::{clear_mfa_enrollment, ClearedMfaEnrollment, UserService};

// Export TempsMiddleware implementation
pub use temps_middleware::AuthMiddleware;
