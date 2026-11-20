// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cookieless visitor ids (ADR 109 D3).
//!
//! A cookieless project stores nothing on the visitor's device. Its visitor
//! id is `SHA-256(daily salt, project, client address, user agent)`, so the
//! same browser maps to the same id for one UTC day and to an unrelated id
//! the next. The salt is 32 random bytes per UTC day, stored encrypted in
//! `analytics_identity_salts` so every ingesting node derives the same id,
//! and deleted once the day is over: after that an id cannot be recomputed
//! from anything Temps keeps.
//!
//! A session is the visitor's activity with no gap longer than
//! [`SESSION_IDLE`]. With nothing on the device to carry a session id, the
//! current session of each visitor is kept in a bounded in-memory cache
//! whose entries expire after [`SESSION_IDLE`] without activity. A miss
//! (first event, a restart, or another node) asks the database once for the
//! visitor's last session inside the window, so a session survives both.
//! When there is none, the new session's id is derived from the visitor and
//! the 30-minute window it starts in, so two nodes that both see a visitor's
//! first events before either is stored still pick the same session.
//!
//! Cost per event: one lock-free load of the in-memory salt, one hash and one
//! cache lookup. The database is read once per node per day for the salt, and
//! once per session start for the session.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use chrono::{NaiveDate, Utc};
use moka::future::Cache;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};
use temps_core::EncryptionService;
use temps_entities::analytics_identity_mode::AnalyticsIdentityMode;
use thiserror::Error;

/// Prefix of every cookieless visitor id, so a stored id says how it was made.
pub const COOKIELESS_VISITOR_PREFIX: &str = "cl_";

/// Inactivity after which a cookieless visitor's next event starts a new
/// session. The same 30 minutes as the persistent `_temps_sid` cookie.
pub const SESSION_IDLE: Duration = Duration::from_secs(30 * 60);

/// Visitors whose current session is held in memory. At roughly 200 bytes
/// per entry this caps the cache near 10 MB; past it the least recently seen
/// visitors are evicted, and their next event looks the session up again.
const SESSION_CACHE_CAPACITY: u64 = 50_000;

/// How long a project's identity mode is cached on the keyed ingest path,
/// which has no route-table entry to read it from.
const MODE_CACHE_TTL: Duration = Duration::from_secs(60);
const MODE_CACHE_CAPACITY: u64 = 10_000;

const SALT_BYTES: usize = 32;

#[derive(Debug, Error)]
pub enum CookielessIdentityError {
    #[error("Database error while {operation} the cookieless salt for {day}: {source}")]
    Database {
        day: NaiveDate,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Cookieless salt for {day} could not be {operation}: {reason}")]
    Crypto {
        day: NaiveDate,
        operation: &'static str,
        reason: String,
    },
    #[error("Cookieless salt for {day} is missing after it was created")]
    Missing { day: NaiveDate },
    #[error("Looking up the current session of cookieless visitor {visitor_id} in project {project_id} failed: {reason}")]
    Session {
        project_id: i32,
        visitor_id: String,
        reason: String,
    },
    #[error("Reading the analytics identity mode of project {project_id} failed: {reason}")]
    Mode { project_id: i32, reason: String },
}

/// The visitor and session a cookieless event belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookielessVisit {
    pub visitor_id: String,
    pub session_id: String,
}

struct DailySalt {
    day: NaiveDate,
    key: [u8; SALT_BYTES],
}

pub struct CookielessIdentity {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    current: ArcSwapOption<DailySalt>,
    refresh: tokio::sync::Mutex<()>,
    /// (project, visitor id) -> current session id.
    sessions: Cache<(i32, String), String>,
    /// project -> identity mode, for the keyed ingest path.
    modes: Cache<i32, AnalyticsIdentityMode>,
}

/// The id of one visitor for one day. Each part is length-prefixed so two
/// different inputs can never concatenate to the same bytes.
pub fn derive_visitor_id(
    salt: &[u8],
    project_id: i32,
    client_ip: &str,
    user_agent: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(project_id.to_be_bytes());
    for part in [client_ip, user_agent] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    // 128 bits is far beyond what one project's visitors in one day can
    // collide on, and keeps the id the length of a UUID.
    format!("{COOKIELESS_VISITOR_PREFIX}{}", hex::encode(&digest[..16]))
}

