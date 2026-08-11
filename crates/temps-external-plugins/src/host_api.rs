//! Serving plugin API calls from the platform's own router.
//!
//! A plugin that needs to create a project or start a deployment has two bad
//! options and one good one. It could re-implement the platform's services
//! in its own process (duplicating a service graph that changes underneath
//! it), or it could be handed a credential and call the API over the network
//! (a credential to mint, store and leak). Neither is necessary: the
//! platform is already running the router, so the channel carries the call
//! into it.
//!
//! What that buys, concretely:
//!
//! - **One API.** A plugin reaches exactly the endpoints the product has.
//!   There is no second surface to keep in sync, so an endpoint cannot exist
//!   for the console and be missing for plugins.
//! - **Real authorization.** The call runs the real handler, so its
//!   `permission_guard!` runs, against the real acting user, and its audit
//!   log is written where every other write is logged.
//! - **No new credential.** Identity arrives as an actor token the platform
//!   itself minted (see [`temps_core::external_plugin::actor`]), not as an
//!   API key the plugin holds.
//!
//! ## Why this injects `AuthContext` rather than replaying a session
//!
//! The router's auth middleware inserts an `AuthContext` only when it can
//! authenticate a request, and leaves an existing one alone. So resolving
//! the actor here and inserting it directly is enough for `RequireAuth` and
//! `permission_guard!` to work, without minting a session cookie or a token
//! that would then exist as a second, longer-lived credential.
//!
//! The safety of that rests entirely on the actor token being unforgeable
//! and on this being the only place that inserts an `AuthContext` a request
//! did not earn.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_auth::context::AuthContext;
use temps_auth::permissions::{Permission, Role};
use temps_auth::UserService;
use temps_core::external_plugin::actor::{verify_plugin_actor_token, ActorPrincipal};
use temps_core::external_plugin::channel::{
    ApiCall, ApiCallResult, CallBody, ChannelError, ChannelErrorCode, JsonBody, MultipartPart,
    PartContent, VerifiedPluginApiCaller, MAX_CALL_BODY_BYTES,
};
use temps_core::CookieCrypto;
use tower::ServiceExt;
use tracing::{debug, warn};

use crate::channel::HostApiBridge;

/// Largest response body a plugin API call may return.
///
/// The whole body is buffered to put it on the channel, so an unbounded
/// response (a log tail, a big export) would be a memory spike on a small
/// box. Plugins that need bulk data should page.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Runs plugin API calls against the console's router.
pub struct RouterHostApi {
    router: Router,
    db: Arc<DatabaseConnection>,
    crypto: Arc<CookieCrypto>,
    users: Arc<UserService>,
}

impl RouterHostApi {
    pub fn new(
        router: Router,
        db: Arc<DatabaseConnection>,
        crypto: Arc<CookieCrypto>,
        users: Arc<UserService>,
    ) -> Self {
        Self {
            router,
            db,
            crypto,
            users,
        }
    }

    /// Resolve the acting user, or say precisely why not.
    ///
    /// Verifying the token's signature only proves the platform minted it; it
    /// says nothing about whether the session or API key it names is *still*
    /// good. That is re-checked here, against the row itself, on every call —
    /// otherwise a logged-out session or a revoked API key would go on
    /// authorizing plugin calls until the token's TTL expired on its own.
    async fn resolve_actor(
        &self,
        plugin: &str,
        call: &ApiCall,
    ) -> Result<AuthContext, ChannelError> {
        let verified =
            verify_plugin_actor_token(&self.crypto, call.actor.as_str(), plugin, SystemTime::now())
                .map_err(|e| {
                    // Every rejection reason is already spelled out by the error
                    // type, and a plugin author debugging alone needs it: "expired"
                    // and "not your token" call for completely different fixes.
                    warn!(plugin = %plugin, "Rejected plugin actor token: {e}");
                    ChannelError::new(ChannelErrorCode::Unauthenticated, e.to_string())
                })?;
        let user_id = verified.user_id;

        // `deleted_at IS NULL`, matching every other path that resolves a user
        // from a credential (`auth_service`, `apikey_service`, `oidc_service`).
        //
        // Deletion here is soft, so a bare `find_by_id` keeps returning the
        // row: an administrator could delete an account and the plugin would
        // go on acting as them until the token aged out — up to the 15-minute
        // TTL, and longer if the SDK's registry had already remembered it.
        // Revocation has to take effect at redemption, because that is the
        // only moment the platform gets to say no.
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .filter(temps_entities::users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                ChannelError::new(
                    ChannelErrorCode::Internal,
                    format!("Failed to load the acting user {user_id} for plugin '{plugin}': {e}"),
                )
            })?
            .ok_or_else(|| {
                // The token was valid but the user is gone — deleted between
                // minting and use. Not an internal error; the plugin simply
                // has no one to act as any more.
                ChannelError::new(
                    ChannelErrorCode::Unauthenticated,
                    format!(
                        "Plugin '{plugin}' presented a valid actor token for user {user_id}, \
                         but that user no longer exists"
                    ),
                )
            })?;

