use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Slug derivation is identical to OIDC's -- reuse the same function so the
// two protocols can never drift, and so a provider named the same in both
// tables still gets distinct, deterministic slugs (the hash suffix is
// derived from `(id, name)`, and OIDC/SAML providers have independent id
// sequences, so a collision would require both the id AND the name to
// match across tables, which the UNIQUE(name) constraint on each table
// only prevents within, not across, protocols -- acceptable, since slugs
// are looked up against one table at a time via the `/auth/{protocol}/...`
// route prefix).
pub use crate::oidc_types::derive_provider_slug;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SamlProviderSummary {
    /// Stable opaque slug -- use this as the path parameter when
    /// initiating SAML login (`/auth/saml/login/{slug}`). The integer
    /// database ID is intentionally omitted to prevent provider
    /// enumeration, matching `OidcProviderSummary`.
    pub slug: String,
    pub name: String,
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SamlProviderResponse {
    pub id: i32,
    pub name: String,
    pub template: String,
    pub sp_entity_id: String,
    pub idp_entity_id: String,
    pub idp_sso_url: String,
    /// Returned unmasked -- this is the IdP's PUBLIC signing certificate,
    /// not a secret. Masking it would prevent admins from verifying which
    /// cert is configured. Contrast with `OidcProviderResponse::client_secret`.
    pub idp_x509_cert: String,
    pub idp_metadata_url: Option<String>,
    pub group_attribute: String,
    pub role_attribute: String,
    pub default_role: String,
    pub email_attribute: Option<String>,
    pub jit_provisioning: bool,
    pub enabled: bool,
    pub trust_idp_email: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSamlProviderRequest {
    pub name: String,
    #[serde(default = "default_template")]
    pub template: String,
    /// Overrides the computed default
    /// (`{base_url}/api/auth/saml/metadata/{slug}`). Most IdPs are fine
    /// with the default; some enterprise IdPs require a specific format.
    pub sp_entity_id: Option<String>,
    /// When set, the service fetches this URL at creation time and
    /// parses `idp_entity_id` / `idp_sso_url` / `idp_x509_cert` from the
    /// metadata XML. Any of those three fields, if also supplied,
    /// override the fetched values.
    pub idp_metadata_url: Option<String>,
    pub idp_entity_id: Option<String>,
    pub idp_sso_url: Option<String>,
    pub idp_x509_cert: Option<String>,
    #[serde(default = "default_group_attribute")]
    pub group_attribute: String,
    #[serde(default = "default_role_attribute")]
    pub role_attribute: String,
    #[serde(default = "default_role")]
    pub default_role: String,
    pub email_attribute: Option<String>,
    #[serde(default = "default_true")]
    pub jit_provisioning: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Defaults `true` for SAML (unlike OIDC's `CreateOidcProviderRequest`,
    /// which defaults `trust_idp_email` to `false`) -- see
    /// `saml_providers::Model::trust_idp_email` for why.
    #[serde(default = "default_true")]
    pub trust_idp_email: bool,
}

fn default_template() -> String {
    "generic".to_string()
}

fn default_group_attribute() -> String {
    "groups".to_string()
}

fn default_role_attribute() -> String {
    "roles".to_string()
}

fn default_role() -> String {
    "user".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateSamlProviderRequest {
    pub name: Option<String>,
    pub template: Option<String>,
    pub sp_entity_id: Option<String>,
    pub idp_entity_id: Option<String>,
    pub idp_sso_url: Option<String>,
    pub idp_x509_cert: Option<String>,
    pub idp_metadata_url: Option<String>,
    pub group_attribute: Option<String>,
    pub role_attribute: Option<String>,
    pub default_role: Option<String>,
    pub email_attribute: Option<String>,
    pub jit_provisioning: Option<bool>,
    pub enabled: Option<bool>,
    pub trust_idp_email: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SamlTestConnectionResponse {
    pub success: bool,
    pub message: String,
}

/// A user that has logged in via a given SAML provider. Mirrors
/// `OidcProviderUserResponse`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SamlProviderUserResponse {
    pub id: i32,
    pub name: String,
    pub email: String,
    pub email_verified: bool,
    pub mfa_enabled: bool,
    pub saml_subject: Option<String>,
    #[schema(value_type = String, format = DateTime, example = "2024-01-15T14:30:00Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = String, format = DateTime, example = "2024-01-15T14:30:00Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub fn saml_provider_user_to_response(
    user: &temps_entities::users::Model,
) -> SamlProviderUserResponse {
    SamlProviderUserResponse {
        id: user.id,
        name: user.name.clone(),
        email: user.email.clone(),
        email_verified: user.email_verified,
        mfa_enabled: user.mfa_enabled,
        saml_subject: user.saml_subject.clone(),
        created_at: user.created_at,
        updated_at: user.updated_at,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SamlRoleMappingResponse {
    pub id: i32,
    pub provider_id: i32,
    pub priority: i32,
    pub idp_group: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSamlRoleMappingRequest {
    pub priority: i32,
    pub idp_group: String,
    pub role: String,
}

pub fn saml_provider_to_response(
    provider: &temps_entities::saml_providers::Model,
) -> SamlProviderResponse {
    SamlProviderResponse {
        id: provider.id,
        name: provider.name.clone(),
        template: provider.template.clone(),
        sp_entity_id: provider.sp_entity_id.clone(),
        idp_entity_id: provider.idp_entity_id.clone(),
        idp_sso_url: provider.idp_sso_url.clone(),
        idp_x509_cert: provider.idp_x509_cert.clone(),
        idp_metadata_url: provider.idp_metadata_url.clone(),
        group_attribute: provider.group_attribute.clone(),
        role_attribute: provider.role_attribute.clone(),
        default_role: provider.default_role.clone(),
        email_attribute: provider.email_attribute.clone(),
        jit_provisioning: provider.jit_provisioning,
        enabled: provider.enabled,
        trust_idp_email: provider.trust_idp_email,
    }
}

pub fn saml_role_mapping_to_response(
    mapping: &temps_entities::saml_role_mappings::Model,
) -> SamlRoleMappingResponse {
    SamlRoleMappingResponse {
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
    fn create_request_trust_idp_email_defaults_true() {
        // SAML has no email_verified equivalent; the signed assertion is
        // the trust anchor. Unlike OIDC, a missing field here must
        // default to `true` -- see saml_providers::Model::trust_idp_email
        // and ADR 0013 §3 for the full rationale.
        let json = r#"{
            "name": "Corp Okta",
            "idp_entity_id": "https://idp.example.com/metadata",
            "idp_sso_url": "https://idp.example.com/sso",
            "idp_x509_cert": "-----BEGIN CERTIFICATE-----\nMII...\n-----END CERTIFICATE-----"
        }"#;
        let req: CreateSamlProviderRequest = serde_json::from_str(json).unwrap();
        assert!(
            req.trust_idp_email,
            "trust_idp_email must default to true when omitted for SAML"
        );
    }

    #[test]
    fn create_request_trust_idp_email_round_trip_false() {
        let json = r#"{
            "name": "Corp Okta",
            "idp_entity_id": "https://idp.example.com/metadata",
            "idp_sso_url": "https://idp.example.com/sso",
            "idp_x509_cert": "cert",
            "trust_idp_email": false
        }"#;
        let req: CreateSamlProviderRequest = serde_json::from_str(json).unwrap();
        assert!(!req.trust_idp_email);
    }

    #[test]
    fn provider_to_response_does_not_mask_cert() {
        let model = temps_entities::saml_providers::Model {
            id: 1,
            name: "okta".into(),
            template: "okta".into(),
            sp_entity_id: "https://temps.example.com/api/auth/saml/metadata/okta-abcd1234".into(),
            idp_entity_id: "https://idp.okta.com/metadata".into(),
            idp_sso_url: "https://idp.okta.com/sso".into(),
            idp_x509_cert: "-----BEGIN CERTIFICATE-----\nMII...\n-----END CERTIFICATE-----".into(),
            idp_metadata_url: None,
            group_attribute: "groups".into(),
            role_attribute: "roles".into(),
            default_role: "user".into(),
            email_attribute: None,
            jit_provisioning: true,
            enabled: true,
            trust_idp_email: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let resp = saml_provider_to_response(&model);
        // Contrast with OIDC's `client_secret`, which is always "***".
        assert_eq!(resp.idp_x509_cert, model.idp_x509_cert);
    }

    #[test]
    fn saml_provider_summary_no_id_field() {
        let summary = SamlProviderSummary {
            slug: "test-slug-aabbccdd".to_string(),
            name: "Test Provider".to_string(),
            template: "generic".to_string(),
        };
        assert_eq!(summary.slug, "test-slug-aabbccdd");
        // Compile-time check: the following line would fail to compile if
        // `id` were present on SamlProviderSummary.
        // let _ = summary.id;
    }
}