/// The id of a session `visitor_id` starts at `started_at`: a UUIDv5 of the
/// project, the visitor and the [`SESSION_IDLE`] window containing the start.
/// Nodes racing on a visitor's first events derive the same id. A later
/// session of the same visitor always starts in a later window, since it
/// begins at least [`SESSION_IDLE`] after the previous one started.
pub fn derive_session_id(
    project_id: i32,
    visitor_id: &str,
    started_at: chrono::DateTime<Utc>,
) -> String {
    let window = started_at
        .timestamp()
        .div_euclid(SESSION_IDLE.as_secs() as i64);
    let name = format!("temps-cookieless-session:{project_id}:{visitor_id}:{window}");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

impl CookielessIdentity {
    pub fn new(db: Arc<DatabaseConnection>, encryption: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption,
            current: ArcSwapOption::empty(),
            refresh: tokio::sync::Mutex::new(()),
            sessions: Cache::builder()
                .max_capacity(SESSION_CACHE_CAPACITY)
                .time_to_idle(SESSION_IDLE)
                .build(),
            modes: Cache::builder()
                .max_capacity(MODE_CACHE_CAPACITY)
                .time_to_live(MODE_CACHE_TTL)
                .build(),
        }
    }

    /// The visitor and session of an event from `client_ip` and `user_agent`.
    pub async fn visit(
        &self,
        project_id: i32,
        client_ip: &str,
        user_agent: &str,
    ) -> Result<CookielessVisit, CookielessIdentityError> {
        let visitor_id = self.visitor_id(project_id, client_ip, user_agent).await?;
        let session_id = self.session_id(project_id, &visitor_id).await?;
        Ok(CookielessVisit {
            visitor_id,
            session_id,
        })
    }

    /// Today's id of the visitor behind `client_ip` and `user_agent`.
    pub async fn visitor_id(
        &self,
        project_id: i32,
        client_ip: &str,
        user_agent: &str,
    ) -> Result<String, CookielessIdentityError> {
        let salt = self.salt_for(Utc::now().date_naive()).await?;
        Ok(derive_visitor_id(
            &salt.key, project_id, client_ip, user_agent,
        ))
    }

    /// The visitor's current session, or a new one after [`SESSION_IDLE`]
    /// without activity. Concurrent first events of one visitor share the
    /// one lookup instead of each starting a session.
    async fn session_id(
        &self,
        project_id: i32,
        visitor_id: &str,
    ) -> Result<String, CookielessIdentityError> {
        let db = self.db.clone();
        let visitor = visitor_id.to_string();
        let derived = derive_session_id(project_id, visitor_id, Utc::now());
        self.sessions
            .try_get_with((project_id, visitor_id.to_string()), async move {
                let since = Utc::now() - chrono::Duration::minutes(30);
                let row = db
                    .query_one(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT e.session_id FROM visitor v \
                         JOIN events e ON e.visitor_id = v.id AND e.timestamp > $3 \
                         WHERE v.project_id = $1 AND v.visitor_id = $2 AND v.last_seen > $3 \
                         ORDER BY e.timestamp DESC LIMIT 1",
                        [project_id.into(), visitor.into(), since.into()],
                    ))
                    .await?;
                let existing = match row {
                    Some(row) => row.try_get::<Option<String>>("", "session_id")?,
                    None => None,
                };
                Ok::<_, sea_orm::DbErr>(existing.unwrap_or(derived))
            })
            .await
            .map_err(|error| CookielessIdentityError::Session {
                project_id,
                visitor_id: visitor_id.to_string(),
                reason: error.to_string(),
            })
    }

    /// The identity mode of `project_id`, cached for [`MODE_CACHE_TTL`]. For
    /// ingest paths that have no route-table entry carrying the project row.
    /// An unknown project reads as `persistent`, today's behaviour.
    pub async fn identity_mode(
        &self,
        project_id: i32,
    ) -> Result<AnalyticsIdentityMode, CookielessIdentityError> {
        let db = self.db.clone();
        self.modes
            .try_get_with(project_id, async move {
                let row = db
                    .query_one(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT analytics_identity_mode FROM projects WHERE id = $1",
                        [project_id.into()],
                    ))
                    .await?;
                let stored = match row {
                    Some(row) => Some(row.try_get::<String>("", "analytics_identity_mode")?),
                    None => None,
                };
                Ok::<_, sea_orm::DbErr>(mode_from_stored(stored.as_deref()))
            })
            .await
            .map_err(|error| CookielessIdentityError::Mode {
                project_id,
                reason: error.to_string(),
            })
    }

    /// Drop the cached mode of `project_id` after it changed, so this
    /// process applies the new mode at once instead of after the TTL.
    pub async fn forget_identity_mode(&self, project_id: i32) {
        self.modes.invalidate(&project_id).await;
    }

    async fn salt_for(&self, day: NaiveDate) -> Result<Arc<DailySalt>, CookielessIdentityError> {
        if let Some(salt) = self.current.load_full() {
            if salt.day == day {
                return Ok(salt);
            }
        }
        // Day changed (or first event since start): one caller refreshes,
        // the others wait for it and then reuse what it loaded.
        let _refreshing = self.refresh.lock().await;
        if let Some(salt) = self.current.load_full() {
            if salt.day == day {
                return Ok(salt);
            }
        }
        let salt = Arc::new(self.load_or_create(day).await?);
        self.current.store(Some(salt.clone()));
        Ok(salt)
    }

    /// The salt of `day`, creating it when no node has yet. Two nodes racing
    /// at midnight both insert; `ON CONFLICT DO NOTHING` keeps the first, and
    /// both then read that one back.
    async fn load_or_create(&self, day: NaiveDate) -> Result<DailySalt, CookielessIdentityError> {
        let fresh: [u8; SALT_BYTES] = rand::random();
        let sealed =
            self.encryption
                .encrypt(&fresh)
                .map_err(|e| CookielessIdentityError::Crypto {
                    day,
                    operation: "encrypted",
                    reason: e.to_string(),
                })?;
        let db_error = |operation| {
            move |source| CookielessIdentityError::Database {
                day,
                operation,
                source,
            }
        };
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO analytics_identity_salts (day, salt) VALUES ($1, $2) \
                 ON CONFLICT (day) DO NOTHING",
                [day.into(), sealed.into()],
            ))
            .await
            .map_err(db_error("creating"))?;
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT salt FROM analytics_identity_salts WHERE day = $1",
                [day.into()],
            ))
            .await
            .map_err(db_error("reading"))?
            .ok_or(CookielessIdentityError::Missing { day })?;
        let stored: String = row.try_get("", "salt").map_err(db_error("decoding"))?;
        let bytes =
            self.encryption
                .decrypt(&stored)
                .map_err(|e| CookielessIdentityError::Crypto {
                    day,
                    operation: "decrypted",
                    reason: e.to_string(),
                })?;
        let key: [u8; SALT_BYTES] =
            bytes
                .try_into()
                .map_err(|bytes: Vec<u8>| CookielessIdentityError::Crypto {
                    day,
                    operation: "used",
                    reason: format!("expected {SALT_BYTES} bytes, got {}", bytes.len()),
                })?;
        self.delete_before(day).await?;
        Ok(DailySalt { day, key })
    }

    /// Delete every salt older than `day`. Runs on each daily refresh and
    /// from an hourly sweep, so an instance that receives no cookieless
    /// traffic still deletes yesterday's salt. Returns the rows deleted.
    pub async fn delete_before(&self, day: NaiveDate) -> Result<u64, CookielessIdentityError> {
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM analytics_identity_salts WHERE day < $1",
                [day.into()],
            ))
            .await
            .map_err(|source| CookielessIdentityError::Database {
                day,
                operation: "deleting salts before",
                source,
            })?;
        Ok(result.rows_affected())
    }
}

