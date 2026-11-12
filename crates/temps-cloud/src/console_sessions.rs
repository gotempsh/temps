// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-094: Temps Cloud console sessions end when Cloud stops trusting the
//! user, not days later when the local session expires.
//!
//! - [`ConsoleLogoutAdapter`] applies the signed logout tokens Cloud pushes on
//!   the management channel (`temps-auth` verifies them).
//! - [`run_session_checker`] re-checks live Cloud sessions with Cloud every
//!   [`CLOUD_SESSION_CHECK_INTERVAL`], and ends them all once Cloud has not
//!   answered for [`CLOUD_SESSION_STALE_AFTER`]. That clock is the last
//!   confirmation recorded on the managed provider's row, not process
//!   memory, so restarting the instance during a Cloud outage does not give
//!   the sessions another [`CLOUD_SESSION_STALE_AFTER`].
//!
//! Both act only on sessions issued through the Cloud-managed provider. On an
//! instance without that provider both are no-ops, and local logins are
//! never touched: when Cloud is down, the instance keeps working.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use temps_auth::oidc_service::{
    CloudLogoutRefusal, CloudSessionCheck, OidcService, CLOUD_SESSION_CHECK_INTERVAL,
    CLOUD_SESSION_STALE_AFTER,
};
use temps_cloud_client::{ConsoleLogoutOutcome, ConsoleLogoutSink};
use tokio::sync::watch;

/// Applies `console_logout` frames through `temps-auth`.
pub struct ConsoleLogoutAdapter {
    oidc: Arc<OidcService>,
}

impl ConsoleLogoutAdapter {
    pub fn new(oidc: Arc<OidcService>) -> Self {
        Self { oidc }
    }
}

#[async_trait::async_trait]
impl ConsoleLogoutSink for ConsoleLogoutAdapter {
    async fn apply(&self, logout_token: &str) -> ConsoleLogoutOutcome {
        match self.oidc.apply_cloud_logout_token(logout_token).await {
            Ok(applied) => ConsoleLogoutOutcome::Applied {
                jti: applied.jti,
                sessions_ended: applied.sessions_ended,
            },
            Err(CloudLogoutRefusal::Rejected { jti, reason }) => ConsoleLogoutOutcome::Rejected {
                jti,
                reason: reason.to_string(),
            },
            Err(CloudLogoutRefusal::Failed { jti, reason }) => {
                // Not answered: Cloud sends it again, and the re-check below
                // still applies. The detail stays in this instance's log.
                tracing::error!(
                    request_id = %jti,
                    %reason,
                    "could not end Temps Cloud console sessions for a verified logout; Cloud will retry"
                );
                ConsoleLogoutOutcome::Unreadable
            }
            Err(CloudLogoutRefusal::Unreadable) => ConsoleLogoutOutcome::Unreadable,
        }
    }
}

/// Failed re-checks since the last answered one, for the logs. The
/// fail-closed clock itself is the persisted confirmation
/// ([`OidcService::cloud_sessions_confirmed_at`]).
#[derive(Debug, Default)]
struct Freshness {
    failures: u32,
}

impl Freshness {
    fn confirmed(&mut self) -> Option<u32> {
        let failures = std::mem::take(&mut self.failures);
        (failures > 0).then_some(failures)
    }
}

/// Whether Cloud sessions last confirmed at `confirmed_at` have gone
/// unconfirmed for [`CLOUD_SESSION_STALE_AFTER`] at `now`. No confirmation
/// on record counts as unconfirmed, and so does one that far in the future
/// (the clock was set back): neither may keep sessions alive.
fn unconfirmed_too_long(confirmed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let Some(confirmed_at) = confirmed_at else {
        return true;
    };
    let Ok(bound) = chrono::Duration::from_std(CLOUD_SESSION_STALE_AFTER) else {
        return true;
    };
    (now - confirmed_at).abs() >= bound
}

/// Re-check Cloud sessions until `cancel` fires. The first pass runs at
/// once; the bound is counted from the confirmation persisted before a
/// restart, so a restart never extends it.
pub async fn run_session_checker(oidc: Arc<OidcService>, mut cancel: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(CLOUD_SESSION_CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut freshness = Freshness::default();
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return;
                }
            }
            _ = ticker.tick() => check_once(&oidc, &mut freshness).await,
        }
    }
}

