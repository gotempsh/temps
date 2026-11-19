// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ending Temps Cloud console sessions when Cloud stops trusting a user
//! (ADR-094).
//!
//! A sign-in through the Cloud-managed provider mints a local session that
//! lives for days, while Cloud's own tokens live two minutes. Two things end
//! those sessions early:
//!
//! - **A signed logout token** pushed by Cloud on the management channel: an
//!   OpenID Connect Back-Channel Logout 1.0 token, verified here against the
//!   same JWKS that signs Cloud ID tokens. The channel is only a carrier.
//! - **A periodic re-check**: the instance asks Cloud which of the users behind
//!   its live Cloud sessions may keep them, and ends the rest. If Cloud has not
//!   answered for [`CLOUD_SESSION_STALE_AFTER`], every Cloud session ends. The
//!   last answer's time is kept on the managed provider's row
//!   (`cloud_sessions_confirmed_at`), so a restart does not start that bound
//!   again.
//!
//! Only sessions whose origin is `oidc:<managed provider id>` are touched.
//! Password sessions, other providers and every workload are unaffected, and
//! an instance without the managed provider does nothing at all.

use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use openidconnect::core::{CoreJsonWebKey, CoreJwsSigningAlgorithm};
use openidconnect::JsonWebKey;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
use uuid::Uuid;

use super::{check_request_target, normalize_issuer_url, OidcService};
use crate::oidc_errors::OidcError;
use temps_entities::oidc_providers;

/// `events` member that makes a JWT a logout token.
pub const BACKCHANNEL_LOGOUT_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";
/// JOSE `typ` Cloud signs logout tokens with.
pub const LOGOUT_TOKEN_TYPE: &str = "logout+jwt";
/// How often the instance re-checks its live Cloud sessions with Cloud.
pub const CLOUD_SESSION_CHECK_INTERVAL: Duration = Duration::from_secs(60);
/// How long Cloud sessions survive without a successful re-check. Signing in
/// again needs Cloud anyway, so this ends nothing that was otherwise usable.
pub const CLOUD_SESSION_STALE_AFTER: Duration = Duration::from_secs(10 * 60);
/// Subjects per session-status request; Cloud refuses more.
const STATUS_BATCH: usize = 50;
/// Largest logout token considered.
const MAX_LOGOUT_TOKEN_BYTES: usize = 8192;
/// Clock skew allowed on a logout token's `iat`.
const MAX_IAT_SKEW_SECS: i64 = 300;

/// A verified logout token's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudLogoutApplied {
    pub jti: Uuid,
    pub sessions_ended: u32,
}

/// Why a logout token was not applied. `reason` is fixed text, safe to send
/// back to Cloud and to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudLogoutRefusal {
    /// Not even a request id could be read.
    Unreadable,
    Rejected {
        jti: Uuid,
        reason: &'static str,
    },
    /// Verified, but ending the sessions failed. Cloud sends it again.
    Failed {
        jti: Uuid,
        reason: String,
    },
}

/// Cloud's answer to a session re-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudSessionStatus {
    /// These subjects may keep their sessions; every other one may not.
    Active(Vec<String>),
    /// Cloud no longer recognises this instance's console client: every
    /// Cloud session must end.
    InvalidClient,
    /// No usable answer (network, timeout, 5xx, 429). Changes nothing yet.
    Unavailable(String),
}

/// What one re-check pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudSessionCheck {
    /// No managed provider, or no live Cloud sessions: nothing to ask.
    NothingToCheck,
    /// Cloud answered; `sessions_ended` sessions of no-longer-active users ended.
    Checked { sessions_ended: u64 },
    /// Cloud no longer knows this client; every Cloud session ended.
    ClientRevoked { sessions_ended: u64 },
    /// Cloud did not answer.
    Unavailable { reason: String },
}

fn b64(segment: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .ok()
}

fn origin(provider_id: i32) -> String {
    format!("oidc:{provider_id}")
}

