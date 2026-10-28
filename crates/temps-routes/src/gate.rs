// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's half of the Temps gate (ADR 065 D8).
//!
//! - the gate key: one Ed25519 key, generated on first use, the private half
//!   sealed with `EncryptionService`;
//! - sign-in codes: single use, 60 seconds, bound to the address, the console
//!   session and the PKCE challenge, stored by hash;
//! - redeeming a code into a gate session and its signed token;
//! - logout and "Sign out of apps";
//! - the revocation data the route snapshot carries: which live sessions no
//!   longer hold, worked out from the database each reload, so removing
//!   someone from the project or its teams, deleting the user, or ending the
//!   console session revokes their gate sessions without any hook.
//!
//! The console's authorize endpoint (which checks who may sign in) lives with
//! the reachability handlers; this module is what every listener needs.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set,
    Statement,
};
use sha2::{Digest, Sha256};
use temps_core::gate::{
    self, GateAllowList, GateClaims, GateError, GateKind, GateLogoutRequest, GateRedeemRequest,
    GateRedeemResponse, SigningKey,
};
use temps_core::EncryptionService;
use temps_entities::{
    audience, environment_domains, gate_codes, gate_keys, gate_sessions, project_custom_domains,
};
use thiserror::Error;
use tokio::sync::OnceCell;
use tracing::{debug, info};

#[derive(Debug, Error)]
pub enum GateServiceError {
    #[error("Database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: DbErr,
    },
    #[error("Gate key could not be {operation}: {reason}")]
    Key {
        operation: &'static str,
        reason: String,
    },
    #[error("Address id '{address_id}' is not a custom or environment domain id")]
    BadAddressId { address_id: String },
    #[error("Address {address_id} does not exist")]
    AddressNotFound { address_id: String },
    #[error("Address {address_id} ({hostname}) is not a signed-in address with the Temps sign-in")]
    NotGated {
        address_id: String,
        hostname: String,
    },
    #[error("Sign-in code for address {address_id} is invalid, expired or already used")]
    InvalidCode { address_id: String },
    #[error(
        "The console session that signed in to address {address_id} has ended (user {user_id})"
    )]
    SessionEnded { address_id: String, user_id: i32 },
    #[error("Gate token for address {address_id} is not valid")]
    InvalidToken { address_id: String },
    #[error("Invalid sign-in request for address {address_id}: {message}")]
    Validation { address_id: String, message: String },
    #[error("User {user_id} may not sign in to address {address_id}: {reason}")]
    NotAllowed {
        address_id: String,
        user_id: i32,
        reason: &'static str,
    },
    #[error(transparent)]
    Gate(#[from] GateError),
}

impl GateServiceError {
    /// The HTTP status a listener or the node channel answers with.
    pub fn http_status(&self) -> u16 {
        match self {
            GateServiceError::InvalidCode { .. }
            | GateServiceError::SessionEnded { .. }
            | GateServiceError::InvalidToken { .. }
            | GateServiceError::Validation { .. }
            | GateServiceError::BadAddressId { .. } => 400,
            GateServiceError::NotAllowed { .. } => 403,
            GateServiceError::AddressNotFound { .. } => 404,
            GateServiceError::NotGated { .. } => 409,
            GateServiceError::Database { .. }
            | GateServiceError::Key { .. }
            | GateServiceError::Gate(_) => 500,
        }
    }

