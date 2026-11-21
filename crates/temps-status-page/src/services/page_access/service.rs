// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Setting a page's password and letting visitors in (ADR 108 D2).

use std::sync::Arc;
use std::time::Instant;

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::http::HeaderMap;
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};
use temps_core::{EncryptionService, UtcDateTime};
use temps_entities::status_pages;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use super::cookie::{access_cookie_values, AccessKey};
use super::error::PageAccessError;
use super::limiter::UnlockLimiter;
use super::types::{PagePasswordResponse, SetPagePasswordRequest};
use crate::services::public_pages::{PublishedSnapshot, StatusSnapshotCache};

pub const MIN_PASSWORD_CHARS: usize = 8;
/// Bounds what one unlock request can make argon2 hash.
pub const MAX_PASSWORD_CHARS: usize = 256;
/// argon2 operations running at once, unlock verifications and password
/// changes together. Each takes tens of milliseconds and ~19 MiB; a burst
/// beyond this is told to retry instead of queueing.
pub const MAX_CONCURRENT_VERIFICATIONS: usize = 2;

/// What an unlock attempt led to.
#[derive(Debug, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The password is right: set this cookie value.
    Unlocked(String),
    /// The password is wrong.
    Wrong,
    /// Too many wrong passwords from this client: retry after this long.
    Throttled(std::time::Duration),
    /// Too many verifications are running.
    Busy,
    /// The page is not protected (or no longer is).
    Open,
}

pub struct PageAccessService {
    db: Arc<DatabaseConnection>,
    cache: Arc<StatusSnapshotCache>,
    key: AccessKey,
    verifications: Arc<Semaphore>,
    limiter: UnlockLimiter,
}

fn validate(status_page_id: i32, password: &str) -> Result<(), PageAccessError> {
    let chars = password.chars().count();
    let reason = if chars < MIN_PASSWORD_CHARS {
        format!("a password has at least {MIN_PASSWORD_CHARS} characters")
    } else if chars > MAX_PASSWORD_CHARS {
        format!("a password has at most {MAX_PASSWORD_CHARS} characters")
    } else if password.chars().any(char::is_control) {
        "a password cannot contain control characters".to_string()
    } else {
        return Ok(());
    };
    Err(PageAccessError::Validation {
        status_page_id,
        reason,
    })
}