/// `aud` is a string or an array of strings (RFC 7519 §4.1.3).
fn audience_contains(aud: &serde_json::Value, client_id: &str) -> bool {
    match aud {
        serde_json::Value::String(value) => value == client_id,
        serde_json::Value::Array(values) => values.iter().any(|v| v.as_str() == Some(client_id)),
        _ => false,
    }
}

/// Every claim check of Back-Channel Logout 1.0 §2.6 that does not need the
/// key, against the managed provider's issuer and client id. Returns the
/// subject.
pub(crate) fn validate_logout_claims(
    claims: &serde_json::Value,
    issuer: &str,
    client_id: &str,
    now: i64,
) -> Result<String, &'static str> {
    if claims.get("iss").and_then(|v| v.as_str()) != Some(issuer) {
        return Err("issuer does not match the Temps Cloud provider");
    }
    if !claims
        .get("aud")
        .is_some_and(|aud| audience_contains(aud, client_id))
    {
        return Err("audience is not this instance's console client");
    }
    if !claims
        .get("events")
        .and_then(|events| events.get(BACKCHANNEL_LOGOUT_EVENT))
        .is_some_and(serde_json::Value::is_object)
    {
        return Err("not a back-channel logout token");
    }
    if claims.get("nonce").is_some() {
        return Err("a logout token must not carry a nonce");
    }
    let Some(iat) = claims.get("iat").and_then(|v| v.as_i64()) else {
        return Err("missing iat");
    };
    if iat > now + MAX_IAT_SKEW_SECS {
        return Err("issued in the future");
    }
    match claims.get("exp").and_then(|v| v.as_i64()) {
        Some(exp) if exp > now => {}
        Some(_) => return Err("expired"),
        None => return Err("missing exp"),
    }
    match claims.get("sub").and_then(|v| v.as_str()) {
        Some(sub) if !sub.is_empty() && sub.len() <= 255 => Ok(sub.to_string()),
        _ => Err("missing subject"),
    }
}

impl OidcService {
    /// Verify a Cloud logout token and end that user's Cloud sessions here.
    pub async fn apply_cloud_logout_token(
        &self,
        token: &str,
    ) -> Result<CloudLogoutApplied, CloudLogoutRefusal> {
        if token.len() > MAX_LOGOUT_TOKEN_BYTES {
            return Err(CloudLogoutRefusal::Unreadable);
        }
        let mut parts = token.split('.');
        let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(CloudLogoutRefusal::Unreadable);
        };
        let claims: serde_json::Value = b64(payload_b64)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(CloudLogoutRefusal::Unreadable)?;
        let jti = claims
            .get("jti")
            .and_then(|v| v.as_str())
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or(CloudLogoutRefusal::Unreadable)?;
        let reject = |reason: &'static str| CloudLogoutRefusal::Rejected { jti, reason };

