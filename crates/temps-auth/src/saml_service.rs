//! SAML 2.0 SP-initiated SSO (ADR 0013, Phase 2 of SSO -- OIDC is Phase 1,
//! `oidc_service.rs`). Mirrors the OIDC architecture exactly: same
//! three-layer pattern, same login-state/CSRF mechanism, same
//! (provider_id, subject) -> verified-email -> JIT-provisioning user
//! resolution flow, same role-mapping-via-IdP-attribute mechanism.
//!
//! XML signature verification is delegated entirely to `samael`'s
//! `ServiceProvider::parse_base64_response`, which internally reduces the
//! response XML to only the elements covered by a valid signature before
//! any field is read -- the defense against XML Signature Wrapping (XSW)
//! attacks. This service MUST NOT parse the raw incoming SAMLResponse XML
//! itself for any purpose; every field used in an authorization or
//! identity decision is read from the `samael::schema::Assertion` value
//! `parse_base64_response` returns, never from a fresh parse of the raw
//! document. See ADR 0013 §6 Step 3.

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use openssl::x509::X509;
use rand::RngCore;
use samael::key_info::{KeyInfo, X509Data};
use samael::metadata::KeyDescriptor;
use samael::metadata::{
    Endpoint, EntityDescriptor, EntityDescriptorType, IdpSsoDescriptor, HTTP_REDIRECT_BINDING,
};
use samael::schema::Assertion;
use samael::service_provider::{ServiceProvider, ServiceProviderBuilder};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use temps_entities::types::RoleType;
use temps_entities::{saml_login_states, saml_providers, saml_role_mappings, users};

use crate::saml_errors::SamlError;
use crate::saml_types::{derive_provider_slug, SamlProviderSummary};
use crate::user_service::UserService;

const LOGIN_STATE_TTL_MINUTES: i64 = 10;
const SAML_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Clock-skew tolerance for assertion time-window checks. Standard for
/// SAML interoperability; matches what `samael` and most other
/// implementations use.
const CLOCK_SKEW_SECONDS: i64 = 300;

pub struct SamlLoginStart {
    pub redirect_url: String,
}

pub struct SamlLoginState {
    pub provider_id: i32,
    pub authn_request_id: String,
    pub return_to: Option<String>,
}

pub struct SamlResolvedUser {
    pub user: users::Model,
}

/// Fields extracted from a verified `Assertion` before user resolution.
#[derive(Debug)]
struct ExtractedIdentity {
    subject: String,
    email: String,
    groups: Vec<String>,
}

pub struct SamlService {
    db: Arc<DatabaseConnection>,
    user_service: Arc<UserService>,
    http_client: reqwest::Client,
}

