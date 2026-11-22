// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bypass tokens this ingress accepts (ADR 066, D7).
//!
//! Only token hashes are held: a loader reads the live rows of
//! `shield_bypass_tokens` every [`POLL_INTERVAL`] and swaps in a new
//! snapshot, so a created token works and a revoked one stops working
//! within that interval. A row without an environment applies to every
//! environment of its project.
//!
//! When a request is admitted on a token, its `last_used_at` is recorded off
//! the request path: at most once per [`USE_DEBOUNCE_SECS`] per token,
//! through a bounded channel to a task that writes the timestamps in one
//! update. A full channel drops the record; the next use a minute later
//! writes it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QuerySelect};
use temps_entities::shield_bypass_tokens;
use temps_shield::bypass::TokenSet;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Shortest interval between two `last_used_at` writes for one token.
pub const USE_DEBOUNCE_SECS: u64 = 60;

/// Most live tokens loaded; more than any installation creates by hand.
const MAX_TOKENS: u64 = 100_000;

/// Token uses waiting to be written.
const USE_CHANNEL_CAPACITY: usize = 256;

/// Token uses written in one update.
const MAX_USES_PER_WRITE: usize = 100;

/// The columns of a live token the ingress needs.
#[derive(sea_orm::FromQueryResult)]
struct LiveToken {
    id: i64,
    project_id: i32,
    environment_id: Option<i32>,
    token_hash: String,
}

#[derive(Default)]
struct Snapshot {
    by_project: HashMap<i32, TokenSet>,
    by_environment: HashMap<i32, TokenSet>,
    /// When each live token's use was last recorded, carried over from one
    /// snapshot to the next.
    last_recorded: HashMap<i64, AtomicU64>,
}

pub struct BypassTokens {
    snapshot: ArcSwap<Snapshot>,
    uses: mpsc::Sender<i64>,
    /// Taken by [`Self::spawn`], which runs the writer.
    receiver: Mutex<Option<mpsc::Receiver<i64>>>,
}

impl Default for BypassTokens {
    fn default() -> Self {
        let (uses, receiver) = mpsc::channel(USE_CHANNEL_CAPACITY);
        Self {
            snapshot: ArcSwap::from_pointee(Snapshot::default()),
            uses,
            receiver: Mutex::new(Some(receiver)),
        }
    }
}

impl BypassTokens {
    /// The tokens valid for `environment_id`: its own and its project's.
    /// Lock-free; allocates only when the environment or its project has a
    /// token.
    pub fn tokens_for(&self, project_id: i32, environment_id: i32) -> TokenSet {
        let snapshot = self.snapshot.load();
        match (
            snapshot.by_project.get(&project_id),
            snapshot.by_environment.get(&environment_id),
        ) {
            (None, None) => TokenSet::default(),
            (Some(one), None) | (None, Some(one)) => one.clone(),
            (Some(project), Some(environment)) => {
                TokenSet::new(project.hashes().chain(environment.hashes()).copied())
            }
        }
    }