async fn check_once(oidc: &OidcService, freshness: &mut Freshness) {
    match oidc.check_cloud_sessions().await {
        Ok(CloudSessionCheck::NothingToCheck) => {
            freshness.confirmed();
        }
        Ok(CloudSessionCheck::Checked { sessions_ended }) => {
            if let Some(failures) = freshness.confirmed() {
                tracing::info!(
                    failures,
                    "Temps Cloud console session check recovered after {failures} failed attempt(s)"
                );
            }
            if sessions_ended > 0 {
                tracing::info!(
                    sessions_ended,
                    "ended Temps Cloud console sessions of users Cloud no longer lets in"
                );
            }
        }
        Ok(CloudSessionCheck::ClientRevoked { sessions_ended }) => {
            freshness.confirmed();
            tracing::warn!(
                sessions_ended,
                "Temps Cloud no longer recognises this instance's console client; ended every \
                 Temps Cloud console session. Local logins are unaffected"
            );
        }
        Ok(CloudSessionCheck::Unavailable { reason }) => {
            freshness.failures = freshness.failures.saturating_add(1);
            let confirmed_at = match oidc.cloud_sessions_confirmed_at().await {
                Ok(confirmed_at) => confirmed_at,
                Err(error) => {
                    // The same database ends sessions: nothing could be
                    // ended now either. The next pass tries again.
                    tracing::error!(
                        %error,
                        "could not read when Temps Cloud last confirmed console sessions"
                    );
                    return;
                }
            };
            if unconfirmed_too_long(confirmed_at, Utc::now()) {
                match oidc.end_all_cloud_sessions().await {
                    Ok(sessions_ended) => tracing::warn!(
                        sessions_ended,
                        %reason,
                        stale_after_secs = CLOUD_SESSION_STALE_AFTER.as_secs(),
                        "Temps Cloud could not confirm console sessions for {} minutes; ended \
                         {sessions_ended} Temps Cloud session(s). Sign in again once Cloud is \
                         reachable; local logins are unaffected",
                        CLOUD_SESSION_STALE_AFTER.as_secs() / 60
                    ),
                    Err(error) => tracing::error!(
                        %error,
                        "could not end unconfirmed Temps Cloud console sessions"
                    ),
                }
            } else {
                tracing::warn!(
                    %reason,
                    failures = freshness.failures,
                    "Temps Cloud console session check failed; Temps Cloud sessions end if this \
                     lasts {} minutes",
                    CLOUD_SESSION_STALE_AFTER.as_secs() / 60
                );
            }
        }
        Err(error) => {
            // A local database problem: nothing here could end sessions
            // either, so this does not advance the fail-closed clock's action.
            tracing::error!(%error, "Temps Cloud console session check could not read local sessions");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_unconfirmed_without_a_recent_confirmation() {
        let now = Utc::now();
        let stale = chrono::Duration::from_std(CLOUD_SESSION_STALE_AFTER).unwrap();
        let minute = chrono::Duration::minutes(1);
        assert!(!unconfirmed_too_long(Some(now), now));
        assert!(!unconfirmed_too_long(Some(now - stale + minute), now));
        assert!(unconfirmed_too_long(Some(now - stale), now));
        assert!(unconfirmed_too_long(None, now), "nothing on record");
        assert!(
            unconfirmed_too_long(Some(now + stale), now),
            "a clock set back does not extend the bound"
        );
    }

    // ── Database-backed: a restart reads the persisted confirmation ──
    //
    // Docker-dependent; skips when Docker is unavailable.

    use sea_orm::sea_query::Expr;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use temps_entities::{oidc_providers, sessions, users};

    struct Fixture {
        _db: temps_test_utils::TestDatabase,
        conn: Arc<sea_orm::DatabaseConnection>,
        oidc: OidcService,
        provider_id: i32,
        user_id: i32,
    }

    /// A Cloud-managed provider whose Cloud the instance cannot reach (an
    /// address it refuses to call), and a user linked to it.
    async fn fixture() -> Option<Fixture> {
        let db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Docker not available, skipping test: {e}");
                return None;
            }
        };
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "console-session-tests",
        ));
        let users_service = Arc::new(temps_auth::UserService::new(
            db.db.clone(),
            encryption.clone(),
        ));
        let oidc = OidcService::new(db.db.clone(), encryption, users_service);
        let provider = oidc
            .upsert_managed_cloud_provider(temps_auth::oidc_service::ManagedCloudOidcConfig {
                issuer: "https://cloud.example.com".to_string(),
                client_id: "cloud-client-id".to_string(),
                client_secret: "cloud-client-secret".to_string(),
            })
            .await
            .expect("managed provider");
        oidc_providers::Entity::update_many()
            .col_expr(
                oidc_providers::Column::IssuerUrl,
                Expr::value("https://127.0.0.1"),
            )
            .filter(oidc_providers::Column::Id.eq(provider.id))
            .exec(db.db.as_ref())
            .await
            .expect("point the provider at a refused address");
        let user = users::ActiveModel {
            name: Set("alice".to_string()),
            email: Set("alice@example.com".to_string()),
            email_verified: Set(true),
            oidc_provider_id: Set(Some(provider.id)),
            oidc_subject: Set(Some("cloud-alice".to_string())),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("user");
        Some(Fixture {
            conn: db.db.clone(),
            _db: db,
            oidc,
            provider_id: provider.id,
            user_id: user.id,
        })
    }

    impl Fixture {
        async fn session(&self, origin: Option<String>) -> i32 {
            sessions::ActiveModel {
                user_id: Set(self.user_id),
                session_token: Set(uuid::Uuid::new_v4().to_string()),
                expires_at: Set(Utc::now() + chrono::Duration::days(7)),
                mfa_pending: Set(false),
                mfa_pending_origin: Set(origin),
                ..Default::default()
            }
            .insert(self.conn.as_ref())
            .await
            .expect("session")
            .id
        }

        async fn cloud_session(&self) -> i32 {
            self.session(Some(format!("oidc:{}", self.provider_id)))
                .await
        }

        async fn confirmed(&self, ago: chrono::Duration) {
            oidc_providers::Entity::update_many()
                .col_expr(
                    oidc_providers::Column::CloudSessionsConfirmedAt,
                    Expr::value(Utc::now() - ago),
                )
                .filter(oidc_providers::Column::Id.eq(self.provider_id))
                .exec(self.conn.as_ref())
                .await
                .expect("record a confirmation");
        }

        async fn exists(&self, session_id: i32) -> bool {
            sessions::Entity::find_by_id(session_id)
                .one(self.conn.as_ref())
                .await
                .expect("query")
                .is_some()
        }
    }

    /// Cloud last confirmed the sessions 11 minutes ago, then the instance
    /// restarted (a fresh checker) while Cloud is still unreachable: its
    /// first pass ends the Cloud sessions instead of granting another 10
    /// minutes. A confirmation within the bound keeps them. Local logins
    /// are never touched.
    #[tokio::test]
    async fn a_restart_does_not_reset_the_cloud_session_cutoff() {
        let Some(fixture) = fixture().await else {
            return;
        };
        let local = fixture.session(None).await;

        let recent = fixture.cloud_session().await;
        fixture.confirmed(chrono::Duration::minutes(2)).await;
        check_once(&fixture.oidc, &mut Freshness::default()).await;
        assert!(fixture.exists(recent).await, "confirmed within the bound");

        fixture.confirmed(chrono::Duration::minutes(11)).await;
        // As after a restart: no state carried over from earlier passes.
        check_once(&fixture.oidc, &mut Freshness::default()).await;
        assert!(
            !fixture.exists(recent).await,
            "a restart must not extend unconfirmed Cloud sessions"
        );
        assert!(fixture.exists(local).await, "local logins are unaffected");
        let confirmed_at = fixture
            .oidc
            .cloud_sessions_confirmed_at()
            .await
            .unwrap()
            .expect("the earlier confirmation stays on record");
        assert!(
            Utc::now() - confirmed_at >= chrono::Duration::minutes(11),
            "an unanswered pass records no confirmation: {confirmed_at}"
        );
    }

    /// Nothing on record (just upgraded, never answered): Cloud sessions
    /// count as unconfirmed, so an outage ends them at once.
    #[tokio::test]
    async fn cloud_sessions_without_a_recorded_confirmation_end_on_an_outage() {
        let Some(fixture) = fixture().await else {
            return;
        };
        let local = fixture.session(None).await;
        let cloud = fixture.cloud_session().await;
        assert_eq!(
            fixture.oidc.cloud_sessions_confirmed_at().await.unwrap(),
            None
        );
        check_once(&fixture.oidc, &mut Freshness::default()).await;
        assert!(!fixture.exists(cloud).await);
        assert!(fixture.exists(local).await, "local logins are unaffected");
    }

    /// An instance that never opted into Cloud has no managed provider: a
    /// pass does nothing and local logins keep working.
    #[tokio::test]
    async fn without_cloud_a_pass_touches_nothing() {
        let db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Docker not available, skipping test: {e}");
                return;
            }
        };
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "console-session-tests",
        ));
        let users_service = Arc::new(temps_auth::UserService::new(
            db.db.clone(),
            encryption.clone(),
        ));
        let oidc = OidcService::new(db.db.clone(), encryption, users_service);
        let user = users::ActiveModel {
            name: Set("local".to_string()),
            email: Set("local@example.com".to_string()),
            email_verified: Set(true),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("user");
        let local = sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set(uuid::Uuid::new_v4().to_string()),
            expires_at: Set(Utc::now() + chrono::Duration::days(7)),
            mfa_pending: Set(false),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .expect("session")
        .id;
        check_once(&oidc, &mut Freshness::default()).await;
        assert!(sessions::Entity::find_by_id(local)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .is_some());
        assert_eq!(oidc.cloud_sessions_confirmed_at().await.unwrap(), None);
    }
}