    /// What the visitor reads on the sign-in failure page. Server-side
    /// failures are described without their internals, which go to the log.
    pub fn visitor_message(&self) -> String {
        match self {
            GateServiceError::InvalidCode { .. } => {
                "This sign-in link expired or was already used. Open the address again to sign in."
                    .to_string()
            }
            GateServiceError::SessionEnded { .. } => {
                "Your console session ended while signing in. Open the address again to sign in."
                    .to_string()
            }
            GateServiceError::NotGated { hostname, .. } => {
                format!("{hostname} no longer asks for sign-in. Open the address again.")
            }
            GateServiceError::NotAllowed { reason, .. } => {
                format!("You cannot open this address: {reason}. Ask a project admin for access.")
            }
            GateServiceError::InvalidToken { .. }
            | GateServiceError::Validation { .. }
            | GateServiceError::BadAddressId { .. }
            | GateServiceError::AddressNotFound { .. } => {
                "This sign-in could not be completed. Open the address again to sign in."
                    .to_string()
            }
            GateServiceError::Database { .. }
            | GateServiceError::Key { .. }
            | GateServiceError::Gate(_) => {
                "Sign-in failed on the control plane. Try again in a moment.".to_string()
            }
        }
    }
}

fn db(operation: &'static str) -> impl FnOnce(DbErr) -> GateServiceError {
    move |source| GateServiceError::Database { operation, source }
}

/// A signed-in address, as the gate needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatedAddress {
    pub address_id: String,
    pub hostname: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub audience: String,
    pub gate: GateKind,
    pub allow: GateAllowList,
}

impl GatedAddress {
    /// Whether this address has the Temps gate in force.
    pub fn has_temps_gate(&self) -> bool {
        self.audience == audience::SIGNED_IN && self.gate == GateKind::Temps
    }
}

/// Which domain row an address id names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressRef {
    Custom(i32),
    Environment(i32),
}

impl AddressRef {
    pub fn parse(address_id: &str) -> Option<AddressRef> {
        let (kind, id) = address_id.split_once(':')?;
        let id: i32 = id.parse().ok().filter(|id| *id > 0)?;
        match kind {
            "custom" => Some(AddressRef::Custom(id)),
            "environment" => Some(AddressRef::Environment(id)),
            _ => None,
        }
    }
}

/// Parse a stored `gate_allow` value. Anything unreadable is treated as a
/// list nobody is on: fail closed rather than widen to the whole project.
pub fn parse_allow(value: Option<&serde_json::Value>) -> GateAllowList {
    match value {
        None | Some(serde_json::Value::Null) => GateAllowList::default(),
        Some(value) => serde_json::from_value::<GateAllowList>(value.clone())
            .map(GateAllowList::normalized)
            .unwrap_or(GateAllowList {
                teams: vec![],
                users: vec![0],
            }),
    }
}

fn sha256_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

/// The console's authorize request, after authentication.
#[derive(Debug, Clone, Copy)]
pub struct AuthorizeRequest<'a> {
    pub address_id: &'a str,
    pub user_id: i32,
    /// The persisted console session (MFA complete) the code is bound to.
    pub console_session_id: i32,
    pub is_admin: bool,
    pub state: &'a str,
    pub code_challenge: &'a str,
}

pub struct GateService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    key: OnceCell<Arc<SigningKey>>,
}

