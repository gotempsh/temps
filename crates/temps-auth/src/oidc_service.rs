use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreIdToken, CoreIdTokenClaims, CoreProviderMetadata,
};
use openidconnect::{
    reqwest::async_http_client, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl,
    Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use tokio::sync::Mutex;

use crate::oidc_errors::OidcError;
use crate::oidc_types::{
    role_mapping_to_response, CreateOidcProviderRequest, CreateOidcRoleMappingRequest,
    OidcProviderSummary, OidcRoleMappingResponse, UpdateOidcProviderRequest,
};
use crate::user_service::UserService;
use temps_core::EncryptionService;
use temps_entities::oidc_login_states;
use temps_entities::oidc_providers;
use temps_entities::oidc_role_mappings;
use temps_entities::types::RoleType;
use temps_entities::users;

const LOGIN_STATE_TTL_MINUTES: i64 = 10;
const DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(3600);

struct CachedClient {
    metadata: CoreProviderMetadata,
    /// Decrypted client secret, kept in memory next to the metadata so
    /// `core_client_for_provider` doesn't have to round-trip
    /// `EncryptionService::decrypt_string` on every authorize / token-exchange
    /// call. The plaintext has to be in memory anyway when we POST to the IdP,
    /// so caching it for the metadata TTL is no worse than the status quo.
    client_secret: String,
    /// The encrypted blob the plaintext was derived from. If a later
    /// `provider.client_secret_encrypted` doesn't match this value (e.g.
    /// secret rotated via direct DB edit and the `update_provider`
    /// invalidation was skipped), we treat the cache entry as stale.
    client_secret_ciphertext: String,
    cached_at: Instant,
}

pub struct OidcService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
    user_service: Arc<UserService>,
    discovery_cache: Mutex<HashMap<i32, CachedClient>>,
}

pub struct OidcLoginStart {
    pub authorize_url: String,
}

pub struct OidcLoginState {
    pub provider_id: i32,
    pub nonce: String,
    pub pkce_verifier: String,
    pub return_to: Option<String>,
}

pub struct OidcResolvedUser {
    pub user: users::Model,
}

pub struct OidcExchangeResult {
    pub claims: CoreIdTokenClaims,
    pub raw_claims: serde_json::Value,
}