    /// Records that token `id` admitted a request, unless its use was
    /// recorded less than [`USE_DEBOUNCE_SECS`] ago. Never blocks.
    pub fn record_use(&self, id: i64, now: u64) {
        let snapshot = self.snapshot.load();
        let Some(slot) = snapshot.last_recorded.get(&id) else {
            return;
        };
        let last = slot.load(Ordering::Relaxed);
        if now < last.saturating_add(USE_DEBOUNCE_SECS) {
            return;
        }
        // One request per minute wins the slot and sends.
        if slot
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            let _ = self.uses.try_send(id);
        }
    }

    /// Loads the live tokens and swaps them in. Returns how many there are.
    pub async fn refresh(&self, db: &DatabaseConnection) -> Result<usize, DbErr> {
        let rows: Vec<LiveToken> = shield_bypass_tokens::Entity::find()
            .select_only()
            .column(shield_bypass_tokens::Column::Id)
            .column(shield_bypass_tokens::Column::ProjectId)
            .column(shield_bypass_tokens::Column::EnvironmentId)
            .column(shield_bypass_tokens::Column::TokenHash)
            .filter(shield_bypass_tokens::Column::RevokedAt.is_null())
            .limit(MAX_TOKENS)
            .into_model()
            .all(db)
            .await?;
        if rows.len() as u64 == MAX_TOKENS {
            warn!(
                limit = MAX_TOKENS,
                "More live Shield bypass tokens than this ingress loads; some are ignored"
            );
        }
        let count = rows.len();
        let previous = self.snapshot.load();
        let mut by_project: HashMap<i32, Vec<&str>> = HashMap::new();
        let mut by_environment: HashMap<i32, Vec<&str>> = HashMap::new();
        let mut last_recorded = HashMap::with_capacity(rows.len());
        for LiveToken {
            id,
            project_id,
            environment_id,
            token_hash,
        } in &rows
        {
            match environment_id {
                Some(environment_id) => by_environment.entry(*environment_id),
                None => by_project.entry(*project_id),
            }
            .or_default()
            .push(token_hash.as_str());
            let last = previous
                .last_recorded
                .get(id)
                .map(|slot| slot.load(Ordering::Relaxed))
                .unwrap_or(0);
            last_recorded.insert(*id, AtomicU64::new(last));
        }
        let sets = |grouped: HashMap<i32, Vec<&str>>| {
            grouped
                .into_iter()
                .map(|(key, hashes)| (key, TokenSet::from_hex(&hashes)))
                .filter(|(_, set)| !set.is_empty())
                .collect()
        };
        self.snapshot.store(Arc::new(Snapshot {
            by_project: sets(by_project),
            by_environment: sets(by_environment),
            last_recorded,
        }));
        Ok(count)
    }

    /// Starts the loader and the `last_used_at` writer on `handle`'s
    /// runtime.
    pub fn spawn(self: &Arc<Self>, db: Arc<DatabaseConnection>, handle: &tokio::runtime::Handle) {
        let receiver = self
            .receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(receiver) = receiver {
            handle.spawn(write_uses(receiver, db.clone()));
        }
        handle.spawn(Arc::clone(self).run_loader(db));
    }

    async fn run_loader(self: Arc<Self>, db: Arc<DatabaseConnection>) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut consecutive_failures: u64 = 0;
        loop {
            ticker.tick().await;
            match self.refresh(&db).await {
                Ok(count) => {
                    consecutive_failures = 0;
                    debug!(tokens = count, "Shield bypass tokens loaded");
                }
                Err(error) => {
                    consecutive_failures += 1;
                    // First failure, then about every ten minutes.
                    if consecutive_failures == 1 || consecutive_failures.is_multiple_of(20) {
                        warn!(
                            failed_polls = consecutive_failures,
                            %error,
                            "Could not load Shield bypass tokens; keeping the loaded ones"
                        );
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn take_receiver(&self) -> Option<mpsc::Receiver<i64>> {
        self.receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

async fn write_uses(mut receiver: mpsc::Receiver<i64>, db: Arc<DatabaseConnection>) {
    let mut ids = Vec::with_capacity(MAX_USES_PER_WRITE);
    while receiver.recv_many(&mut ids, MAX_USES_PER_WRITE).await > 0 {
        ids.sort_unstable();
        ids.dedup();
        if let Err(error) = record_last_used(&db, &ids).await {
            warn!(%error, tokens = ids.len(), "Could not record Shield bypass token use");
        }
        ids.clear();
    }
}

/// Stamps `last_used_at` on the live tokens among `ids`.
pub(crate) async fn record_last_used(db: &DatabaseConnection, ids: &[i64]) -> Result<(), DbErr> {
    shield_bypass_tokens::Entity::update_many()
        .col_expr(
            shield_bypass_tokens::Column::LastUsedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(shield_bypass_tokens::Column::Id.is_in(ids.iter().copied()))
        .filter(shield_bypass_tokens::Column::RevokedAt.is_null())
        .exec(db)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_shield::bypass;

    fn rows(
        rows: &[(i64, i32, Option<i32>, &str)],
    ) -> Vec<std::collections::BTreeMap<&'static str, sea_orm::Value>> {
        rows.iter()
            .map(|(id, project, environment, hash)| {
                std::collections::BTreeMap::from([
                    ("id", sea_orm::Value::from(*id)),
                    ("project_id", sea_orm::Value::from(*project)),
                    ("environment_id", sea_orm::Value::from(*environment)),
                    ("token_hash", sea_orm::Value::from(hash.to_string())),
                ])
            })
            .collect()
    }

    #[tokio::test]
    async fn project_tokens_apply_to_every_environment_and_environment_tokens_to_one() {
        let (project_token, project_hash) = bypass::generate(1);
        let (env_token, env_hash) = bypass::generate(2);
        let project_hex = bypass::to_hex(&project_hash);
        let env_hex = bypass::to_hex(&env_hash);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows(&[
                (1, 10, None, &project_hex),
                (2, 10, Some(100), &env_hex),
            ])])
            .append_query_results([rows(&[(2, 10, Some(100), &env_hex)])])
            .into_connection();
        let tokens = BypassTokens::default();
        assert_eq!(tokens.refresh(&db).await.unwrap(), 2);

        let env = tokens.tokens_for(10, 100);
        assert_eq!(env.check(&project_token), Some(1));
        assert_eq!(env.check(&env_token), Some(2));
        let sibling = tokens.tokens_for(10, 101);
        assert_eq!(sibling.check(&project_token), Some(1));
        assert_eq!(sibling.check(&env_token), None);
        assert!(tokens.tokens_for(11, 110).is_empty());

        // The next load no longer holds token 1: revoked.
        tokens.refresh(&db).await.unwrap();
        assert_eq!(tokens.tokens_for(10, 100).check(&project_token), None);
        assert_eq!(tokens.tokens_for(10, 100).check(&env_token), Some(2));
        let sql = db.into_transaction_log()[0].statements()[0].sql.clone();
        assert!(sql.contains(r#""revoked_at" IS NULL"#), "{sql}");
    }

    #[tokio::test]
    async fn a_use_is_recorded_at_most_once_a_minute_and_survives_reloads() {
        let (_, hash) = bypass::generate(7);
        let hex = bypass::to_hex(&hash);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows(&[(7, 1, None, &hex)])])
            .append_query_results([rows(&[(7, 1, None, &hex)])])
            .into_connection();
        let tokens = BypassTokens::default();
        let mut uses = tokens.take_receiver().unwrap();
        tokens.refresh(&db).await.unwrap();

        let now = 1_800_000_000;
        tokens.record_use(7, now);
        tokens.record_use(7, now + 1);
        // A reload keeps the debounce.
        tokens.refresh(&db).await.unwrap();
        tokens.record_use(7, now + USE_DEBOUNCE_SECS - 1);
        // An id no live token has is not recorded.
        tokens.record_use(8, now);
        tokens.record_use(7, now + USE_DEBOUNCE_SECS);
        assert_eq!(uses.try_recv().ok(), Some(7));
        assert_eq!(uses.try_recv().ok(), Some(7));
        assert!(uses.try_recv().is_err());
    }

    #[tokio::test]
    async fn last_used_is_stamped_only_on_live_tokens() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .into_connection();
        record_last_used(&db, &[3, 4]).await.unwrap();
        let sql = db.into_transaction_log()[0].statements()[0].sql.clone();
        assert!(
            sql.starts_with(r#"UPDATE "shield_bypass_tokens" SET "last_used_at""#),
            "{sql}"
        );
        assert!(sql.contains(r#""revoked_at" IS NULL"#), "{sql}");
    }
}