impl GateService {
    pub fn new(db: Arc<DatabaseConnection>, encryption: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption,
            key: OnceCell::new(),
        }
    }

    /// The gate key, generated and stored on first use. Two processes racing
    /// to create it both read back the one row that won.
    pub async fn signing_key(&self) -> Result<Arc<SigningKey>, GateServiceError> {
        self.key
            .get_or_try_init(|| async { self.load_or_create_key().await.map(Arc::new) })
            .await
            .cloned()
    }

    /// The public half, base64url, creating the key if needed.
    pub async fn ensure_public_key(&self) -> Result<String, GateServiceError> {
        let key = self.signing_key().await?;
        Ok(gate::encode_public_key(&key.verifying_key()))
    }

    async fn load_or_create_key(&self) -> Result<SigningKey, GateServiceError> {
        if let Some(row) = gate_keys::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .map_err(db("reading the gate key"))?
        {
            return self.open_key(&row);
        }
        let key = gate::generate_signing_key()?;
        let sealed =
            self.encryption
                .encrypt(&key.to_bytes())
                .map_err(|error| GateServiceError::Key {
                    operation: "sealed",
                    reason: error.to_string(),
                })?;
        let public = gate::encode_public_key(&key.verifying_key());
        self.db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "INSERT INTO gate_keys (id, public_key, private_key_encrypted) VALUES (1, $1, $2) \
                 ON CONFLICT (id) DO NOTHING",
                [public.into(), sealed.into()],
            ))
            .await
            .map_err(db("storing the gate key"))?;
        let row = gate_keys::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .map_err(db("reading the gate key back"))?
            .ok_or_else(|| GateServiceError::Key {
                operation: "stored",
                reason: "the gate_keys row is missing right after it was written".into(),
            })?;
        info!("generated the signed-in gate key");
        self.open_key(&row)
    }

    fn open_key(&self, row: &gate_keys::Model) -> Result<SigningKey, GateServiceError> {
        let secret = self
            .encryption
            .decrypt(&row.private_key_encrypted)
            .map_err(|error| GateServiceError::Key {
                operation: "unsealed",
                reason: error.to_string(),
            })?;
        let key = gate::signing_key_from_bytes(&secret)?;
        if gate::encode_public_key(&key.verifying_key()) != row.public_key {
            return Err(GateServiceError::Key {
                operation: "verified",
                reason: "the sealed private key does not match the stored public key".into(),
            });
        }
        Ok(key)
    }

    /// Look up the address an id names.
    pub async fn address(&self, address_id: &str) -> Result<GatedAddress, GateServiceError> {
        let not_found = || GateServiceError::AddressNotFound {
            address_id: address_id.to_string(),
        };
        match AddressRef::parse(address_id).ok_or_else(|| GateServiceError::BadAddressId {
            address_id: address_id.to_string(),
        })? {
            AddressRef::Custom(id) => {
                let row = project_custom_domains::Entity::find_by_id(id)
                    .one(self.db.as_ref())
                    .await
                    .map_err(db("reading a custom domain for the gate"))?
                    .ok_or_else(not_found)?;
                Ok(GatedAddress {
                    address_id: address_id.to_string(),
                    hostname: row.domain.to_ascii_lowercase(),
                    project_id: row.project_id,
                    environment_id: row.environment_id,
                    gate: row
                        .gate
                        .as_deref()
                        .map_or(GateKind::Unsupported, GateKind::parse),
                    allow: parse_allow(row.gate_allow.as_ref()),
                    audience: row.audience,
                })
            }
            AddressRef::Environment(id) => {
                let row = environment_domains::Entity::find_by_id(id)
                    .one(self.db.as_ref())
                    .await
                    .map_err(db("reading an environment domain for the gate"))?
                    .ok_or_else(not_found)?;
                let environment =
                    temps_entities::environments::Entity::find_by_id(row.environment_id)
                        .one(self.db.as_ref())
                        .await
                        .map_err(db("reading the environment of a gated address"))?
                        .ok_or_else(not_found)?;
                Ok(GatedAddress {
                    address_id: address_id.to_string(),
                    hostname: row.domain.to_ascii_lowercase(),
                    project_id: environment.project_id,
                    environment_id: row.environment_id,
                    gate: row
                        .gate
                        .as_deref()
                        .map_or(GateKind::Unsupported, GateKind::parse),
                    allow: parse_allow(row.gate_allow.as_ref()),
                    audience: row.audience,
                })
            }
        }
    }

    /// The teams `user_id` belongs to, for an address's list.
    pub async fn user_team_ids(&self, user_id: i32) -> Result<Vec<i32>, GateServiceError> {
        use temps_entities::team_members;
        Ok(team_members::Entity::find()
            .filter(team_members::Column::UserId.eq(user_id))
            .all(self.db.as_ref())
            .await
            .map_err(db("reading the user's teams for the gate"))?
            .into_iter()
            .map(|m| m.team_id)
            .collect())
    }

    /// Issue a sign-in code for an address the caller has already checked
    /// the user may open. Returns the code; [`Self::authorize`] is the
    /// checked entry point.
    pub async fn issue_code(
        &self,
        address: &GatedAddress,
        user_id: i32,
        console_session_id: i32,
        code_challenge: &str,
    ) -> Result<String, GateServiceError> {
        if !address.has_temps_gate() {
            return Err(GateServiceError::NotGated {
                address_id: address.address_id.clone(),
                hostname: address.hostname.clone(),
            });
        }
        if !gate::valid_random_value(code_challenge) {
            return Err(GateServiceError::Validation {
                address_id: address.address_id.clone(),
                message: "code_challenge must be an S256 challenge (43 base64url characters)"
                    .into(),
            });
        }
        // The key must exist before anyone holds a code, or the redeem that
        // follows would be the first use.
        self.signing_key().await?;
        self.sweep().await;
        let code = gate::new_random_value("sign-in code")?;
        let now = Utc::now();
        gate_codes::ActiveModel {
            code_hash: Set(sha256_hex(&code)),
            address_id: Set(address.address_id.clone()),
            user_id: Set(user_id),
            console_session_id: Set(console_session_id),
            code_challenge: Set(code_challenge.to_string()),
            expires_at: Set(now + ChronoDuration::seconds(gate::CODE_TTL.as_secs() as i64)),
            created_at: Set(now),
        }
        .insert_ignoring_return(self.db.as_ref())
        .await?;
        Ok(code)
    }

    /// The console's authorize step (D8 step 2): check that the user may
    /// sign in to the address and return the callback URL with a fresh
    /// code. The URL is built from the address's own hostname, never from
    /// the request, so this is not an open redirect.
    ///
    /// Project access is the project's team grants (admins always have it);
    /// the address's own list, when it has one, applies to everyone.
    pub async fn authorize(
        &self,
        request: &AuthorizeRequest<'_>,
        access: Option<&dyn temps_core::ProjectAccessChecker>,
    ) -> Result<String, GateServiceError> {
        let address_id = request.address_id;
        if !gate::valid_random_value(request.state) {
            return Err(GateServiceError::Validation {
                address_id: address_id.to_string(),
                message: "state must be 43 base64url characters".into(),
            });
        }
        let address = self.address(address_id).await?;
        if !address.has_temps_gate() {
            return Err(GateServiceError::NotGated {
                address_id: address.address_id,
                hostname: address.hostname,
            });
        }
        // A wildcard row names no one host to send the code back to; the
        // reachability service refuses to make one signed-in, and a row
        // written some other way still never gets a callback.
        if address.hostname.starts_with("*.") {
            return Err(GateServiceError::Validation {
                address_id: address.address_id,
                message: format!(
                    "{} is a wildcard; sign-in needs a single hostname to return to",
                    address.hostname
                ),
            });
        }
        let not_allowed = |reason| GateServiceError::NotAllowed {
            address_id: address_id.to_string(),
            user_id: request.user_id,
            reason,
        };
        if !request.is_admin {
            let Some(access) = access else {
                return Err(not_allowed(
                    "project access cannot be checked on this server",
                ));
            };
            match access
                .user_can_access_project(request.user_id, address.project_id)
                .await
            {
                Ok(true) => {}
                Ok(false) => return Err(not_allowed("you do not have access to its project")),
                Err(error) => {
                    tracing::warn!(
                        address_id,
                        user_id = request.user_id,
                        project_id = address.project_id,
                        %error,
                        "project access check failed; refusing sign-in"
                    );
                    return Err(not_allowed("project access could not be checked"));
                }
            }
        }
        if !address.allow.is_empty() {
            let teams = self.user_team_ids(request.user_id).await?;
            if !address.allow.admits(request.user_id, &teams) {
                return Err(not_allowed("you are not on its list of teams and people"));
            }
        }
        let code = self
            .issue_code(
                &address,
                request.user_id,
                request.console_session_id,
                request.code_challenge,
            )
            .await?;
        Ok(gate::callback_location(
            &address.hostname,
            &code,
            request.state,
        ))
    }

    /// Exchange a code for a gate session (D8 step 3). The code row is
    /// deleted first, so a code is consumed whether or not it then checks out.
    pub async fn redeem(
        &self,
        request: &GateRedeemRequest,
    ) -> Result<GateRedeemResponse, GateServiceError> {
        let invalid = || GateServiceError::InvalidCode {
            address_id: request.address_id.clone(),
        };
        if !gate::valid_random_value(&request.code) || !gate::valid_verifier(&request.code_verifier)
        {
            return Err(invalid());
        }
        let consumed = gate_codes::Entity::delete_many()
            .filter(gate_codes::Column::CodeHash.eq(sha256_hex(&request.code)))
            .exec_with_returning(self.db.as_ref())
            .await
            .map_err(db("consuming a sign-in code"))?;
        let Some(code) = consumed.into_iter().next() else {
            debug!(address_id = %request.address_id, "unknown or already used sign-in code");
            return Err(invalid());
        };
        let now = Utc::now();
        if code.expires_at <= now
            || code.address_id != request.address_id
            || !gate::constant_time_eq(
                gate::code_challenge(&request.code_verifier).as_bytes(),
                code.code_challenge.as_bytes(),
            )
        {
            return Err(invalid());
        }
        let address = self.address(&request.address_id).await?;
        if !address.has_temps_gate() {
            return Err(GateServiceError::NotGated {
                address_id: address.address_id,
                hostname: address.hostname,
            });
        }
        let ended = || GateServiceError::SessionEnded {
            address_id: request.address_id.clone(),
            user_id: code.user_id,
        };
        let session = temps_entities::sessions::Entity::find_by_id(code.console_session_id)
            .one(self.db.as_ref())
            .await
            .map_err(db("reading the console session behind a sign-in"))?
            .filter(|s| s.user_id == code.user_id && !s.mfa_pending && s.expires_at > now)
            .ok_or_else(ended)?;
        let user = temps_entities::users::Entity::find_by_id(code.user_id)
            .one(self.db.as_ref())
            .await
            .map_err(db("reading the user behind a sign-in"))?
            .filter(|u| u.deleted_at.is_none())
            .ok_or_else(ended)?;
        let expires_at = session_expiry(now, session.expires_at);
        let session_id = gate::new_random_value("session id")?;
        gate_sessions::ActiveModel {
            id: Set(session_id.clone()),
            address_id: Set(address.address_id.clone()),
            user_id: Set(user.id),
            console_session_id: Set(session.id),
            created_at: Set(now),
            expires_at: Set(expires_at),
            revoked_at: Set(None),
        }
        .insert_ignoring_return(self.db.as_ref())
        .await?;
        let token = gate::sign_token(
            self.signing_key().await?.as_ref(),
            &GateClaims {
                sid: session_id,
                uid: user.id,
                email: user.email,
                aid: address.address_id.clone(),
                iat: now.timestamp(),
                exp: expires_at.timestamp(),
            },
        )?;
        info!(
            address_id = %address.address_id,
            user_id = user.id,
            "signed in to a gated address"
        );
        Ok(GateRedeemResponse {
            token,
            expires_at: expires_at.timestamp(),
        })
    }

    /// Revoke the session behind a token a listener received on logout.
    /// The token must be signed by the gate key and issued for the address.
    pub async fn logout(&self, request: &GateLogoutRequest) -> Result<(), GateServiceError> {
        let key = self.signing_key().await?;
        let claims = gate::verify_signature(&request.token, &key.verifying_key())
            .ok()
            .filter(|claims| claims.aid == request.address_id)
            .ok_or_else(|| GateServiceError::InvalidToken {
                address_id: request.address_id.clone(),
            })?;
        let revoked = gate_sessions::Entity::update_many()
            .col_expr(
                gate_sessions::Column::RevokedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(gate_sessions::Column::Id.eq(claims.sid.clone()))
            .filter(gate_sessions::Column::RevokedAt.is_null())
            .exec(self.db.as_ref())
            .await
            .map_err(db("revoking a gate session"))?;
        info!(
            address_id = %request.address_id,
            user_id = claims.uid,
            revoked = revoked.rows_affected,
            "signed out of a gated address"
        );
        self.announce().await;
        Ok(())
    }

    /// The console's "Sign out of apps": revoke every live gate session of
    /// `user_id`. Returns how many were revoked.
    pub async fn revoke_user(&self, user_id: i32) -> Result<u64, GateServiceError> {
        let now = Utc::now();
        let revoked = gate_sessions::Entity::update_many()
            .col_expr(
                gate_sessions::Column::RevokedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(gate_sessions::Column::UserId.eq(user_id))
            .filter(gate_sessions::Column::RevokedAt.is_null())
            .filter(gate_sessions::Column::ExpiresAt.gt(now))
            .exec(self.db.as_ref())
            .await
            .map_err(db("revoking a user's gate sessions"))?
            .rows_affected;
        info!(user_id, revoked, "signed a user out of every gated address");
        self.announce().await;
        Ok(revoked)
    }

    /// An admin signs everyone out of one address: every token issued
    /// before now is refused.
    pub async fn sign_everyone_out(&self, address_id: &str) -> Result<(), GateServiceError> {
        let now = Utc::now();
        let affected =
            match AddressRef::parse(address_id).ok_or_else(|| GateServiceError::BadAddressId {
                address_id: address_id.to_string(),
            })? {
                AddressRef::Custom(id) => {
                    project_custom_domains::Entity::update_many()
                        .col_expr(
                            project_custom_domains::Column::GateNotBefore,
                            sea_orm::sea_query::Expr::value(now),
                        )
                        .filter(project_custom_domains::Column::Id.eq(id))
                        .exec(self.db.as_ref())
                        .await
                        .map_err(db("signing everyone out of a custom domain"))?
                        .rows_affected
                }
                AddressRef::Environment(id) => {
                    environment_domains::Entity::update_many()
                        .col_expr(
                            environment_domains::Column::GateNotBefore,
                            sea_orm::sea_query::Expr::value(now),
                        )
                        .filter(environment_domains::Column::Id.eq(id))
                        .exec(self.db.as_ref())
                        .await
                        .map_err(db("signing everyone out of an environment domain"))?
                        .rows_affected
                }
            };
        if affected == 0 {
            return Err(GateServiceError::AddressNotFound {
                address_id: address_id.to_string(),
            });
        }
        info!(address_id, "signed everyone out of a gated address");
        self.announce().await;
        Ok(())
    }

    /// Delete expired codes and sessions long past their expiry. Best
    /// effort: a failure only leaves rows for the next sweep.
    async fn sweep(&self) {
        let now = Utc::now();
        if let Err(error) = gate_codes::Entity::delete_many()
            .filter(gate_codes::Column::ExpiresAt.lt(now))
            .exec(self.db.as_ref())
            .await
        {
            debug!(%error, "could not sweep expired sign-in codes");
        }
        if let Err(error) = gate_sessions::Entity::delete_many()
            .filter(gate_sessions::Column::ExpiresAt.lt(now - ChronoDuration::hours(1)))
            .exec(self.db.as_ref())
            .await
        {
            debug!(%error, "could not sweep expired gate sessions");
        }
    }

    /// Ask every process to reload routes, so revocation reaches listeners
    /// with the next snapshot.
    async fn announce(&self) {
        announce_route_change(self.db.as_ref()).await;
    }
}

/// `pg_notify('route_table_changes')`, best effort: the idle reconcile picks
/// up anything a lost notification misses.
pub async fn announce_route_change(db: &DatabaseConnection) {
    if let Err(error) = db
        .execute_unprepared("SELECT pg_notify('route_table_changes', '')")
        .await
    {
        debug!(%error, "could not announce a route change");
    }
}

/// When a gate session ends: the earlier of 8 hours and the console
/// session's expiry.
pub fn session_expiry(now: DateTime<Utc>, console_expires_at: DateTime<Utc>) -> DateTime<Utc> {
    let max = now + ChronoDuration::seconds(gate::SESSION_MAX_TTL.as_secs() as i64);
    max.min(console_expires_at)
}

/// Live gate sessions that no longer hold, by address id: revoked by
/// logout, their console session gone or expired, the user deleted, the
/// user no longer able to see the project (not an admin, the project has
/// team grants and the user is in none of those teams — the rule of
/// `TeamProjectAccessChecker`), or no longer on the address's list.
///
/// One query, O(live gate sessions), run on each route reload.
pub const REVOKED_SESSIONS_SQL: &str = r#"
WITH addr AS (
    SELECT 'custom:' || d.id AS address_id, d.project_id, d.gate_allow
    FROM project_custom_domains d
    WHERE d.audience = 'signed_in'
    UNION ALL
    SELECT 'environment:' || d.id, e.project_id, d.gate_allow
    FROM environment_domains d
    JOIN environments e ON e.id = d.environment_id
    WHERE d.audience = 'signed_in'
)
SELECT gs.id, gs.address_id
FROM gate_sessions gs
JOIN addr a ON a.address_id = gs.address_id
LEFT JOIN sessions s ON s.id = gs.console_session_id AND s.user_id = gs.user_id
LEFT JOIN users u ON u.id = gs.user_id
WHERE gs.expires_at > now()
  AND (
    gs.revoked_at IS NOT NULL
    OR s.id IS NULL OR s.expires_at <= now() OR s.mfa_pending
    OR u.id IS NULL OR u.deleted_at IS NOT NULL
    OR NOT (
        EXISTS (
            SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id
            WHERE ur.user_id = gs.user_id AND r.name = 'admin'
        )
        OR NOT EXISTS (
            SELECT 1 FROM project_team_access p WHERE p.project_id = a.project_id
        )
        OR EXISTS (
            SELECT 1 FROM project_team_access p
            JOIN team_members tm ON tm.team_id = p.team_id
            WHERE p.project_id = a.project_id AND tm.user_id = gs.user_id
        )
    )
    OR (
        a.gate_allow IS NOT NULL
        AND jsonb_typeof(a.gate_allow) = 'object'
        AND (
            jsonb_array_length(COALESCE(a.gate_allow->'users', '[]'::jsonb)) > 0
            OR jsonb_array_length(COALESCE(a.gate_allow->'teams', '[]'::jsonb)) > 0
        )
        AND NOT (
            COALESCE(a.gate_allow->'users', '[]'::jsonb) @> to_jsonb(gs.user_id)
            OR EXISTS (
                SELECT 1 FROM team_members tm
                WHERE tm.user_id = gs.user_id
                  AND COALESCE(a.gate_allow->'teams', '[]'::jsonb) @> to_jsonb(tm.team_id)
            )
        )
    )
  )
ORDER BY gs.address_id, gs.id
"#;

/// Run [`REVOKED_SESSIONS_SQL`].
pub async fn revoked_sessions<C: ConnectionTrait>(
    db: &C,
) -> Result<HashMap<String, Vec<String>>, DbErr> {
    let rows = db
        .query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            REVOKED_SESSIONS_SQL,
        ))
        .await?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let id: String = row.try_get("", "id")?;
        let address_id: String = row.try_get("", "address_id")?;
        out.entry(address_id).or_default().push(id);
    }
    Ok(out)
}