impl OidcService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<EncryptionService>,
        user_service: Arc<UserService>,
    ) -> Self {
        Self {
            db,
            encryption_service,
            user_service,
            discovery_cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn list_enabled_providers(&self) -> Result<Vec<OidcProviderSummary>, OidcError> {
        let providers = oidc_providers::Entity::find()
            .filter(oidc_providers::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;

        Ok(providers
            .into_iter()
            .map(|p| OidcProviderSummary {
                id: p.id,
                name: p.name,
                template: p.template,
            })
            .collect())
    }

    pub async fn list_providers(&self) -> Result<Vec<oidc_providers::Model>, OidcError> {
        Ok(oidc_providers::Entity::find().all(self.db.as_ref()).await?)
    }

    pub async fn get_provider(&self, provider_id: i32) -> Result<oidc_providers::Model, OidcError> {
        oidc_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OidcError::ProviderNotFound { provider_id })
    }

    pub async fn create_provider(
        &self,
        request: CreateOidcProviderRequest,
    ) -> Result<oidc_providers::Model, OidcError> {
        let existing = oidc_providers::Entity::find()
            .count(self.db.as_ref())
            .await?;
        if existing > 0 {
            return Err(OidcError::ProviderAlreadyExists);
        }

        validate_issuer_url(&request.issuer_url)?;
        let encrypted_secret = self
            .encryption_service
            .encrypt_string(&request.client_secret)
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: request.issuer_url.clone(),
                reason: format!("failed to encrypt client secret: {e}"),
            })?;

        let provider = oidc_providers::ActiveModel {
            name: Set(request.name.trim().to_string()),
            issuer_url: Set(normalize_issuer_url(&request.issuer_url)?),
            client_id: Set(request.client_id.trim().to_string()),
            client_secret_encrypted: Set(encrypted_secret),
            scopes: Set(normalize_scopes(&request.scopes)),
            jit_provisioning: Set(request.jit_provisioning),
            enabled: Set(request.enabled),
            template: Set(normalize_template(&request.template)),
            group_claim: Set(normalize_claim_name(&request.group_claim, "groups")),
            role_claim: Set(normalize_claim_name(&request.role_claim, "roles")),
            default_role: Set(parse_sso_role(&request.default_role)?.as_str().to_string()),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

        self.discovery_cache.lock().await.remove(&provider.id);
        Ok(provider)
    }

    pub async fn update_provider(
        &self,
        provider_id: i32,
        request: UpdateOidcProviderRequest,
    ) -> Result<oidc_providers::Model, OidcError> {
        let provider = self.get_provider(provider_id).await?;
        let mut active: oidc_providers::ActiveModel = provider.into();

        if let Some(name) = request.name {
            active.name = Set(name.trim().to_string());
        }
        if let Some(issuer_url) = request.issuer_url {
            active.issuer_url = Set(normalize_issuer_url(&issuer_url)?);
        }
        if let Some(client_id) = request.client_id {
            active.client_id = Set(client_id.trim().to_string());
        }
        if let Some(client_secret) = request.client_secret {
            let encrypted_secret = self
                .encryption_service
                .encrypt_string(&client_secret)
                .map_err(|e| OidcError::DiscoveryFailed {
                    issuer: "local".into(),
                    reason: format!("failed to encrypt client secret: {e}"),
                })?;
            active.client_secret_encrypted = Set(encrypted_secret);
        }
        if let Some(scopes) = request.scopes {
            // Mirror create_provider: a PATCH that sets scopes to "" or
            // whitespace gets the OIDC-minimum default instead of silently
            // persisting an empty string (which then makes start_login send
            // an empty scopes vector and breaks login on strict IdPs).
            active.scopes = Set(normalize_scopes(&scopes));
        }
        if let Some(jit_provisioning) = request.jit_provisioning {
            active.jit_provisioning = Set(jit_provisioning);
        }
        if let Some(enabled) = request.enabled {
            active.enabled = Set(enabled);
        }
        if let Some(template) = request.template {
            active.template = Set(normalize_template(&template));
        }
        if let Some(group_claim) = request.group_claim {
            active.group_claim = Set(normalize_claim_name(&group_claim, "groups"));
        }
        if let Some(role_claim) = request.role_claim {
            active.role_claim = Set(normalize_claim_name(&role_claim, "roles"));
        }
        if let Some(default_role) = request.default_role {
            active.default_role = Set(parse_sso_role(&default_role)?.as_str().to_string());
        }

        let updated = active.update(self.db.as_ref()).await?;
        self.discovery_cache.lock().await.remove(&provider_id);
        Ok(updated)
    }

    pub async fn delete_provider(&self, provider_id: i32) -> Result<(), OidcError> {
        let provider = self.get_provider(provider_id).await?;
        oidc_providers::Entity::delete_by_id(provider.id)
            .exec(self.db.as_ref())
            .await?;
        self.discovery_cache.lock().await.remove(&provider_id);
        Ok(())
    }

    pub async fn list_role_mappings(
        &self,
        provider_id: i32,
    ) -> Result<Vec<OidcRoleMappingResponse>, OidcError> {
        self.get_provider(provider_id).await?;
        let mappings = oidc_role_mappings::Entity::find()
            .filter(oidc_role_mappings::Column::ProviderId.eq(provider_id))
            .order_by_asc(oidc_role_mappings::Column::Priority)
            .order_by_asc(oidc_role_mappings::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(mappings.iter().map(role_mapping_to_response).collect())
    }

    pub async fn create_role_mapping(
        &self,
        provider_id: i32,
        request: CreateOidcRoleMappingRequest,
    ) -> Result<OidcRoleMappingResponse, OidcError> {
        self.get_provider(provider_id).await?;
        let idp_group = request.idp_group.trim();
        if idp_group.is_empty() {
            return Err(OidcError::InvalidIssuer {
                reason: "idp_group cannot be empty".into(),
            });
        }
        let role = parse_sso_role(&request.role)?;
        let mapping = oidc_role_mappings::ActiveModel {
            provider_id: Set(provider_id),
            priority: Set(request.priority),
            idp_group: Set(idp_group.to_string()),
            role: Set(role.as_str().to_string()),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(role_mapping_to_response(&mapping))
    }

    pub async fn delete_role_mapping(&self, mapping_id: i32) -> Result<(), OidcError> {
        let deleted = oidc_role_mappings::Entity::delete_by_id(mapping_id)
            .exec(self.db.as_ref())
            .await?;
        if deleted.rows_affected == 0 {
            return Err(OidcError::RoleMappingNotFound { mapping_id });
        }
        Ok(())
    }

    pub async fn test_connection(&self, provider_id: i32) -> Result<String, OidcError> {
        let provider = self.get_provider(provider_id).await?;
        let metadata = self.fetch_provider_metadata(&provider, true).await?;
        Ok(format!(
            "Connected to {} (issuer: {})",
            provider.name,
            metadata.issuer().as_str()
        ))
    }

    pub async fn start_login(
        &self,
        provider_id: i32,
        redirect_uri: &str,
        return_to: Option<String>,
    ) -> Result<OidcLoginStart, OidcError> {
        self.cleanup_expired_login_states().await?;

        let provider = self.get_provider(provider_id).await?;
        if !provider.enabled {
            return Err(OidcError::ProviderDisabled { provider_id });
        }

        if let Some(ref path) = return_to {
            validate_return_to(path)?;
        }

        let client = self
            .core_client_for_provider(&provider, redirect_uri)
            .await?;
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let pkce_verifier_str = pkce_verifier.secret().to_string();

        let (authorize_url, csrf_token, nonce_token) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(pkce_challenge)
            .add_scopes(parse_scopes(&provider.scopes))
            .url();

        let expires_at = Utc::now() + ChronoDuration::minutes(LOGIN_STATE_TTL_MINUTES);
        oidc_login_states::ActiveModel {
            state: Set(csrf_token.secret().clone()),
            nonce: Set(nonce_token.secret().clone()),
            pkce_verifier: Set(pkce_verifier_str),
            provider_id: Set(provider_id),
            return_to: Set(return_to),
            expires_at: Set(expires_at),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

        Ok(OidcLoginStart {
            authorize_url: authorize_url.to_string(),
        })
    }

    pub async fn consume_login_state(&self, state: &str) -> Result<OidcLoginState, OidcError> {
        let row = oidc_login_states::Entity::find()
            .filter(oidc_login_states::Column::State.eq(state))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| OidcError::StateNotFound {
                state: state.to_string(),
            })?;

        oidc_login_states::Entity::delete_by_id(row.id)
            .exec(self.db.as_ref())
            .await?;

        if row.expires_at < Utc::now() {
            let age_secs = (Utc::now() - row.expires_at).num_seconds().abs();
            return Err(OidcError::StateExpired {
                state: state.to_string(),
                age_secs,
            });
        }

        Ok(OidcLoginState {
            provider_id: row.provider_id,
            nonce: row.nonce,
            pkce_verifier: row.pkce_verifier,
            return_to: row.return_to,
        })
    }

    pub async fn exchange_code(
        &self,
        provider: &oidc_providers::Model,
        redirect_uri: &str,
        code: &str,
        login_state: &OidcLoginState,
    ) -> Result<OidcExchangeResult, OidcError> {
        let client = self
            .core_client_for_provider(provider, redirect_uri)
            .await?;
        let token_response = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(PkceCodeVerifier::new(login_state.pkce_verifier.clone()))
            .request_async(async_http_client)
            .await
            .map_err(|e| OidcError::TokenExchangeFailed {
                status: 0,
                body: e.to_string(),
            })?;

        let id_token = token_response
            .id_token()
            .ok_or_else(|| OidcError::IdTokenInvalid {
                reason: "token response did not include an id_token".into(),
            })?;

        let verifier = client.id_token_verifier();
        let nonce = Nonce::new(login_state.nonce.clone());
        let claims = id_token
            .claims(&verifier, &nonce)
            .map_err(|e| OidcError::IdTokenInvalid {
                reason: e.to_string(),
            })
            .cloned()?;
        let raw_claims = decode_verified_id_token_payload(id_token)?;

        Ok(OidcExchangeResult { claims, raw_claims })
    }

    pub async fn resolve_user(
        &self,
        provider_id: i32,
        claims: &CoreIdTokenClaims,
        raw_claims: &serde_json::Value,
    ) -> Result<OidcResolvedUser, OidcError> {
        let provider = self.get_provider(provider_id).await?;
        let mappings = self.load_role_mappings(provider_id).await?;
        let groups = string_slice_claim(
            raw_claims,
            claim_name_or_default(&provider.group_claim, "groups"),
        );
        let role = evaluate_role(&provider, &mappings, &groups, raw_claims);

        let sub = claims.subject().as_str();
        let email = claims
            .email()
            .ok_or(OidcError::EmailClaimMissing)?
            .as_str()
            .trim()
            .to_lowercase();

        if let Some(user) = users::Entity::find()
            .filter(users::Column::OidcProviderId.eq(provider_id))
            .filter(users::Column::OidcSubject.eq(sub))
            .filter(users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
        {
            self.sync_user_sso_role(user.id, role).await?;
            return Ok(OidcResolvedUser { user });
        }

        if let Some(user) = users::Entity::find()
            .filter(users::Column::Email.eq(email.clone()))
            .filter(users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
        {
            let mut active: users::ActiveModel = user.clone().into();
            active.oidc_provider_id = Set(Some(provider_id));
            active.oidc_subject = Set(Some(sub.to_string()));
            if claims.email_verified().unwrap_or(false) {
                active.email_verified = Set(true);
            }
            let linked = active.update(self.db.as_ref()).await?;
            self.sync_user_sso_role(linked.id, role).await?;
            return Ok(OidcResolvedUser { user: linked });
        }

        if !provider.jit_provisioning {
            return Err(OidcError::UserNotProvisioned { email });
        }

        let display_name = claims
            .name()
            .and_then(|n| n.get(None))
            .map(|s| s.to_string())
            .unwrap_or_else(|| email.split('@').next().unwrap_or("user").to_string());

        let created = self
            .user_service
            .create_user(display_name, email.clone(), None, vec![role.clone()])
            .await
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("JIT user creation failed: {e}"),
            })?;

        let user = users::Entity::find_by_id(created.user.id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("JIT user {} not found after creation", created.user.id),
            })?;

        let mut active: users::ActiveModel = user.into();
        active.oidc_provider_id = Set(Some(provider_id));
        active.oidc_subject = Set(Some(sub.to_string()));
        active.email_verified = Set(claims.email_verified().unwrap_or(true));
        let user = active.update(self.db.as_ref()).await?;
        self.sync_user_sso_role(user.id, role).await?;

        Ok(OidcResolvedUser { user })
    }

    async fn load_role_mappings(
        &self,
        provider_id: i32,
    ) -> Result<Vec<oidc_role_mappings::Model>, OidcError> {
        Ok(oidc_role_mappings::Entity::find()
            .filter(oidc_role_mappings::Column::ProviderId.eq(provider_id))
            .order_by_asc(oidc_role_mappings::Column::Priority)
            .order_by_asc(oidc_role_mappings::Column::Id)
            .all(self.db.as_ref())
            .await?)
    }

    async fn sync_user_sso_role(&self, user_id: i32, role: RoleType) -> Result<(), OidcError> {
        let user = self
            .user_service
            .get_user_with_roles(user_id)
            .await
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: "local".into(),
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
                .map_err(|e| OidcError::DiscoveryFailed {
                    issuer: "local".into(),
                    reason: format!("failed to assign SSO role: {e}"),
                })?;
        }

        Ok(())
    }

    pub async fn cleanup_expired_login_states(&self) -> Result<(), OidcError> {
        oidc_login_states::Entity::delete_many()
            .filter(oidc_login_states::Column::ExpiresAt.lt(Utc::now()))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    pub fn sanitize_return_to(return_to: Option<String>) -> String {
        match return_to {
            Some(path) if validate_return_to(&path).is_ok() => path,
            _ => "/dashboard".to_string(),
        }
    }

    async fn core_client_for_provider(
        &self,
        provider: &oidc_providers::Model,
        redirect_uri: &str,
    ) -> Result<CoreClient, OidcError> {
        let (metadata, client_secret) = self.provider_client_bundle(provider, false).await?;

        Ok(CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(provider.client_id.clone()),
            Some(ClientSecret::new(client_secret)),
        )
        .set_redirect_uri(RedirectUrl::new(redirect_uri.to_string()).map_err(|e| {
            OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("invalid redirect URI: {e}"),
            }
        })?))
    }

    /// Returns `(provider_metadata, decrypted_client_secret)` for the given
    /// provider, populating both from cache when possible. Pass
    /// `force_refresh: true` from the operator-driven test-connection path
    /// so the operator sees the result of a *fresh* discovery + decrypt
    /// rather than whatever's been sitting in cache for up to an hour.
    async fn provider_client_bundle(
        &self,
        provider: &oidc_providers::Model,
        force_refresh: bool,
    ) -> Result<(CoreProviderMetadata, String), OidcError> {
        if !force_refresh {
            let cache = self.discovery_cache.lock().await;
            if let Some(entry) = cache.get(&provider.id) {
                if entry.cached_at.elapsed() < DISCOVERY_CACHE_TTL
                    && entry.client_secret_ciphertext == provider.client_secret_encrypted
                {
                    return Ok((entry.metadata.clone(), entry.client_secret.clone()));
                }
            }
        }

        let issuer = IssuerUrl::new(normalize_issuer_url(&provider.issuer_url)?).map_err(|e| {
            OidcError::InvalidIssuer {
                reason: e.to_string(),
            }
        })?;

        let metadata = CoreProviderMetadata::discover_async(issuer, async_http_client)
            .await
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: e.to_string(),
            })?;

        let client_secret = self
            .encryption_service
            .decrypt_string(&provider.client_secret_encrypted)
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("failed to decrypt client secret: {e}"),
            })?;

        self.discovery_cache.lock().await.insert(
            provider.id,
            CachedClient {
                metadata: metadata.clone(),
                client_secret: client_secret.clone(),
                client_secret_ciphertext: provider.client_secret_encrypted.clone(),
                cached_at: Instant::now(),
            },
        );

        Ok((metadata, client_secret))
    }

    async fn fetch_provider_metadata(
        &self,
        provider: &oidc_providers::Model,
        force_refresh: bool,
    ) -> Result<CoreProviderMetadata, OidcError> {
        let (metadata, _secret) = self.provider_client_bundle(provider, force_refresh).await?;
        Ok(metadata)
    }
}

