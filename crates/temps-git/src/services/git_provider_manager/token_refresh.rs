// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One OAuth refresh at a time per connection, and what a rejection means.
//!
//! Refresh tokens are reported to be single-use, so two callers refreshing at
//! once could make the loser's rejection look like a dead credential, and tokens
//! that are issued but then not stored are gone for good. Four rules follow:
//!
//! - **One refresh per connection, across processes.** A lease row claimed by
//!   compare-and-set serializes refreshes. It is a lease rather than an advisory
//!   lock so no pooled connection is held across the network call, and it lapses
//!   so a crashed holder cannot block the connection forever. Later callers
//!   re-read the row and reuse the result.
//! - **Issued tokens survive the caller.** The exchange and the write run in a
//!   detached task, so a cancelled request cannot lose a rotated token.
//! - **Rotation is not replacement.** A refresh writes while the credential
//!   generation it read is current. Another refresh does not move the generation,
//!   so issued tokens are kept; a reauthorization does, so it is never overwritten.
//! - **Only a proven rejection blocks.** `is_expired` is set for a 400
//!   `invalid_grant`, and only while the stored credentials are still the ones sent.

use super::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::Serialize;
use temps_core::retry::RetryConfig;
use temps_core::AuditOperation;

use crate::services::git_provider::RefreshedOAuthToken;

/// How long a refresh lease lasts. Longer than the provider client's 30-second
/// timeout plus writing the result, so a live holder does not lose it in practice.
/// Losing it is safe: an overlapping refresh cannot overwrite a replaced grant.
const REFRESH_LEASE_SECS: i64 = 60;
/// How long a caller waits for someone else's refresh before giving up.
const REFRESH_WAIT_LIMIT: std::time::Duration =
    std::time::Duration::from_secs(REFRESH_LEASE_SECS as u64 + 5);
const REFRESH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// What a refresh should do about a connection that is already blocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlockedConnection {
    /// Syncs and deploys: fail without calling the provider.
    Fail,
    /// Health checks: try the stored refresh token once, and clear the block
    /// if the provider accepts it.
    TryOnce,
}

/// The ciphertexts a caller read. Encryption uses a fresh nonce per write, so
/// these also tell apart two grants with identical plaintext tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CredentialSnapshot {
    access_token: Option<String>,
    refresh_token: Option<String>,
}

impl CredentialSnapshot {
    fn of(connection: &git_provider_connections::Model) -> Self {
        Self {
            access_token: connection.access_token.clone(),
            refresh_token: connection.refresh_token.clone(),
        }
    }
}

/// A connection entered or left the state where its authorization is rejected.
/// Records the connection and the transition, never a credential. A system event
/// when triggered by a background job, so it names no acting user.
#[derive(Debug, Clone, Serialize)]
struct GitConnectionRefreshBlockAudit {
    connection_id: i32,
    provider_id: i32,
    blocked: bool,
}