impl SamlService {
    pub fn new(db: Arc<DatabaseConnection>, user_service: Arc<UserService>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(SAML_HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self {
            db,
            user_service,
            http_client,
        }
    }

    // ------------------------------------------------------------------
    // Provider CRUD
    // ------------------------------------------------------------------

    pub async fn list_enabled_providers(&self) -> Result<Vec<SamlProviderSummary>, SamlError> {
        let providers = saml_providers::Entity::find()
            .filter(saml_providers::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;
        Ok(providers
            .iter()
            .map(|p| SamlProviderSummary {
                slug: derive_provider_slug(p.id, &p.name),
                name: p.name.clone(),
                template: p.template.clone(),
            })
            .collect())
    }

    pub async fn list_providers(&self) -> Result<Vec<saml_providers::Model>, SamlError> {
        Ok(saml_providers::Entity::find()
            .order_by_asc(saml_providers::Column::Id)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn get_provider(&self, provider_id: i32) -> Result<saml_providers::Model, SamlError> {
        saml_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(SamlError::ProviderNotFound { provider_id })
    }

    pub async fn get_provider_by_slug(
        &self,
        slug: &str,
    ) -> Result<saml_providers::Model, SamlError> {
        // Same linear scan as OIDC's get_provider_by_slug -- provider
        // counts are small (single digits to low tens), and this is not a
        // hot path (once per login, not once per request).
        let providers = self.list_providers().await?;
        providers
            .into_iter()
            .find(|p| derive_provider_slug(p.id, &p.name) == slug)
            .ok_or(SamlError::ProviderNotFound { provider_id: -1 })
    }

    pub async fn create_provider(
        &self,
        request: crate::saml_types::CreateSamlProviderRequest,
        base_url: &str,
    ) -> Result<saml_providers::Model, SamlError> {
        let name = request.name.trim().to_string();
        if saml_providers::Entity::find()
            .filter(saml_providers::Column::Name.eq(name.clone()))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(SamlError::ProviderAlreadyExists { name });
        }

        let (idp_entity_id, idp_sso_url, idp_x509_cert) = self
            .resolve_idp_fields(
                request.idp_metadata_url.as_deref(),
                request.idp_entity_id,
                request.idp_sso_url,
                request.idp_x509_cert,
            )
            .await?;
        validate_cert_pem(&idp_x509_cert)?;

        // Provisional slug computed from a not-yet-inserted row's id is
        // impossible (id is DB-assigned), so sp_entity_id defaults are
        // computed AFTER insert, in a follow-up UPDATE, mirroring the
        // two-step pattern OIDC would need if it had a self-referential
        // default (it doesn't; SAML's is unique to needing its own id in
        // its own default value).
        let active = saml_providers::ActiveModel {
            name: Set(name),
            template: Set(normalize_or(&request.template, "generic")),
            sp_entity_id: Set(String::new()), // placeholder, fixed below
            idp_entity_id: Set(idp_entity_id),
            idp_sso_url: Set(idp_sso_url),
            idp_x509_cert: Set(idp_x509_cert),
            idp_metadata_url: Set(request.idp_metadata_url),
            group_attribute: Set(normalize_or(&request.group_attribute, "groups")),
            role_attribute: Set(normalize_or(&request.role_attribute, "roles")),
            default_role: Set(normalize_or(&request.default_role, "user")),
            email_attribute: Set(request.email_attribute),
            jit_provisioning: Set(request.jit_provisioning),
            enabled: Set(request.enabled),
            trust_idp_email: Set(request.trust_idp_email),
            ..Default::default()
        };
        let provider = active.insert(self.db.as_ref()).await?;

        let slug = derive_provider_slug(provider.id, &provider.name);
        let sp_entity_id = request.sp_entity_id.unwrap_or_else(|| {
            format!(
                "{}/api/auth/saml/metadata/{}",
                base_url.trim_end_matches('/'),
                slug
            )
        });
        let mut active: saml_providers::ActiveModel = provider.into();
        active.sp_entity_id = Set(sp_entity_id);
        let provider = active.update(self.db.as_ref()).await?;

        Ok(provider)
    }

    pub async fn update_provider(
        &self,
        provider_id: i32,
        request: crate::saml_types::UpdateSamlProviderRequest,
    ) -> Result<saml_providers::Model, SamlError> {
        let provider = self.get_provider(provider_id).await?;
        let mut active: saml_providers::ActiveModel = provider.into();

        // Same disabling-detection pattern as OIDC's update_provider --
        // revoke sessions after the update only when transitioning
        // enabled=true -> enabled=false.
        let was_enabled = matches!(active.enabled, sea_orm::ActiveValue::Unchanged(true));
        let disabling = matches!(request.enabled, Some(false)) && was_enabled;

        if let Some(name) = request.name {
            active.name = Set(name.trim().to_string());
        }
        if let Some(template) = request.template {
            active.template = Set(normalize_or(&template, "generic"));
        }
        if let Some(sp_entity_id) = request.sp_entity_id {
            active.sp_entity_id = Set(sp_entity_id);
        }
        if let Some(idp_entity_id) = request.idp_entity_id {
            active.idp_entity_id = Set(idp_entity_id);
        }
        if let Some(idp_sso_url) = request.idp_sso_url {
            active.idp_sso_url = Set(idp_sso_url);
        }
        if let Some(idp_x509_cert) = request.idp_x509_cert {
            validate_cert_pem(&idp_x509_cert)?;
            active.idp_x509_cert = Set(idp_x509_cert);
        }
        if let Some(idp_metadata_url) = request.idp_metadata_url {
            active.idp_metadata_url = Set(Some(idp_metadata_url));
        }
        if let Some(group_attribute) = request.group_attribute {
            active.group_attribute = Set(normalize_or(&group_attribute, "groups"));
        }
        if let Some(role_attribute) = request.role_attribute {
            active.role_attribute = Set(normalize_or(&role_attribute, "roles"));
        }
        if let Some(default_role) = request.default_role {
            active.default_role = Set(normalize_or(&default_role, "user"));
        }
        if let Some(email_attribute) = request.email_attribute {
            active.email_attribute = Set(Some(email_attribute));
        }
        if let Some(jit) = request.jit_provisioning {
            active.jit_provisioning = Set(jit);
        }
        if let Some(enabled) = request.enabled {
            active.enabled = Set(enabled);
        }
        if let Some(trust) = request.trust_idp_email {
            active.trust_idp_email = Set(trust);
        }

        let provider = active.update(self.db.as_ref()).await?;

        if disabling {
            self.revoke_sessions_for_provider(provider_id).await?;
        }

        Ok(provider)
    }

    pub async fn delete_provider(&self, provider_id: i32) -> Result<(), SamlError> {
        let provider = self.get_provider(provider_id).await?;
        // SECURITY: revoke sessions before dropping the row -- identical
        // rationale to OidcService::delete_provider.
        self.revoke_sessions_for_provider(provider_id).await?;
        saml_providers::Entity::delete_by_id(provider.id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    pub async fn refresh_metadata(
        &self,
        provider_id: i32,
    ) -> Result<saml_providers::Model, SamlError> {
        let provider = self.get_provider(provider_id).await?;
        let url = provider
            .idp_metadata_url
            .clone()
            .ok_or(SamlError::NoMetadataUrl { provider_id })?;
        let (idp_entity_id, idp_sso_url, idp_x509_cert) =
            self.fetch_and_parse_idp_metadata(&url).await?;
        validate_cert_pem(&idp_x509_cert)?;

        let mut active: saml_providers::ActiveModel = provider.into();
        active.idp_entity_id = Set(idp_entity_id);
        active.idp_sso_url = Set(idp_sso_url);
        active.idp_x509_cert = Set(idp_x509_cert);
        Ok(active.update(self.db.as_ref()).await?)
    }

    pub async fn test_connection(&self, provider_id: i32) -> Result<String, SamlError> {
        let provider = self.get_provider(provider_id).await?;
        validate_cert_pem(&provider.idp_x509_cert)?;
        if let Some(url) = &provider.idp_metadata_url {
            assert_metadata_url_allowed(url).await?;
            let resp = self.http_client.head(url).send().await.map_err(|e| {
                SamlError::MetadataFetchFailed {
                    url: url.clone(),
                    reason: e.to_string(),
                }
            })?;
            if !resp.status().is_success() {
                return Ok(format!(
                    "Certificate is valid, but the metadata URL returned HTTP {}",
                    resp.status()
                ));
            }
        }
        Ok("IdP certificate parses correctly and is reachable.".to_string())
    }

    /// SP metadata XML for this provider (`GET /auth/saml/metadata/{slug}`).
    /// No SP signing key/cert in v1: `WantAssertionsSigned="true"` (we
    /// require signed assertions), `AuthnRequestsSigned="false"` (we don't
    /// sign our own requests -- see ADR 0013 §Out of Scope item 2).
    pub fn sp_metadata_xml(
        &self,
        provider: &saml_providers::Model,
        acs_url: &str,
    ) -> Result<String, SamlError> {
        use samael::traits::ToXml;

        let sp = build_service_provider(provider, acs_url)?;
        let entity_descriptor =
            sp.metadata()
                .map_err(|e| SamlError::AssertionValidationFailed {
                    reason: format!("failed to build SP metadata: {e}"),
                })?;
        entity_descriptor
            .to_string()
            .map_err(|e| SamlError::AssertionValidationFailed {
                reason: format!("failed to serialize SP metadata XML: {e:?}"),
            })
    }

    pub async fn list_users_for_provider(
        &self,
        provider_id: i32,
    ) -> Result<Vec<users::Model>, SamlError> {
        self.get_provider(provider_id).await?;
        Ok(users::Entity::find()
            .filter(users::Column::SamlProviderId.eq(provider_id))
            .filter(users::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn list_role_mappings(
        &self,
        provider_id: i32,
    ) -> Result<Vec<saml_role_mappings::Model>, SamlError> {
        self.get_provider(provider_id).await?;
        Ok(saml_role_mappings::Entity::find()
            .filter(saml_role_mappings::Column::ProviderId.eq(provider_id))
            .order_by_asc(saml_role_mappings::Column::Priority)
            .order_by_asc(saml_role_mappings::Column::Id)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn create_role_mapping(
        &self,
        provider_id: i32,
        request: crate::saml_types::CreateSamlRoleMappingRequest,
    ) -> Result<saml_role_mappings::Model, SamlError> {
        self.get_provider(provider_id).await?;
        parse_sso_role(&request.role)?;
        let active = saml_role_mappings::ActiveModel {
            provider_id: Set(provider_id),
            priority: Set(request.priority),
            idp_group: Set(request.idp_group.trim().to_string()),
            role: Set(request.role.trim().to_ascii_lowercase()),
            ..Default::default()
        };
        Ok(active.insert(self.db.as_ref()).await?)
    }

    pub async fn delete_role_mapping(&self, mapping_id: i32) -> Result<(), SamlError> {
        let result = saml_role_mappings::Entity::delete_by_id(mapping_id)
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(SamlError::RoleMappingNotFound { mapping_id });
        }
        Ok(())
    }

    async fn revoke_sessions_for_provider(&self, provider_id: i32) -> Result<(), SamlError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM sessions WHERE user_id IN \
                 (SELECT id FROM users WHERE saml_provider_id = $1 AND deleted_at IS NULL)",
                vec![provider_id.into()],
            ))
            .await?;

        tracing::info!(
            target: "temps_auth::saml",
            provider_id = provider_id,
            sessions_revoked = result.rows_affected(),
            "Revoked SSO sessions for SAML provider"
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // Login flow
    // ------------------------------------------------------------------

    pub async fn start_login(
        &self,
        provider_id: i32,
        acs_url: &str,
        return_to: Option<String>,
    ) -> Result<SamlLoginStart, SamlError> {
        self.cleanup_expired_login_states().await?;

        let provider = self.get_provider(provider_id).await?;
        if !provider.enabled {
            return Err(SamlError::ProviderDisabled { provider_id });
        }
        if let Some(ref path) = return_to {
            validate_return_to(path)?;
        }

        let sp = build_service_provider(&provider, acs_url)?;
        let authn_request = sp
            .make_authentication_request(&provider.idp_sso_url)
            .map_err(|e| SamlError::AssertionValidationFailed {
                reason: format!("failed to build AuthnRequest: {e}"),
            })?;
        let authn_request_id = authn_request.id.clone();

        let relay_state = random_token();
        let redirect_url = authn_request
            .redirect(&relay_state)
            .map_err(|e| SamlError::AssertionValidationFailed {
                reason: format!("failed to build redirect binding URL: {e}"),
            })?
            .ok_or_else(|| SamlError::AssertionValidationFailed {
                reason: "AuthnRequest has no destination".to_string(),
            })?;

        let expires_at = Utc::now() + ChronoDuration::minutes(LOGIN_STATE_TTL_MINUTES);
        saml_login_states::ActiveModel {
            relay_state: Set(relay_state),
            authn_request_id: Set(authn_request_id),
            provider_id: Set(provider_id),
            return_to: Set(return_to),
            expires_at: Set(expires_at),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

        Ok(SamlLoginStart {
            redirect_url: redirect_url.to_string(),
        })
    }

    pub async fn consume_login_state(
        &self,
        relay_state: &str,
    ) -> Result<SamlLoginState, SamlError> {
        // SECURITY: atomic DELETE ... RETURNING, same rationale and same
        // pattern as OidcService::consume_login_state -- a naive
        // SELECT-then-DELETE races under concurrent ACS callbacks.
        use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

        let row: Option<saml_login_states::Model> =
            saml_login_states::Model::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM saml_login_states WHERE relay_state = $1 \
                 RETURNING id, relay_state, authn_request_id, provider_id, return_to, expires_at, created_at",
                vec![relay_state.into()],
            ))
            .one(self.db.as_ref())
            .await?;

        let row = row.ok_or_else(|| SamlError::StateNotFound {
            relay_state: relay_state.to_string(),
        })?;

        if row.expires_at < Utc::now() {
            let age_secs = (Utc::now() - row.expires_at).num_seconds().abs();
            return Err(SamlError::StateExpired {
                relay_state: relay_state.to_string(),
                age_secs,
            });
        }

        Ok(SamlLoginState {
            provider_id: row.provider_id,
            authn_request_id: row.authn_request_id,
            return_to: row.return_to,
        })
    }

    /// The ACS validation pipeline (ADR 0013 §6). `login_state` must
    /// already have been consumed via `consume_login_state` by the
    /// caller (the handler owns state consumption so it can decide what
    /// to do with a `StateNotFound`/`StateExpired` error independently of
    /// this function, mirroring `oidc_handler::complete_oidc_login`).
    pub async fn process_acs_response(
        &self,
        provider: &saml_providers::Model,
        login_state: &SamlLoginState,
        saml_response_b64: &str,
        acs_url: &str,
    ) -> Result<SamlResolvedUser, SamlError> {
        if !provider.enabled {
            return Err(SamlError::ProviderDisabled {
                provider_id: provider.id,
            });
        }

        let sp = build_service_provider(provider, acs_url)?;

        // `parse_base64_response` is the ONLY place raw SAMLResponse XML
        // is parsed. It internally reduces the document to the elements
        // covered by a valid signature (XSW defense) BEFORE returning the
        // Assertion, and validates Destination, Audience, NotBefore/
        // NotOnOrAfter, and InResponseTo (via `possible_request_ids`)
        // against `sp.acs_url` / `sp.entity_id` / the passed IDs. See ADR
        // 0013 §6 for the full mapping of samael's internal checks to the
        // pipeline steps this function is documenting.
        let assertion: Assertion = sp
            .parse_base64_response(
                saml_response_b64,
                Some(&[login_state.authn_request_id.as_str()]),
            )
            .map_err(|e| {
                tracing::warn!(
                    target: "temps_auth::saml::abuse",
                    provider_id = provider.id,
                    error = %e,
                    "SAML ACS validation failed"
                );
                SamlError::AssertionValidationFailed {
                    reason: e.to_string(),
                }
            })?;

        // Every field below is read from `assertion`, the value samael
        // just returned as signature-verified -- never from a fresh
        // parse of `saml_response_b64`. Do not add a second XML parse of
        // the raw response anywhere in this codebase.
        let identity = extract_identity(&assertion, provider)?;

        let mappings = self.list_role_mappings(provider.id).await?;
        let role = evaluate_role(provider, &mappings, &identity.groups);

        self.resolve_user(provider, &identity, role).await
    }

    async fn resolve_user(
        &self,
        provider: &saml_providers::Model,
        identity: &ExtractedIdentity,
        role: RoleType,
    ) -> Result<SamlResolvedUser, SamlError> {
        // Step A: lookup by (provider_id, subject).
        if let Some(user) = users::Entity::find()
            .filter(users::Column::SamlProviderId.eq(provider.id))
            .filter(users::Column::SamlSubject.eq(identity.subject.clone()))
            .filter(users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
        {
            self.sync_user_sso_role(user.id, role).await?;
            return Ok(SamlResolvedUser { user });
        }

        // Step B: lookup by email (account linking).
        if let Some(user) = users::Entity::find()
            .filter(users::Column::Email.eq(identity.email.clone()))
            .filter(users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
        {
            if !provider.trust_idp_email {
                tracing::warn!(
                    target: "temps_auth::saml::abuse",
                    provider_id = provider.id,
                    email = %identity.email,
                    subject = %identity.subject,
                    "Refusing to link SAML identity: trust_idp_email is false"
                );
                return Err(SamlError::EmailNotTrusted {
                    email: identity.email.clone(),
                });
            }
            tracing::warn!(
                target: "temps_auth::saml::trust_bypass",
                provider_id = provider.id,
                email = %identity.email,
                subject = %identity.subject,
                "Linking SAML identity to existing account (trust_idp_email=true)"
            );
            let mut active: users::ActiveModel = user.into();
            active.saml_provider_id = Set(Some(provider.id));
            active.saml_subject = Set(Some(identity.subject.clone()));
            let linked = active.update(self.db.as_ref()).await?;
            self.sync_user_sso_role(linked.id, role).await?;
            return Ok(SamlResolvedUser { user: linked });
        }

        // Step C: JIT provisioning.
        if !provider.jit_provisioning {
            return Err(SamlError::UserNotProvisioned {
                email: identity.email.clone(),
            });
        }
        if !provider.trust_idp_email {
            tracing::warn!(
                target: "temps_auth::saml::abuse",
                provider_id = provider.id,
                email = %identity.email,
                subject = %identity.subject,
                "Refusing to JIT-provision SAML account: trust_idp_email is false"
            );
            return Err(SamlError::EmailNotTrusted {
                email: identity.email.clone(),
            });
        }

        let display_name = identity
            .email
            .split('@')
            .next()
            .unwrap_or("user")
            .to_string();

        let created = self
            .user_service
            .create_user(
                display_name,
                identity.email.clone(),
                None,
                vec![role.clone()],
            )
            .await
            .map_err(|e| SamlError::AssertionValidationFailed {
                reason: format!("JIT user creation failed: {e}"),
            })?;

        let user = users::Entity::find_by_id(created.user.id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SamlError::AssertionValidationFailed {
                reason: format!("JIT user {} not found after creation", created.user.id),
            })?;

        let mut active: users::ActiveModel = user.into();
        active.saml_provider_id = Set(Some(provider.id));
        active.saml_subject = Set(Some(identity.subject.clone()));
        let user = active.update(self.db.as_ref()).await?;
        self.sync_user_sso_role(user.id, role).await?;

        Ok(SamlResolvedUser { user })
    }

    async fn sync_user_sso_role(&self, user_id: i32, role: RoleType) -> Result<(), SamlError> {
        let user = self
            .user_service
            .get_user_with_roles(user_id)
            .await
            .map_err(|e| SamlError::AssertionValidationFailed {
                reason: format!("failed to load user roles for SSO sync: {e}"),
            })?;

        let has_role = user
            .roles
            .iter()
            .any(|existing| existing.name == role.as_str());

        for existing in &user.roles {
            if let Ok(existing_role) = RoleType::from_str(&existing.name) {
                if existing_role != role {
                    let _ = self
                        .user_service
                        .remove_role_from_user(user_id, existing_role)
                        .await;
                }
            }
        }

        if !has_role {
            self.user_service
                .assign_role_by_type(user_id, role)
                .await
                .map_err(|e| SamlError::AssertionValidationFailed {
                    reason: format!("failed to assign SSO role: {e}"),
                })?;
        }
        Ok(())
    }

    pub async fn cleanup_expired_login_states(&self) -> Result<(), SamlError> {
        saml_login_states::Entity::delete_many()
            .filter(saml_login_states::Column::ExpiresAt.lt(Utc::now()))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Metadata
    // ------------------------------------------------------------------

    async fn resolve_idp_fields(
        &self,
        idp_metadata_url: Option<&str>,
        idp_entity_id: Option<String>,
        idp_sso_url: Option<String>,
        idp_x509_cert: Option<String>,
    ) -> Result<(String, String, String), SamlError> {
        let fetched = if let Some(url) = idp_metadata_url {
            Some(self.fetch_and_parse_idp_metadata(url).await?)
        } else {
            None
        };

        let entity_id = idp_entity_id
            .or_else(|| fetched.as_ref().map(|f| f.0.clone()))
            .ok_or_else(|| SamlError::InvalidMetadata {
                reason: "idp_entity_id is required (either directly or via idp_metadata_url)"
                    .to_string(),
            })?;
        let sso_url = idp_sso_url
            .or_else(|| fetched.as_ref().map(|f| f.1.clone()))
            .ok_or_else(|| SamlError::InvalidMetadata {
                reason: "idp_sso_url is required (either directly or via idp_metadata_url)"
                    .to_string(),
            })?;
        let cert = idp_x509_cert
            .or_else(|| fetched.as_ref().map(|f| f.2.clone()))
            .ok_or_else(|| SamlError::InvalidMetadata {
                reason: "idp_x509_cert is required (either directly or via idp_metadata_url)"
                    .to_string(),
            })?;

        Ok((entity_id, sso_url, cert))
    }

    /// Fetches and parses IdP SAML metadata XML. SSRF-guarded identically
    /// to OIDC discovery (`assert_metadata_url_allowed` below mirrors
    /// `oidc_service::assert_issuer_host_allowed`).
    async fn fetch_and_parse_idp_metadata(
        &self,
        url: &str,
    ) -> Result<(String, String, String), SamlError> {
        assert_metadata_url_allowed(url).await?;

        let body = self
            .http_client
            .get(url)
            .send()
            .await
            .map_err(|e| SamlError::MetadataFetchFailed {
                url: url.to_string(),
                reason: e.to_string(),
            })?
            .text()
            .await
            .map_err(|e| SamlError::MetadataFetchFailed {
                url: url.to_string(),
                reason: e.to_string(),
            })?;

        let descriptor: EntityDescriptor = match body.parse::<EntityDescriptorType>() {
            Ok(EntityDescriptorType::EntityDescriptor(d)) => d,
            Ok(EntityDescriptorType::EntitiesDescriptor(entities)) => entities
                .descriptors
                .into_iter()
                .find_map(|d| match d {
                    EntityDescriptorType::EntityDescriptor(d)
                        if d.idp_sso_descriptors.is_some() =>
                    {
                        Some(d)
                    }
                    _ => None,
                })
                .ok_or_else(|| SamlError::InvalidMetadata {
                    reason: "metadata document has no IDPSSODescriptor".to_string(),
                })?,
            Err(e) => {
                return Err(SamlError::InvalidMetadata {
                    reason: format!("failed to parse metadata XML: {e}"),
                })
            }
        };

        let entity_id = descriptor
            .entity_id
            .clone()
            .ok_or_else(|| SamlError::InvalidMetadata {
                reason: "metadata is missing entityID".to_string(),
            })?;

        let idp_sso = descriptor
            .idp_sso_descriptors
            .as_ref()
            .and_then(|v| v.first())
            .ok_or_else(|| SamlError::InvalidMetadata {
                reason: "metadata has no IDPSSODescriptor".to_string(),
            })?;

        let sso_url = idp_sso
            .single_sign_on_services
            .iter()
            .find(|ep| ep.binding == HTTP_REDIRECT_BINDING)
            .or_else(|| idp_sso.single_sign_on_services.first())
            .map(|ep| ep.location.clone())
            .ok_or_else(|| SamlError::InvalidMetadata {
                reason: "metadata has no SingleSignOnService".to_string(),
            })?;

        let signing_cert_b64 = idp_sso
            .key_descriptors
            .iter()
            .find(|kd| kd.is_signing())
            .or_else(|| idp_sso.key_descriptors.first())
            .and_then(|kd| kd.key_info.x509_data.as_ref())
            .and_then(|x509| x509.certificates.first())
            .ok_or_else(|| SamlError::InvalidMetadata {
                reason: "metadata has no signing certificate".to_string(),
            })?;

        // Metadata's <X509Certificate> content is base64 DER with no PEM
        // armor; re-wrap it as PEM so it's stored in the same format an
        // admin would paste manually and so `validate_cert_pem` (which
        // expects PEM) can validate it uniformly regardless of source.
        let pem = base64_der_to_pem(signing_cert_b64);

        Ok((entity_id, sso_url, pem))
    }
}

// ------------------------------------------------------------------
// Free functions
// ------------------------------------------------------------------

fn normalize_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_sso_role(role: &str) -> Result<RoleType, SamlError> {
    RoleType::from_str(role.trim().to_ascii_lowercase().as_str()).map_err(|_| {
        SamlError::InvalidRole {
            role: role.to_string(),
        }
    })
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn validate_cert_pem(pem: &str) -> Result<(), SamlError> {
    X509::from_pem(pem.as_bytes())
        .map(|_| ())
        .map_err(|e| SamlError::InvalidCert {
            reason: format!("could not parse as X.509 PEM: {e}"),
        })
}

/// Strips PEM armor and whitespace, returning the base64 DER body samael's
/// `X509Data.certificates` expects (SAML metadata's `<X509Certificate>`
/// convention -- no `-----BEGIN CERTIFICATE-----` wrapper).
fn pem_to_base64_der_body(pem: &str) -> String {
    pem.lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("")
}

fn base64_der_to_pem(base64_der: &str) -> String {
    let stripped: String = base64_der.chars().filter(|c| !c.is_whitespace()).collect();
    let wrapped: Vec<String> = stripped
        .as_bytes()
        .chunks(64)
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect();
    format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        wrapped.join("\n")
    )
}

/// Builds an `EntityDescriptor` for the IdP directly from the three
/// stored fields, without round-tripping through XML. Field-by-field
/// construction (rather than hand-crafting XML and re-parsing) avoids any
/// risk of the crafted XML disagreeing with `samael`'s own writer/reader
/// vocabulary.
fn build_idp_entity_descriptor(
    provider: &saml_providers::Model,
) -> Result<EntityDescriptor, SamlError> {
    let cert_body = pem_to_base64_der_body(&provider.idp_x509_cert);

    let idp_sso_descriptor = IdpSsoDescriptor {
        id: None,
        valid_until: None,
        cache_duration: None,
        protocol_support_enumeration: Some("urn:oasis:names:tc:SAML:2.0:protocol".to_string()),
        error_url: None,
        signature: None,
        key_descriptors: vec![KeyDescriptor {
            key_use: Some("signing".to_string()),
            key_info: KeyInfo {
                id: None,
                x509_data: Some(X509Data {
                    certificates: vec![cert_body],
                }),
            },
            encryption_methods: None,
        }],
        organization: None,
        contact_people: vec![],
        artifact_resolution_service: vec![],
        single_logout_services: vec![],
        manage_name_id_services: vec![],
        name_id_formats: vec![],
        want_authn_requests_signed: None,
        single_sign_on_services: vec![Endpoint {
            binding: HTTP_REDIRECT_BINDING.to_string(),
            location: provider.idp_sso_url.clone(),
            response_location: None,
        }],
        name_id_mapping_services: vec![],
        assertion_id_request_services: vec![],
        attribute_profiles: vec![],
        attributes: vec![],
    };

    Ok(EntityDescriptor {
        entity_id: Some(provider.idp_entity_id.clone()),
        id: None,
        signature: None,
        valid_until: None,
        cache_duration: None,
        role_descriptors: None,
        idp_sso_descriptors: Some(vec![idp_sso_descriptor]),
        sp_sso_descriptors: None,
        authn_authority_descriptors: None,
        attribute_authority_descriptors: None,
        pdp_descriptors: None,
        affiliation_descriptors: None,
        contact_person: None,
        organization: None,
    })
}

/// Builds the `samael::ServiceProvider` used for both the login-initiation
/// (`make_authentication_request`) and ACS (`parse_base64_response`)
/// paths, so both always agree on `entity_id` / `acs_url` / `idp_metadata`.
/// No SP signing key/cert is configured (v1 does not sign AuthnRequests --
/// see ADR 0013 §Out of Scope item 2).
fn build_service_provider(
    provider: &saml_providers::Model,
    acs_url: &str,
) -> Result<ServiceProvider, SamlError> {
    let idp_metadata = build_idp_entity_descriptor(provider)?;
    ServiceProviderBuilder::default()
        .entity_id(provider.sp_entity_id.clone())
        .acs_url(acs_url.to_string())
        .idp_metadata(idp_metadata)
        .max_clock_skew(chrono::Duration::seconds(CLOCK_SKEW_SECONDS))
        .build()
        .map_err(|e| SamlError::AssertionValidationFailed {
            reason: format!("failed to build ServiceProvider: {e}"),
        })
}

/// Extracts NameID, email, and group attribute values from a verified
/// `Assertion`. Every value comes from `assertion` itself -- the value
/// `parse_base64_response` returned after signature verification and XSW
/// reduction. See the module doc comment.
fn extract_identity(
    assertion: &Assertion,
    provider: &saml_providers::Model,
) -> Result<ExtractedIdentity, SamlError> {
    let subject = assertion
        .subject
        .as_ref()
        .and_then(|s| s.name_id.as_ref())
        .map(|n| n.value.clone())
        .ok_or(SamlError::NameIdMissing)?;

    let name_id_format = assertion
        .subject
        .as_ref()
        .and_then(|s| s.name_id.as_ref())
        .and_then(|n| n.format.clone());

    let attributes: Vec<&samael::attribute::Attribute> = assertion
        .attribute_statements
        .as_ref()
        .map(|stmts| stmts.iter().flat_map(|s| s.attributes.iter()).collect())
        .unwrap_or_default();

    let email = if let Some(attr_name) = &provider.email_attribute {
        attribute_values(&attributes, attr_name)
            .into_iter()
            .next()
            .ok_or(SamlError::EmailMissing)?
    } else if name_id_format.as_deref()
        == Some("urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress")
    {
        subject.clone()
    } else {
        return Err(SamlError::EmailMissing);
    };
    let email = email.trim().to_lowercase();

    let groups = attribute_values(&attributes, &provider.group_attribute);

    Ok(ExtractedIdentity {
        subject,
        email,
        groups,
    })
}

fn attribute_values(attributes: &[&samael::attribute::Attribute], name: &str) -> Vec<String> {
    attributes
        .iter()
        .filter(|a| a.name.as_deref() == Some(name) || a.friendly_name.as_deref() == Some(name))
        .flat_map(|a| a.values.iter())
        .filter_map(|v| v.value.clone())
        .collect()
}

fn evaluate_role(
    provider: &saml_providers::Model,
    mappings: &[saml_role_mappings::Model],
    groups: &[String],
) -> RoleType {
    for mapping in mappings {
        if mapping.idp_group == "*" {
            if let Ok(role) = parse_sso_role(&mapping.role) {
                return role;
            }
            continue;
        }
        for group in groups {
            if group == &mapping.idp_group {
                if let Ok(role) = parse_sso_role(&mapping.role) {
                    return role;
                }
            }
        }
    }
    parse_sso_role(&provider.default_role).unwrap_or(RoleType::User)
}

fn validate_return_to(path: &str) -> Result<(), SamlError> {
    // Identical rules to oidc_service::validate_return_to.
    if !path.starts_with('/') {
        return Err(SamlError::InvalidReturnTo);
    }
    if path.starts_with("//") {
        return Err(SamlError::InvalidReturnTo);
    }
    if path.contains('\\') {
        return Err(SamlError::InvalidReturnTo);
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(SamlError::InvalidReturnTo);
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

fn is_blocked_ip(ip: &IpAddr) -> bool {
    // Identical classification to oidc_service::is_blocked_ip.
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_documentation()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// SSRF guard for `idp_metadata_url`, mirroring
/// `oidc_service::assert_issuer_host_allowed`. See ADR 0013 §9.
async fn assert_metadata_url_allowed(url: &str) -> Result<(), SamlError> {
    let parsed =
        openidconnect::url::Url::parse(url).map_err(|e| SamlError::MetadataUrlNotAllowed {
            reason: format!("could not parse metadata URL: {e}"),
        })?;
    let host = parsed
        .host_str()
        .ok_or_else(|| SamlError::MetadataUrlNotAllowed {
            reason: "metadata URL has no host".to_string(),
        })?;

    if is_loopback_host(host) {
        return Ok(());
    }

    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| SamlError::MetadataFetchFailed {
            url: url.to_string(),
            reason: format!("DNS lookup failed: {e}"),
        })?
        .collect();
    for addr in addrs {
        if is_blocked_ip(&addr.ip()) {
            tracing::warn!(
                target: "temps_auth::saml::abuse",
                url = %url,
                host = %host,
                ip = %addr.ip(),
                "Refusing to fetch SAML metadata that resolves to a private/internal IP"
            );
            return Err(SamlError::MetadataUrlNotAllowed {
                reason: format!(
                    "{host} resolves to non-public IP {} (use a public DNS name, or run the IdP on localhost)",
                    addr.ip()
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