fn parse_scopes(scopes: &str) -> Vec<Scope> {
    scopes
        .split_whitespace()
        .map(|s| Scope::new(s.to_string()))
        .collect()
}

/// OIDC requires the `openid` scope; `email` + `profile` are needed for
/// our claims pipeline (email is the user-identity key, profile gives us a
/// display name). Empty input therefore falls back to all three rather
/// than persisting an empty string.
fn normalize_scopes(scopes: &str) -> String {
    let trimmed = scopes.trim();
    if trimmed.is_empty() {
        "openid email profile".to_string()
    } else {
        trimmed.to_string()
    }
}

fn validate_issuer_url(issuer: &str) -> Result<(), OidcError> {
    normalize_issuer_url(issuer).map(|_| ())
}

fn normalize_issuer_url(issuer: &str) -> Result<String, OidcError> {
    let trimmed = issuer.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(OidcError::InvalidIssuer {
            reason: "issuer URL cannot be empty".into(),
        });
    }
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Err(OidcError::InvalidIssuer {
            reason: "issuer URL must start with http:// or https://".into(),
        });
    }
    Ok(trimmed.to_string())
}

fn validate_return_to(path: &str) -> Result<(), OidcError> {
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(OidcError::InvalidReturnTo);
    }
    Ok(())
}