        let header: serde_json::Value = b64(header_b64)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| reject("unreadable header"))?;
        if header.get("alg").and_then(|v| v.as_str()) != Some("RS256") {
            return Err(reject("unsupported signing algorithm"));
        }
        if header.get("typ").and_then(|v| v.as_str()) != Some(LOGOUT_TOKEN_TYPE) {
            return Err(reject("not a logout+jwt token"));
        }
        let kid = header
            .get("kid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| reject("missing key id"))?
            .to_string();
        let signature = b64(signature_b64).ok_or_else(|| reject("unreadable signature"))?;

        let provider = self
            .managed_cloud_provider()
            .await
            .map_err(|error| CloudLogoutRefusal::Failed {
                jti,
                reason: error.to_string(),
            })?
            .filter(|provider| provider.enabled)
            .ok_or_else(|| reject("no Temps Cloud console provider on this instance"))?;

        // Find the key in the cached JWKS; refresh once for a rotated key.
        let mut key = None;
        let mut issuer = String::new();
        for force_refresh in [false, true] {
            let metadata = self
                .fetch_provider_metadata(&provider, force_refresh)
                .await
                .map_err(|error| CloudLogoutRefusal::Failed {
                    jti,
                    reason: error.to_string(),
                })?;
            issuer = metadata.issuer().as_str().to_string();
            key = metadata
                .jwks()
                .keys()
                .iter()
                .find(|candidate: &&CoreJsonWebKey| {
                    candidate.key_id().map(|id| id.as_str()) == Some(kid.as_str())
                })
                .cloned();
            if key.is_some() {
                break;
            }
        }
        let key = key.ok_or_else(|| reject("signing key is not published by Temps Cloud"))?;
        let signing_input = format!("{header_b64}.{payload_b64}");
        key.verify_signature(
            &CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
            signing_input.as_bytes(),
            &signature,
        )
        .map_err(|_| reject("signature does not verify"))?;

        let subject = validate_logout_claims(
            &claims,
            &issuer,
            &provider.client_id,
            Utc::now().timestamp(),
        )
        .map_err(reject)?;

        let sessions_ended = self
            .end_cloud_sessions(provider.id, Some(std::slice::from_ref(&subject)))
            .await
            .map_err(|error| CloudLogoutRefusal::Failed {
                jti,
                reason: error.to_string(),
            })?;
        tracing::info!(
            target: "temps_auth::oidc",
            provider_id = provider.id,
            request_id = %jti,
            sessions_ended,
            "ended Temps Cloud console sessions for a user Cloud no longer trusts"
        );
        Ok(CloudLogoutApplied {
            jti,
            sessions_ended: u32::try_from(sessions_ended).unwrap_or(u32::MAX),
        })
    }

    /// Delete Cloud-issued sessions of the users linked to `subjects` through
    /// managed provider `provider_id` (all of them for `None`), and revoke
    /// those users' ingress login grants, in one transaction. Returns how many
    /// sessions went.
    pub async fn end_cloud_sessions(
        &self,
        provider_id: i32,
        subjects: Option<&[String]>,
    ) -> Result<u64, OidcError> {
        let txn = self.db.begin().await?;
        let session_origin = origin(provider_id);
        let users: Vec<i32> = match subjects {
            Some(subjects) => {
                let rows = txn
                    .query_all(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT DISTINCT s.user_id FROM sessions s
                         JOIN users u ON u.id = s.user_id
                         WHERE s.mfa_pending_origin = $1
                           AND u.oidc_provider_id = $2 AND u.oidc_subject = ANY($3)",
                        vec![
                            session_origin.clone().into(),
                            provider_id.into(),
                            subjects.to_vec().into(),
                        ],
                    ))
                    .await?;
                rows.iter()
                    .map(|row| row.try_get::<i32>("", "user_id"))
                    .collect::<Result<_, _>>()?
            }
            None => {
                let rows = txn
                    .query_all(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT DISTINCT user_id FROM sessions WHERE mfa_pending_origin = $1",
                        vec![session_origin.clone().into()],
                    ))
                    .await?;
                rows.iter()
                    .map(|row| row.try_get::<i32>("", "user_id"))
                    .collect::<Result<_, _>>()?
            }
        };
        if users.is_empty() {
            txn.commit().await?;
            return Ok(0);
        }
        let deleted = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM sessions WHERE mfa_pending_origin = $1 AND user_id = ANY($2)",
                vec![session_origin.into(), users.clone().into()],
            ))
            .await?
            .rows_affected();
        crate::ingress_grants::revoke_for_users(&txn, &users).await?;
        txn.commit().await?;
        Ok(deleted)
    }

    /// Subjects behind live Cloud-issued sessions. Cloud-issued sessions of a
    /// user no longer linked to a Cloud subject are ended on the spot, since
    /// nobody can vouch for them; the second value counts those.
    async fn live_cloud_subjects(&self, provider_id: i32) -> Result<(Vec<String>, u64), OidcError> {
        let linked = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT u.oidc_subject AS subject
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.mfa_pending_origin = $1 AND s.expires_at > now()
                   AND u.oidc_provider_id = $2 AND u.oidc_subject IS NOT NULL",
                vec![origin(provider_id).into(), provider_id.into()],
            ))
            .await?;
        let subjects = linked
            .iter()
            .map(|row| row.try_get::<String>("", "subject"))
            .collect::<Result<Vec<_>, _>>()?;
        // Their ingress login grants go with them, as in `end_cloud_sessions`.
        let txn = self.db.begin().await?;
        let orphaned_users: Vec<i32> = txn
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT s.user_id FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.mfa_pending_origin = $1
                   AND (u.oidc_provider_id IS DISTINCT FROM $2 OR u.oidc_subject IS NULL)",
                vec![origin(provider_id).into(), provider_id.into()],
            ))
            .await?
            .iter()
            .map(|row| row.try_get::<i32>("", "user_id"))
            .collect::<Result<_, _>>()?;
        let mut orphaned = 0;
        if !orphaned_users.is_empty() {
            orphaned = txn
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM sessions WHERE mfa_pending_origin = $1 AND user_id = ANY($2)",
                    vec![origin(provider_id).into(), orphaned_users.clone().into()],
                ))
                .await?
                .rows_affected();
            crate::ingress_grants::revoke_for_users(&txn, &orphaned_users).await?;
        }
        txn.commit().await?;
        Ok((subjects, orphaned))
    }

    /// Ask Cloud which `subjects` may keep their sessions.
    async fn cloud_session_status(
        &self,
        provider: &oidc_providers::Model,
        subjects: &[String],
    ) -> CloudSessionStatus {
        let issuer = match normalize_issuer_url(&provider.issuer_url) {
            Ok(issuer) => issuer,
            Err(error) => return CloudSessionStatus::Unavailable(error.to_string()),
        };
        let url = format!("{}/console/session-status", issuer.trim_end_matches('/'));
        match url.parse::<openidconnect::http::Uri>() {
            Ok(uri) => {
                if let Err(reason) = check_request_target(&uri) {
                    return CloudSessionStatus::Unavailable(reason);
                }
            }
            Err(error) => return CloudSessionStatus::Unavailable(error.to_string()),
        }
        let client = match &self.http_client.inner {
            Ok(client) => client,
            Err(reason) => return CloudSessionStatus::Unavailable(reason.clone()),
        };
        let secret = match self
            .encryption_service
            .decrypt_string(&provider.client_secret_encrypted)
        {
            Ok(secret) => secret,
            Err(error) => {
                return CloudSessionStatus::Unavailable(format!(
                    "could not decrypt the Temps Cloud client secret: {error}"
                ))
            }
        };
        let response = match client
            .post(&url)
            .basic_auth(&provider.client_id, Some(secret))
            .json(&serde_json::json!({ "subjects": subjects }))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => return CloudSessionStatus::Unavailable(error.to_string()),
        };
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return CloudSessionStatus::InvalidClient;
        }
        if !status.is_success() {
            return CloudSessionStatus::Unavailable(format!("Temps Cloud answered HTTP {status}"));
        }
        #[derive(serde::Deserialize)]
        struct Answer {
            active: Vec<String>,
        }
        match response.json::<Answer>().await {
            Ok(answer) => CloudSessionStatus::Active(answer.active),
            Err(error) => {
                CloudSessionStatus::Unavailable(format!("unreadable Temps Cloud answer: {error}"))
            }
        }
    }

    /// One re-check pass (ADR-094 §4): ask Cloud about every user behind a
    /// live Cloud session and end the sessions of those it no longer lists.
    /// Every pass but an unanswered one records the confirmation on the
    /// managed provider's row ([`Self::cloud_sessions_confirmed_at`]).
    pub async fn check_cloud_sessions(&self) -> Result<CloudSessionCheck, OidcError> {
        let Some(provider) = self
            .managed_cloud_provider()
            .await?
            .filter(|provider| provider.enabled)
        else {
            return Ok(CloudSessionCheck::NothingToCheck);
        };
        let check = self.check_provider_sessions(&provider).await?;
        if !matches!(check, CloudSessionCheck::Unavailable { .. }) {
            self.record_cloud_sessions_confirmed(provider.id).await;
        }
        Ok(check)
    }

    /// When Cloud last confirmed this instance's Cloud sessions (answered a
    /// re-check, or there was nothing to ask it), as recorded on the managed
    /// provider's row, so it survives a restart. `None` without the managed
    /// provider or without a confirmation on record.
    pub async fn cloud_sessions_confirmed_at(
        &self,
    ) -> Result<Option<chrono::DateTime<Utc>>, OidcError> {
        Ok(self
            .managed_cloud_provider()
            .await?
            .and_then(|provider| provider.cloud_sessions_confirmed_at))
    }

    /// Record that Cloud confirmed the sessions of managed provider
    /// `provider_id` now. Not through the active model: the provider's
    /// `updated_at` stays the time its configuration changed. A failure is
    /// logged, never fatal: the sessions were confirmed, and a missed record
    /// only makes a later outage end them sooner.
    async fn record_cloud_sessions_confirmed(&self, provider_id: i32) {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        if let Err(error) = oidc_providers::Entity::update_many()
            .col_expr(
                oidc_providers::Column::CloudSessionsConfirmedAt,
                Expr::value(Utc::now()),
            )
            .filter(oidc_providers::Column::Id.eq(provider_id))
            .exec(self.db.as_ref())
            .await
        {
            tracing::warn!(
                provider_id,
                %error,
                "Temps Cloud confirmed the console sessions, but recording the confirmation on \
                 OIDC provider {provider_id} failed; a Cloud outage after a restart may end them \
                 sooner"
            );
        }
    }

    /// [`Self::check_cloud_sessions`] for the enabled managed `provider`.
    async fn check_provider_sessions(
        &self,
        provider: &oidc_providers::Model,
    ) -> Result<CloudSessionCheck, OidcError> {
        let (subjects, orphaned) = self.live_cloud_subjects(provider.id).await?;
        if subjects.is_empty() {
            return Ok(if orphaned > 0 {
                CloudSessionCheck::Checked {
                    sessions_ended: orphaned,
                }
            } else {
                CloudSessionCheck::NothingToCheck
            });
        }
        let mut inactive = Vec::new();
        for batch in subjects.chunks(STATUS_BATCH) {
            match self.cloud_session_status(provider, batch).await {
                CloudSessionStatus::Active(active) => inactive.extend(
                    batch
                        .iter()
                        .filter(|subject| !active.contains(subject))
                        .cloned(),
                ),
                CloudSessionStatus::InvalidClient => {
                    let sessions_ended = self.end_cloud_sessions(provider.id, None).await?;
                    return Ok(CloudSessionCheck::ClientRevoked { sessions_ended });
                }
                CloudSessionStatus::Unavailable(reason) => {
                    return Ok(CloudSessionCheck::Unavailable { reason });
                }
            }
        }
        let sessions_ended = if inactive.is_empty() {
            0
        } else {
            self.end_cloud_sessions(provider.id, Some(&inactive))
                .await?
        };
        Ok(CloudSessionCheck::Checked {
            sessions_ended: sessions_ended + orphaned,
        })
    }

    /// End every Cloud-issued session (the fail-closed bound). Returns how
    /// many went; zero without a managed provider.
    pub async fn end_all_cloud_sessions(&self) -> Result<u64, OidcError> {
        match self.managed_cloud_provider().await? {
            Some(provider) => self.end_cloud_sessions(provider.id, None).await,
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUER: &str = "https://cloud.example/oidc";
    const CLIENT: &str = "11111111-1111-1111-1111-111111111111";

    fn claims(now: i64) -> serde_json::Value {
        serde_json::json!({
            "iss": ISSUER, "aud": CLIENT, "sub": "user-1", "iat": now, "exp": now + 900,
            "jti": Uuid::nil(), "events": { BACKCHANNEL_LOGOUT_EVENT: {} },
        })
    }

    #[test]
    fn a_well_formed_logout_token_names_its_subject() {
        let now = 1_000_000;
        assert_eq!(
            validate_logout_claims(&claims(now), ISSUER, CLIENT, now),
            Ok("user-1".into())
        );
        let mut array_aud = claims(now);
        array_aud["aud"] = serde_json::json!(["other", CLIENT]);
        assert!(validate_logout_claims(&array_aud, ISSUER, CLIENT, now).is_ok());
    }

    /// Breaks one claim rule in a valid token's claims.
    type Mutation = Box<dyn Fn(&mut serde_json::Value)>;

    #[test]
    fn every_back_channel_logout_claim_rule_is_enforced() {
        let now = 1_000_000;
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "issuer",
                Box::new(|c| c["iss"] = "https://evil.example/oidc".into()),
            ),
            (
                "audience",
                Box::new(|c| c["aud"] = "another-instance".into()),
            ),
            ("events", Box::new(|c| c["events"] = serde_json::json!({}))),
            ("nonce", Box::new(|c| c["nonce"] = "n".into())),
            (
                "future iat",
                Box::new(move |c| c["iat"] = (now + 3600).into()),
            ),
            ("expired", Box::new(move |c| c["exp"] = (now - 1).into())),
            (
                "no exp",
                Box::new(|c| {
                    c.as_object_mut().unwrap().remove("exp");
                }),
            ),
            (
                "no sub",
                Box::new(|c| {
                    c.as_object_mut().unwrap().remove("sub");
                }),
            ),
            ("empty sub", Box::new(|c| c["sub"] = "".into())),
        ];
        for (name, mutate) in cases {
            let mut c = claims(now);
            mutate(&mut c);
            assert!(
                validate_logout_claims(&c, ISSUER, CLIENT, now).is_err(),
                "{name} must be refused"
            );
        }
    }

    // ── Database-backed: the real verifier and session ender ─────────
    //
    // Docker-dependent; skips when Docker is unavailable, like the rest of
    // this crate's database tests.

    use std::sync::Arc;

    use chrono::Duration as ChronoDuration;
    use openidconnect::core::{
        CoreJsonWebKeySet, CoreProviderMetadata, CoreResponseType, CoreRsaPrivateSigningKey,
        CoreSubjectIdentifierType,
    };
    use openidconnect::{
        AuthUrl, EmptyAdditionalProviderMetadata, IssuerUrl, JsonWebKeyId, JsonWebKeySetUrl,
        PrivateSigningKey, ResponseTypes,
    };
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{sessions, users};

    use crate::oidc_service::ManagedCloudOidcConfig;
    use crate::user_service::UserService;

    const CLOUD_ISSUER: &str = "https://cloud.example.com";
    const CLOUD_CLIENT: &str = "cloud-client-id";
    const KID: &str = "cloud-key-1";

    fn rsa_key(kid: &str) -> CoreRsaPrivateSigningKey {
        let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).expect("rsa key");
        let pem = private
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("pem");
        CoreRsaPrivateSigningKey::from_pem(&pem, Some(JsonWebKeyId::new(kid.to_string())))
            .expect("signing key")
    }

    fn jwt(
        key: &CoreRsaPrivateSigningKey,
        header: serde_json::Value,
        claims: &serde_json::Value,
    ) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let input = format!(
            "{}.{}",
            engine.encode(serde_json::to_vec(&header).unwrap()),
            engine.encode(serde_json::to_vec(claims).unwrap())
        );
        let signature = key
            .sign(
                &CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
                input.as_bytes(),
            )
            .expect("sign");
        format!("{input}.{}", engine.encode(signature))
    }

    fn logout_header() -> serde_json::Value {
        serde_json::json!({ "alg": "RS256", "typ": LOGOUT_TOKEN_TYPE, "kid": KID })
    }

    fn logout_claims(sub: &str, jti: Uuid) -> serde_json::Value {
        let now = Utc::now().timestamp();
        serde_json::json!({
            "iss": CLOUD_ISSUER, "aud": CLOUD_CLIENT, "sub": sub, "iat": now, "exp": now + 900,
            "jti": jti, "events": { BACKCHANNEL_LOGOUT_EVENT: {} },
        })
    }

    async fn user(
        db: &sea_orm::DatabaseConnection,
        name: &str,
        link: Option<(i32, &str)>,
    ) -> users::Model {
        users::ActiveModel {
            name: Set(name.to_string()),
            email: Set(format!("{name}@example.com")),
            email_verified: Set(true),
            oidc_provider_id: Set(link.map(|(provider, _)| provider)),
            oidc_subject: Set(link.map(|(_, subject)| subject.to_string())),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("user must insert")
    }

    async fn session(
        db: &sea_orm::DatabaseConnection,
        user_id: i32,
        origin: Option<String>,
    ) -> i32 {
        sessions::ActiveModel {
            user_id: Set(user_id),
            session_token: Set(Uuid::new_v4().to_string()),
            expires_at: Set(Utc::now() + ChronoDuration::days(7)),
            mfa_pending: Set(false),
            mfa_pending_origin: Set(origin),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("session must insert")
        .id
    }

    async fn exists(db: &sea_orm::DatabaseConnection, session_id: i32) -> bool {
        sessions::Entity::find()
            .filter(sessions::Column::Id.eq(session_id))
            .one(db)
            .await
            .expect("query")
            .is_some()
    }

    #[tokio::test]
    async fn a_signed_logout_ends_only_that_users_cloud_sessions() {
        let db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Docker not available, skipping test: {e}");
                return;
            }
        };
        let conn = db.db.as_ref();
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "cloud-session-tests",
        ));
        let user_service = Arc::new(UserService::new(db.db.clone(), encryption.clone()));
        let service = OidcService::new(db.db.clone(), encryption, user_service);
        let provider = service
            .upsert_managed_cloud_provider(ManagedCloudOidcConfig {
                issuer: CLOUD_ISSUER.to_string(),
                client_id: CLOUD_CLIENT.to_string(),
                client_secret: "cloud-client-secret".to_string(),
            })
            .await
            .expect("managed provider");
        let key = rsa_key(KID);
        let metadata = CoreProviderMetadata::new(
            IssuerUrl::new(CLOUD_ISSUER.to_string()).unwrap(),
            AuthUrl::new(format!("{CLOUD_ISSUER}/authorize")).unwrap(),
            JsonWebKeySetUrl::new(format!("{CLOUD_ISSUER}/jwks")).unwrap(),
            vec![ResponseTypes::new(vec![CoreResponseType::Code])],
            vec![CoreSubjectIdentifierType::Public],
            vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
            EmptyAdditionalProviderMetadata {},
        )
        .set_jwks(CoreJsonWebKeySet::new(vec![key.as_verification_key()]));
        service
            .seed_discovery_for_test(&provider, metadata, "cloud-client-secret")
            .await;

        let cloud_origin = Some(format!("oidc:{}", provider.id));
        let alice = user(conn, "alice", Some((provider.id, "cloud-alice"))).await;
        let alice_cloud = session(conn, alice.id, cloud_origin.clone()).await;
        let alice_local = session(conn, alice.id, None).await;
        let alice_grant = crate::ingress_grants::test_support::live_grant(&db.db, alice.id).await;
        let bob = user(conn, "bob", Some((provider.id, "cloud-bob"))).await;
        let bob_cloud = session(conn, bob.id, cloud_origin.clone()).await;
        let bob_grant = crate::ingress_grants::test_support::live_grant(&db.db, bob.id).await;

        // Refused: each one changes nothing.
        let jti = Uuid::new_v4();
        let refused = [
            (
                "wrong audience",
                jwt(&key, logout_header(), &{
                    let mut c = logout_claims("cloud-alice", jti);
                    c["aud"] = "another-instance".into();
                    c
                }),
            ),
            (
                "an ID-token type",
                jwt(
                    &key,
                    serde_json::json!({ "alg": "RS256", "typ": "JWT", "kid": KID }),
                    &logout_claims("cloud-alice", jti),
                ),
            ),
            (
                "a key Cloud never published",
                jwt(
                    &rsa_key(KID),
                    logout_header(),
                    &logout_claims("cloud-alice", jti),
                ),
            ),
            (
                "a nonce",
                jwt(&key, logout_header(), &{
                    let mut c = logout_claims("cloud-alice", jti);
                    c["nonce"] = "n".into();
                    c
                }),
            ),
        ];
        for (name, token) in refused {
            match service.apply_cloud_logout_token(&token).await {
                Err(CloudLogoutRefusal::Rejected { jti: got, .. }) => {
                    assert_eq!(got, jti, "{name}")
                }
                other => panic!("{name} must be rejected, got {other:?}"),
            }
        }
        assert!(exists(conn, alice_cloud).await);
        assert_eq!(
            service.apply_cloud_logout_token("not-a-jwt").await,
            Err(CloudLogoutRefusal::Unreadable)
        );

        // Accepted: Alice's Cloud session and grant go; her password session
        // and Bob stay.
        let applied = service
            .apply_cloud_logout_token(&jwt(
                &key,
                logout_header(),
                &logout_claims("cloud-alice", jti),
            ))
            .await
            .expect("a genuine logout token applies");
        assert_eq!(
            applied,
            CloudLogoutApplied {
                jti,
                sessions_ended: 1
            }
        );
        assert!(!exists(conn, alice_cloud).await);
        assert!(
            exists(conn, alice_local).await,
            "local logins are never touched"
        );
        assert!(crate::ingress_grants::test_support::is_revoked(&db.db, alice_grant).await);
        assert!(exists(conn, bob_cloud).await);
        assert!(!crate::ingress_grants::test_support::is_revoked(&db.db, bob_grant).await);

        // A Cloud-issued session of a user no longer linked to a subject is
        // ended at the next check; linked users are reported to Cloud.
        let carol = user(conn, "carol", None).await;
        let carol_cloud = session(conn, carol.id, cloud_origin.clone()).await;
        let carol_local = session(conn, carol.id, None).await;
        let (subjects, orphaned) = service
            .live_cloud_subjects(provider.id)
            .await
            .expect("live subjects");
        assert_eq!(subjects, vec!["cloud-bob".to_string()]);
        assert_eq!(orphaned, 1);
        assert!(!exists(conn, carol_cloud).await);
        assert!(exists(conn, carol_local).await);

        // Fail closed: every Cloud session ends, local sessions stay.
        assert_eq!(service.end_all_cloud_sessions().await.expect("end all"), 1);
        assert!(!exists(conn, bob_cloud).await);
        assert!(crate::ingress_grants::test_support::is_revoked(&db.db, bob_grant).await);
        assert!(exists(conn, alice_local).await);
        assert_eq!(
            service.cloud_sessions_confirmed_at().await.expect("read"),
            None,
            "no confirmation is on record before a check"
        );
        let before = Utc::now();
        assert_eq!(
            service.check_cloud_sessions().await.expect("check"),
            CloudSessionCheck::NothingToCheck,
            "with no Cloud sessions left, nothing is sent to Cloud"
        );
        // Nothing to confirm is a confirmation, kept on the provider's row
        // without touching its configuration time.
        let confirmed = service
            .cloud_sessions_confirmed_at()
            .await
            .expect("read")
            .expect("recorded");
        assert!(confirmed >= before, "{confirmed} < {before}");
        let row = oidc_providers::Entity::find_by_id(provider.id)
            .one(conn)
            .await
            .expect("query")
            .expect("provider");
        assert_eq!(row.updated_at, provider.updated_at);

        // Cloud unreachable: the pass records nothing, so the confirmation
        // keeps its time.
        bob_unconfirmed(conn, &provider).await;
        assert!(matches!(
            service.check_cloud_sessions().await.expect("check"),
            CloudSessionCheck::Unavailable { .. }
        ));
        assert_eq!(
            service.cloud_sessions_confirmed_at().await.expect("read"),
            Some(confirmed)
        );
    }

    /// Give Bob a live Cloud session again and point the provider at an
    /// address the instance refuses to call, so a re-check is unanswered.
    async fn bob_unconfirmed(conn: &sea_orm::DatabaseConnection, provider: &oidc_providers::Model) {
        let bob = users::Entity::find()
            .filter(users::Column::OidcSubject.eq("cloud-bob"))
            .one(conn)
            .await
            .expect("query")
            .expect("bob");
        session(conn, bob.id, Some(format!("oidc:{}", provider.id))).await;
        oidc_providers::Entity::update_many()
            .col_expr(
                oidc_providers::Column::IssuerUrl,
                sea_orm::sea_query::Expr::value("https://127.0.0.1"),
            )
            .filter(oidc_providers::Column::Id.eq(provider.id))
            .exec(conn)
            .await
            .expect("point the provider at a refused address");
    }
}