        match verified.principal {
            ActorPrincipal::Session { session_id } => {
                // Same shape session middleware checks: the row must exist,
                // belong to this user, not be expired, and not be a
                // first-factor-only MFA challenge. Logging out, an admin
                // force-expiring a session, or the session simply expiring
                // all show up here as "no matching row" — the token alone can
                // never outlive the session it was minted from.
                let session = temps_entities::sessions::Entity::find_by_id(session_id)
                    .filter(temps_entities::sessions::Column::UserId.eq(user_id))
                    .filter(temps_entities::sessions::Column::ExpiresAt.gt(chrono::Utc::now()))
                    .filter(temps_entities::sessions::Column::MfaPending.eq(false))
                    .one(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        ChannelError::new(
                            ChannelErrorCode::Internal,
                            format!(
                                "Failed to load session {session_id} for plugin '{plugin}': {e}"
                            ),
                        )
                    })?
                    .ok_or_else(|| {
                        ChannelError::new(
                            ChannelErrorCode::Unauthenticated,
                            format!(
                                "Plugin '{plugin}' presented an actor token for session \
                                 {session_id}, but that session is no longer valid"
                            ),
                        )
                    })?;

                // Role is resolved now rather than carried in the token, so a
                // user demoted after the token was minted does not keep the
                // old access for the rest of its lifetime.
                let is_admin = self.users.is_admin(user.id).await.map_err(|e| {
                    ChannelError::new(
                        ChannelErrorCode::Internal,
                        format!(
                            "Failed to resolve the current role for user {user_id} while \
                             redeeming an actor token for plugin '{plugin}': {e}"
                        ),
                    )
                })?;
                let role = if is_admin { Role::Admin } else { Role::User };
                Ok(AuthContext::new_persisted_session(user, role, session.id))
            }
            ActorPrincipal::ApiKey { key_id } => {
                // Role/permissions are re-parsed fresh from the row, exactly
                // like `ApiKeyService::validate_api_key` — never trusted from
                // the token. A restricted key stays restricted for every
                // plugin call, even though the token only carries its id.
                let key = temps_entities::api_keys::Entity::find_by_id(key_id)
                    .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
                    .filter(temps_entities::api_keys::Column::IsActive.eq(true))
                    .one(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        ChannelError::new(
                            ChannelErrorCode::Internal,
                            format!("Failed to load API key {key_id} for plugin '{plugin}': {e}"),
                        )
                    })?
                    .ok_or_else(|| {
                        ChannelError::new(
                            ChannelErrorCode::Unauthenticated,
                            format!(
                                "Plugin '{plugin}' presented an actor token for API key \
                                 {key_id}, but that key is no longer active"
                            ),
                        )
                    })?;

                if let Some(expires_at) = key.expires_at {
                    if expires_at <= chrono::Utc::now() {
                        return Err(ChannelError::new(
                            ChannelErrorCode::Unauthenticated,
                            format!(
                                "Plugin '{plugin}' presented an actor token for API key \
                                 {key_id}, but that key has expired"
                            ),
                        ));
                    }
                }