impl PageAccessService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        cache: Arc<StatusSnapshotCache>,
        encryption: &EncryptionService,
    ) -> Self {
        Self {
            db,
            cache,
            key: AccessKey::new(encryption),
            verifications: Arc::new(Semaphore::new(MAX_CONCURRENT_VERIFICATIONS)),
            limiter: UnlockLimiter::default(),
        }
    }

    pub async fn status(
        &self,
        status_page_id: i32,
    ) -> Result<PagePasswordResponse, PageAccessError> {
        let hash: Option<Option<String>> = status_pages::Entity::find_by_id(status_page_id)
            .select_only()
            .column(status_pages::Column::PasswordHash)
            .into_tuple()
            .one(self.db.as_ref())
            .await?;
        let hash = hash.ok_or(PageAccessError::StatusPageNotFound { status_page_id })?;
        Ok(PagePasswordResponse {
            status_page_id,
            has_password: hash.is_some(),
        })
    }

    /// Set, change or remove the password. Every call bumps the version, so
    /// every open session ends, then the page is rebuilt.
    pub async fn set_password(
        &self,
        status_page_id: i32,
        request: SetPagePasswordRequest,
    ) -> Result<PagePasswordResponse, PageAccessError> {
        let hash = match request.password {
            Some(password) => {
                let password = password.into_exposed();
                validate(status_page_id, &password)?;
                // The same permits as unlock: hashing is as heavy as
                // verifying, so concurrent changes cannot exhaust memory.
                let permit = self
                    .verifications
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| PageAccessError::Busy { status_page_id })?;
                let hashed = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    Argon2::default()
                        .hash_password(password.as_bytes())
                        .map(|hash| hash.to_string())
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| PageAccessError::Hash {
                    status_page_id,
                    reason: e.to_string(),
                })?
                .map_err(|reason| PageAccessError::Hash {
                    status_page_id,
                    reason,
                })?;
                Some(hashed)
            }
            None => None,
        };
        let has_password = hash.is_some();
        let updated = status_pages::Entity::update_many()
            .col_expr(status_pages::Column::PasswordHash, Expr::value(hash))
            .col_expr(
                status_pages::Column::PasswordVersion,
                Expr::col(status_pages::Column::PasswordVersion).add(1),
            )
            .col_expr(status_pages::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(status_pages::Column::Id.eq(status_page_id))
            .exec(self.db.as_ref())
            .await?;
        if updated.rows_affected == 0 {
            return Err(PageAccessError::StatusPageNotFound { status_page_id });
        }
        info!(status_page_id, has_password, "changed status page password");
        if let Err(error) = self.cache.refresh_page(status_page_id).await {
            // A published page must not stay open after a password was set:
            // stop serving it until the refresh loop rebuilds it.
            warn!(
                status_page_id,
                error = %error,
                "failed to rebuild the status page after a password change; not serving it \
                 until the next refresh"
            );
            self.cache.remove_page(status_page_id).await;
        }
        Ok(PagePasswordResponse {
            status_page_id,
            has_password,
        })
    }

    /// Whether the request may read `snapshot`: an open page always, a
    /// protected one with a valid access cookie. No I/O.
    pub fn allows(&self, snapshot: &PublishedSnapshot, headers: &HeaderMap) -> bool {
        self.allows_at(snapshot, headers, Utc::now())
    }

    pub fn allows_at(
        &self,
        snapshot: &PublishedSnapshot,
        headers: &HeaderMap,
        now: UtcDateTime,
    ) -> bool {
        let Some(gate) = snapshot.gate() else {
            return true;
        };
        access_cookie_values(headers).any(|value| {
            self.key
                .verify(value, snapshot.status_page_id(), gate.version, now)
        })
    }

    /// Check `password` against the snapshot's hash for `client` (a rate
    /// limit key). Only the snapshot is read.
    pub async fn unlock(
        &self,
        snapshot: &PublishedSnapshot,
        password: &str,
        client: &str,
    ) -> UnlockOutcome {
        let Some(gate) = snapshot.gate() else {
            return UnlockOutcome::Open;
        };
        let page_id = snapshot.status_page_id();
        if let Err(retry) = self.limiter.check(page_id, client, Instant::now()) {
            return UnlockOutcome::Throttled(retry);
        }
        if password.is_empty() || password.chars().count() > MAX_PASSWORD_CHARS {
            self.limiter.record_failure(page_id, client, Instant::now());
            return UnlockOutcome::Wrong;
        }
        let Ok(permit) = self.verifications.clone().try_acquire_owned() else {
            return UnlockOutcome::Busy;
        };
        let hash = gate.hash.clone();
        let attempt = password.to_string();
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            match PasswordHash::new(&hash) {
                Ok(parsed) => Argon2::default()
                    .verify_password(attempt.as_bytes(), &parsed)
                    .is_ok(),
                Err(error) => {
                    warn!(error = %error, "stored status page password hash is unreadable");
                    false
                }
            }
        })
        .await
        .unwrap_or(false);
        if verified {
            self.limiter.clear(page_id, client);
            info!(
                status_page_id = page_id,
                "visitor unlocked a password-protected status page"
            );
            UnlockOutcome::Unlocked(self.key.mint(page_id, gate.version, Utc::now()))
        } else {
            self.limiter.record_failure(page_id, client, Instant::now());
            UnlockOutcome::Wrong
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_password_change_waits_for_a_free_hashing_permit() {
        // No database is reached: the permit is taken before the write.
        let db = Arc::new(DatabaseConnection::Disconnected);
        let service = PageAccessService::new(
            db.clone(),
            Arc::new(StatusSnapshotCache::new(db)),
            &EncryptionService::new_from_password("page-access-permits"),
        );
        let held: Vec<_> = (0..MAX_CONCURRENT_VERIFICATIONS)
            .map(|_| {
                service
                    .verifications
                    .clone()
                    .try_acquire_owned()
                    .expect("a free permit")
            })
            .collect();
        let busy = service
            .set_password(
                7,
                SetPagePasswordRequest {
                    password: Some("correct horse battery".into()),
                },
            )
            .await
            .expect_err("every hashing permit is taken");
        assert!(
            matches!(busy, PageAccessError::Busy { status_page_id: 7 }),
            "{busy}"
        );
        assert_eq!(service.verifications.available_permits(), 0);
        drop(held);
        assert_eq!(
            service.verifications.available_permits(),
            MAX_CONCURRENT_VERIFICATIONS,
            "a refused change holds no permit"
        );
    }

    #[test]
    fn passwords_are_bounded_and_printable() {
        assert!(validate(1, "12345678").is_ok());
        let err = validate(1, "short").expect_err("too short");
        assert!(err.to_string().contains("status page 1"), "{err}");
        assert!(validate(1, &"x".repeat(257)).is_err());
        assert!(validate(1, "1234567\n8").is_err());
    }
}