fn normalize_template(template: &str) -> String {
    let trimmed = template.trim();
    if trimmed.is_empty() {
        "generic".to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_claim_name(claim: &str, fallback: &str) -> String {
    let trimmed = claim.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn claim_name_or_default<'a>(claim: &'a str, fallback: &'a str) -> &'a str {
    let trimmed = claim.trim();
    if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    }
}

fn parse_sso_role(role: &str) -> Result<RoleType, OidcError> {
    RoleType::from_str(role.trim().to_ascii_lowercase().as_str()).map_err(|_| {
        OidcError::InvalidRole {
            role: role.to_string(),
        }
    })
}

fn decode_verified_id_token_payload(
    id_token: &CoreIdToken,
) -> Result<serde_json::Value, OidcError> {
    use base64::Engine;

    let jwt = id_token.to_string();
    let payload_b64 = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| OidcError::IdTokenInvalid {
            reason: "malformed id_token".into(),
        })?;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| OidcError::IdTokenInvalid {
            reason: format!("failed to decode id_token payload: {e}"),
        })?;
    serde_json::from_slice(&payload_bytes).map_err(|e| OidcError::IdTokenInvalid {
        reason: format!("failed to parse id_token payload JSON: {e}"),
    })
}

fn string_slice_claim(claims: &serde_json::Value, key: &str) -> Vec<String> {
    let Some(value) = claims.get(key) else {
        return Vec::new();
    };

    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(item) => vec![item.clone()],
        _ => Vec::new(),
    }
}

