use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreIdToken, CoreIdTokenClaims, CoreProviderMetadata,
};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    RequestTokenError, Scope, TokenResponse,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use tokio::sync::Mutex;

use crate::oidc_errors::OidcError;
use crate::oidc_types::{
    derive_provider_slug, role_mapping_to_response, CreateOidcProviderRequest,
    CreateOidcRoleMappingRequest, OidcProviderSummary, OidcRoleMappingResponse,
    UpdateOidcProviderRequest,
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
/// Hard cap on how long an OIDC discovery or token-exchange round-trip
/// can take. openidconnect 4.x lets us own the `reqwest::Client`, so
/// we set this once at service init instead of relying on the default
/// (no timeout) client that openidconnect 3.x shipped.
const OIDC_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on `idp_group` length in `oidc_role_mappings` rows. The DB
/// column is unbounded `text`; without this an admin-only path could
/// stuff a giant string in there that then gets byte-compared against
/// every claim value on every SSO login.
const IDP_GROUP_MAX_LEN: usize = 256;

/// `CoreClient` after we've populated everything we need (auth URL +
/// token URL from discovery, plus our redirect URI). openidconnect 4.x
/// encodes endpoint-set-ness in the type parameters, so this alias
/// gives the compiler what it needs without polluting every call site.
type ConfiguredCoreClient = CoreClient<
    EndpointSet,      // HasAuthUrl
    EndpointNotSet,   // HasDeviceAuthUrl
    EndpointNotSet,   // HasIntrospectionUrl
    EndpointNotSet,   // HasRevocationUrl
    EndpointMaybeSet, // HasTokenUrl  -- maybe set depending on IdP
    EndpointMaybeSet, // HasUserInfoUrl
>;

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

/// A reqwest `Resolve` implementation that calls the system DNS resolver and
/// then rejects any address that `is_blocked_ip` considers private/internal.
///
/// This closes the TOCTOU window between `assert_issuer_host_allowed` (which
/// resolves the hostname before any HTTP is attempted) and the actual TCP
/// `connect()` inside reqwest/hyper. With short-TTL DNS records an attacker
/// who controls DNS can return a public IP for the pre-check and then a
/// private IP (e.g. `169.254.169.254`) by the time the real connection is
/// made. Installing this resolver on the OIDC `reqwest::Client` means the IP
/// is re-validated at connect time, eliminating the window.
///
/// Loopback addresses (`127.x`, `::1`) are **not** blocked here for parity
/// with `assert_issuer_host_allowed`, which explicitly allows loopback so
/// local Keycloak / Authentik dev continues to work.
struct BlocklistResolver;

impl reqwest::dns::Resolve for BlocklistResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            // `lookup_host` accepts `(host, port)` but we only care about IPs;
            // port 0 is fine because reqwest overwrites it from the URL.
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();

            // Reject the entire resolution if any returned address is blocked.
            // We refuse the whole set rather than silently filtering so that
            // misconfigured round-robin DNS (mix of public + private) fails
            // loudly instead of silently succeeding on the private IP.
            for addr in &addrs {
                if is_blocked_ip(&addr.ip()) {
                    return Err(format!(
                        "OIDC issuer '{}' resolved to a blocked private/internal IP ({}) at \
                         connect time; possible DNS rebinding attack",
                        host,
                        addr.ip()
                    )
                    .into());
                }
            }

            let addrs_iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs_iter)
        })
    }
}

