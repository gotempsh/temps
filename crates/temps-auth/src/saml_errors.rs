use axum::http::StatusCode;
use temps_core::problemdetails::{new as problem_new, Problem};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SamlError {
    #[error("SAML provider with name '{name}' already exists")]
    ProviderAlreadyExists { name: String },

    #[error("SAML provider {provider_id} not found")]
    ProviderNotFound { provider_id: i32 },

    #[error("SAML provider {provider_id} is disabled")]
    ProviderDisabled { provider_id: i32 },

    #[error("SAML login state not found: {relay_state}")]
    StateNotFound { relay_state: String },

    #[error("SAML login state expired: {relay_state} (age {age_secs}s)")]
    StateExpired { relay_state: String, age_secs: i64 },

    #[error("Invalid IdP certificate: {reason}")]
    InvalidCert { reason: String },

    #[error("Invalid IdP metadata: {reason}")]
    InvalidMetadata { reason: String },

    #[error("Failed to fetch IdP metadata from {url}: {reason}")]
    MetadataFetchFailed { url: String, reason: String },

    #[error("IdP metadata URL is not configured for provider {provider_id}")]
    NoMetadataUrl { provider_id: i32 },

    #[error("Refusing to fetch IdP metadata: {reason}")]
    MetadataUrlNotAllowed { reason: String },

    #[error("Failed to parse SAMLResponse: {reason}")]
    ResponseParseFailed { reason: String },

    #[error("SAML assertion validation failed: {reason}")]
    AssertionValidationFailed { reason: String },

    #[error("SAML assertion is missing a NameID")]
    NameIdMissing,

    #[error("SAML assertion is missing an email (configure email_attribute, or use an emailAddress-format NameID)")]
    EmailMissing,

    #[error("Encrypted SAML assertions are not supported. Configure the IdP to send unencrypted (signed-only) assertions.")]
    EncryptedAssertionNotSupported,

    #[error("User {email} is not provisioned for SAML login")]
    UserNotProvisioned { email: String },

    #[error("Refusing to trust SAML identity for {email}: trust_idp_email is disabled for this provider")]
    EmailNotTrusted { email: String },

    #[error("Invalid redirect target")]
    InvalidReturnTo,

    #[error("SAML role mapping {mapping_id} not found")]
    RoleMappingNotFound { mapping_id: i32 },

    #[error("Invalid SSO role {role}: must be 'admin' or 'user'")]
    InvalidRole { role: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl From<SamlError> for Problem {
    fn from(err: SamlError) -> Self {
        match err {
            SamlError::ProviderAlreadyExists { name } => problem_new(StatusCode::CONFLICT)
                .with_title("SAML Provider Already Exists")
                .with_detail(format!(
                    "A SAML provider named '{name}' already exists. Pick a different name."
                )),
            SamlError::ProviderNotFound { provider_id } => problem_new(StatusCode::NOT_FOUND)
                .with_title("SAML Provider Not Found")
                .with_detail(format!("SAML provider {provider_id} was not found")),
            SamlError::ProviderDisabled { provider_id } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("SAML Provider Disabled")
                .with_detail(format!("SAML provider {provider_id} is disabled")),
            SamlError::StateNotFound { relay_state } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid SAML State")
                .with_detail(format!(
                    "SAML login state {relay_state} was not found or was already used"
                )),
            SamlError::StateExpired {
                relay_state,
                age_secs,
            } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("SAML State Expired")
                .with_detail(format!(
                    "SAML login state {relay_state} expired after {age_secs}s. Please start login again."
                )),
            SamlError::InvalidCert { reason } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid IdP Certificate")
                .with_detail(reason),
            SamlError::InvalidMetadata { reason } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid IdP Metadata")
                .with_detail(reason),
            SamlError::MetadataFetchFailed { url, reason } => {
                problem_new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("IdP Metadata Unreachable")
                    .with_detail(format!("Could not fetch IdP metadata from {url}: {reason}"))
            }
            SamlError::NoMetadataUrl { provider_id } => problem_new(StatusCode::UNPROCESSABLE_ENTITY)
                .with_title("No Metadata URL Configured")
                .with_detail(format!(
                    "SAML provider {provider_id} was not created from a metadata URL, so it cannot be refreshed automatically."
                )),
            SamlError::MetadataUrlNotAllowed { reason } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Metadata URL Not Allowed")
                .with_detail(reason),
            SamlError::ResponseParseFailed { .. } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid SAML Response")
                .with_detail("The SAML response could not be parsed"),
            SamlError::AssertionValidationFailed { .. } => problem_new(StatusCode::UNAUTHORIZED)
                .with_title("SAML Assertion Invalid")
                .with_detail("The SAML assertion failed validation"),
            SamlError::NameIdMissing => problem_new(StatusCode::BAD_GATEWAY)
                .with_title("SAML NameID Missing")
                .with_detail("The IdP did not return a NameID in the assertion"),
            SamlError::EmailMissing => problem_new(StatusCode::BAD_GATEWAY)
                .with_title("SAML Email Missing")
                .with_detail(
                    "The IdP did not return an email address. Configure email_attribute on the provider, or ensure the NameID format is emailAddress.",
                ),
            SamlError::EncryptedAssertionNotSupported => problem_new(StatusCode::BAD_GATEWAY)
                .with_title("Encrypted Assertion Not Supported")
                .with_detail(err.to_string()),
            SamlError::UserNotProvisioned { email } => problem_new(StatusCode::FORBIDDEN)
                .with_title("User Not Provisioned")
                .with_detail(format!(
                    "No Temps account exists for {email} and just-in-time provisioning is disabled. Ask an administrator to create your account first."
                )),
            SamlError::EmailNotTrusted { email } => problem_new(StatusCode::FORBIDDEN)
                .with_title("Email Not Trusted")
                .with_detail(format!(
                    "This provider does not allow linking or provisioning by email. Ask an administrator to pre-provision the account for {email}."
                )),
            SamlError::InvalidReturnTo => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Return URL")
                .with_detail("return_to must be a same-origin relative path"),
            SamlError::RoleMappingNotFound { mapping_id } => problem_new(StatusCode::NOT_FOUND)
                .with_title("Role Mapping Not Found")
                .with_detail(format!("SAML role mapping {mapping_id} was not found")),
            SamlError::InvalidRole { role } => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Role")
                .with_detail(format!(
                    "Role '{role}' is invalid for Temps SSO mapping (use 'admin' or 'user')"
                )),
            SamlError::Database(err) => {
                // Same rationale as OidcError::Database: never surface raw
                // Sea-ORM error text (table/column names, SQL snippets).
                tracing::error!(target: "temps_auth::saml", "SAML database error: {err}");
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(
                        "An internal database error occurred while processing the SAML request. Check the server logs for details.",
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
        let problem: Problem = SamlError::ProviderAlreadyExists {
            name: "Okta".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn user_not_provisioned_maps_to_403() {
        let problem: Problem = SamlError::UserNotProvisioned {
            email: "user@example.com".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn email_not_trusted_maps_to_403() {
        let problem: Problem = SamlError::EmailNotTrusted {
            email: "user@example.com".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn encrypted_assertion_maps_to_502() {
        let problem: Problem = SamlError::EncryptedAssertionNotSupported.into();
        assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
    }
}