                let (role, permissions) = if key.role_type == "custom" {
                    let perms = if let Some(perms_json) = &key.permissions {
                        let perm_strings: Vec<String> =
                            serde_json::from_str(perms_json).map_err(|e| {
                                ChannelError::new(
                                    ChannelErrorCode::Internal,
                                    format!("API key {key_id} has invalid permissions JSON: {e}"),
                                )
                            })?;
                        Some(
                            perm_strings
                                .iter()
                                .filter_map(|s| Permission::from_str(s))
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        return Err(ChannelError::new(
                            ChannelErrorCode::Internal,
                            format!(
                                "API key {key_id} has a custom role but no permissions defined"
                            ),
                        ));
                    };
                    (None, perms)
                } else {
                    let role = Role::from_str(&key.role_type).ok_or_else(|| {
                        ChannelError::new(
                            ChannelErrorCode::Internal,
                            format!(
                                "API key {key_id} has an unrecognized role type '{}'",
                                key.role_type
                            ),
                        )
                    })?;
                    (Some(role), None)
                };

                Ok(AuthContext::new_api_key(
                    user,
                    role,
                    permissions,
                    key.name,
                    key.id,
                ))
            }
        }
    }
}

/// A boundary that provably does not occur in the payload it delimits.
///
/// RFC 7578 leaves boundary choice to the sender, and the usual answer is a
/// random string — which is *probably* absent from the body. Hashing the
/// content instead makes it deterministic (so the encoder is testable) and
/// removes the "probably": the hash of the parts is not a substring of the
/// parts unless they were built to contain it, and the check below catches
/// even that.
fn multipart_boundary(parts: &[MultipartPart]) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    for part in parts {
        part.name.hash(&mut hasher);
        part.filename.hash(&mut hasher);
        part.content_type.hash(&mut hasher);
        match &part.content {
            PartContent::Text(t) => t.hash(&mut hasher),
            PartContent::Binary(b) => b.as_bytes().hash(&mut hasher),
        }
    }

    // Extend with a counter until nothing in the payload contains it. In
    // practice this loop runs once; it exists so a crafted upload cannot
    // smuggle an extra part by embedding the boundary.
    for salt in 0u32.. {
        let candidate = format!("temps-plugin-{:016x}-{salt}", hasher.finish());
        let collides = parts.iter().any(|p| match &p.content {
            PartContent::Text(t) => t.contains(&candidate),
            PartContent::Binary(b) => find_subslice(b.as_bytes(), candidate.as_bytes()).is_some(),
        });
        if !collides {
            return candidate;
        }
    }
    // `0u32..` is only exhausted after 4 billion collisions, which cannot
    // happen; the loop above always returns.
    unreachable!("boundary search exhausted the entire u32 range")
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Render parts as an RFC 7578 `multipart/form-data` body.
fn encode_multipart(parts: &[MultipartPart], boundary: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());

        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(filename) = &part.filename {
            // Quotes and CR/LF are the only characters that could break out
            // of the header; stripping them is enough and keeps the filename
            // recognisable, which matters because handlers log it.
            let safe: String = filename
                .chars()
                .filter(|c| *c != '"' && *c != '\r' && *c != '\n')
                .collect();
            disposition.push_str(&format!("; filename=\"{safe}\""));
        }
        out.extend_from_slice(disposition.as_bytes());
        out.extend_from_slice(b"\r\n");

        if let Some(content_type) = &part.content_type {
            let safe: String = content_type
                .chars()
                .filter(|c| *c != '\r' && *c != '\n')
                .collect();
            out.extend_from_slice(format!("Content-Type: {safe}\r\n").as_bytes());
        }

        out.extend_from_slice(b"\r\n");
        match &part.content {
            PartContent::Text(t) => out.extend_from_slice(t.as_bytes()),
            PartContent::Binary(b) => out.extend_from_slice(b.as_bytes()),
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

#[async_trait::async_trait]
impl HostApiBridge for RouterHostApi {
    async fn call(&self, plugin: &str, call: ApiCall) -> Result<ApiCallResult, ChannelError> {
        let auth = self.resolve_actor(plugin, &call).await?;

        let uri = match &call.query {
            Some(q) if !q.is_empty() => format!("/api{}?{}", call.path, q),
            _ => format!("/api{}", call.path),
        };

        if let Some(body) = &call.body {
            let len = body.payload_len();
            if len > MAX_CALL_BODY_BYTES {
                return Err(ChannelError::new(
                    ChannelErrorCode::InvalidParams,
                    format!(
                        "Plugin '{plugin}' sent a {len}-byte body for {uri}, over the \
                         {MAX_CALL_BODY_BYTES}-byte channel limit"
                    ),
                ));
            }
        }

        let (content_type, body) = match call.body.as_ref() {
            None => ("application/json".to_string(), Body::empty()),
            Some(CallBody::Json(json)) => (
                "application/json".to_string(),
                Body::from(json.as_str().to_owned()),
            ),
            Some(CallBody::Multipart(parts)) => {
                // The boundary is derived from the payload rather than chosen
                // at random, so it is reproducible in a test and — because
                // it is a hash of everything being sent — cannot appear
                // inside the content it delimits.
                let boundary = multipart_boundary(parts);
                let encoded = encode_multipart(parts, &boundary);
                (
                    format!("multipart/form-data; boundary={boundary}"),
                    Body::from(encoded),
                )
            }
        };

        let mut request = Request::builder()
            .method(call.method.as_str())
            .uri(&uri)
            .header("content-type", content_type)
            .body(body)
            .map_err(|e| {
                ChannelError::new(
                    ChannelErrorCode::InvalidParams,
                    format!(
                        "Plugin '{plugin}' asked for {} {uri}, which is not a valid request: {e}",
                        call.method.as_str()
                    ),
                )
            })?;

        // This is the line that makes the call authenticated. See the module
        // docs: the middleware will not overwrite it, and nothing else in
        // the codebase inserts an AuthContext a request did not earn.
        request.extensions_mut().insert(auth);
        request
            .extensions_mut()
            .insert(VerifiedPluginApiCaller::new(plugin));

        debug!(plugin = %plugin, method = %call.method.as_str(), uri = %uri, "Serving plugin API call");

        let response = self.router.clone().oneshot(request).await.map_err(|e| {
            ChannelError::new(
                ChannelErrorCode::Internal,
                format!("Platform router failed to serve {uri} for plugin '{plugin}': {e}"),
            )
        })?;

        let status = response.status().as_u16();
        let collected = response
            .into_body()
            .collect()
            .await
            .map_err(|e| {
                ChannelError::new(
                    ChannelErrorCode::Internal,
                    format!("Failed to read the response to {uri} for plugin '{plugin}': {e}"),
                )
            })?
            .to_bytes();

        if collected.len() > MAX_RESPONSE_BYTES {
            return Err(ChannelError::new(
                ChannelErrorCode::Internal,
                format!(
                    "Response to {uri} for plugin '{plugin}' is {} bytes, over the \
                     {MAX_RESPONSE_BYTES}-byte channel limit; use a paged endpoint",
                    collected.len()
                ),
            ));
        }

        // Non-2xx is returned to the plugin rather than raised as a channel
        // error: a 404 or a 403 from the API is a real answer the plugin
        // needs to see and act on, not a channel malfunction. Channel errors
        // are reserved for "the call never happened".
        let body = String::from_utf8(collected.to_vec()).map_err(|e| {
            ChannelError::new(
                ChannelErrorCode::Internal,
                format!(
                    "Response to {uri} for plugin '{plugin}' was not valid UTF-8 \
                     (status {status}): {e}"
                ),
            )
        })?;

        if status == StatusCode::UNAUTHORIZED.as_u16() {
            // Worth flagging loudly: it means the AuthContext injection is
            // not reaching handlers, which would otherwise look like a
            // permissions problem in the plugin.
            warn!(
                plugin = %plugin,
                uri = %uri,
                "Plugin API call was rejected as unauthenticated despite an injected actor"
            );
        }

        Ok(ApiCallResult {
            status,
            body: JsonBody::from_raw(body),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::context::AuthSource;
    use temps_core::external_plugin::actor::{encode_plugin_actor_token, PLUGIN_ACTOR_TOKEN_TTL};
    use temps_core::external_plugin::channel::{ActorToken, BinaryPayload, HttpMethod};

    const TEST_PLUGIN: &str = "security-test-plugin";

    fn crypto() -> Arc<CookieCrypto> {
        Arc::new(
            CookieCrypto::new("host-api-security-test-key-00001")
                .expect("test encryption key should be valid"),
        )
    }

    fn user() -> temps_entities::users::Model {
        let now = Utc::now();
        temps_entities::users::Model {
            id: 42,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn session() -> temps_entities::sessions::Model {
        temps_entities::sessions::Model {
            id: 7,
            user_id: 42,
            session_token: "hashed-session-token".to_string(),
            expires_at: Utc::now() + ChronoDuration::minutes(30),
            mfa_pending: false,
            step_up_expires_at: None,
        }
    }

    fn api_key(expires_at: Option<chrono::DateTime<Utc>>) -> temps_entities::api_keys::Model {
        let now = Utc::now();
        temps_entities::api_keys::Model {
            id: 9,
            name: "restricted-plugin-key".to_string(),
            key_hash: "hash".to_string(),
            key_prefix: "tk_test".to_string(),
            user_id: 42,
            role_type: "custom".to_string(),
            permissions: Some(r#"["projects:read"]"#.to_string()),
            is_active: true,
            expires_at,
            last_used_at: None,
            created_at: now,
            updated_at: now,
            service_id: None,
        }
    }

    fn api_with_database(
        db: sea_orm::DatabaseConnection,
        crypto: Arc<CookieCrypto>,
    ) -> (RouterHostApi, Arc<DatabaseConnection>) {
        let db = Arc::new(db);
        let users = Arc::new(UserService::new(db.clone()));
        (
            RouterHostApi::new(Router::new(), db.clone(), crypto, users),
            db,
        )
    }

    fn actor_call(crypto: &CookieCrypto, principal: ActorPrincipal) -> ApiCall {
        let (token, _) = encode_plugin_actor_token(
            crypto,
            TEST_PLUGIN,
            42,
            principal,
            PLUGIN_ACTOR_TOKEN_TTL,
            SystemTime::now(),
        )
        .expect("actor token should be minted");
        ApiCall {
            method: HttpMethod::Get,
            path: "/projects".to_string(),
            query: None,
            body: None,
            actor: ActorToken::new(token),
        }
    }

    fn transaction_sql(db: Arc<DatabaseConnection>) -> String {
        Arc::try_unwrap(db)
            .expect("host API should release the test database")
            .into_transaction_log()
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_unauthenticated(error: ChannelError, reason: &str) {
        assert_eq!(error.code, ChannelErrorCode::Unauthenticated);
        assert!(
            error.message.contains(reason),
            "expected {reason:?} in error: {}",
            error.message
        );
    }

    fn upload_parts() -> Vec<MultipartPart> {
        vec![
            MultipartPart {
                name: "file".into(),
                filename: Some("bundle.tar.gz".into()),
                content_type: Some("application/gzip".into()),
                content: PartContent::Binary(BinaryPayload::new(vec![0x1f, 0x8b, 0x00, 0xff])),
            },
            MultipartPart {
                name: "content_type".into(),
                filename: None,
                content_type: None,
                content: PartContent::Text("application/gzip".into()),
            },
        ]
    }

    #[test]
    fn multipart_body_has_the_rfc_7578_shape() {
        let parts = upload_parts();
        let boundary = multipart_boundary(&parts);
        let encoded = encode_multipart(&parts, &boundary);
        let text = String::from_utf8_lossy(&encoded);

        assert!(text.starts_with(&format!("--{boundary}\r\n")), "{text}");
        assert!(text.contains(
            "Content-Disposition: form-data; name=\"file\"; filename=\"bundle.tar.gz\"\r\n"
        ));
        assert!(text.contains("Content-Type: application/gzip\r\n"));
        assert!(text.contains("Content-Disposition: form-data; name=\"content_type\"\r\n"));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")), "{text}");
        // The raw bytes must survive verbatim, including the non-UTF-8 ones.
        assert!(find_subslice(&encoded, &[0x1f, 0x8b, 0x00, 0xff]).is_some());
    }

    /// A part with no declared type emits no `Content-Type` header, rather
    /// than an empty one a strict parser would reject.
    #[test]
    fn parts_without_a_content_type_omit_the_header() {
        let parts = vec![MultipartPart {
            name: "metadata".into(),
            filename: None,
            content_type: None,
            content: PartContent::Text("{}".into()),
        }];
        let encoded = encode_multipart(&parts, "b");
        let text = String::from_utf8_lossy(&encoded);
        assert!(!text.contains("Content-Type"), "{text}");
    }

    /// Same input, same boundary — the encoder must not depend on hidden
    /// state, or a retry would produce a different body.
    #[test]
    fn boundary_is_deterministic() {
        assert_eq!(
            multipart_boundary(&upload_parts()),
            multipart_boundary(&upload_parts())
        );
    }

    /// A payload that already contains the natural boundary must get a
    /// different one, otherwise it could inject a part of its own.
    #[test]
    fn boundary_avoids_a_payload_that_embeds_it() {
        let natural = multipart_boundary(&[]);
        let parts = vec![MultipartPart {
            name: "file".into(),
            filename: None,
            content_type: None,
            content: PartContent::Text(format!("--{natural}\r\nContent-Disposition: evil\r\n")),
        }];
        let chosen = multipart_boundary(&parts);
        let encoded = encode_multipart(&parts, &chosen);
        // Exactly two occurrences: the opening delimiter and the closing one.
        let count = String::from_utf8_lossy(&encoded)
            .matches(&format!("--{chosen}"))
            .count();
        assert_eq!(count, 2, "boundary {chosen} appears {count} times");
    }

    /// A filename cannot break out of the Content-Disposition header.
    #[test]
    fn filenames_cannot_inject_headers() {
        let parts = vec![MultipartPart {
            name: "file".into(),
            filename: Some("a\"\r\nX-Injected: yes\r\n".into()),
            content_type: None,
            content: PartContent::Text("x".into()),
        }];
        let encoded = encode_multipart(&parts, "b");
        let text = String::from_utf8_lossy(&encoded);
        assert!(!text.contains("X-Injected: yes\r\n"), "{text}");
        assert!(text.contains("filename=\"aX-Injected: yes\""), "{text}");
    }

    #[tokio::test]
    async fn valid_session_reconstructs_current_user_authority() {
        let crypto = crypto();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![user()]])
            .append_query_results([vec![session()]])
            // No admin role row: current authority is an ordinary user even
            // if the actor token was minted before a role change.
            .append_query_results([Vec::<temps_entities::roles::Model>::new()])
            .into_connection();
        let (api, db) = api_with_database(db, crypto.clone());
        let call = actor_call(crypto.as_ref(), ActorPrincipal::Session { session_id: 7 });

        let auth = api
            .resolve_actor(TEST_PLUGIN, &call)
            .await
            .expect("live session should resolve");

        assert_eq!(auth.user_id_opt(), Some(42));
        assert_eq!(auth.session_id(), Some(7));
        assert_eq!(auth.effective_role, Role::User);
        assert!(auth.has_permission(&Permission::ProjectsRead));
        drop(api);
        let sql = transaction_sql(db);
        assert!(sql.contains("\"sessions\".\"expires_at\" >"), "{sql}");
        assert!(sql.contains("\"sessions\".\"mfa_pending\" ="), "{sql}");
    }

    #[tokio::test]
    async fn revoked_expired_or_mfa_pending_session_fails_closed() {
        // MockDatabase does not evaluate WHERE predicates. An empty second
        // result represents each reason the database excludes the row; the
        // SQL assertions prove all fail-closed predicates are present.
        for reason in ["revoked", "expired", "MFA-pending"] {
            let crypto = crypto();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![user()]])
                .append_query_results([Vec::<temps_entities::sessions::Model>::new()])
                .into_connection();
            let (api, db) = api_with_database(db, crypto.clone());
            let call = actor_call(crypto.as_ref(), ActorPrincipal::Session { session_id: 7 });

            let error = api
                .resolve_actor(TEST_PLUGIN, &call)
                .await
                .expect_err("non-live session must be rejected");
            assert_unauthenticated(error, "session 7, but that session is no longer valid");
            drop(api);
            let sql = transaction_sql(db);
            assert!(
                sql.contains("\"sessions\".\"expires_at\" >"),
                "{reason}: {sql}"
            );
            assert!(
                sql.contains("\"sessions\".\"mfa_pending\" ="),
                "{reason}: {sql}"
            );
        }
    }

    #[tokio::test]
    async fn valid_restricted_api_key_reconstructs_narrowed_permissions() {
        let crypto = crypto();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![user()]])
            .append_query_results([vec![api_key(None)]])
            .into_connection();
        let (api, db) = api_with_database(db, crypto.clone());
        let call = actor_call(crypto.as_ref(), ActorPrincipal::ApiKey { key_id: 9 });

        let auth = api
            .resolve_actor(TEST_PLUGIN, &call)
            .await
            .expect("active API key should resolve");

        assert_eq!(auth.user_id_opt(), Some(42));
        assert_eq!(auth.effective_role, Role::Custom);
        assert_eq!(
            auth.custom_permissions,
            Some(vec![Permission::ProjectsRead])
        );
        assert!(auth.has_permission(&Permission::ProjectsRead));
        assert!(!auth.has_permission(&Permission::ProjectsWrite));
        assert!(matches!(auth.source, AuthSource::ApiKey { key_id: 9, .. }));
        drop(api);
        let sql = transaction_sql(db);
        assert!(sql.contains("\"api_keys\".\"is_active\" ="), "{sql}");
    }

    #[tokio::test]
    async fn revoked_api_key_fails_closed() {
        let crypto = crypto();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![user()]])
            .append_query_results([Vec::<temps_entities::api_keys::Model>::new()])
            .into_connection();
        let (api, db) = api_with_database(db, crypto.clone());
        let call = actor_call(crypto.as_ref(), ActorPrincipal::ApiKey { key_id: 9 });

        let error = api
            .resolve_actor(TEST_PLUGIN, &call)
            .await
            .expect_err("revoked API key must be rejected");
        assert_unauthenticated(error, "key 9, but that key is no longer active");
        drop(api);
        let sql = transaction_sql(db);
        assert!(sql.contains("\"api_keys\".\"is_active\" ="), "{sql}");
    }

    #[tokio::test]
    async fn expired_api_key_fails_closed() {
        let crypto = crypto();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![user()]])
            .append_query_results([vec![api_key(Some(Utc::now() - ChronoDuration::minutes(1)))]])
            .into_connection();
        let (api, _db) = api_with_database(db, crypto.clone());
        let call = actor_call(crypto.as_ref(), ActorPrincipal::ApiKey { key_id: 9 });

        let error = api
            .resolve_actor(TEST_PLUGIN, &call)
            .await
            .expect_err("expired API key must be rejected");
        assert_unauthenticated(error, "key 9, but that key has expired");
    }

    #[tokio::test]
    async fn deleted_or_missing_user_fails_closed_before_principal_lookup() {
        let crypto = crypto();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::users::Model>::new()])
            .into_connection();
        let (api, db) = api_with_database(db, crypto.clone());
        let call = actor_call(crypto.as_ref(), ActorPrincipal::Session { session_id: 7 });

        let error = api
            .resolve_actor(TEST_PLUGIN, &call)
            .await
            .expect_err("deleted user must be rejected");
        assert_unauthenticated(error, "user 42, but that user no longer exists");
        drop(api);
        let sql = transaction_sql(db);
        assert!(sql.contains("\"users\".\"deleted_at\" IS NULL"), "{sql}");
        assert!(!sql.contains("FROM \"sessions\""), "{sql}");
    }
}