fn evaluate_role(
    provider: &oidc_providers::Model,
    mappings: &[oidc_role_mappings::Model],
    groups: &[String],
    raw_claims: &serde_json::Value,
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

    let role_claim = claim_name_or_default(&provider.role_claim, "roles");
    if !role_claim.is_empty() {
        let roles = string_slice_claim(raw_claims, role_claim);
        if let Some(first) = roles.first() {
            if let Ok(role) = parse_sso_role(first) {
                return role;
            }
        }
    }

    parse_sso_role(&provider.default_role).unwrap_or(RoleType::User)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_return_to_rejects_open_redirect() {
        assert_eq!(
            OidcService::sanitize_return_to(Some("//evil.com".into())),
            "/dashboard"
        );
        assert_eq!(
            OidcService::sanitize_return_to(Some("https://evil.com".into())),
            "/dashboard"
        );
        assert_eq!(
            OidcService::sanitize_return_to(Some("/projects".into())),
            "/projects"
        );
    }

    #[test]
    fn normalize_issuer_url_strips_trailing_slash() {
        assert_eq!(
            normalize_issuer_url("https://auth.example.com/").unwrap(),
            "https://auth.example.com"
        );
    }

    #[test]
    fn normalize_scopes_falls_back_to_default_on_empty() {
        assert_eq!(normalize_scopes(""), "openid email profile");
        assert_eq!(normalize_scopes("   "), "openid email profile");
        assert_eq!(normalize_scopes("\t\n  "), "openid email profile");
    }

    #[test]
    fn normalize_scopes_preserves_caller_value_when_present() {
        assert_eq!(normalize_scopes("openid"), "openid");
        assert_eq!(
            normalize_scopes("  openid email profile groups "),
            "openid email profile groups"
        );
    }

    #[test]
    fn validate_return_to_accepts_relative_paths() {
        assert!(validate_return_to("/dashboard").is_ok());
        assert!(validate_return_to("//evil.com").is_err());
    }

    #[test]
    fn evaluate_role_matches_group_then_wildcard() {
        let provider = oidc_providers::Model {
            id: 1,
            name: "test".into(),
            issuer_url: "https://auth.example.com".into(),
            client_id: "client".into(),
            client_secret_encrypted: "secret".into(),
            scopes: "openid".into(),
            jit_provisioning: true,
            enabled: true,
            template: "generic".into(),
            group_claim: "groups".into(),
            role_claim: "roles".into(),
            default_role: "user".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let mappings = vec![
            oidc_role_mappings::Model {
                id: 1,
                provider_id: 1,
                priority: 10,
                idp_group: "temps-admins".into(),
                role: "admin".into(),
                created_at: chrono::Utc::now(),
            },
            oidc_role_mappings::Model {
                id: 2,
                provider_id: 1,
                priority: 100,
                idp_group: "*".into(),
                role: "user".into(),
                created_at: chrono::Utc::now(),
            },
        ];

        assert_eq!(
            evaluate_role(
                &provider,
                &mappings,
                &["temps-admins".into()],
                &serde_json::json!({})
            ),
            RoleType::Admin
        );
        assert_eq!(
            evaluate_role(
                &provider,
                &mappings,
                &["other-group".into()],
                &serde_json::json!({})
            ),
            RoleType::User
        );
    }

    #[test]
    fn evaluate_role_falls_back_to_role_claim() {
        let provider = oidc_providers::Model {
            id: 1,
            name: "test".into(),
            issuer_url: "https://auth.example.com".into(),
            client_id: "client".into(),
            client_secret_encrypted: "secret".into(),
            scopes: "openid".into(),
            jit_provisioning: true,
            enabled: true,
            template: "generic".into(),
            group_claim: "groups".into(),
            role_claim: "roles".into(),
            default_role: "user".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(
            evaluate_role(
                &provider,
                &[],
                &[],
                &serde_json::json!({ "roles": ["admin"] })
            ),
            RoleType::Admin
        );
    }
}
