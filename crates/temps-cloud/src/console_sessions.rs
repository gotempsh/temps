// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-094: Temps Cloud console sessions end when Cloud stops trusting the
//! user, not days later when the local session expires.
//!
//! - [`ConsoleLogoutAdapter`] applies the signed logout tokens Cloud pushes on
//!   the management channel (`temps-auth` verifies them).
//! - [`run_session_checker`] re-checks live Cloud sessions with Cloud every
//!   [`CLOUD_SESSION_CHECK_INTERVAL`], and ends them all once Cloud has not
//!   answered for [`CLOUD_SESSION_STALE_AFTER`].
//!
//! Both act only on sessions issued through the Cloud-managed provider. On an
//! instance without that provider both are no-ops, and local logins are
//! never touched: when Cloud is down, the instance keeps working.

use std::sync::Arc;
use std::time::Instant;

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

/// Where the fail-closed clock stands.
#[derive(Debug)]
struct Freshness {
    last_confirmed: Instant,
    failures: u32,
}

impl Freshness {
    fn confirmed(&mut self) -> Option<u32> {
        self.last_confirmed = Instant::now();
        let failures = std::mem::take(&mut self.failures);
        (failures > 0).then_some(failures)
    }

    fn stale(&self) -> bool {
        self.last_confirmed.elapsed() >= CLOUD_SESSION_STALE_AFTER
    }
}

/// Re-check Cloud sessions until `cancel` fires. The first pass runs at
/// once, so a restart never extends the bound.
pub async fn run_session_checker(oidc: Arc<OidcService>, mut cancel: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(CLOUD_SESSION_CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut freshness = Freshness {
        last_confirmed: Instant::now(),
        failures: 0,
    };
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
            if freshness.stale() {
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
