use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OidcProviderSummary {
    pub id: i32,
    pub name: String,
    /// The template the provider was created from — e.g. `keycloak`,
    /// `okta`, `auth0`, `google`, `azure-ad`, or `generic`. Surfaced on
    /// the public login endpoint so the unauthenticated login page can
    /// render the right brand logo on the "Sign in with X" button.
    /// Never sensitive — the template name is part of the provider's
    /// public identity, not configuration.
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OidcProviderResponse {
    pub id: i32,
    pub name: String,
    pub issuer_url: String,
    pub client_id: String,
    /// Always masked — the secret is never returned after creation.
    pub client_secret: String,
    pub scopes: String,
    pub jit_provisioning: bool,
    pub enabled: bool,
    pub template: String,
    pub group_claim: String,
    pub role_claim: String,
    pub default_role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateOidcProviderRequest {
    pub name: String,
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    #[serde(default = "default_scopes")]
    pub scopes: String,
    #[serde(default = "default_true")]
    pub jit_provisioning: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_template")]
    pub template: String,
    #[serde(default = "default_group_claim")]
    pub group_claim: String,
    #[serde(default = "default_role_claim")]
    pub role_claim: String,
    #[serde(default = "default_role")]
    pub default_role: String,
}

fn default_scopes() -> String {
    "openid email profile".to_string()
}

fn default_template() -> String {
    "generic".to_string()
}

fn default_group_claim() -> String {
    "groups".to_string()
}

fn default_role_claim() -> String {
    "roles".to_string()
}

fn default_role() -> String {
    "user".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateOidcProviderRequest {
    pub name: Option<String>,
    pub issuer_url: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Option<String>,
    pub jit_provisioning: Option<bool>,
    pub enabled: Option<bool>,
    pub template: Option<String>,
    pub group_claim: Option<String>,
    pub role_claim: Option<String>,
    pub default_role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OidcTestConnectionResponse {
    pub success: bool,
    pub message: String,
}

/// A user that has logged in via a given OIDC provider. Used by the
/// admin "Users for provider" panel — the `oidc_subject` is the
/// IdP-side identifier we matched on, useful when diagnosing why a
/// user can or can't log in.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OidcProviderUserResponse {
    pub id: i32,
    pub name: String,
    pub email: String,
    pub email_verified: bool,
    pub mfa_enabled: bool,
    pub oidc_subject: Option<String>,
    #[schema(value_type = String, format = DateTime, example = "2024-01-15T14:30:00Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = String, format = DateTime, example = "2024-01-15T14:30:00Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub fn provider_user_to_response(user: &temps_entities::users::Model) -> OidcProviderUserResponse {
    OidcProviderUserResponse {
        id: user.id,
        name: user.name.clone(),
        email: user.email.clone(),
        email_verified: user.email_verified,
        mfa_enabled: user.mfa_enabled,
        oidc_subject: user.oidc_subject.clone(),
        created_at: user.created_at,
        updated_at: user.updated_at,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OidcRoleMappingResponse {
    pub id: i32,
    pub provider_id: i32,
    pub priority: i32,
    pub idp_group: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateOidcRoleMappingRequest {
    pub priority: i32,
    pub idp_group: String,
    pub role: String,
}

const MASKED_SECRET: &str = "***";

pub fn mask_secret() -> String {
    MASKED_SECRET.to_string()
}

pub fn provider_to_response(
    provider: &temps_entities::oidc_providers::Model,
) -> OidcProviderResponse {
    OidcProviderResponse {
        id: provider.id,
        name: provider.name.clone(),
        issuer_url: provider.issuer_url.clone(),
        client_id: provider.client_id.clone(),
        client_secret: mask_secret(),
        scopes: provider.scopes.clone(),
        jit_provisioning: provider.jit_provisioning,
        enabled: provider.enabled,
        template: provider.template.clone(),
        group_claim: provider.group_claim.clone(),
        role_claim: provider.role_claim.clone(),
        default_role: provider.default_role.clone(),
    }
}

pub fn role_mapping_to_response(
    mapping: &temps_entities::oidc_role_mappings::Model,
) -> OidcRoleMappingResponse {
    OidcRoleMappingResponse {
        id: mapping.id,
        provider_id: mapping.provider_id,
        priority: mapping.priority,
        idp_group: mapping.idp_group.clone(),
        role: mapping.role.clone(),
    }
}