/// The gate key's public half, if one has been generated. The route table
/// reads it; only the gate service creates it.
pub async fn current_public_key<C: ConnectionTrait>(db: &C) -> Result<Option<String>, DbErr> {
    Ok(gate_keys::Entity::find_by_id(1)
        .one(db)
        .await?
        .map(|row| row.public_key))
}

trait InsertIgnoringReturn {
    async fn insert_ignoring_return(self, db: &DatabaseConnection) -> Result<(), GateServiceError>;
}

impl<A> InsertIgnoringReturn for A
where
    A: sea_orm::ActiveModelTrait + Send,
    <A::Entity as EntityTrait>::Model: sea_orm::IntoActiveModel<A>,
{
    async fn insert_ignoring_return(self, db: &DatabaseConnection) -> Result<(), GateServiceError> {
        <A::Entity as EntityTrait>::insert(self)
            .exec_without_returning(db)
            .await
            .map_err(db_insert)?;
        Ok(())
    }
}

fn db_insert(source: DbErr) -> GateServiceError {
    GateServiceError::Database {
        operation: "writing a gate row",
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wildcard row never gets a sign-in callback, however it became
    /// signed-in: `https://*.example.com/...` names no host to return to.
    #[tokio::test]
    async fn a_wildcard_row_is_never_sent_a_code() {
        let now = Utc::now();
        let row = project_custom_domains::Model {
            id: 7,
            project_id: 3,
            environment_id: 5,
            domain: "*.example.com".to_string(),
            redirect_to: None,
            status_code: None,
            branch: None,
            status: "active".to_string(),
            message: None,
            created_at: now,
            updated_at: now,
            certificate_id: None,
            service_name: None,
            audience: "signed_in".to_string(),
            private_range: None,
            private_node_id: None,
            private_listener: None,
            gate: Some("temps".to_string()),
            gate_allow: None,
            gate_not_before: None,
            gate_access_provider_id: None,
            gate_access_app_id: None,
            gate_access_aud: None,
            private_via: None,
            private_service: None,
            http_policy: None,
        };
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .into_connection();
        let service = GateService::new(
            Arc::new(db),
            Arc::new(EncryptionService::new_from_password("gate-test")),
        );
        let state = "s".repeat(43);
        let error = service
            .authorize(
                &AuthorizeRequest {
                    address_id: "custom:7",
                    user_id: 1,
                    console_session_id: 1,
                    is_admin: true,
                    state: &state,
                    code_challenge: &"c".repeat(43),
                },
                None,
            )
            .await
            .expect_err("a wildcard must not be authorized");
        assert!(
            matches!(&error, GateServiceError::Validation { message, .. } if message.contains("*.example.com")),
            "{error}"
        );
    }

    #[test]
    fn address_ids_parse_strictly() {
        assert_eq!(AddressRef::parse("custom:7"), Some(AddressRef::Custom(7)));
        assert_eq!(
            AddressRef::parse("environment:3"),
            Some(AddressRef::Environment(3))
        );
        assert_eq!(AddressRef::parse("custom:0"), None);
        assert_eq!(AddressRef::parse("custom:-1"), None);
        assert_eq!(AddressRef::parse("project:1"), None);
        assert_eq!(AddressRef::parse("custom"), None);
    }

    #[test]
    fn a_gate_session_never_outlives_eight_hours_or_the_console_session() {
        let now = Utc::now();
        let week = now + ChronoDuration::days(7);
        assert_eq!(session_expiry(now, week), now + ChronoDuration::hours(8));
        let soon = now + ChronoDuration::minutes(20);
        assert_eq!(session_expiry(now, soon), soon);
    }

    #[test]
    fn an_unreadable_list_admits_nobody() {
        assert!(parse_allow(None).is_empty());
        let list = parse_allow(Some(&serde_json::json!({"teams": [2, 1, 2], "users": []})));
        assert_eq!(list.teams, vec![1, 2]);
        let broken = parse_allow(Some(&serde_json::json!("everyone")));
        assert!(!broken.is_empty());
        assert!(!broken.admits(5, &[1, 2, 3]));
    }

    #[test]
    fn the_revocation_query_covers_every_reason() {
        for clause in [
            "gs.revoked_at IS NOT NULL",
            "s.id IS NULL",
            "s.expires_at <= now()",
            "s.mfa_pending",
            "u.deleted_at IS NOT NULL",
            "project_team_access",
            "r.name = 'admin'",
            "gate_allow->'users'",
            "gate_allow->'teams'",
            "gs.expires_at > now()",
        ] {
            assert!(REVOKED_SESSIONS_SQL.contains(clause), "missing {clause}");
        }
    }
}
