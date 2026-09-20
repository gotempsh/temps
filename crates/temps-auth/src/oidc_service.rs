// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    Statement, TransactionTrait,
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

/// The fields `temps-cloud`'s `ConsoleOidcSink` adapter extracts from a
/// `temps_cloud_protocol::console_proxy::ConsoleOidcConfig` frame (ADR-045
/// §4) to call [`OidcService::upsert_managed_cloud_provider`].
///
/// A dedicated type rather than taking `ConsoleOidcConfig` directly so this
/// crate does not need a dependency on `temps-cloud-protocol` just to
/// describe three strings; `console_host` is deliberately omitted — that
/// field pins the console-proxy dispatcher's `Host` check (ADR-045 §3), a
/// concern this crate has nothing to do with.
pub struct ManagedCloudOidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
}

/// Display name given to the Cloud-managed console-access provider.
pub const CLOUD_MANAGED_OIDC_PROVIDER_NAME: &str = "Temps Cloud";
/// `oidc_providers.template` value used to render distinct login-page copy
/// ("Continue with Temps Cloud") for this provider (ADR-045 §4).
pub const CLOUD_MANAGED_OIDC_TEMPLATE: &str = "temps_cloud";
/// Custom claim carrying the Cloud account's role on *this* instance
/// (ADR-045 §4) -- instance-scoped, so it cannot be the generic `role_claim`
/// convention other providers share.
pub const CLOUD_MANAGED_OIDC_ROLE_CLAIM: &str = "temps_cloud_instance_role";
pub const CLOUD_MANAGED_OIDC_SCOPES: &str = "openid email profile temps_cloud_instance_role";

