use axum::http::StatusCode;
use temps_core::problemdetails::{new as problem_new, Problem};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OidcError {
    #[error("No OIDC provider configured")]
    NoProviderConfigured,

    #[error("OIDC provider with name '{name}' already exists")]
    ProviderAlreadyExists { name: String },

    #[error("OIDC provider {provider_id} not found")]
    ProviderNotFound { provider_id: i32 },

    #[error("OIDC discovery failed for issuer {issuer}: {reason}")]
    DiscoveryFailed { issuer: String, reason: String },

    #[error("OIDC login state not found: {state}")]
    StateNotFound { state: String },

    #[error("OIDC login state expired: {state} (age {age_secs}s)")]
    StateExpired { state: String, age_secs: i64 },

    #[error("OIDC token exchange failed (HTTP {status}): {body}")]
    TokenExchangeFailed { status: u16, body: String },

    #[error("OIDC ID token invalid: {reason}")]
    IdTokenInvalid { reason: String },

    #[error("User {email} is not provisioned for OIDC login")]
    UserNotProvisioned { email: String },

    #[error(
        "Refusing to use OIDC identity for {email}: the IdP did not assert email_verified=true"
    )]
    EmailNotVerified { email: String },

    #[error("OIDC ID token is missing the email claim")]
    EmailClaimMissing,

    #[error("OIDC provider {provider_id} is disabled")]
    ProviderDisabled { provider_id: i32 },

    #[error("Invalid issuer URL: {reason}")]
    InvalidIssuer { reason: String },

    #[error("Invalid redirect target")]
    InvalidReturnTo,

    #[error("OIDC role mapping {mapping_id} not found")]
    RoleMappingNotFound { mapping_id: i32 },

    #[error("Invalid SSO role {role}: must be 'admin' or 'user'")]
    InvalidRole { role: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl From<OidcError> for Problem {
    fn from(err: OidcError) -> Self {
        match err {
            OidcError::NoProviderConfigured => problem_new(StatusCode::NOT_FOUND)
                .with_title("No OIDC Provider")
                .with_detail("No OIDC provider is configured on this Temps instance"),
            OidcError::ProviderAlreadyExists { name } => problem_new(StatusCode::CONFLICT)
                .with_title("OIDC Provider Already Exists")
                .with_detail(format!(
                    "An OIDC provider named '{name}' already exists. Pick a different name."
                )),
            OidcError::ProviderNotFound { provider_id } => problem_new(StatusCode::NOT_FOUND)
                .with_title("OIDC Provider Not Found")
                .with_detail(format!("OIDC provider {provider_id} was not found")),
            OidcError::DiscoveryFailed { issuer, reason } => problem_new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("OIDC Provider Unreachable")
                .with_detail(format!(
                    "Could not reach OIDC provider at {issuer}: {reason}. Try again or use password login."
                )),
            OidcError::StateNotFound { state } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid OIDC State")
                .with_detail(format!(
                    "OIDC login state {state} was not found or was already used"
                )),
            OidcError::StateExpired { state, age_secs } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("OIDC State Expired")
                .with_detail(format!(
                    "OIDC login state {state} expired after {age_secs}s. Please start login again."
                )),
            OidcError::TokenExchangeFailed { status, body } => problem_new(StatusCode::BAD_GATEWAY)
                .with_title("OIDC Token Exchange Failed")
                .with_detail(format!(
                    "OIDC provider returned HTTP {status} during token exchange: {body}"
                )),
            OidcError::IdTokenInvalid { reason } => problem_new(StatusCode::UNAUTHORIZED)
                .with_title("OIDC ID Token Invalid")
                .with_detail(format!("OIDC ID token validation failed: {reason}")),
            OidcError::UserNotProvisioned { email } => problem_new(StatusCode::FORBIDDEN)
                .with_title("User Not Provisioned")
                .with_detail(format!(
                    "No Temps account exists for {email} and just-in-time provisioning is disabled. Ask an administrator to create your account first."
                )),
            OidcError::EmailNotVerified { email } => problem_new(StatusCode::FORBIDDEN)
                .with_title("Email Not Verified")
                .with_detail(format!(
                    "Your identity provider has not confirmed that {email} belongs to you. Verify the email at your IdP and try again, or ask an administrator to provision the account manually."
                )),
            OidcError::EmailClaimMissing => problem_new(StatusCode::BAD_GATEWAY)
                .with_title("OIDC Email Claim Missing")
                .with_detail(
                    "The OIDC provider did not return an email address. Ensure the 'email' scope is granted.",
                ),
            OidcError::ProviderDisabled { provider_id } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("OIDC Provider Disabled")
                .with_detail(format!("OIDC provider {provider_id} is disabled")),
            OidcError::InvalidIssuer { reason } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Issuer URL")
                .with_detail(reason),
            OidcError::InvalidReturnTo => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Return URL")
                .with_detail("return_to must be a same-origin relative path"),
            OidcError::RoleMappingNotFound { mapping_id } => problem_new(StatusCode::NOT_FOUND)
                .with_title("Role Mapping Not Found")
                .with_detail(format!("OIDC role mapping {mapping_id} was not found")),
            OidcError::InvalidRole { role } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Role")
                .with_detail(format!(
                    "Role '{role}' is invalid for Temps SSO mapping (use 'admin' or 'user')"
                )),
            OidcError::Database(err) => {
                // Don't return the raw Sea-ORM error text to the
                // caller: it can include table names, column names,
                // and snippets of failed SQL that help an attacker
                // map the schema. Log the full error server-side
                // and hand the operator a stable, generic message.
                tracing::error!(target: "temps_auth::oidc", "OIDC database error: {err}");
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(
                        "An internal database error occurred while processing the OIDC request. Check the server logs for details.",
                    )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_already_exists_maps_to_409() {
        let problem: Problem = OidcError::ProviderAlreadyExists {
            name: "Keycloak".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn user_not_provisioned_maps_to_403() {
        let problem: Problem = OidcError::UserNotProvisioned {
            email: "user@example.com".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn discovery_failed_maps_to_503() {
        let problem: Problem = OidcError::DiscoveryFailed {
            issuer: "https://auth.example.com".into(),
            reason: "timeout".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::SERVICE_UNAVAILABLE);
    }
}