impl AuditOperation for GitConnectionRefreshBlockAudit {
    fn operation_type(&self) -> String {
        if self.blocked {
            "GIT_CONNECTION_REFRESH_BLOCKED"
        } else {
            "GIT_CONNECTION_REFRESH_UNBLOCKED"
        }
        .to_string()
    }
    fn user_id(&self) -> Option<i32> {
        None
    }
    fn ip_address(&self) -> Option<String> {
        None
    }
    fn user_agent(&self) -> &str {
        "temps-git-token-refresh"
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

impl GitProviderManager {
    /// Whether the provider rejected this connection's refresh token. PATs and
    /// GitHub App installations have no refresh token and keep their own handling.
    fn is_refresh_blocked(connection: &git_provider_connections::Model) -> bool {
        connection.is_expired
            && connection.refresh_token.is_some()
            && connection.installation_id.is_none()
    }

    /// Fail a sync or deploy on a blocked connection before any provider call.
    pub(super) fn ensure_refresh_not_blocked(
        connection: &git_provider_connections::Model,
    ) -> Result<(), GitProviderManagerError> {
        if Self::is_refresh_blocked(connection) {
            return Err(GitProviderManagerError::ConnectionReauthorizationRequired {
                connection_id: connection.id,
            });
        }
        Ok(())
    }

    /// A usable token for a health check, which unlike a sync or deploy still
    /// tries a blocked connection once.
    ///
    /// A connection in good standing is validated first, so a token that the
    /// recorded expiry wrongly calls valid gets a refresh rather than a "rejected"
    /// report. A blocked one goes straight to the refresh, because what is in
    /// question is the refresh token: a still-valid access token would only hide
    /// a connection that is about to stop working.
    pub(crate) async fn token_for_health_check(
        &self,
        connection_id: i32,
    ) -> Result<String, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let provider = self.get_provider(connection.provider_id).await?;
        let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;
        let auth_method = serde_json::from_value::<AuthMethod>(auth_config).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Invalid auth config on provider {}: {}",
                provider.id, e
            ))
        })?;
        let refreshable = matches!(
            auth_method,
            AuthMethod::OAuth { .. } | AuthMethod::GitLabApp { .. }
        ) && connection.refresh_token.is_some();
        if !refreshable {
            return self
                .validate_and_refresh_connection_token(connection_id)
                .await;
        }

        if !Self::is_refresh_blocked(&connection) {
            let access_token = self.stored_access_token(&connection).await?;
            let provider_service = self.get_provider_service(provider.id).await?;
            if provider_service.validate_token(&access_token).await? {
                return Ok(access_token);
            }
        }
        self.refresh_connection_token(&connection, BlockedConnection::TryOnce)
            .await
    }

    /// Refresh `connection`'s access token, or reuse the result if another caller
    /// is doing so or already has. `connection` is what the caller last read:
    /// credentials that have moved on since mean the work is already done.
    pub(super) async fn refresh_connection_token(
        &self,
        connection: &git_provider_connections::Model,
        on_blocked: BlockedConnection,
    ) -> Result<String, GitProviderManagerError> {
        let connection_id = connection.id;
        let observed = CredentialSnapshot::of(connection);
        let lease_id = uuid::Uuid::new_v4();

        if !self.claim_refresh_lease(connection_id, lease_id).await? {
            return self
                .wait_for_concurrent_refresh(connection_id, &observed)
                .await;
        }

        // Detached so that dropping the caller, say on a client disconnect, can
        // neither lose tokens the provider has issued nor strand the lease.
        let manager = self.clone();
        tokio::spawn(async move {
            let result = manager
                .refresh_under_lease(connection_id, &observed, on_blocked)
                .await;
            manager.release_refresh_lease(connection_id, lease_id).await;
            result
        })
        .await
        .map_err(|error| {
            GitProviderManagerError::ProviderError(GitProviderError::Other(format!(
                "Token refresh task for connection {connection_id} failed: {error}"
            )))
        })?
    }

    /// Claim the lease if nobody holds a live one. Uses the database's clock, so
    /// processes on different hosts agree on when a lease has lapsed.
    async fn claim_refresh_lease(
        &self,
        connection_id: i32,
        lease_id: uuid::Uuid,
    ) -> Result<bool, GitProviderManagerError> {
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE git_provider_connections
                 SET token_refresh_lease_id = $1,
                     token_refresh_lease_expires_at = now() + make_interval(secs => $2)
                 WHERE id = $3
                   AND (token_refresh_lease_expires_at IS NULL
                        OR token_refresh_lease_expires_at <= now())",
                [
                    lease_id.into(),
                    (REFRESH_LEASE_SECS as f64).into(),
                    connection_id.into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Give the lease back, only if it is still ours. A failure is only logged:
    /// the lease lapses by itself, and the refresh result must not be lost to it.
    async fn release_refresh_lease(&self, connection_id: i32, lease_id: uuid::Uuid) {
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE git_provider_connections
                 SET token_refresh_lease_id = NULL, token_refresh_lease_expires_at = NULL
                 WHERE id = $1 AND token_refresh_lease_id = $2",
                [connection_id.into(), lease_id.into()],
            ))
            .await;
        if let Err(error) = result {
            warn!(
                connection_id,
                error = %error,
                "Failed to release token refresh lease; it will lapse by itself"
            );
        }
    }

    /// Another caller holds the lease: wait for its result, never refresh here.
    /// A burst of callers therefore costs one request even when that request
    /// fails, which is what keeps a provider error from turning into retries.
    async fn wait_for_concurrent_refresh(
        &self,
        connection_id: i32,
        observed: &CredentialSnapshot,
    ) -> Result<String, GitProviderManagerError> {
        let deadline = tokio::time::Instant::now() + REFRESH_WAIT_LIMIT;
        loop {
            let current = self.get_connection(connection_id).await?;
            if CredentialSnapshot::of(&current) != *observed {
                return self.stored_access_token(&current).await;
            }
            let lease_live = current
                .token_refresh_lease_expires_at
                .is_some_and(|expires_at| expires_at > chrono::Utc::now());
            if !lease_live || tokio::time::Instant::now() >= deadline {
                Self::ensure_refresh_not_blocked(&current)?;
                return Err(GitProviderManagerError::TokenRefreshIncomplete { connection_id });
            }
            tokio::time::sleep(REFRESH_POLL_INTERVAL).await;
        }
    }

    async fn refresh_under_lease(
        &self,
        connection_id: i32,
        observed: &CredentialSnapshot,
        on_blocked: BlockedConnection,
    ) -> Result<String, GitProviderManagerError> {
        // Re-read now that nobody else can be refreshing: if the caller's read
        // has gone stale the token it would send is probably already used up.
        let current = self.get_connection(connection_id).await?;
        if CredentialSnapshot::of(&current) != *observed {
            debug!(
                connection_id,
                "Credentials changed before the lease was claimed; reusing them"
            );
            return self.stored_access_token(&current).await;
        }
        if on_blocked == BlockedConnection::Fail {
            Self::ensure_refresh_not_blocked(&current)?;
        }

        let Some(encrypted_refresh) = current.refresh_token.as_deref() else {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Connection {connection_id} has no refresh token to refresh with"
            )));
        };
        let refresh_token = self.decrypt_string(encrypted_refresh).await?;
        let provider_service = self.get_provider_service(current.provider_id).await?;

        match provider_service.refresh_oauth_token(&refresh_token).await {
            Ok(issued) => self.store_issued_tokens(&current, issued).await,
            Err(GitProviderError::RefreshTokenRejected { .. }) => {
                self.handle_rejected_refresh(&current).await
            }
            Err(error) => {
                warn!(
                    connection_id,
                    error = %error,
                    "Token refresh failed; the connection stays as it is"
                );
                Err(error.into())
            }
        }
    }

    /// Persist tokens the provider has issued. The old refresh token may already
    /// be used up, so a database blip is retried rather than losing them.
    async fn store_issued_tokens(
        &self,
        sent: &git_provider_connections::Model,
        issued: RefreshedOAuthToken,
    ) -> Result<String, GitProviderManagerError> {
        let connection_id = sent.id;
        // A refresh that overlapped this one may have blocked the connection
        // while this exchange was in flight, so ask the row rather than `sent`.
        let was_blocked = self
            .get_connection(connection_id)
            .await
            .map_or(sent.is_expired, |current| current.is_expired);

        let written = RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_millis(200))
            .with_max_delay(std::time::Duration::from_secs(2))
            .retry_if(
                || {
                    self.update_connection_tokens(
                        sent,
                        issued.access_token.clone(),
                        issued.refresh_token.clone(),
                        issued.expires_at,
                    )
                },
                |error| matches!(error, GitProviderManagerError::DatabaseError(_)),
            )
            .await;

        match written {
            Ok(()) => {
                info!(connection_id, "Stored refreshed connection tokens");
                if was_blocked {
                    info!(
                        connection_id,
                        provider_id = sent.provider_id,
                        "Git connection refresh succeeded; cleared its reauthorization block"
                    );
                    self.audit_block_transition(sent, false).await;
                }
                Ok(issued.access_token)
            }
            // A reauthorization won the race. Its grant replaces the one these
            // tokens belong to, so dropping them costs nothing: the row holds a
            // working pair, and that is the one callers should use.
            Err(GitProviderManagerError::ConnectionGrantReplaced { .. }) => {
                warn!(
                    connection_id,
                    "Connection was reauthorized during a token refresh; keeping the new grant"
                );
                let current = self.get_connection(connection_id).await?;
                self.stored_access_token(&current).await
            }
            Err(error) => {
                error!(
                    connection_id,
                    error = %error,
                    "Could not store issued tokens; the next refresh may be rejected"
                );
                Err(error)
            }
        }
    }

    /// The provider said `invalid_grant`. That only proves the token we sent is
    /// dead, so block the connection only if that token is still the stored one.
    async fn handle_rejected_refresh(
        &self,
        sent: &git_provider_connections::Model,
    ) -> Result<String, GitProviderManagerError> {
        let connection_id = sent.id;
        let sent_credentials = CredentialSnapshot::of(sent);
        let current = self.get_connection(connection_id).await?;
        if CredentialSnapshot::of(&current) != sent_credentials {
            info!(
                connection_id,
                "Provider rejected a refresh token that was already replaced; using the new one"
            );
            return self.stored_access_token(&current).await;
        }

        if self.mark_refresh_blocked(sent).await? {
            warn!(
                connection_id,
                provider_id = sent.provider_id,
                "Provider rejected the refresh token; blocking the connection until it is reauthorized"
            );
            self.audit_block_transition(sent, true).await;
            return Err(GitProviderManagerError::ConnectionReauthorizationRequired {
                connection_id,
            });
        }

        // Nothing written: it was already blocked, or the credentials moved.
        let current = self.get_connection(connection_id).await?;
        if CredentialSnapshot::of(&current) != sent_credentials {
            return self.stored_access_token(&current).await;
        }
        Err(GitProviderManagerError::ConnectionReauthorizationRequired { connection_id })
    }

    /// Set `is_expired` only if the stored credentials are still the ones sent,
    /// so a reauthorization landing after the caller's last read cannot be marked
    /// dead. Returns whether this call made the transition. `updated_at` is left
    /// alone: bumping it would fail a reauthorization in the middle of its code
    /// exchange.
    async fn mark_refresh_blocked(
        &self,
        sent: &git_provider_connections::Model,
    ) -> Result<bool, GitProviderManagerError> {
        let marked = git_provider_connections::Entity::update_many()
            .col_expr(
                git_provider_connections::Column::IsExpired,
                Expr::value(true),
            )
            .filter(git_provider_connections::Column::Id.eq(sent.id))
            .filter(git_provider_connections::Column::IsExpired.eq(false))
            .filter(match &sent.access_token {
                Some(token) => git_provider_connections::Column::AccessToken.eq(token.clone()),
                None => git_provider_connections::Column::AccessToken.is_null(),
            })
            .filter(match &sent.refresh_token {
                Some(token) => git_provider_connections::Column::RefreshToken.eq(token.clone()),
                None => git_provider_connections::Column::RefreshToken.is_null(),
            })
            .exec(self.db.as_ref())
            .await?;
        Ok(marked.rows_affected == 1)
    }

    async fn stored_access_token(
        &self,
        connection: &git_provider_connections::Model,
    ) -> Result<String, GitProviderManagerError> {
        let Some(encrypted) = connection.access_token.as_deref() else {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Connection {} has no access token",
                connection.id
            )));
        };
        self.decrypt_string(encrypted).await
    }

    /// A failed audit write is logged and does not fail the refresh.
    async fn audit_block_transition(
        &self,
        connection: &git_provider_connections::Model,
        blocked: bool,
    ) {
        let audit = GitConnectionRefreshBlockAudit {
            connection_id: connection.id,
            provider_id: connection.provider_id,
            blocked,
        };
        if let Err(error) = self.audit_logger.create_audit_log(&audit).await {
            error!(
                connection_id = connection.id,
                blocked,
                error = %error,
                "Failed to write the git connection refresh audit log"
            );
        }
    }
}

#[cfg(test)]
mod tests;