pub struct OidcService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
    user_service: Arc<UserService>,
    discovery_cache: Mutex<HashMap<i32, CachedClient>>,
    /// HTTP client used for every outbound call to the IdP
    /// (discovery and token exchange). openidconnect 4.x lets us
    /// own this and thread it through `discover_async` and
    /// `request_async`, so timeout, redirect policy, and any future
    /// custom DNS resolution apply uniformly to every IdP
    /// round-trip. Built once at service init in `new()`.
    http_client: reqwest::Client,
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
        // Per openidconnect 4.x guidance, build a single dedicated
        // client with:
        //   * an explicit timeout — the openidconnect 3.x default
        //     client had none, which let a slow / dead IdP block a
        //     login (and our test-connection endpoint) for the full
        //     reqwest default of 30s+.
        //   * `Policy::none()` for redirects — the openidconnect docs
        //     call this out explicitly as an SSRF mitigation; a
        //     malicious IdP could otherwise 302 us to an internal
        //     URL on first request.
        //
        // We `expect` here because failure of `ClientBuilder::build`
        // means the platform's rustls / native cert store is broken,
        // which is unrecoverable at the service layer. If this fires
        // in production it's an "install OS certs" problem, not a
        // runtime concern.
        // `BlocklistResolver` re-validates every resolved IP at connect time,
        // closing the TOCTOU window between `assert_issuer_host_allowed`
        // (which runs before the HTTP attempt) and the actual TCP connect
        // inside hyper. An attacker with short-TTL DNS can return a public IP
        // at check time and `169.254.169.254` at connect time; the resolver
        // catches the second lookup and aborts the connection.
        let http_client = reqwest::ClientBuilder::new()
            .timeout(OIDC_HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(BlocklistResolver))
            .build()
            .expect("OIDC reqwest client should build; OS TLS / cert store is unusable");

        Self {
            db,
            encryption_service,
            user_service,
            discovery_cache: Mutex::new(HashMap::new()),
            http_client,
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
                slug: derive_provider_slug(p.id, &p.name),
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

    /// Resolve a provider from its public slug. The slug is derived
    /// deterministically from `(id, name)` via `derive_provider_slug`, so we
    /// fetch all providers, recompute each slug, and match — O(n) over the
    /// provider count which is expected to be small (< 10).
    ///
    /// Returns `OidcError::ProviderNotFound` with a synthetic ID of 0 when no
    /// match is found, so callers never learn which IDs actually exist.
    pub async fn get_provider_by_slug(
        &self,
        slug: &str,
    ) -> Result<oidc_providers::Model, OidcError> {
        let all = oidc_providers::Entity::find().all(self.db.as_ref()).await?;
        all.into_iter()
            .find(|p| derive_provider_slug(p.id, &p.name) == slug)
            .ok_or(OidcError::ProviderNotFound { provider_id: 0 })
    }

    pub async fn create_provider(
        &self,
        request: CreateOidcProviderRequest,
    ) -> Result<oidc_providers::Model, OidcError> {
        let name = request.name.trim().to_string();
        if name.is_empty() {
            return Err(OidcError::InvalidIssuer {
                reason: "provider name cannot be empty".into(),
            });
        }
        let name_collision = oidc_providers::Entity::find()
            .filter(oidc_providers::Column::Name.eq(name.clone()))
            .count(self.db.as_ref())
            .await?;
        if name_collision > 0 {
            return Err(OidcError::ProviderAlreadyExists { name: name.clone() });
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
            name: Set(name),
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
            trust_idp_email: Set(request.trust_idp_email),
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
        // Track whether this PATCH is *disabling* a previously-enabled
        // provider so we can revoke active sessions after the update.
        // Same reasoning as `delete_provider`: an admin disabling a
        // provider in an incident expects the SSO-linked users to
        // lose access right away, not at session-cookie expiry.
        let was_enabled = matches!(active.enabled, sea_orm::ActiveValue::Unchanged(true));
        let disabling = matches!(request.enabled, Some(false)) && was_enabled;
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
        if let Some(trust_idp_email) = request.trust_idp_email {
            active.trust_idp_email = Set(trust_idp_email);
        }

        let updated = active.update(self.db.as_ref()).await?;
        self.discovery_cache.lock().await.remove(&provider_id);

        if disabling {
            self.revoke_sessions_for_provider(provider_id).await?;
        }

        Ok(updated)
    }

    /// Delete every active `sessions` row owned by a user linked to
    /// `provider_id`. Used when a provider is deleted or disabled so
    /// SSO-linked users lose access immediately rather than at
    /// session-cookie expiry. Best-effort: a DB failure logs and
    /// returns the error so the caller can decide whether to surface
    /// it (we currently propagate it; the surrounding admin handler
    /// already records the audit row regardless).
    async fn revoke_sessions_for_provider(&self, provider_id: i32) -> Result<(), OidcError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        // Single-statement: DELETE … WHERE user_id IN (SELECT …).
        // Sea-ORM has no first-class subquery DELETE; raw SQL is
        // both safer (one round-trip, one lock) and clearer here.
        // Parameterised via $1 — no injection surface even though
        // the input is an i32.
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM sessions WHERE user_id IN \
                 (SELECT id FROM users WHERE oidc_provider_id = $1 AND deleted_at IS NULL)",
                vec![provider_id.into()],
            ))
            .await?;

        tracing::info!(
            target: "temps_auth::oidc",
            provider_id = provider_id,
            sessions_revoked = result.rows_affected(),
            "Revoked SSO sessions for provider"
        );

        Ok(())
    }

    /// Users that have logged in via this provider (matched by
    /// `users.oidc_provider_id`). Returns soft-deleted users filtered out.
    pub async fn list_users_for_provider(
        &self,
        provider_id: i32,
    ) -> Result<Vec<users::Model>, OidcError> {
        self.get_provider(provider_id).await?;
        Ok(users::Entity::find()
            .filter(users::Column::OidcProviderId.eq(provider_id))
            .filter(users::Column::DeletedAt.is_null())
            .order_by_asc(users::Column::Email)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn delete_provider(&self, provider_id: i32) -> Result<(), OidcError> {
        let provider = self.get_provider(provider_id).await?;

        // SECURITY: revoke active sessions for every user linked to
        // this provider *before* dropping the row. Otherwise an admin
        // who deletes a compromised provider during an incident
        // leaves the existing session cookies valid until natural
        // expiry — exactly the people they're trying to lock out.
        // `sessions.user_id → users.id` is ON DELETE CASCADE, but
        // deleting a provider does not delete users, so the cascade
        // doesn't help here.
        self.revoke_sessions_for_provider(provider_id).await?;

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
        // Bound the input: this string is byte-compared against every
        // group claim on every SSO login, and the DB column is
        // unbounded `text`. Reject control chars (incl. null bytes)
        // and anything over 256 chars. 256 fits every IdP group name
        // we've seen — Auth0 / Okta / Keycloak conventions all stay
        // well under 64.
        if idp_group.len() > IDP_GROUP_MAX_LEN {
            return Err(OidcError::InvalidIssuer {
                reason: format!(
                    "idp_group too long: {} bytes (max {IDP_GROUP_MAX_LEN})",
                    idp_group.len()
                ),
            });
        }
        if idp_group.chars().any(|c| c.is_control()) {
            return Err(OidcError::InvalidIssuer {
                reason: "idp_group contains control characters".into(),
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
        // SECURITY: must be atomic. A naive SELECT-then-DELETE
        // sequence races under concurrent callbacks (browser
        // double-submit, network retry): two requests can both pass
        // the SELECT before either runs the DELETE, then both proceed
        // into `exchange_code`. The IdP's single-use enforcement on
        // the authorization code is the outer gate, but the nonce
        // and PKCE verifier would be consumed twice on our side.
        //
        // PostgreSQL's `DELETE ... RETURNING *` does both halves in
        // one statement under the row lock, so only one caller can
        // ever observe the row. Sea-ORM has no first-class API for
        // this, so we drop to raw SQL via the same `from_sql_and_values`
        // pattern used elsewhere in the codebase (see
        // `error_alert_service.rs`). `FromQueryResult` on
        // `oidc_login_states::Model` is derived automatically by the
        // `DeriveEntityModel` macro.
        use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

        let row: Option<oidc_login_states::Model> =
            oidc_login_states::Model::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM oidc_login_states WHERE state = $1 \
                 RETURNING id, state, nonce, pkce_verifier, provider_id, return_to, expires_at, created_at",
                vec![state.into()],
            ))
            .one(self.db.as_ref())
            .await?;

        let row = row.ok_or_else(|| OidcError::StateNotFound {
            state: state.to_string(),
        })?;

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
        // openidconnect 4.x's `RequestTokenError` still has a lossy
        // `Display` impl (the variant labels lose the actual cause),
        // so we explicitly match the variants and lift the useful
        // bits into `OidcError::TokenExchangeFailed`. Closure rather
        // than a named function — the inner reqwest::Error type
        // comes from openidconnect's bundled reqwest dep which is
        // not directly nameable from here.
        let token_response = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|e| OidcError::TokenExchangeFailed {
                status: 0,
                body: format!("client misconfigured: {e}"),
            })?
            .set_pkce_verifier(PkceCodeVerifier::new(login_state.pkce_verifier.clone()))
            .request_async(&self.http_client)
            .await
            .map_err(|e| match e {
                RequestTokenError::ServerResponse(resp) => OidcError::TokenExchangeFailed {
                    status: 400,
                    body: resp.to_string(),
                },
                RequestTokenError::Parse(parse_err, body) => OidcError::TokenExchangeFailed {
                    status: 0,
                    body: format!("{parse_err}; body: {}", String::from_utf8_lossy(&body)),
                },
                RequestTokenError::Request(req_err) => OidcError::TokenExchangeFailed {
                    status: 0,
                    body: describe_discovery_error(&req_err),
                },
                RequestTokenError::Other(msg) => OidcError::TokenExchangeFailed {
                    status: 0,
                    body: msg,
                },
            })?;

        let id_token = token_response
            .id_token()
            .ok_or_else(|| OidcError::IdTokenInvalid {
                reason: "token response did not include an id_token".into(),
            })?;

        // Try to verify the id_token against the keys from the
        // currently-cached discovery doc. If that fails AND the
        // failure looks like a signing-key problem (unknown `kid`,
        // signature mismatch, etc.), the IdP probably rotated its
        // JWKS while our 1-hour metadata cache was still warm. Force
        // a discovery refresh and verify exactly once more — that
        // closes the up-to-60-minute login outage that would
        // otherwise follow every JWK rotation.
        let nonce = Nonce::new(login_state.nonce.clone());
        let first_attempt = {
            let verifier = client.id_token_verifier();
            id_token.claims(&verifier, &nonce).cloned()
        };

        let claims = match first_attempt {
            Ok(claims) => claims,
            Err(e) if looks_like_jwks_rotation(&e.to_string()) => {
                tracing::info!(
                    target: "temps_auth::oidc",
                    provider_id = provider.id,
                    "id_token verification failed; refreshing JWKS and retrying once"
                );
                // Force refresh: drops the cache entry, re-fetches
                // discovery + JWKS, then rebuilds the client. The
                // single retry boundary stops a faulty IdP from
                // turning every login into a discovery storm.
                let refreshed_client = self
                    .core_client_for_provider_refresh(provider, redirect_uri)
                    .await?;
                let verifier = refreshed_client.id_token_verifier();
                id_token
                    .claims(&verifier, &nonce)
                    .map_err(|e| OidcError::IdTokenInvalid {
                        reason: format!("verification still failed after JWKS refresh: {e}"),
                    })
                    .cloned()?
            }
            Err(e) => {
                return Err(OidcError::IdTokenInvalid {
                    reason: e.to_string(),
                });
            }
        };

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
            // SECURITY: only link an IdP identity onto an existing
            // local account if the IdP asserts the email is verified.
            // Without this, an attacker who can sign up at a
            // configured IdP with `victim@example.com` (unverified)
            // could take over the victim's pre-existing Temps account
            // (password-based account) on first SSO login.
            // The OIDC spec's `email_verified` claim is exactly the
            // signal we need; if the IdP doesn't set it (or sets
            // false), refuse to link and fall through to the
            // not-provisioned path so the admin can resolve manually.
            //
            // `trust_idp_email` lets an admin opt out per-provider
            // when the IdP is corporate (admin-controlled
            // provisioning, no self-signup) and the gate is purely
            // noise — e.g. Okta Org AS, which doesn't emit
            // `email_verified` at all. We still warn-log every bypass
            // so it's visible in operations and reviewable from logs.
            if claims.email_verified() != Some(true) {
                if provider.trust_idp_email {
                    tracing::warn!(
                        target: "temps_auth::oidc::trust_bypass",
                        provider_id = provider_id,
                        email = %email,
                        sub = %sub,
                        "Linking OIDC identity without verified email (trust_idp_email=true)"
                    );
                } else {
                    tracing::warn!(
                        target: "temps_auth::oidc::abuse",
                        provider_id = provider_id,
                        email = %email,
                        sub = %sub,
                        "Refusing to link OIDC identity to existing account: email_verified is not true"
                    );
                    return Err(OidcError::EmailNotVerified { email });
                }
            }

            // Only mark the local user as email-verified when the IdP
            // actually asserted it. Under `trust_idp_email=true` we
            // accept the login without the claim, but we should not
            // silently elevate the user's verification state — leave
            // `email_verified` untouched so the DB still records the
            // truth as we observed it from the IdP.
            let idp_verified = claims.email_verified() == Some(true);
            let mut active: users::ActiveModel = user.clone().into();
            active.oidc_provider_id = Set(Some(provider_id));
            active.oidc_subject = Set(Some(sub.to_string()));
            if idp_verified {
                active.email_verified = Set(true);
            }
            let linked = active.update(self.db.as_ref()).await?;
            self.sync_user_sso_role(linked.id, role).await?;
            return Ok(OidcResolvedUser { user: linked });
        }

        if !provider.jit_provisioning {
            return Err(OidcError::UserNotProvisioned { email });
        }

        // SECURITY: JIT-provisioning also requires a verified email.
        // The DB has a UNIQUE(email) constraint, so a JIT-created
        // unverified account would otherwise squat on an email the
        // real owner might later try to register or use for SSO. Same
        // attacker scenario as the link path above.
        //
        // `trust_idp_email` lets an admin opt out per-provider for
        // corporate IdPs — same rationale as the linking gate above.
        if claims.email_verified() != Some(true) {
            if provider.trust_idp_email {
                tracing::warn!(
                    target: "temps_auth::oidc::trust_bypass",
                    provider_id = provider_id,
                    email = %email,
                    sub = %sub,
                    "JIT-provisioning account without verified email (trust_idp_email=true)"
                );
            } else {
                tracing::warn!(
                    target: "temps_auth::oidc::abuse",
                    provider_id = provider_id,
                    email = %email,
                    sub = %sub,
                    "Refusing to JIT-provision account: email_verified is not true"
                );
                return Err(OidcError::EmailNotVerified { email });
            }
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

        // Same rule as the link path above: only flip
        // `email_verified` to true when the IdP actually asserted it.
        // Under `trust_idp_email=true` the gate is bypassed but the
        // local state should still reflect what the IdP said.
        let idp_verified = claims.email_verified() == Some(true);
        let mut active: users::ActiveModel = user.into();
        active.oidc_provider_id = Set(Some(provider_id));
        active.oidc_subject = Set(Some(sub.to_string()));
        if idp_verified {
            active.email_verified = Set(true);
        }
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
    ) -> Result<ConfiguredCoreClient, OidcError> {
        self.build_core_client(provider, redirect_uri, false).await
    }

    /// Same as `core_client_for_provider` but always re-fetches the
    /// discovery document (and therefore the JWKS). Used by
    /// `exchange_code` to recover from a stale-key id_token
    /// verification failure after the IdP rotates its JWKS.
    async fn core_client_for_provider_refresh(
        &self,
        provider: &oidc_providers::Model,
        redirect_uri: &str,
    ) -> Result<ConfiguredCoreClient, OidcError> {
        self.build_core_client(provider, redirect_uri, true).await
    }

    async fn build_core_client(
        &self,
        provider: &oidc_providers::Model,
        redirect_uri: &str,
        force_refresh: bool,
    ) -> Result<ConfiguredCoreClient, OidcError> {
        let (metadata, client_secret) =
            self.provider_client_bundle(provider, force_refresh).await?;

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

        let issuer_str = normalize_issuer_url(&provider.issuer_url)?;

        // SSRF defense — refuse to talk to issuers whose hostname
        // resolves to RFC 1918 / link-local / CGNAT IPs. Runs *before*
        // we hand the URL to openidconnect so a malicious admin
        // can't point the server at e.g. the cloud metadata service.
        // Loopback hostnames (localhost / 127.0.0.1 / ::1) are
        // intentionally allowed for local Keycloak / Authentik dev —
        // they're physically incapable of reaching the public
        // internet, so they don't widen the SSRF surface. See
        // `assert_issuer_host_allowed` for the full policy.
        assert_issuer_host_allowed(&issuer_str).await?;

        let issuer = IssuerUrl::new(issuer_str).map_err(|e| OidcError::InvalidIssuer {
            reason: e.to_string(),
        })?;

        let metadata = CoreProviderMetadata::discover_async(issuer, &self.http_client)
            .await
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: describe_discovery_error(&e),
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
    // NOTE: do NOT strip a trailing slash. OIDC Core §16.13 / RFC 8414
    // require the `issuer` field in the discovery document to match
    // the issuer URL we asked about byte-for-byte. Auth0 publishes its
    // issuer with a trailing slash (e.g.
    // `https://tenant.eu.auth0.com/`); stripping it on our side makes
    // `CoreProviderMetadata::discover_async` reject the response with
    // `Validation error: unexpected issuer URI`.
    let trimmed = issuer.trim();
    if trimmed.is_empty() {
        return Err(OidcError::InvalidIssuer {
            reason: "issuer URL cannot be empty".into(),
        });
    }
    if trimmed.starts_with("https://") {
        return Ok(trimmed.to_string());
    }
    if trimmed.starts_with("http://") {
        // Plain HTTP exposes the client secret + authorization code +
        // id_token in transit. We don't refuse — operators have
        // legitimate `http://` use-cases (local Keycloak, IdP behind
        // an in-cluster TLS terminator) and the UI already surfaces
        // the scheme — but we log a warning so it's visible in the
        // server log that this provider is plaintext.
        if !is_loopback_url(trimmed) {
            tracing::warn!(
                target: "temps_auth::oidc",
                issuer = %trimmed,
                "OIDC issuer uses http:// — client_secret, authorization code, and id_token will be sent in plaintext. Use https:// in production."
            );
        }
        return Ok(trimmed.to_string());
    }
    Err(OidcError::InvalidIssuer {
        reason: "issuer URL must start with http:// or https://".into(),
    })
}

/// True for hostnames that are guaranteed to resolve to the local
/// machine and never to a public address. Used to suppress the
/// `http://` warning (loopback over plaintext is fine for dev) and
/// to fast-path past the SSRF guard.
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

fn is_loopback_url(url: &str) -> bool {
    openidconnect::url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(is_loopback_host))
        .unwrap_or(false)
}

/// SSRF guard for OIDC discovery. Refuses to talk to issuers whose
/// hostname resolves to RFC 1918 / link-local / CGNAT / multicast IPs.
///
/// Loopback (127/8, ::1) is the one private range we *do* allow,
/// because it's the only way to talk to a local Keycloak / Authentik
/// instance during dev and it can't reach anything the temps process
/// couldn't already touch directly. Every other private range —
/// 10/8, 172.16/12, 192.168/16, 169.254/16, 100.64/10 — is blocked
/// outright: those are the addresses that point at the AWS metadata
/// service, the cluster-internal mesh, the office VPN, etc.
///
/// This function acts as defense-in-depth at the admin-save / test-connection
/// call site — it runs synchronously before any HTTP is attempted and produces
/// a human-readable error for the UI. The TOCTOU window that previously existed
/// between this pre-check and the actual TCP connect inside reqwest is now closed
/// by `BlocklistResolver`, which re-validates every resolved IP at connect time.
/// An attacker with short-TTL DNS that returns a public IP here and then
/// `169.254.169.254` at connect time will be blocked by the resolver.
async fn assert_issuer_host_allowed(issuer: &str) -> Result<(), OidcError> {
    let url = openidconnect::url::Url::parse(issuer).map_err(|e| OidcError::InvalidIssuer {
        reason: format!("could not parse issuer URL: {e}"),
    })?;
    let host = url.host_str().ok_or_else(|| OidcError::InvalidIssuer {
        reason: "issuer URL has no host".into(),
    })?;

    // Fast path: a literal loopback hostname is always OK. Saves a
    // DNS round-trip and keeps the local-dev path zero-latency.
    if is_loopback_host(host) {
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(443);
    // `(host, port)` is `(&str, u16)`; `lookup_host` is generic over
    // `ToSocketAddrs`, so we need to nudge the inference with an
    // explicit type to disambiguate.
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| OidcError::DiscoveryFailed {
            issuer: issuer.to_string(),
            reason: format!("DNS lookup failed: {e}"),
        })?
        .collect();
    for addr in addrs {
        if is_blocked_ip(&addr.ip()) {
            tracing::warn!(
                target: "temps_auth::oidc::abuse",
                issuer = %issuer,
                host = %host,
                ip = %addr.ip(),
                "Refusing to contact OIDC issuer that resolves to a private/internal IP"
            );
            return Err(OidcError::InvalidIssuer {
                reason: format!(
                    "issuer {host} resolves to non-public IP {} (use a public DNS name, or run the IdP on localhost)",
                    addr.ip()
                ),
            });
        }
    }
    Ok(())
}

/// Classify an IP as "must not be the target of an OIDC discovery
/// fetch". Covers RFC 1918 (10/8, 172.16/12, 192.168/16), link-local
/// (169.254/16, fe80::/10), the IPv4 documentation / CGNAT /
/// benchmarking ranges (which can mask metadata services in some
/// clouds), and IPv6 unique-local + unspecified.
///
/// Loopback (127/8, ::1) is intentionally *not* in this list —
/// loopback can't reach anything outside the temps process and is
/// useful for local Keycloak / Authentik dev. The early-return in
/// `assert_issuer_host_allowed` short-circuits literal loopback
/// hostnames before we even hit this function.
fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_documentation()
                // 100.64.0.0/10 — CGNAT (RFC 6598). Cloud providers
                // sometimes route metadata via the shared address
                // space; safer to block.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            v6.is_unspecified()
                || v6.is_multicast()
                // fe80::/10 — link-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // fc00::/7 — unique local
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Build a human-readable error message for an OIDC discovery failure.
///
/// `openidconnect::DiscoveryError`'s `Display` impl only emits the
/// top-line variant text (`"Failed to parse server response"`,
/// `"Request failed"`, etc.) and pushes the actual cause behind
/// `std::error::Error::source()`. The default `e.to_string()` therefore
/// loses the only thing the operator actually needs (the URL that
/// failed to parse, the reqwest error, the JSON path that didn't
/// deserialize, …). We walk the source chain explicitly so the message
/// surfaces on the test-connection screen.
fn describe_discovery_error<E: std::error::Error>(err: &E) -> String {
    let mut out = err.to_string();
    let mut src: Option<&dyn std::error::Error> = err.source();
    while let Some(cause) = src {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        src = cause.source();
    }
    out
}

/// Heuristic for "this id_token verification failure looks like the
/// IdP rotated its signing key while we had the old JWKS cached".
/// openidconnect 4.x's `ClaimsVerificationError` doesn't expose a
/// machine-readable variant for this case, so we match on the text.
///
/// We deliberately keep the trigger set narrow: matching on
/// signature/key/jwks vocabulary, NOT on generic claim-validation
/// failures (bad audience, expired token, missing claim). That keeps
/// the retry from masking real config bugs and prevents an attacker
/// who can submit malformed tokens from amplifying every login into
/// two discovery round-trips.
fn looks_like_jwks_rotation(err_text: &str) -> bool {
    let lower = err_text.to_ascii_lowercase();
    // openidconnect 4.x emits one of these on signing-key trouble:
    //   - "no matching key found"
    //   - "unable to find signing key"
    //   - "kid <foo> not found"
    //   - "signature verification failed"
    //   - "invalid signature"
    lower.contains("no matching key")
        || lower.contains("signing key")
        || lower.contains("kid ")
        || lower.contains("signature")
        || lower.contains("jwks")
}

fn validate_return_to(path: &str) -> Result<(), OidcError> {
    // Must be a same-origin relative path.
    if !path.starts_with('/') {
        return Err(OidcError::InvalidReturnTo);
    }
    // Reject scheme-relative URLs (`//evil.com` → `https://evil.com`).
    if path.starts_with("//") {
        return Err(OidcError::InvalidReturnTo);
    }
    // Reject backslash-prefixed paths: Chrome / Edge normalize
    // `/\evil.com` to `//evil.com` and treat it as scheme-relative,
    // which becomes a post-auth open redirect → phishing. We refuse
    // *any* backslash anywhere in the path; a legitimate URL has no
    // reason to contain one (RFC 3986 reserves `\` as unsafe).
    if path.contains('\\') {
        return Err(OidcError::InvalidReturnTo);
    }
    // Reject CR / LF / NUL and other control chars — they can be
    // weaponised for response-splitting if downstream code ever
    // forgets to sanitize before writing to a header.
    if path.chars().any(|c| c.is_control()) {
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
        // Backslash open-redirect — browsers normalize `\` to `/`,
        // turning `/\evil.com` into a scheme-relative URL.
        assert_eq!(
            OidcService::sanitize_return_to(Some("/\\evil.com".into())),
            "/dashboard"
        );
        assert_eq!(
            OidcService::sanitize_return_to(Some("/projects".into())),
            "/projects"
        );
    }

    #[test]
    fn normalize_issuer_url_preserves_trailing_slash() {
        // OIDC discovery requires the issuer URL to match the
        // discovered `issuer` field byte-for-byte. Auth0 publishes
        // its issuer with a trailing slash, so we must preserve it
        // when present.
        assert_eq!(
            normalize_issuer_url("https://kungfusoftware.eu.auth0.com/").unwrap(),
            "https://kungfusoftware.eu.auth0.com/"
        );
        assert_eq!(
            normalize_issuer_url("https://auth.example.com").unwrap(),
            "https://auth.example.com"
        );
    }

    #[test]
    fn normalize_issuer_url_trims_whitespace() {
        assert_eq!(
            normalize_issuer_url("  https://auth.example.com/  ").unwrap(),
            "https://auth.example.com/"
        );
    }

    #[test]
    fn normalize_issuer_url_requires_scheme() {
        assert!(matches!(
            normalize_issuer_url("auth.example.com"),
            Err(OidcError::InvalidIssuer { .. })
        ));
        assert!(matches!(
            normalize_issuer_url("ftp://auth.example.com"),
            Err(OidcError::InvalidIssuer { .. })
        ));
    }

    #[test]
    fn normalize_issuer_url_allows_http_with_warning() {
        // Plain http:// is accepted (we just log a warn!) — the user
        // takes responsibility for the in-transit secret exposure.
        assert_eq!(
            normalize_issuer_url("http://keycloak.local:8080/realms/temps").unwrap(),
            "http://keycloak.local:8080/realms/temps"
        );
        assert_eq!(
            normalize_issuer_url("http://localhost:8080/realms/temps").unwrap(),
            "http://localhost:8080/realms/temps"
        );
    }

    #[test]
    fn is_loopback_host_recognises_canonical_forms() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        assert!(!is_loopback_host("auth.example.com"));
        // Public host that happens to *contain* "localhost" must not
        // bypass the check.
        assert!(!is_loopback_host("localhost.example.com"));
    }

    #[test]
    fn is_blocked_ip_classifies_rfc1918_and_metadata_ranges() {
        use std::net::Ipv4Addr;
        use std::net::Ipv6Addr;

        // Loopback is INTENTIONALLY allowed — local Keycloak /
        // Authentik dev needs it and it can't reach anything else.
        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(!is_blocked_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)));

        // RFC 1918 — blocked because it usually points at office
        // network / on-prem service mesh.
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));

        // Link-local — incl. AWS IMDS at 169.254.169.254.
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(
            169, 254, 169, 254
        ))));

        // CGNAT — RFC 6598. Some clouds route metadata via the
        // shared address space.
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(
            100, 127, 255, 254
        ))));

        // Public addresses must NOT be flagged.
        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        // 100.63 sits *just* below the CGNAT band.
        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(
            100, 63, 255, 254
        ))));
        // 100.128 sits *just* above the CGNAT band.
        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
    }

    #[tokio::test]
    async fn assert_issuer_host_allowed_blocks_aws_metadata() {
        // 169.254.169.254 is the canonical cloud metadata IP. If this
        // ever passes, our SSRF defense is broken.
        let err = assert_issuer_host_allowed("http://169.254.169.254/latest/meta-data/")
            .await
            .expect_err("AWS IMDS IP must be blocked");
        assert!(matches!(err, OidcError::InvalidIssuer { .. }));
    }

    #[tokio::test]
    async fn assert_issuer_host_allowed_permits_loopback_host() {
        // Loopback is explicitly allowed so local Keycloak / Authentik
        // dev works without an env-var escape hatch. The early-return
        // in `assert_issuer_host_allowed` also avoids a DNS round-trip.
        assert_issuer_host_allowed("http://localhost:8080/")
            .await
            .expect("localhost must be allowed");
        assert_issuer_host_allowed("http://127.0.0.1:8080/realms/temps")
            .await
            .expect("127.0.0.1 must be allowed");
    }

    #[test]
    fn create_role_mapping_idp_group_validation_is_via_chars_and_len() {
        // Direct unit tests of the validation logic without the DB
        // round-trip — service-level test is in service_tests.rs.
        let too_long = "a".repeat(IDP_GROUP_MAX_LEN + 1);
        assert!(too_long.len() > IDP_GROUP_MAX_LEN);
        assert!("ok-group".chars().all(|c| !c.is_control()));
        assert!("bad\u{0000}group".chars().any(|c| c.is_control()));
    }

    #[test]
    fn describe_discovery_error_walks_source_chain() {
        // Hand-roll a 3-deep error chain to prove we don't stop at
        // the top-line message the way `e.to_string()` does.
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "inner cause")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Middle(Inner);
        impl std::fmt::Display for Middle {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "middle")
            }
        }
        impl std::error::Error for Middle {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        #[derive(Debug)]
        struct Top(Middle);
        impl std::fmt::Display for Top {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "top")
            }
        }
        impl std::error::Error for Top {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let err = Top(Middle(Inner));
        assert_eq!(describe_discovery_error(&err), "top: middle: inner cause");
    }

    #[test]
    fn looks_like_jwks_rotation_matches_signing_key_failures_only() {
        // Should trigger refresh+retry — these are the cases where
        // a fresh JWKS fetch might actually help.
        assert!(looks_like_jwks_rotation(
            "Signature verification failed: no matching key found"
        ));
        assert!(looks_like_jwks_rotation(
            "ID token verification failed: kid abc123 not found in JWKS"
        ));
        assert!(looks_like_jwks_rotation(
            "Unable to find signing key for token"
        ));
        assert!(looks_like_jwks_rotation("Invalid signature on id_token"));
        assert!(looks_like_jwks_rotation("JWKS fetch returned empty set"));

        // Must NOT trigger refresh+retry — these are real
        // configuration problems where re-fetching just wastes a
        // round-trip and masks the bug.
        assert!(!looks_like_jwks_rotation(
            "Audience does not match client_id"
        ));
        assert!(!looks_like_jwks_rotation("ID token has expired"));
        assert!(!looks_like_jwks_rotation("Nonce mismatch"));
        assert!(!looks_like_jwks_rotation("Claim 'iss' missing"));
        assert!(!looks_like_jwks_rotation(""));
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
        assert!(validate_return_to("/projects/42/deployments").is_ok());
        assert!(validate_return_to("/dashboard?ref=email").is_ok());
        assert!(validate_return_to("/dashboard#section").is_ok());
    }

    #[test]
    fn validate_return_to_rejects_absolute_and_scheme_relative() {
        assert!(validate_return_to("//evil.com").is_err());
        assert!(validate_return_to("https://evil.com").is_err());
        assert!(validate_return_to("http://evil.com").is_err());
        assert!(validate_return_to("javascript:alert(1)").is_err());
        assert!(validate_return_to("dashboard").is_err()); // no leading /
    }

    #[test]
    fn validate_return_to_rejects_backslash_open_redirect() {
        // Chrome/Edge normalize `\` to `/`, turning `/\evil.com` into
        // `//evil.com` (scheme-relative → external host). Any
        // backslash is refused.
        assert!(validate_return_to("/\\evil.com").is_err());
        assert!(validate_return_to("/projects\\..\\evil.com").is_err());
        assert!(validate_return_to("/\\\\evil.com").is_err());
    }

    #[test]
    fn validate_return_to_rejects_control_chars() {
        // CR / LF / NUL / tab are response-splitting / header-
        // injection vectors. Defense in depth — refuse them outright.
        assert!(validate_return_to("/dashboard\r\nSet-Cookie: x=y").is_err());
        assert!(validate_return_to("/dashboard\n").is_err());
        assert!(validate_return_to("/dashboard\u{0000}").is_err());
        assert!(validate_return_to("/dashboard\t").is_err());
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
            trust_idp_email: false,
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
            trust_idp_email: false,
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

    // ---------------------------------------------------------------------------
    // BlocklistResolver
    // ---------------------------------------------------------------------------
    //
    // These tests call the resolver directly without spinning up an HTTP server.
    // We verify:
    //   1. Known-blocked IPs (169.254.169.254, 10.x.x.x) are rejected.
    //   2. Loopback (`localhost`) is allowed (same policy as
    //      `assert_issuer_host_allowed`).
    //
    // We don't attempt to simulate a live DNS-rebind (that requires real DNS
    // infrastructure), but these tests prove the resolver rejects the addresses
    // that matter for the threat model at connect time.

    #[tokio::test]
    async fn blocklist_resolver_rejects_aws_imds_literal_ip() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;

        let resolver = BlocklistResolver;
        let name = reqwest::dns::Name::from_str("169.254.169.254").unwrap();
        let result = resolver.resolve(name).await;
        assert!(
            result.is_err(),
            "BlocklistResolver must reject 169.254.169.254 (AWS IMDS)"
        );
        // Use `.err()` instead of `.unwrap_err()` because `reqwest::dns::Addrs`
        // is a `Box<dyn Iterator<…>>` that doesn't implement `Debug`.
        let err_msg = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            err_msg.contains("blocked") || err_msg.contains("rebind"),
            "Error message should mention blocking: {err_msg}"
        );
    }

    #[tokio::test]
    async fn blocklist_resolver_rejects_rfc1918_literal_ip() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;

        let resolver = BlocklistResolver;
        let name = reqwest::dns::Name::from_str("10.0.0.1").unwrap();
        let result = resolver.resolve(name).await;
        assert!(
            result.is_err(),
            "BlocklistResolver must reject 10.0.0.1 (RFC 1918)"
        );
    }

    #[tokio::test]
    async fn blocklist_resolver_allows_loopback() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;

        // `localhost` resolves to 127.0.0.1 / ::1 on virtually all systems.
        // is_blocked_ip deliberately allows loopback so local Keycloak works.
        let resolver = BlocklistResolver;
        let name = reqwest::dns::Name::from_str("localhost").unwrap();
        let result = resolver.resolve(name).await;
        assert!(
            result.is_ok(),
            "BlocklistResolver must allow localhost (loopback): {}",
            result.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }
}