/// A stored mode, read leniently: the CHECK constraint allows only the two
/// values, and anything else (or no row) keeps today's behaviour.
fn mode_from_stored(stored: Option<&str>) -> AnalyticsIdentityMode {
    match stored {
        Some("cookieless") => AnalyticsIdentityMode::Cookieless,
        _ => AnalyticsIdentityMode::Persistent,
    }
}

/// Start the hourly sweep that deletes salts of past days even when no
/// cookieless event arrives to trigger the daily refresh.
pub fn spawn_salt_sweep(identity: Arc<CookielessIdentity>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let today = Utc::now().date_naive();
            match identity.delete_before(today).await {
                Ok(0) => {}
                Ok(deleted) => {
                    tracing::info!(deleted, %today, "Deleted expired cookieless visitor salts")
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Cookieless salt sweep failed; retrying next hour")
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn racing_nodes_derive_the_same_new_session_and_a_later_session_differs() {
        use chrono::TimeZone;
        let start = Utc
            .with_ymd_and_hms(2026, 11, 20, 10, 5, 0)
            .single()
            .expect("valid time");
        let a = derive_session_id(7, "cl_abc", start);
        // Another node, a few seconds later, inside the same window.
        assert_eq!(
            a,
            derive_session_id(7, "cl_abc", start + chrono::Duration::seconds(20))
        );
        assert!(uuid::Uuid::parse_str(&a).is_ok(), "{a}");
        // A new session starts at least SESSION_IDLE later: another window.
        assert_ne!(
            a,
            derive_session_id(7, "cl_abc", start + chrono::Duration::minutes(30))
        );
        // Other visitors and projects never share it.
        assert_ne!(a, derive_session_id(7, "cl_def", start));
        assert_ne!(a, derive_session_id(8, "cl_abc", start));
    }

    #[test]
    fn an_id_is_stable_within_a_salt_and_unrelated_across_salts() {
        let today = [1u8; SALT_BYTES];
        let tomorrow = [2u8; SALT_BYTES];
        let a = derive_visitor_id(&today, 7, "203.0.113.9", "Mozilla/5.0");
        assert_eq!(
            a,
            derive_visitor_id(&today, 7, "203.0.113.9", "Mozilla/5.0")
        );
        assert!(a.starts_with(COOKIELESS_VISITOR_PREFIX));
        assert_eq!(a.len(), COOKIELESS_VISITOR_PREFIX.len() + 32);
        assert_ne!(
            a,
            derive_visitor_id(&tomorrow, 7, "203.0.113.9", "Mozilla/5.0")
        );
    }

    #[test]
    fn every_input_changes_the_id() {
        let salt = [9u8; SALT_BYTES];
        let base = derive_visitor_id(&salt, 7, "203.0.113.9", "UA");
        assert_ne!(base, derive_visitor_id(&salt, 8, "203.0.113.9", "UA"));
        assert_ne!(base, derive_visitor_id(&salt, 7, "203.0.113.10", "UA"));
        assert_ne!(base, derive_visitor_id(&salt, 7, "203.0.113.9", "UA2"));
        // Moving a character between address and user agent is a different
        // visitor, not a collision.
        assert_ne!(
            derive_visitor_id(&salt, 7, "1.2.3.4", "5UA"),
            derive_visitor_id(&salt, 7, "1.2.3.45", "UA")
        );
    }

    #[test]
    fn unknown_stored_modes_keep_persistent_identity() {
        assert_eq!(
            mode_from_stored(Some("cookieless")),
            AnalyticsIdentityMode::Cookieless
        );
        assert_eq!(
            mode_from_stored(Some("persistent")),
            AnalyticsIdentityMode::Persistent
        );
        assert_eq!(
            mode_from_stored(Some("other")),
            AnalyticsIdentityMode::Persistent
        );
        assert_eq!(mode_from_stored(None), AnalyticsIdentityMode::Persistent);
    }

    #[tokio::test]
    async fn the_salt_is_shared_by_nodes_and_only_today_is_kept() {
        let test_db = match crate::testing::TestDatabase::new("cookieless_salt").await {
            Ok(test_db) => test_db,
            Err(error) => {
                println!("Docker not available ({error}), skipping");
                return;
            }
        };
        let db = test_db.db.clone();
        let encryption = Arc::new(
            EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("valid test key"),
        );
        let node_a = CookielessIdentity::new(db.clone(), encryption.clone());
        let node_b = CookielessIdentity::new(db.clone(), encryption.clone());

        // Yesterday's salt exists; today's does not yet.
        let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
        node_a
            .load_or_create(yesterday)
            .await
            .expect("yesterday's salt");

        let a = node_a
            .visitor_id(7, "203.0.113.9", "Mozilla/5.0")
            .await
            .expect("node a derives an id");
        let b = node_b
            .visitor_id(7, "203.0.113.9", "Mozilla/5.0")
            .await
            .expect("node b derives an id");
        assert_eq!(
            a, b,
            "two nodes must derive the same id from the shared salt"
        );

        let remaining = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT day FROM analytics_identity_salts".to_string(),
            ))
            .await
            .expect("list salts");
        assert_eq!(remaining.len(), 1, "only today's salt is kept");
        let stored = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT salt FROM analytics_identity_salts".to_string(),
            ))
            .await
            .expect("read salt")
            .expect("one salt");
        let sealed: String = stored.try_get("", "salt").expect("salt column");
        assert_ne!(
            sealed.len(),
            SALT_BYTES * 2,
            "the salt is stored encrypted, not as plain hex"
        );

        // The same visitor stays in one session; another visitor gets its own.
        let first = node_a
            .visit(7, "203.0.113.9", "Mozilla/5.0")
            .await
            .expect("first visit");
        let second = node_a
            .visit(7, "203.0.113.9", "Mozilla/5.0")
            .await
            .expect("second visit");
        assert_eq!(first, second);
        let other = node_a
            .visit(7, "198.51.100.4", "Mozilla/5.0")
            .await
            .expect("other visitor");
        assert_ne!(other.visitor_id, first.visitor_id);
        assert_ne!(other.session_id, first.session_id);

        // An unknown project reads as persistent.
        assert_eq!(
            node_a.identity_mode(999_999).await.expect("mode"),
            AnalyticsIdentityMode::Persistent
        );
    }
}
