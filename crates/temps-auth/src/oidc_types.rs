use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// Derive a stable URL slug for an OIDC provider that can be shown to
/// unauthenticated callers without leaking the internal integer primary key.
///
/// Algorithm: `lowercase-hyphenated-name` + `-` + first 4 bytes of
/// `SHA-256(id_le_bytes || name_utf8)` as lowercase hex. The hash suffix
/// makes the slug collision-resistant across providers that happen to share
/// the same display name after slugification.
pub fn derive_provider_slug(id: i32, name: &str) -> String {
    // Slugify: lowercase, replace non-alphanumeric runs with hyphens, trim.
    let base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    // 4-byte hash suffix for collision resistance and to prevent reverse-
    // mapping the integer ID from the slug alone.
    let mut hasher = Sha256::new();
    hasher.update(id.to_le_bytes());
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let suffix = hex::encode(&digest[..4]);

    if base.is_empty() {
        // Degenerate name (all non-alphanumeric). Fall back to pure hash.
        suffix
    } else {
        format!("{base}-{suffix}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OidcProviderSummary {
    /// Stable opaque slug — use this as the path parameter when initiating
    /// OIDC login (`/auth/oidc/login/{slug}`). The integer database ID is
    /// intentionally omitted from this public endpoint to prevent provider
    /// enumeration.
    pub slug: String,
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
    /// When true, the resolver skips the `email_verified` claim gate
    /// during SSO login. Only safe for IdPs where an admin controls
    /// user provisioning — see `oidc_providers::Model::trust_idp_email`.
    pub trust_idp_email: bool,
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
    /// Defaults false. Set to true only for IdPs where an admin
    /// controls user provisioning (corporate Okta, Azure AD) and
    /// self-signup of arbitrary emails is not possible — see the
    /// `trust_idp_email` field on `oidc_providers::Model` for the
    /// security tradeoff this enables.
    #[serde(default)]
    pub trust_idp_email: bool,
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
    pub trust_idp_email: Option<bool>,
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
        trust_idp_email: provider.trust_idp_email,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_provider_slug_basic() {
        let slug = derive_provider_slug(1, "Corp Okta");
        // Must start with slugified name
        assert!(slug.starts_with("corp-okta-"), "slug: {slug}");
        // Suffix must be 8 hex chars (4 bytes)
        let suffix = slug.split('-').next_back().unwrap();
        assert_eq!(suffix.len(), 8, "hex suffix length: {slug}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_derive_provider_slug_deterministic() {
        let a = derive_provider_slug(42, "Internal Keycloak");
        let b = derive_provider_slug(42, "Internal Keycloak");
        assert_eq!(a, b, "slug must be deterministic");
    }

    #[test]
    fn test_derive_provider_slug_different_ids_differ() {
        let a = derive_provider_slug(1, "Google");
        let b = derive_provider_slug(2, "Google");
        assert_ne!(
            a, b,
            "same name but different IDs must produce different slugs"
        );
    }

    #[test]
    fn test_derive_provider_slug_empty_name() {
        // All non-alphanumeric name → pure 8-char hash
        let slug = derive_provider_slug(5, "---");
        assert_eq!(slug.len(), 8, "pure hash fallback: {slug}");
    }

    #[test]
    fn create_request_trust_idp_email_defaults_false() {
        // SECURITY-CRITICAL: a missing field must default to `false`,
        // so an admin POSTing an existing payload (e.g. via the API)
        // can never accidentally end up with the email_verified gate
        // disabled. If this test ever fails because someone changed
        // the default to `true`, that change reintroduces the
        // account-takeover vector documented on `oidc_providers::Model::trust_idp_email`.
        let json = r#"{
            "name": "Corp",
            "issuer_url": "https://idp.example.com",
            "client_id": "abc",
            "client_secret": "shh"
        }"#;
        let req: CreateOidcProviderRequest = serde_json::from_str(json).unwrap();
        assert!(
            !req.trust_idp_email,
            "trust_idp_email must default to false when omitted"
        );
    }

    #[test]
    fn create_request_trust_idp_email_round_trip_true() {
        // Confirms the field is wired through serde (typo in
        // `#[serde]` attrs would silently drop it).
        let json = r#"{
            "name": "Corp",
            "issuer_url": "https://idp.example.com",
            "client_id": "abc",
            "client_secret": "shh",
            "trust_idp_email": true
        }"#;
        let req: CreateOidcProviderRequest = serde_json::from_str(json).unwrap();
        assert!(req.trust_idp_email);
    }

    #[test]
    fn provider_to_response_round_trips_trust_idp_email() {
        let model = temps_entities::oidc_providers::Model {
            id: 7,
            name: "okta".into(),
            issuer_url: "https://gala.okta.com".into(),
            client_id: "client".into(),
            client_secret_encrypted: "ciphertext".into(),
            scopes: "openid email profile".into(),
            jit_provisioning: true,
            enabled: true,
            template: "okta".into(),
            group_claim: "groups".into(),
            role_claim: "roles".into(),
            default_role: "user".into(),
            trust_idp_email: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let resp = provider_to_response(&model);
        assert!(resp.trust_idp_email);
        // Sanity: secret is always masked.
        assert_eq!(resp.client_secret, "***");
    }

    #[test]
    fn test_oidc_provider_summary_no_id_field() {
        // Compile-time check: OidcProviderSummary has `slug` but no `id`.
        let summary = OidcProviderSummary {
            slug: "test-slug-aabbccdd".to_string(),
            name: "Test Provider".to_string(),
            template: "generic".to_string(),
        };
        assert_eq!(summary.slug, "test-slug-aabbccdd");
        // The following would be a compile error if `id` were present:
        // let _ = summary.id;
    }
}