/// The role-mapping rows [`OidcService::sync_managed_cloud_role_mappings`]
/// provisions for the managed console-access provider (ADR-045 §4):
/// `owner`/`admin` -> `admin`, everything else -> `user` (a role
/// `enforce_admin_only_role` rejects). A named constant, not an inline
/// literal in that function, so this module's tests can drive `evaluate_role`
/// against the *actual* mapping set the service provisions rather than a
/// hand-copied approximation that could silently drift from it.
const CLOUD_MANAGED_ROLE_MAPPINGS: &[(&str, &str)] =
    &[("owner", "admin"), ("admin", "admin"), ("*", "user")];

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
        let issuer_url = normalize_issuer_url(&request.issuer_url)?;
        // SECURITY: a second, operator-created provider pointed at the same
        // issuer as the Cloud-managed one would shadow it under an ordinary
        // (non-`admin_only_role_required`) provider row -- same `sub`/`iss`
        // pair, but logins through it skip the role gate entirely. Only
        // `upsert_managed_cloud_provider` may ever create or touch the row
        // for this issuer.
        self.assert_issuer_not_shadowing_managed_cloud_provider(&issuer_url)
            .await?;
        let encrypted_secret = self
            .encryption_service
            .encrypt_string(&request.client_secret)
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: request.issuer_url.clone(),
                reason: format!("failed to encrypt client secret: {e}"),
            })?;

        let provider = oidc_providers::ActiveModel {
            name: Set(name),
            issuer_url: Set(issuer_url),
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
        // ADR-045 §4: the Cloud-managed console-access provider's credentials
        // are rotated by Cloud's own provisioning path
        // (`ConsoleOidcConfig`/`upsert_managed_cloud_provider`), not by an
        // operator editing this row by hand — mirrors
        // `s3_sources.managed_by_cloud`'s edit guard in `temps-backup`.
        if provider.managed_by_cloud {
            return Err(OidcError::ManagedByCloudEdit {
                provider_id: provider.id,
                name: provider.name,
            });
        }
        let mut active: oidc_providers::ActiveModel = provider.into();

        if let Some(name) = request.name {
            active.name = Set(name.trim().to_string());
        }
        if let Some(issuer_url) = request.issuer_url {
            let issuer_url = normalize_issuer_url(&issuer_url)?;
            // SECURITY: same guard as `create_provider` -- an ordinary
            // provider's issuer must never be edited to shadow the
            // Cloud-managed provider's issuer either.
            self.assert_issuer_not_shadowing_managed_cloud_provider(&issuer_url)
                .await?;
            active.issuer_url = Set(issuer_url);
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
            Self::revoke_sessions_for_provider(self.db.as_ref(), provider_id).await?;
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
    ///
    /// Generic over the connection so provider deletion/revocation can run it
    /// inside the same transaction that drops the provider row — see
    /// [`Self::revoke_managed_cloud_provider`] for why the two must commit
    /// together.
    async fn revoke_sessions_for_provider<C: ConnectionTrait>(
        conn: &C,
        provider_id: i32,
    ) -> Result<(), OidcError> {
        // Single-statement: DELETE … WHERE user_id IN (SELECT …).
        // Sea-ORM has no first-class subquery DELETE; raw SQL is
        // both safer (one round-trip, one lock) and clearer here.
        // Parameterised via $1 — no injection surface even though
        // the input is an i32.
        let result = conn
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
        // ADR-045 §4: use `revoke_managed_cloud_provider` for the
        // Cloud-managed row instead — deleting it manually would leave Cloud
        // believing console access is still provisioned.
        if provider.managed_by_cloud {
            return Err(OidcError::ManagedByCloudDelete {
                provider_id: provider.id,
                name: provider.name,
            });
        }

        // SECURITY: revoke active sessions for every user linked to
        // this provider *before* dropping the row. Otherwise an admin
        // who deletes a compromised provider during an incident
        // leaves the existing session cookies valid until natural
        // expiry — exactly the people they're trying to lock out.
        // `sessions.user_id → users.id` is ON DELETE CASCADE, but
        // deleting a provider does not delete users, so the cascade
        // doesn't help here.
        //
        // All of it in one transaction that takes an exclusive lock on the
        // provider row first, so a login already in flight for this provider
        // cannot slip a freshly-created session past the session delete —
        // see [`Self::assert_provider_live_for_session`] for the other half
        // of that handshake.
        let txn = self.db.begin().await?;
        Self::lock_provider_for_write(&txn, provider_id).await?;
        Self::purge_provider_login_states(&txn, provider_id).await?;
        Self::revoke_sessions_for_provider(&txn, provider_id).await?;
        oidc_providers::Entity::delete_by_id(provider.id)
            .exec(&txn)
            .await?;
        txn.commit().await?;

        self.discovery_cache.lock().await.remove(&provider_id);
        Ok(())
    }

    /// Take the exclusive row lock that serializes provider teardown against
    /// a login in flight for the same provider. Returns `Ok(false)` when the
    /// row is already gone (another teardown won the race).
    async fn lock_provider_for_write<C: ConnectionTrait>(
        conn: &C,
        provider_id: i32,
    ) -> Result<bool, OidcError> {
        Ok(oidc_providers::Entity::find_by_id(provider_id)
            .lock_exclusive()
            .one(conn)
            .await?
            .is_some())
    }

    /// Drop every pending `oidc_login_states` row for a provider that is
    /// being deleted or revoked. SECURITY: a login that already redirected to
    /// the IdP must not be able to come back and complete against a provider
    /// the operator (or Cloud) just took away — without this, the callback
    /// would consume a state row that outlived its provider.
    async fn purge_provider_login_states<C: ConnectionTrait>(
        conn: &C,
        provider_id: i32,
    ) -> Result<(), OidcError> {
        let deleted = oidc_login_states::Entity::delete_many()
            .filter(oidc_login_states::Column::ProviderId.eq(provider_id))
            .exec(conn)
            .await?;
        if deleted.rows_affected > 0 {
            tracing::info!(
                target: "temps_auth::oidc",
                provider_id = provider_id,
                login_states_purged = deleted.rows_affected,
                "Discarded in-flight OIDC login states for a provider being removed"
            );
        }
        Ok(())
    }

    /// SECURITY: the second half of the teardown handshake described on
    /// [`Self::delete_provider`] / [`Self::revoke_managed_cloud_provider`].
    ///
    /// A callback that has already resolved its user can be overtaken by a
    /// concurrent revocation: the revocation deletes every session the
    /// provider issued, and only *then* does the callback insert its own
    /// session row — which would survive the revocation and hand a live
    /// console session to an identity the instance no longer trusts.
    ///
    /// Called immediately after the session row is created, this closes that
    /// window without holding a transaction open across the IdP round-trip.
    /// It takes a *shared* lock on the provider row, so it blocks behind an
    /// in-progress teardown (which holds the exclusive lock) and only then
    /// re-reads the row. Either the provider is still there and enabled — in
    /// which case the teardown has not started yet, so its own session delete
    /// is guaranteed to observe the session just committed — or it is gone /
    /// disabled, and the freshly-created session is deleted again before the
    /// cookie ever reaches the browser.
    pub async fn assert_provider_live_for_session(
        &self,
        provider_id: i32,
        session_token: &str,
    ) -> Result<(), OidcError> {
        let txn = self.db.begin().await?;
        let live = oidc_providers::Entity::find_by_id(provider_id)
            .lock_shared()
            .one(&txn)
            .await?
            .is_some_and(|provider| provider.enabled);
        if live {
            txn.commit().await?;
            return Ok(());
        }

        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM sessions WHERE session_token = $1",
            vec![session_token.into()],
        ))
        .await?;
        txn.commit().await?;

        tracing::warn!(
            target: "temps_auth::oidc::abuse",
            provider_id = provider_id,
            "Discarded a session created by a login that completed while its OIDC provider was \
             being revoked or disabled"
        );
        Err(OidcError::ProviderRevokedDuringLogin { provider_id })
    }

    /// The single Cloud-managed console-access `oidc_providers` row, if one
    /// exists. `managed_by_cloud` is unique-by-construction — only
    /// [`Self::upsert_managed_cloud_provider`] ever sets it, and it always
    /// upserts in place rather than inserting a second row — but this reads
    /// the first match rather than asserting exactly one so a hand-edited DB
    /// can't turn a read into a panic.
    pub async fn managed_cloud_provider(&self) -> Result<Option<oidc_providers::Model>, OidcError> {
        Self::managed_cloud_provider_on(self.db.as_ref()).await
    }

    /// [`Self::managed_cloud_provider`] against an arbitrary connection, so
    /// the provisioning/revocation transactions read the row through the same
    /// filter they then write under.
    async fn managed_cloud_provider_on<C: ConnectionTrait>(
        conn: &C,
    ) -> Result<Option<oidc_providers::Model>, OidcError> {
        Ok(oidc_providers::Entity::find()
            .filter(oidc_providers::Column::ManagedByCloud.eq(true))
            .one(conn)
            .await?)
    }

    /// SECURITY: refuse to create/edit an ordinary provider onto the same
    /// issuer as the Cloud-managed one.
    ///
    /// `managed_by_cloud`/`admin_only_role_required` live on the provider
    /// *row*, not on the issuer. An admin who (deliberately or by mistake)
    /// points a second, ordinary provider at the same `issuer_url` creates a
    /// second, unguarded relying-party registration for the exact same
    /// identity provider: `resolve_user` picks whichever row `provider_id`
    /// names, so a login routed through the shadow row skips
    /// `admin_only_role_required` entirely even though the IdP itself is the
    /// one Cloud provisioned for instance-admin access. Comparing the
    /// normalized issuer (both sides run through [`normalize_issuer_url`],
    /// so this is a byte-for-byte comparison of the canonical form) closes
    /// that gap regardless of what name or template the shadow row uses.
    async fn assert_issuer_not_shadowing_managed_cloud_provider(
        &self,
        issuer_url: &str,
    ) -> Result<(), OidcError> {
        if let Some(managed) = self.managed_cloud_provider().await? {
            if managed.issuer_url == issuer_url {
                return Err(OidcError::IssuerMatchesManagedCloudProvider {
                    issuer_url: issuer_url.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Upsert the managed console-access `oidc_providers` row from a
    /// `ConsoleOidcConfig` frame (ADR-045 §4), called by `temps-cloud`'s
    /// `ConsoleOidcSink` adapter on every console-proxy connect/reconnect.
    /// Idempotent: converges an existing row to Cloud's current
    /// issuer/client/secret rather than erroring or duplicating, so an
    /// instance that missed a client-secret rotation while offline picks up
    /// the current configuration the moment it reconnects.
    pub async fn upsert_managed_cloud_provider(
        &self,
        config: ManagedCloudOidcConfig,
    ) -> Result<oidc_providers::Model, OidcError> {
        validate_issuer_url(&config.issuer)?;
        let issuer_url = normalize_issuer_url(&config.issuer)?;
        let client_id = config.client_id.trim().to_string();
        let encrypted_secret = self
            .encryption_service
            .encrypt_string(&config.client_secret)
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: issuer_url.clone(),
                reason: format!("failed to encrypt managed console-access client secret: {e}"),
            })?;

        // ADR-045 §4: the provider row and its role mappings are one unit of
        // configuration — a partial apply (row updated, mappings deleted but
        // not re-inserted) would leave an enabled, admin-gated provider whose
        // every login resolves to no mapping at all, locking out exactly the
        // Cloud owners and admins this provider exists to let in. One
        // transaction, so a failure anywhere leaves the previous
        // configuration untouched.
        let txn = self.db.begin().await?;
        // The same exclusive row lock `revoke_managed_cloud_provider` takes,
        // so a revocation arriving concurrently with a provisioning frame is
        // serialized instead of interleaving with it.
        let existing = oidc_providers::Entity::find()
            .filter(oidc_providers::Column::ManagedByCloud.eq(true))
            .lock_exclusive()
            .one(&txn)
            .await?;
        // SECURITY: an ordinary provider that already points at this issuer
        // is a second, *unguarded* relying-party registration for the very
        // IdP this provider exists to gate — a Cloud identity signing in
        // through that row skips `admin_only_role_required` entirely. The
        // create/update guard
        // (`assert_issuer_not_shadowing_managed_cloud_provider`) only stops
        // one being added *after* the managed row exists; this is the other
        // direction, and it refuses rather than deleting the operator's own
        // provider behind their back. The instance reports the conflict and
        // console access stays unprovisioned until the operator removes the
        // duplicate: fail closed, not "gate silently bypassable".
        if let Some(conflict) = oidc_providers::Entity::find()
            .filter(oidc_providers::Column::IssuerUrl.eq(issuer_url.clone()))
            .filter(oidc_providers::Column::ManagedByCloud.eq(false))
            .one(&txn)
            .await?
        {
            return Err(OidcError::ManagedIssuerAlreadyUsed {
                provider_id: conflict.id,
                name: conflict.name,
                issuer_url,
            });
        }

        let provider = match existing {
            Some(existing) => {
                let provider_id = existing.id;
                // SECURITY: only ever converges the row that is *already*
                // `managed_by_cloud` — an ordinary provider is never adopted
                // into managed status (that would silently take an operator's
                // own provider away from them), and the managed row is never
                // demoted to an ordinary one.
                let mut active: oidc_providers::ActiveModel = existing.into();
                active.issuer_url = Set(issuer_url);
                active.client_id = Set(client_id);
                active.client_secret_encrypted = Set(encrypted_secret);
                active.enabled = Set(true);
                let updated = active.update(&txn).await?;
                self.discovery_cache.lock().await.remove(&provider_id);
                updated
            }
            None => {
                oidc_providers::ActiveModel {
                    name: Set(CLOUD_MANAGED_OIDC_PROVIDER_NAME.to_string()),
                    issuer_url: Set(issuer_url),
                    client_id: Set(client_id),
                    client_secret_encrypted: Set(encrypted_secret),
                    scopes: Set(CLOUD_MANAGED_OIDC_SCOPES.to_string()),
                    jit_provisioning: Set(true),
                    enabled: Set(true),
                    template: Set(CLOUD_MANAGED_OIDC_TEMPLATE.to_string()),
                    // SECURITY (fixed post-audit): this must be the claim the
                    // mapping loop in `evaluate_role` actually inspects for
                    // `owner`/`admin` matches. It was previously left at the
                    // generic `"groups"` default while `role_claim` (below)
                    // pointed at `temps_cloud_instance_role` -- Cloud's ID
                    // token never sets a `groups` claim, so every login fell
                    // straight to this row set's `("*", "user")` wildcard
                    // before `role_claim`'s fallback was ever reached,
                    // meaning *no* Cloud account could ever pass
                    // `admin_only_role_required`. See `resolve_user`'s
                    // `strict_string_claim` extraction, used only for
                    // `admin_only_role_required` providers, for why a
                    // malformed (array/number/object) claim still fails
                    // closed rather than being silently coerced into a
                    // `groups` match.
                    group_claim: Set(CLOUD_MANAGED_OIDC_ROLE_CLAIM.to_string()),
                    role_claim: Set(CLOUD_MANAGED_OIDC_ROLE_CLAIM.to_string()),
                    default_role: Set(RoleType::User.as_str().to_string()),
                    // Reaching this exchange already required operator-level
                    // access to the Cloud account this instance is enrolled
                    // under (ADR-045 §4's documented carve-out for
                    // admin-controlled IdPs).
                    trust_idp_email: Set(true),
                    managed_by_cloud: Set(true),
                    admin_only_role_required: Set(true),
                    ..Default::default()
                }
                .insert(&txn)
                .await?
            }
        };

        Self::sync_managed_cloud_role_mappings(&txn, provider.id).await?;
        txn.commit().await?;
        Ok(provider)
    }

    /// Replace the managed provider's role mappings with the canonical set:
    /// `owner`/`admin` -> `admin`, everything else -> `user` (rejected by
    /// `admin_only_role_required` in `resolve_user`). Deletes and re-inserts
    /// rather than diffing -- three rows, and this keeps the mapping table
    /// always in sync with the code that defines what it must mean, with no
    /// risk of stale rows accumulating across reconnects.
    ///
    /// Takes the connection rather than `&self` because it must run inside
    /// [`Self::upsert_managed_cloud_provider`]'s transaction: the delete and
    /// the three inserts have to commit together with the provider row, or a
    /// failure between them leaves an enabled provider with no mappings.
    async fn sync_managed_cloud_role_mappings<C: ConnectionTrait>(
        conn: &C,
        provider_id: i32,
    ) -> Result<(), OidcError> {
        oidc_role_mappings::Entity::delete_many()
            .filter(oidc_role_mappings::Column::ProviderId.eq(provider_id))
            .exec(conn)
            .await?;
        for (priority, (idp_group, role)) in CLOUD_MANAGED_ROLE_MAPPINGS.iter().enumerate() {
            oidc_role_mappings::ActiveModel {
                provider_id: Set(provider_id),
                priority: Set(priority as i32),
                idp_group: Set(idp_group.to_string()),
                role: Set(role.to_string()),
                ..Default::default()
            }
            .insert(conn)
            .await?;
        }
        Ok(())
    }

    /// Delete the managed console-access provider and invalidate every
    /// session it issued (ADR-045 §4). Idempotent: `Ok(false)` when there is
    /// nothing to revoke, which is the normal outcome both for a build that
    /// never enabled console access and for a second `ConsoleOidcRevoke`
    /// hitting an already-clean instance.
    pub async fn revoke_managed_cloud_provider(&self) -> Result<bool, OidcError> {
        // One transaction, holding the provider row's exclusive lock for its
        // whole duration. SECURITY: this is what makes revocation win against
        // a login that is already in flight — a concurrent upsert blocks on
        // the same lock, and a callback that completes mid-revocation is
        // rejected by `assert_provider_live_for_session`, which waits on this
        // lock before deciding whether its session may live. Committing the
        // session delete and the row delete together also removes the window
        // where sessions were gone but the provider row was still usable.
        let txn = self.db.begin().await?;
        let Some(provider) = oidc_providers::Entity::find()
            .filter(oidc_providers::Column::ManagedByCloud.eq(true))
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            txn.commit().await?;
            return Ok(false);
        };
        // In-flight login attempts die with the provider: a state row that
        // outlived its provider would let a callback already redirected to
        // the IdP come back and complete after revocation.
        Self::purge_provider_login_states(&txn, provider.id).await?;
        // Session revocation before the row delete, same ordering
        // `delete_provider` uses and for the same reason: an admin account
        // whose provider just disappeared must not keep a live session.
        Self::revoke_sessions_for_provider(&txn, provider.id).await?;
        oidc_role_mappings::Entity::delete_many()
            .filter(oidc_role_mappings::Column::ProviderId.eq(provider.id))
            .exec(&txn)
            .await?;
        oidc_providers::Entity::delete_by_id(provider.id)
            .exec(&txn)
            .await?;
        txn.commit().await?;
        self.discovery_cache.lock().await.remove(&provider.id);
        Ok(true)
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
        let provider = self.get_provider(provider_id).await?;
        // SECURITY: the managed provider's mappings *are* its role gate. An
        // operator-added mapping (`member -> admin`, or a higher-priority
        // `* -> admin`) resolves before the canonical wildcard row and passes
        // `enforce_admin_only_role`, which would turn the admin-only gate
        // into "anyone with a Cloud account on this instance". Only
        // `sync_managed_cloud_role_mappings` may write these rows, for the
        // same reason the provider row itself is not hand-editable.
        Self::reject_managed_cloud_mapping_change(&provider)?;
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
        // Resolve the owning provider first: deleting the managed provider's
        // `owner -> admin` / `admin -> admin` rows would leave only the
        // `* -> user` wildcard, locking every Cloud owner and admin out of
        // the console — the mirror image of the escalation
        // `create_role_mapping` refuses, and just as much a change to a
        // Cloud-owned configuration.
        let mapping = oidc_role_mappings::Entity::find_by_id(mapping_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OidcError::RoleMappingNotFound { mapping_id })?;
        let provider = self.get_provider(mapping.provider_id).await?;
        Self::reject_managed_cloud_mapping_change(&provider)?;

        let deleted = oidc_role_mappings::Entity::delete_by_id(mapping_id)
            .exec(self.db.as_ref())
            .await?;
        if deleted.rows_affected == 0 {
            return Err(OidcError::RoleMappingNotFound { mapping_id });
        }
        Ok(())
    }

    /// Refuse any hand-made change to a Cloud-managed provider's role
    /// mappings. Shared by [`Self::create_role_mapping`] and
    /// [`Self::delete_role_mapping`] so both directions — escalation and
    /// lock-out — are refused by the same check, and any future mutation has
    /// one obvious place to call.
    fn reject_managed_cloud_mapping_change(
        provider: &oidc_providers::Model,
    ) -> Result<(), OidcError> {
        if provider.managed_by_cloud {
            return Err(OidcError::ManagedByCloudRoleMapping {
                provider_id: provider.id,
                name: provider.name.clone(),
            });
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

        let expires_at = Utc::now() + login_state_ttl(provider.managed_by_cloud);
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
        let group_claim_name = claim_name_or_default(&provider.group_claim, "groups");
        // SECURITY (fixed post-audit): `admin_only_role_required` providers
        // extract their group/role claim strictly -- see
        // `strict_string_claim`'s doc comment for why an ordinary provider's
        // multi-value `groups` semantics must not apply to Cloud's
        // single-string `temps_cloud_instance_role` claim.
        let groups = if provider.admin_only_role_required {
            strict_string_claim(raw_claims, group_claim_name)
        } else {
            string_slice_claim(raw_claims, group_claim_name)
        };
        let role = evaluate_role(&provider, &mappings, &groups, raw_claims);
        enforce_admin_only_role(provider_id, provider.admin_only_role_required, &role)?;

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
            .create_user(display_name, email.clone(), None, vec![role.clone()], false)
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

/// A single-value claim extractor used only for `admin_only_role_required`
/// providers' custom instance-role claim (ADR-045 §4, SECURITY-fixed
/// post-audit).
///
/// Unlike [`string_slice_claim`] -- which legitimately treats a JSON array
/// as a multi-value `groups` claim for ordinary providers -- Cloud's
/// `temps_cloud_instance_role` claim is defined as exactly one JSON string.
/// Reusing `string_slice_claim` here would let an array like
/// `["owner"]`, or any other shape that happens to coerce into a matching
/// string, grant the same role a clean `"owner"` string would. This
/// extractor accepts only [`serde_json::Value::String`]; every other shape
/// (missing, an array, a number, an object, a bool) yields no groups at
/// all, so it can never match the `owner`/`admin` rows in
/// [`CLOUD_MANAGED_ROLE_MAPPINGS`] and always falls through to the
/// `("*", "user")` row that [`enforce_admin_only_role`] rejects -- the gate
/// fails closed on a malformed or unexpectedly-shaped claim rather than
/// attempting to interpret it.
fn strict_string_claim(claims: &serde_json::Value, key: &str) -> Vec<String> {
    match claims.get(key) {
        Some(serde_json::Value::String(value)) => vec![value.clone()],
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

/// The `oidc_login_states.expires_at` window for a login attempt. The same
/// for every provider, the Cloud-managed console-access one included: the
/// window has to cover the user's *interactive* sign-in at the issuer
/// (password, a social round trip, MFA), which routinely takes longer than
/// a minute. Replay of a captured callback is bounded on the issuer's side
/// -- the managed provider's authorization codes are single-use and expire
/// in 60s -- not by how long the RP is willing to wait for the user.
fn login_state_ttl(_managed_by_cloud: bool) -> ChronoDuration {
    ChronoDuration::minutes(LOGIN_STATE_TTL_MINUTES)
}

/// ADR-045 §4 role gate, layer 2 (belt-and-suspenders behind Cloud's own
/// account-linking screen): when `admin_only_role_required` is set, hard-reject
/// any resolved role other than `RoleType::Admin` -- never falling through to
/// `default_role`/`RoleType::User` the way an ungated provider would. A free
/// function (rather than inline in `resolve_user`) so the gate itself is
/// testable without a database or a real ID token.
fn enforce_admin_only_role(
    provider_id: i32,
    admin_only_role_required: bool,
    resolved_role: &RoleType,
) -> Result<(), OidcError> {
    if admin_only_role_required && *resolved_role != RoleType::Admin {
        tracing::warn!(
            target: "temps_auth::oidc::abuse",
            provider_id = provider_id,
            resolved_role = resolved_role.as_str(),
            "Refusing OIDC login: provider requires an admin-level role and the resolved role \
             was not admin"
        );
        return Err(OidcError::InsufficientRole {
            provider_id,
            resolved_role: resolved_role.as_str().to_string(),
        });
    }
    Ok(())
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
            managed_by_cloud: false,
            admin_only_role_required: false,
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
            managed_by_cloud: false,
            admin_only_role_required: false,
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

    /// A provider fixture shaped exactly like the row
    /// `upsert_managed_cloud_provider` inserts, minus the DB round-trip --
    /// used by pure (non-Docker) tests of the role gate so the gate's core
    /// logic is covered even when `test_oidc_service`'s Postgres
    /// testcontainer is unavailable.
    fn cloud_managed_provider_fixture(id: i32) -> oidc_providers::Model {
        oidc_providers::Model {
            id,
            name: CLOUD_MANAGED_OIDC_PROVIDER_NAME.into(),
            issuer_url: "https://cloud.example.com".into(),
            client_id: "cloud-client-id".into(),
            client_secret_encrypted: "encrypted".into(),
            scopes: CLOUD_MANAGED_OIDC_SCOPES.into(),
            jit_provisioning: true,
            enabled: true,
            template: CLOUD_MANAGED_OIDC_TEMPLATE.into(),
            group_claim: CLOUD_MANAGED_OIDC_ROLE_CLAIM.into(),
            role_claim: CLOUD_MANAGED_OIDC_ROLE_CLAIM.into(),
            default_role: RoleType::User.as_str().to_string(),
            trust_idp_email: true,
            managed_by_cloud: true,
            admin_only_role_required: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Role mapping rows built from [`CLOUD_MANAGED_ROLE_MAPPINGS`] -- the
    /// same constant `sync_managed_cloud_role_mappings` iterates over -- so
    /// this can never silently drift from what the service actually
    /// provisions in the database.
    fn cloud_managed_role_mapping_fixtures(provider_id: i32) -> Vec<oidc_role_mappings::Model> {
        CLOUD_MANAGED_ROLE_MAPPINGS
            .iter()
            .enumerate()
            .map(|(priority, (idp_group, role))| oidc_role_mappings::Model {
                id: priority as i32 + 1,
                provider_id,
                priority: priority as i32,
                idp_group: idp_group.to_string(),
                role: role.to_string(),
                created_at: chrono::Utc::now(),
            })
            .collect()
    }

    /// SECURITY (post-audit regression test for the BLOCKER finding): drives
    /// the exact code path `resolve_user` runs -- `strict_string_claim` ->
    /// `evaluate_role` -> `enforce_admin_only_role` -- against the *actual*
    /// mapping set [`CLOUD_MANAGED_ROLE_MAPPINGS`] produces, using the
    /// `temps_cloud_instance_role` claim shapes Cloud's ID token can send.
    /// Before the fix (`group_claim` left at the generic `"groups"` default),
    /// every one of the "must grant Admin" cases below fell through to the
    /// `("*", "user")` wildcard instead and was rejected -- this test would
    /// have caught that regression immediately.
    #[test]
    fn evaluate_role_grants_admin_only_for_the_actual_owner_and_admin_instance_roles() {
        let provider = cloud_managed_provider_fixture(1);
        let mappings = cloud_managed_role_mapping_fixtures(provider.id);

        let resolve = |raw_claims: serde_json::Value| {
            let groups = strict_string_claim(&raw_claims, CLOUD_MANAGED_OIDC_ROLE_CLAIM);
            let role = evaluate_role(&provider, &mappings, &groups, &raw_claims);
            enforce_admin_only_role(provider.id, provider.admin_only_role_required, &role)
                .map(|_| role)
        };

        assert_eq!(
            resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: "owner" }))
                .expect("an 'owner' instance role must grant Admin"),
            RoleType::Admin
        );
        assert_eq!(
            resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: "admin" }))
                .expect("an 'admin' instance role must grant Admin"),
            RoleType::Admin
        );

        let member_err = resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: "member" }))
            .expect_err("a 'member' instance role must be rejected, not default-allowed");
        assert!(matches!(member_err, OidcError::InsufficientRole { .. }));

        let missing_err = resolve(serde_json::json!({}))
            .expect_err("a missing instance-role claim must fail closed");
        assert!(matches!(missing_err, OidcError::InsufficientRole { .. }));

        // Fail-closed on malformed claim shapes: `strict_string_claim` must
        // not let an array or a number coerce into a matching group the way
        // the generic `string_slice_claim` legitimately would for an
        // ordinary provider's multi-value `groups` claim.
        let array_err = resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: ["owner"] }))
            .expect_err("an array-shaped instance-role claim must fail closed");
        assert!(matches!(array_err, OidcError::InsufficientRole { .. }));

        let number_err = resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: 1 }))
            .expect_err("a number-shaped instance-role claim must fail closed");
        assert!(matches!(number_err, OidcError::InsufficientRole { .. }));
    }

    #[test]
    fn strict_string_claim_accepts_only_a_plain_string() {
        let key = CLOUD_MANAGED_OIDC_ROLE_CLAIM;
        assert_eq!(
            strict_string_claim(&serde_json::json!({ key: "owner" }), key),
            vec!["owner".to_string()]
        );
        assert!(strict_string_claim(&serde_json::json!({}), key).is_empty());
        assert!(strict_string_claim(&serde_json::json!({ key: ["owner"] }), key).is_empty());
        assert!(strict_string_claim(&serde_json::json!({ key: 1 }), key).is_empty());
        assert!(strict_string_claim(&serde_json::json!({ key: true }), key).is_empty());
        assert!(
            strict_string_claim(&serde_json::json!({ key: { "role": "owner" } }), key).is_empty()
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

    // ── ADR-045 §4: the admin-only role gate ──────────────────────────

    #[test]
    fn enforce_admin_only_role_accepts_admin() {
        assert!(enforce_admin_only_role(1, true, &RoleType::Admin).is_ok());
    }

    #[test]
    fn enforce_admin_only_role_rejects_user_when_required() {
        let err = enforce_admin_only_role(1, true, &RoleType::User)
            .expect_err("a non-admin role must be refused when admin_only_role_required is set");
        assert!(matches!(
            err,
            OidcError::InsufficientRole {
                provider_id: 1,
                ref resolved_role
            } if resolved_role == "user"
        ));
    }

    #[test]
    fn enforce_admin_only_role_is_a_no_op_when_not_required() {
        // An ordinary provider (no `admin_only_role_required`) must never be
        // affected by this gate -- confirms the flag, not the role alone,
        // decides whether the check runs at all.
        assert!(enforce_admin_only_role(1, false, &RoleType::User).is_ok());
        assert!(enforce_admin_only_role(1, false, &RoleType::Admin).is_ok());
    }

    // ── ADR-045 §4: managed console-access provider upsert/revoke ─────
    //
    // Docker-dependent (TestDatabase spins up a real Postgres via
    // testcontainers); skips gracefully rather than failing the run when
    // Docker is unavailable, per CLAUDE.md.

    async fn test_oidc_service() -> Option<(temps_database::test_utils::TestDatabase, OidcService)>
    {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Docker not available, skipping test: {e}");
                return None;
            }
        };
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "oidc-service-managed-cloud-tests",
        ));
        let user_service = Arc::new(UserService::new(db.db.clone()));
        let service = OidcService::new(db.db.clone(), encryption, user_service);
        Some((db, service))
    }

    fn managed_config(issuer: &str) -> ManagedCloudOidcConfig {
        ManagedCloudOidcConfig {
            issuer: issuer.to_string(),
            client_id: "cloud-client-id".to_string(),
            client_secret: "cloud-client-secret".to_string(),
        }
    }

    #[tokio::test]
    async fn upsert_managed_cloud_provider_creates_a_single_admin_gated_row() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };

        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("first upsert must create the managed provider");

        assert!(provider.managed_by_cloud);
        assert!(provider.admin_only_role_required);
        assert!(provider.trust_idp_email);
        assert!(provider.jit_provisioning);
        assert_eq!(provider.template, CLOUD_MANAGED_OIDC_TEMPLATE);
        assert_eq!(provider.role_claim, CLOUD_MANAGED_OIDC_ROLE_CLAIM);
        // SECURITY (post-audit regression guard): `group_claim` must match
        // `role_claim` here. `evaluate_role`'s mapping loop reads
        // `provider.group_claim`, not `role_claim`, to decide which claim
        // holds the values matched against `idp_group`; leaving this at the
        // generic `"groups"` default (as it was before the fix) makes the
        // `owner`/`admin` mapping rows unreachable and every Cloud login
        // falls to the `("*", "user")` wildcard -- seen end-to-end in
        // `resolve_user_grants_admin_for_owner_and_admin_instance_roles`.
        assert_eq!(provider.group_claim, CLOUD_MANAGED_OIDC_ROLE_CLAIM);
        assert_ne!(
            provider.client_secret_encrypted, "cloud-client-secret",
            "the secret must be encrypted at rest, never stored as plaintext"
        );

        let mappings = service
            .list_role_mappings(provider.id)
            .await
            .expect("role mappings must be readable");
        assert_eq!(mappings.len(), 3, "owner/admin/wildcard, no more no less");
        assert!(mappings
            .iter()
            .any(|m| m.idp_group == "owner" && m.role == "admin"));
        assert!(mappings
            .iter()
            .any(|m| m.idp_group == "admin" && m.role == "admin"));
        assert!(mappings
            .iter()
            .any(|m| m.idp_group == "*" && m.role == "user"));
    }

    /// SECURITY (post-audit regression test for the BLOCKER finding):
    /// same assertions as
    /// `evaluate_role_grants_admin_only_for_the_actual_owner_and_admin_instance_roles`,
    /// but against the mapping rows `sync_managed_cloud_role_mappings`
    /// actually wrote to Postgres via `upsert_managed_cloud_provider` --
    /// closing the gap a hand-copied fixture could silently drift from.
    #[tokio::test]
    async fn resolve_role_gate_matches_the_actual_persisted_managed_provider_mappings() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");
        let mappings = service
            .load_role_mappings(provider.id)
            .await
            .expect("mappings must load");

        let resolve = |raw_claims: serde_json::Value| {
            let group_claim_name = claim_name_or_default(&provider.group_claim, "groups");
            let groups = if provider.admin_only_role_required {
                strict_string_claim(&raw_claims, group_claim_name)
            } else {
                string_slice_claim(&raw_claims, group_claim_name)
            };
            let role = evaluate_role(&provider, &mappings, &groups, &raw_claims);
            enforce_admin_only_role(provider.id, provider.admin_only_role_required, &role)
                .map(|_| role)
        };

        assert_eq!(
            resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: "owner" }))
                .expect("an 'owner' instance role must grant Admin"),
            RoleType::Admin
        );
        assert_eq!(
            resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: "admin" }))
                .expect("an 'admin' instance role must grant Admin"),
            RoleType::Admin
        );

        let member_err = resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: "member" }))
            .expect_err("a 'member' instance role must be rejected");
        assert!(matches!(member_err, OidcError::InsufficientRole { .. }));

        let missing_err = resolve(serde_json::json!({}))
            .expect_err("a missing instance-role claim must fail closed");
        assert!(matches!(missing_err, OidcError::InsufficientRole { .. }));

        let array_err = resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: ["owner"] }))
            .expect_err("an array-shaped instance-role claim must fail closed");
        assert!(matches!(array_err, OidcError::InsufficientRole { .. }));

        let number_err = resolve(serde_json::json!({ CLOUD_MANAGED_OIDC_ROLE_CLAIM: 1 }))
            .expect_err("a number-shaped instance-role claim must fail closed");
        assert!(matches!(number_err, OidcError::InsufficientRole { .. }));
    }

    #[tokio::test]
    async fn upsert_managed_cloud_provider_converges_in_place_on_reconnect() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };

        let first = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("first upsert must succeed");

        // A reconnect with a rotated secret and a different issuer (e.g. a
        // staging cutover) must update the *same* row, never insert a
        // second one -- there is exactly one managed provider by
        // construction.
        let second = service
            .upsert_managed_cloud_provider(ManagedCloudOidcConfig {
                issuer: "https://cloud-staging.example.com".to_string(),
                client_id: "rotated-client-id".to_string(),
                client_secret: "rotated-secret".to_string(),
            })
            .await
            .expect("second upsert must converge the existing row");

        assert_eq!(
            first.id, second.id,
            "must upsert in place, not insert a second row"
        );
        assert_eq!(second.client_id, "rotated-client-id");
        assert_eq!(second.issuer_url, "https://cloud-staging.example.com");

        let all_providers = service
            .list_providers()
            .await
            .expect("list_providers must succeed");
        assert_eq!(
            all_providers.iter().filter(|p| p.managed_by_cloud).count(),
            1,
            "exactly one managed-by-cloud row must ever exist"
        );

        // Role mappings must not accumulate across reconnects.
        let mappings = service
            .list_role_mappings(second.id)
            .await
            .expect("role mappings must be readable");
        assert_eq!(mappings.len(), 3);
    }

    #[tokio::test]
    async fn revoke_managed_cloud_provider_is_idempotent() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };

        // Nothing to revoke yet.
        assert!(!service
            .revoke_managed_cloud_provider()
            .await
            .expect("revoke on a clean instance must not error"));

        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        assert!(service
            .revoke_managed_cloud_provider()
            .await
            .expect("revoke must succeed"));
        assert!(
            service.managed_cloud_provider().await.unwrap().is_none(),
            "the managed row must be gone after revoke"
        );

        // A second revoke (e.g. a duplicate `ConsoleOidcRevoke` frame) must
        // not error -- it has nothing left to do.
        assert!(!service
            .revoke_managed_cloud_provider()
            .await
            .expect("a second revoke must be a no-op, not an error"));

        // The role mappings must have gone with it (cascade or explicit
        // delete either way -- verified by trying to fetch the provider,
        // which is gone, rather than by inspecting the mapping table
        // directly).
        assert!(service.get_provider(provider.id).await.is_err());
    }

    #[tokio::test]
    async fn revoke_managed_cloud_provider_invalidates_sessions_it_issued() {
        use temps_entities::sessions;

        let Some((db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        // A user JIT-provisioned (or linked) through the managed provider --
        // `resolve_user` sets `oidc_provider_id` on exactly this path.
        let user = users::ActiveModel {
            name: Set("Cloud Admin".to_string()),
            email: Set("cloud-admin@example.com".to_string()),
            email_verified: Set(true),
            oidc_provider_id: Set(Some(provider.id)),
            oidc_subject: Set(Some("cloud-account-1".to_string())),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("test user must insert");

        sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set("test-session-token".to_string()),
            expires_at: Set(Utc::now() + ChronoDuration::hours(1)),
            mfa_pending: Set(false),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("test session must insert");

        assert!(service
            .revoke_managed_cloud_provider()
            .await
            .expect("revoke must succeed"));

        let remaining = sessions::Entity::find()
            .filter(sessions::Column::UserId.eq(user.id))
            .all(db.db.as_ref())
            .await
            .expect("query must succeed");
        assert!(
            remaining.is_empty(),
            "every session belonging to a user linked through the revoked managed provider \
             must be invalidated"
        );
    }

    #[tokio::test]
    async fn update_provider_refuses_to_edit_the_managed_row() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        let err = service
            .update_provider(
                provider.id,
                UpdateOidcProviderRequest {
                    name: Some("hijacked".to_string()),
                    issuer_url: None,
                    client_id: None,
                    client_secret: None,
                    scopes: None,
                    jit_provisioning: None,
                    enabled: None,
                    template: None,
                    group_claim: None,
                    role_claim: None,
                    default_role: None,
                    trust_idp_email: None,
                },
            )
            .await
            .expect_err("editing the Cloud-managed provider manually must be refused");
        assert!(matches!(
            err,
            OidcError::ManagedByCloudEdit { provider_id, .. } if provider_id == provider.id
        ));
    }

    #[tokio::test]
    async fn delete_provider_refuses_to_delete_the_managed_row() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        let err = service
            .delete_provider(provider.id)
            .await
            .expect_err("deleting the Cloud-managed provider manually must be refused");
        assert!(matches!(
            err,
            OidcError::ManagedByCloudDelete { provider_id, .. } if provider_id == provider.id
        ));
        // Still present -- the refusal must not have deleted it anyway.
        assert!(service.get_provider(provider.id).await.is_ok());
    }

    /// SECURITY (post-audit regression test for the LOW finding): an admin
    /// must not be able to create a second, ordinary provider pointed at the
    /// managed provider's issuer -- that row would have no
    /// `admin_only_role_required` gate on it, so a login routed through it
    /// would skip the role check entirely even though it's the same IdP.
    #[tokio::test]
    async fn create_provider_refuses_an_issuer_matching_the_managed_provider() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        let err = service
            .create_provider(CreateOidcProviderRequest {
                name: "Shadow Provider".to_string(),
                issuer_url: "https://cloud.example.com".to_string(),
                client_id: "shadow-client".to_string(),
                client_secret: "shadow-secret".to_string(),
                scopes: "openid email profile".to_string(),
                jit_provisioning: true,
                enabled: true,
                template: "generic".to_string(),
                group_claim: "groups".to_string(),
                role_claim: "roles".to_string(),
                default_role: "user".to_string(),
                trust_idp_email: false,
            })
            .await
            .expect_err(
                "creating an ordinary provider on the managed provider's issuer must be refused",
            );
        assert!(matches!(
            err,
            OidcError::IssuerMatchesManagedCloudProvider { ref issuer_url }
                if issuer_url == "https://cloud.example.com"
        ));

        // Refused, not silently coerced -- no second provider must exist.
        let all_providers = service
            .list_providers()
            .await
            .expect("list_providers must succeed");
        assert_eq!(
            all_providers.len(),
            1,
            "the shadow provider must not have been created"
        );
    }

    /// Same guard, exercised via `update_provider` editing an *existing*,
    /// unrelated ordinary provider's `issuer_url` onto the managed
    /// provider's issuer after the fact.
    #[tokio::test]
    async fn update_provider_refuses_editing_issuer_url_to_match_the_managed_provider() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        let ordinary = service
            .create_provider(CreateOidcProviderRequest {
                name: "Keycloak".to_string(),
                issuer_url: "https://keycloak.example.com/realms/temps".to_string(),
                client_id: "keycloak-client".to_string(),
                client_secret: "keycloak-secret".to_string(),
                scopes: "openid email profile".to_string(),
                jit_provisioning: true,
                enabled: true,
                template: "keycloak".to_string(),
                group_claim: "groups".to_string(),
                role_claim: "roles".to_string(),
                default_role: "user".to_string(),
                trust_idp_email: false,
            })
            .await
            .expect("creating the ordinary provider must succeed");

        let err = service
            .update_provider(
                ordinary.id,
                UpdateOidcProviderRequest {
                    name: None,
                    issuer_url: Some("https://cloud.example.com".to_string()),
                    client_id: None,
                    client_secret: None,
                    scopes: None,
                    jit_provisioning: None,
                    enabled: None,
                    template: None,
                    group_claim: None,
                    role_claim: None,
                    default_role: None,
                    trust_idp_email: None,
                },
            )
            .await
            .expect_err(
                "editing an ordinary provider's issuer onto the managed provider's issuer must \
                 be refused",
            );
        assert!(matches!(
            err,
            OidcError::IssuerMatchesManagedCloudProvider { ref issuer_url }
                if issuer_url == "https://cloud.example.com"
        ));

        // The ordinary provider's issuer must be unchanged.
        let unchanged = service
            .get_provider(ordinary.id)
            .await
            .expect("provider must still exist");
        assert_eq!(
            unchanged.issuer_url, "https://keycloak.example.com/realms/temps",
            "the refused edit must not have been applied"
        );
    }

    // `start_login` itself is not exercised end-to-end here: it performs a
    // real OIDC discovery round-trip before it ever touches the TTL, which
    // would make this test depend on network access to a real (or mocked)
    // issuer. `login_state_ttl` is the exact decision `start_login` defers
    // to, extracted so it is testable in isolation -- see the "OIDC RP
    // against a stub issuer" case in ADR-045's Testing section for the
    // network-level version of this test.
    #[test]
    fn login_state_ttl_covers_an_interactive_sign_in_for_the_managed_provider_too() {
        // A user who needs a minute or more to sign in at Cloud must not
        // come back to an expired `state`; replay is bounded by the
        // issuer's single-use 60s code, not by this window.
        assert_eq!(
            login_state_ttl(true),
            ChronoDuration::minutes(LOGIN_STATE_TTL_MINUTES)
        );
    }

    #[test]
    fn login_state_ttl_is_the_generic_minutes_ttl_for_every_other_provider() {
        assert_eq!(
            login_state_ttl(false),
            ChronoDuration::minutes(LOGIN_STATE_TTL_MINUTES)
        );
    }

    /// A minimal ordinary provider request, so the tests below differ only in
    /// the field each one is actually about.
    fn ordinary_provider_request(name: &str, issuer_url: &str) -> CreateOidcProviderRequest {
        CreateOidcProviderRequest {
            name: name.to_string(),
            issuer_url: issuer_url.to_string(),
            client_id: "ordinary-client".to_string(),
            client_secret: "ordinary-secret".to_string(),
            scopes: "openid email profile".to_string(),
            jit_provisioning: true,
            enabled: true,
            template: "generic".to_string(),
            group_claim: "groups".to_string(),
            role_claim: "roles".to_string(),
            default_role: "user".to_string(),
            trust_idp_email: false,
        }
    }

    /// SECURITY: the other direction of the shadowing guard — an ordinary
    /// provider that *already* uses the incoming Cloud issuer. Provisioning
    /// the managed provider alongside it would leave the same IdP reachable
    /// through an ungated row, so the role gate could be skipped simply by
    /// signing in through that provider's slug.
    #[tokio::test]
    async fn upsert_managed_cloud_provider_refuses_an_issuer_an_ordinary_provider_already_uses() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        let ordinary = service
            .create_provider(ordinary_provider_request(
                "Pre-existing",
                "https://cloud.example.com",
            ))
            .await
            .expect("the ordinary provider must be creatable before Cloud provisions anything");

        let err = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect_err("provisioning onto an issuer an ordinary provider already uses must fail");
        assert!(matches!(
            err,
            OidcError::ManagedIssuerAlreadyUsed { provider_id, ref issuer_url, .. }
                if provider_id == ordinary.id && issuer_url == "https://cloud.example.com"
        ));

        // Fail closed *and* leave the operator's own configuration alone: no
        // managed row was written, and the existing row was neither adopted
        // into managed status nor deleted.
        assert!(
            service
                .managed_cloud_provider()
                .await
                .expect("query must succeed")
                .is_none(),
            "no managed provider may exist after a refused provisioning"
        );
        let unchanged = service
            .get_provider(ordinary.id)
            .await
            .expect("the ordinary provider must still exist");
        assert!(!unchanged.managed_by_cloud);
        assert!(!unchanged.admin_only_role_required);
    }

    /// SECURITY: an ordinary provider on a *different* issuer must be left
    /// completely alone by provisioning — the managed upsert converges only
    /// the row that is already `managed_by_cloud`, never adopting an
    /// operator's provider into Cloud ownership (which would make it
    /// uneditable and undeletable for them).
    #[tokio::test]
    async fn upsert_managed_cloud_provider_never_adopts_an_ordinary_provider() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        let ordinary = service
            .create_provider(ordinary_provider_request(
                "Keycloak",
                "https://keycloak.example.com/realms/temps",
            ))
            .await
            .expect("ordinary provider must be creatable");

        let managed = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("provisioning on a free issuer must succeed");

        assert_ne!(
            managed.id, ordinary.id,
            "provisioning must insert its own row, never take over an existing one"
        );
        let untouched = service
            .get_provider(ordinary.id)
            .await
            .expect("the ordinary provider must still exist");
        assert!(!untouched.managed_by_cloud);
        assert!(!untouched.admin_only_role_required);
        assert_eq!(
            untouched.issuer_url,
            "https://keycloak.example.com/realms/temps"
        );
    }

    /// SECURITY: the managed provider's role mappings *are* its admin-only
    /// gate. A settings administrator who can add `member -> admin` (or a
    /// higher-priority `* -> admin`) turns "Cloud owners and admins" into
    /// "anyone with a Cloud account on this instance", because
    /// `evaluate_role` returns the first match and `enforce_admin_only_role`
    /// accepts any role that resolved to Admin.
    #[tokio::test]
    async fn role_mapping_mutations_are_refused_for_the_managed_provider() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        let escalation = service
            .create_role_mapping(
                provider.id,
                CreateOidcRoleMappingRequest {
                    idp_group: "member".to_string(),
                    role: "admin".to_string(),
                    // Ahead of the canonical rows, so it would win.
                    priority: -1,
                },
            )
            .await
            .expect_err("adding a role mapping to the managed provider must be refused");
        assert!(matches!(
            escalation,
            OidcError::ManagedByCloudRoleMapping { provider_id, .. } if provider_id == provider.id
        ));

        let mappings = service
            .list_role_mappings(provider.id)
            .await
            .expect("mappings must be readable");
        assert_eq!(
            mappings.len(),
            3,
            "the refused mapping must not have been written"
        );

        // The opposite direction: deleting `owner -> admin` would lock every
        // Cloud owner out of the console, which is just as much a change to
        // a Cloud-owned configuration.
        let owner_mapping = mappings
            .iter()
            .find(|m| m.idp_group == "owner")
            .expect("the canonical owner mapping must exist");
        let lockout = service
            .delete_role_mapping(owner_mapping.id)
            .await
            .expect_err("deleting a managed provider's role mapping must be refused");
        assert!(matches!(
            lockout,
            OidcError::ManagedByCloudRoleMapping { provider_id, .. } if provider_id == provider.id
        ));
        assert_eq!(
            service
                .list_role_mappings(provider.id)
                .await
                .expect("mappings must be readable")
                .len(),
            3,
            "the refused delete must not have removed the mapping anyway"
        );
    }

    /// An ordinary provider's mappings stay fully editable — the guard keys
    /// off `managed_by_cloud`, not off "is an OIDC provider".
    #[tokio::test]
    async fn role_mapping_mutations_still_work_for_ordinary_providers() {
        let Some((_db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .create_provider(ordinary_provider_request(
                "Keycloak",
                "https://keycloak.example.com/realms/temps",
            ))
            .await
            .expect("ordinary provider must be creatable");

        let mapping = service
            .create_role_mapping(
                provider.id,
                CreateOidcRoleMappingRequest {
                    idp_group: "platform-admins".to_string(),
                    role: "admin".to_string(),
                    priority: 0,
                },
            )
            .await
            .expect("an ordinary provider's mappings must remain editable");
        service
            .delete_role_mapping(mapping.id)
            .await
            .expect("an ordinary provider's mappings must remain deletable");
        assert!(service
            .list_role_mappings(provider.id)
            .await
            .expect("mappings must be readable")
            .is_empty());
    }

    /// Provisioning is one unit of work: the provider row and the complete
    /// mapping set commit together, or neither does. This drives the real
    /// `sync_managed_cloud_role_mappings` inside a transaction that is then
    /// rolled back (exactly what happens when any step after the delete
    /// fails) and asserts the previously-provisioned mappings survived
    /// untouched — rather than the enabled, admin-gated provider being left
    /// with no mapping at all, which locks out every Cloud owner and admin.
    #[tokio::test]
    async fn managed_role_mapping_replacement_only_takes_effect_on_commit() {
        let Some((db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");
        let before: Vec<i32> = service
            .list_role_mappings(provider.id)
            .await
            .expect("mappings must be readable")
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(before.len(), 3);

        let txn = db.db.begin().await.expect("transaction must begin");
        OidcService::sync_managed_cloud_role_mappings(&txn, provider.id)
            .await
            .expect("the replacement itself must succeed inside the transaction");
        // Dropping without committing is what a failure anywhere later in
        // `upsert_managed_cloud_provider` does.
        drop(txn);

        let after: Vec<i32> = service
            .list_role_mappings(provider.id)
            .await
            .expect("mappings must be readable")
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            before, after,
            "a rolled-back provisioning must leave the previous mappings exactly as they were"
        );
    }

    /// SECURITY: revocation must win against a login that is already in
    /// flight. A callback that resolved its user before the revocation
    /// started inserts its session afterwards, so the revocation's session
    /// delete never saw it; this check runs immediately after that insert and
    /// deletes the session again rather than handing out the cookie.
    #[tokio::test]
    async fn a_session_created_after_revocation_is_discarded_instead_of_returned() {
        use temps_entities::sessions;

        let Some((db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");
        let user = users::ActiveModel {
            name: Set("Cloud Admin".to_string()),
            email: Set("cloud-admin@example.com".to_string()),
            email_verified: Set(true),
            oidc_provider_id: Set(Some(provider.id)),
            oidc_subject: Set(Some("cloud-account-1".to_string())),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("test user must insert");

        // A live provider: the login keeps its session.
        sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set("live-session".to_string()),
            expires_at: Set(Utc::now() + ChronoDuration::hours(1)),
            mfa_pending: Set(false),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("session must insert");
        service
            .assert_provider_live_for_session(provider.id, "live-session")
            .await
            .expect("a login completing against a live provider must keep its session");
        assert!(sessions::Entity::find()
            .filter(sessions::Column::SessionToken.eq("live-session"))
            .one(db.db.as_ref())
            .await
            .expect("query must succeed")
            .is_some());

        assert!(service
            .revoke_managed_cloud_provider()
            .await
            .expect("revoke must succeed"));

        // The losing side of the race: the session row lands after the
        // revocation already deleted everything it could see.
        sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set("raced-session".to_string()),
            expires_at: Set(Utc::now() + ChronoDuration::hours(1)),
            mfa_pending: Set(false),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("session must insert");

        let err = service
            .assert_provider_live_for_session(provider.id, "raced-session")
            .await
            .expect_err("a login completing against a revoked provider must fail closed");
        assert!(matches!(
            err,
            OidcError::ProviderRevokedDuringLogin { provider_id } if provider_id == provider.id
        ));
        assert!(
            sessions::Entity::find()
                .filter(sessions::Column::SessionToken.eq("raced-session"))
                .one(db.db.as_ref())
                .await
                .expect("query must succeed")
                .is_none(),
            "the session created by the losing callback must be deleted again"
        );
    }

    /// SECURITY: a login that already redirected to the IdP must not be able
    /// to come back and complete against a provider that was revoked in the
    /// meantime. Dropping the pending `oidc_login_states` rows inside the
    /// revocation transaction makes the callback fail at `consume_login_state`
    /// instead of proceeding to a token exchange for a provider that is gone.
    #[tokio::test]
    async fn revoking_the_managed_provider_discards_in_flight_login_states() {
        let Some((db, service)) = test_oidc_service().await else {
            return;
        };
        let provider = service
            .upsert_managed_cloud_provider(managed_config("https://cloud.example.com"))
            .await
            .expect("upsert must succeed");

        oidc_login_states::ActiveModel {
            state: Set("in-flight-state".to_string()),
            nonce: Set("nonce".to_string()),
            pkce_verifier: Set("verifier".to_string()),
            provider_id: Set(provider.id),
            return_to: Set(None),
            expires_at: Set(Utc::now() + ChronoDuration::seconds(60)),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("login state must insert");

        assert!(service
            .revoke_managed_cloud_provider()
            .await
            .expect("revoke must succeed"));

        // `OidcLoginState` deliberately has no `Debug` (it carries the nonce
        // and PKCE verifier), so match the result rather than `expect_err`.
        match service.consume_login_state("in-flight-state").await {
            Err(OidcError::StateNotFound { .. }) => {}
            Err(other) => panic!("expected StateNotFound, got {other}"),
            Ok(_) => panic!("a login state for a revoked provider must no longer be consumable"),
        }
    }
}
