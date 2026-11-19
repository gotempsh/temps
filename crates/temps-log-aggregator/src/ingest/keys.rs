// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Log ingest keys (ADR 108 D1): the credential a collector outside Temps
//! presents to `POST /logs/ingest`.
//!
//! A key belongs to one project and optionally pins one environment. Unlike
//! an analytics ingest key, which ships in browser JavaScript and is public
//! by design, a log key lives in a server-side collector (Vector, Fluent Bit,
//! an app) and writes arbitrary text into the project's log store, so it is a
//! secret: only its SHA-256 is stored, the plaintext is returned once when the
//! key is created, and the first characters are kept for display.
//!
//! Authentication runs once per request, not per line. A resolved key is
//! cached for [`KEY_CACHE_TTL`] in a bounded map, and revoking a key through
//! this service drops it from the cache at once. Usage counters are summed in
//! memory and written by [`LogIngestKeyService::flush_usage`] on an interval,
//! so a busy collector costs no database write per request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rand::RngExt;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Statement, TransactionTrait,
};
use sha2::{Digest, Sha256};
use tracing::{error, warn};

use temps_entities::log_ingest_keys::{ActiveModel, Column, Entity, Model};

use crate::error::LogAggregatorError;

/// Every log ingest key starts with this, so a leaked one is recognisable in
/// a secret scanner and never confused with an API key (`tk_`).
pub const LOG_INGEST_KEY_PREFIX: &str = "tlk_";
const KEY_BYTES: usize = 32;
/// Characters of the plaintext kept for display (`tlk_` plus eight).
const DISPLAY_PREFIX_LEN: usize = 12;

/// Active keys per project. A key per collector or host is plenty.
pub const MAX_ACTIVE_KEYS_PER_PROJECT: u64 = 20;
const MAX_NAME_LEN: usize = 100;

/// How long a resolved key is trusted without asking the database again. A
/// key revoked on another path than this service (a project delete) stops
/// working within this long.
pub const KEY_CACHE_TTL: Duration = Duration::from_secs(30);
const KEY_CACHE_CAPACITY: usize = 1_024;

/// Service label used when neither the line nor the key names one.
pub const DEFAULT_INGEST_SERVICE: &str = "external";
/// Longest service label accepted from a line or a key.
pub const MAX_SERVICE_LEN: usize = 63;

/// What a presented key resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedKey {
    pub key_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    /// Service label for lines that do not carry one.
    pub default_service: String,
}

/// A key to create.
#[derive(Debug)]
pub struct NewLogIngestKey {
    pub name: String,
    pub environment_id: Option<i32>,
    /// Defaults to [`DEFAULT_INGEST_SERVICE`].
    pub default_service: Option<String>,
    pub created_by: Option<i32>,
}

/// A key and, only right after creation, its plaintext.
#[derive(Debug)]
pub struct CreatedLogIngestKey {
    pub key: Model,
    pub plaintext: String,
}

#[derive(Debug, Default)]
struct Usage {
    lines: u64,
    last_used_at: Option<DateTime<Utc>>,
}

/// Resolved keys, and how many revocations this service has seen.
///
/// A lookup notes `revocations` before it reads the database and publishes
/// its result only if the number is unchanged. A revoke that lands while the
/// lookup awaits its queries bumps the number under this same lock, so a
/// key read just before it was revoked is never put back after the revoke
/// evicted it.
#[derive(Default)]
struct KeyCache {
    entries: HashMap<String, (AuthenticatedKey, Instant)>,
    revocations: u64,
}

impl KeyCache {
    /// Cache `resolved` unless a revocation happened since `seen`. Returns
    /// whether it was cached.
    fn publish(&mut self, hash: String, resolved: AuthenticatedKey, seen: u64) -> bool {
        if self.revocations != seen {
            return false;
        }
        if self.entries.len() >= KEY_CACHE_CAPACITY {
            let now = Instant::now();
            self.entries
                .retain(|_, (_, at)| now.duration_since(*at) < KEY_CACHE_TTL);
            if self.entries.len() >= KEY_CACHE_CAPACITY {
                self.entries.clear();
            }
        }
        self.entries.insert(hash, (resolved, Instant::now()));
        true
    }

    fn revoke(&mut self, hash: &str) {
        self.revocations = self.revocations.wrapping_add(1);
        self.entries.remove(hash);
    }
}

pub struct LogIngestKeyService {
    db: Arc<DatabaseConnection>,
    cache: Mutex<KeyCache>,
    usage: Mutex<HashMap<i32, Usage>>,
}

impl LogIngestKeyService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            cache: Mutex::new(KeyCache::default()),
            usage: Mutex::new(HashMap::new()),
        }
    }

    /// A project's keys, active first, newest first.
    pub async fn list(
        &self,
        project_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<Model>, u64), LogAggregatorError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let paginator = Entity::find()
            .filter(Column::ProjectId.eq(project_id))
            .order_by_desc(Column::IsActive)
            .order_by_desc(Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;
        Ok((items, total))
    }

    pub async fn get(&self, project_id: i32, key_id: i32) -> Result<Model, LogAggregatorError> {
        Entity::find_by_id(key_id)
            .filter(Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(LogAggregatorError::IngestKeyNotFound { project_id, key_id })
    }

    /// Mint a key. The plaintext is in the result and nowhere else.
    pub async fn create(
        &self,
        project_id: i32,
        new: NewLogIngestKey,
    ) -> Result<CreatedLogIngestKey, LogAggregatorError> {
        let name = new.name.trim().to_string();
        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return Err(LogAggregatorError::Validation {
                message: format!("A log ingest key needs a name of 1 to {MAX_NAME_LEN} characters"),
            });
        }
        let default_service = match new.default_service.as_deref().map(str::trim) {
            None | Some("") => DEFAULT_INGEST_SERVICE.to_string(),
            Some(raw) => sanitize_service(raw).ok_or_else(|| LogAggregatorError::Validation {
                message: format!(
                    "Invalid default service '{raw}': use letters, digits, '.', '_' or '-', at \
                     most {MAX_SERVICE_LEN} characters"
                ),
            })?,
        };

        self.ensure_project(project_id).await?;
        if let Some(environment_id) = new.environment_id {
            self.ensure_environment(project_id, environment_id).await?;
        }
        // Counted and inserted under a lock on the project row, so two
        // concurrent creations cannot both pass the active-key limit.
        let txn = self.db.begin().await?;
        if let Err(e) = crate::services::saved_search::lock_project(&txn, project_id).await {
            txn.rollback().await?;
            return Err(e);
        }
        let active = Entity::find()
            .filter(Column::ProjectId.eq(project_id))
            .filter(Column::IsActive.eq(true))
            .count(&txn)
            .await?;
        if active >= MAX_ACTIVE_KEYS_PER_PROJECT {
            txn.rollback().await?;
            return Err(LogAggregatorError::Validation {
                message: format!(
                    "Project {project_id} already has {active} active log ingest keys; the limit \
                     is {MAX_ACTIVE_KEYS_PER_PROJECT}. Revoke one you no longer use"
                ),
            });
        }

        let plaintext = generate_key();
        let key = ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(new.environment_id),
            name: Set(name),
            key_prefix: Set(plaintext[..DISPLAY_PREFIX_LEN].to_string()),
            key_hash: Set(hash_key(&plaintext)),
            default_service: Set(default_service),
            is_active: Set(true),
            created_by_user_id: Set(new.created_by),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
        txn.commit().await?;
        Ok(CreatedLogIngestKey { key, plaintext })
    }

    /// Stop a key from working. Soft: the row (and what it ingested) stays.
    pub async fn revoke(&self, project_id: i32, key_id: i32) -> Result<Model, LogAggregatorError> {
        let existing = self.get(project_id, key_id).await?;
        let hash = existing.key_hash.clone();
        let model = if existing.is_active {
            let mut active: ActiveModel = existing.into();
            active.is_active = Set(false);
            active.revoked_at = Set(Some(Utc::now()));
            active.update(self.db.as_ref()).await?
        } else {
            existing
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.revoke(&hash);
        }
        Ok(model)
    }

    /// Resolve a presented key, or `IngestKeyRejected`.
    ///
    /// The shape is checked before anything else, so a malformed value costs
    /// neither a cache slot nor a query.
    pub async fn authenticate(
        &self,
        presented: &str,
    ) -> Result<AuthenticatedKey, LogAggregatorError> {
        if !is_well_formed(presented) {
            return Err(LogAggregatorError::IngestKeyRejected);
        }
        let hash = hash_key(presented);
        // A poisoned lock reads as a miss; publishing then fails on the same
        // lock, so every lookup goes to the database and nothing is cached.
        let (cached, revocations_seen) = self.cached(&hash).unwrap_or((None, 0));
        if let Some(found) = cached {
            return Ok(found);
        }

        let Some(key) = Entity::find()
            .filter(Column::KeyHash.eq(hash.clone()))
            .filter(Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
        else {
            return Err(LogAggregatorError::IngestKeyRejected);
        };
        // A key of a deleted project, or pinned to a deleted environment,
        // must not resolve: lines would land under an owner nobody can see.
        if !self.project_is_live(key.project_id).await? {
            return Err(LogAggregatorError::IngestKeyRejected);
        }
        if let Some(environment_id) = key.environment_id {
            if !self
                .environment_is_live(key.project_id, environment_id)
                .await?
            {
                return Err(LogAggregatorError::IngestKeyRejected);
            }
        }

        let resolved = AuthenticatedKey {
            key_id: key.id,
            project_id: key.project_id,
            environment_id: key.environment_id,
            default_service: key.default_service,
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.publish(hash, resolved.clone(), revocations_seen);
        }
        Ok(resolved)
    }

    /// The cached resolution of `hash`, if fresh, and the revocation count
    /// at the time of asking. `None` only when the lock is poisoned.
    fn cached(&self, hash: &str) -> Option<(Option<AuthenticatedKey>, u64)> {
        let cache = self.cache.lock().ok()?;
        let found = cache
            .entries
            .get(hash)
            .filter(|(_, at)| at.elapsed() < KEY_CACHE_TTL)
            .map(|(key, _)| key.clone());
        Some((found, cache.revocations))
    }

    /// Count lines a key delivered. In memory; see [`Self::flush_usage`].
    pub fn record_usage(&self, key_id: i32, lines: u64, at: DateTime<Utc>) {
        match self.usage.lock() {
            Ok(mut usage) => {
                let entry = usage.entry(key_id).or_default();
                entry.lines += lines;
                entry.last_used_at = Some(at);
            }
            Err(e) => warn!(key_id, error = %e, "log ingest key usage counter is poisoned"),
        }
    }

    /// Write the summed usage counters, one statement per key that was used
    /// since the last flush. Returns how many keys were written. A failed
    /// write puts its counts back for the next flush.
    pub async fn flush_usage(&self) -> u64 {
        let pending: Vec<(i32, Usage)> = match self.usage.lock() {
            Ok(mut usage) => usage.drain().collect(),
            Err(e) => {
                error!(error = %e, "log ingest key usage counter is poisoned; not flushing");
                return 0;
            }
        };
        let mut written = 0;
        for (key_id, usage) in pending {
            let lines = i64::try_from(usage.lines).unwrap_or(i64::MAX);
            let result = self
                .db
                .execute(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    "UPDATE log_ingest_keys SET line_count = line_count + $1, \
                     last_used_at = GREATEST(COALESCE(last_used_at, $2), $2) WHERE id = $3",
                    [lines.into(), usage.last_used_at.into(), key_id.into()],
                ))
                .await;
            match result {
                Ok(_) => written += 1,
                Err(e) => {
                    warn!(key_id, error = %e, "Failed to record log ingest key usage; retrying on the next flush");
                    if let Ok(mut map) = self.usage.lock() {
                        let entry = map.entry(key_id).or_default();
                        entry.lines += usage.lines;
                        entry.last_used_at = entry.last_used_at.max(usage.last_used_at);
                    }
                }
            }
        }
        written
    }

    async fn ensure_project(&self, project_id: i32) -> Result<(), LogAggregatorError> {
        if self.project_is_live(project_id).await? {
            Ok(())
        } else {
            Err(LogAggregatorError::ProjectNotFound { project_id })
        }
    }

    async fn ensure_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), LogAggregatorError> {
        if self.environment_is_live(project_id, environment_id).await? {
            Ok(())
        } else {
            Err(LogAggregatorError::IngestKeyEnvironmentNotFound {
                project_id,
                environment_id,
            })
        }
    }

    async fn project_is_live(&self, project_id: i32) -> Result<bool, LogAggregatorError> {
        use temps_entities::projects;
        Ok(projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .count(self.db.as_ref())
            .await?
            > 0)
    }

    async fn environment_is_live(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<bool, LogAggregatorError> {
        use temps_entities::environments;
        Ok(environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .count(self.db.as_ref())
            .await?
            > 0)
    }
}

/// `tlk_` and 64 lowercase hex characters from 32 CSPRNG bytes.
fn generate_key() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..KEY_BYTES).map(|_| rng.random()).collect();
    format!("{LOG_INGEST_KEY_PREFIX}{}", hex::encode(bytes))
}

/// Exactly what [`generate_key`] produces. Uppercase hex is refused so one
/// key has one spelling.
pub fn is_well_formed(presented: &str) -> bool {
    presented
        .strip_prefix(LOG_INGEST_KEY_PREFIX)
        .is_some_and(|hex| {
            hex.len() == KEY_BYTES * 2
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn hash_key(plaintext: &str) -> String {
    hex::encode(Sha256::digest(plaintext.as_bytes()))
}

/// A service label safe to use as a label and inside a stream id: lowercase
/// letters, digits, `.`, `_` and `-`, at most [`MAX_SERVICE_LEN`] characters.
/// Anything else is folded to `-`; an empty result is `None`.
pub fn sanitize_service(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .take(MAX_SERVICE_LEN)
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[test]
    fn a_generated_key_is_well_formed_and_hashes_stably() {
        let key = generate_key();
        assert!(key.starts_with(LOG_INGEST_KEY_PREFIX));
        assert!(is_well_formed(&key), "{key}");
        assert_eq!(hash_key(&key), hash_key(&key));
        assert_ne!(generate_key(), key, "keys are random");
    }

    #[test]
    fn malformed_keys_are_refused_before_any_lookup() {
        let valid = generate_key();
        for bad in [
            String::new(),
            "tk_abc".to_string(),
            valid.to_uppercase(),
            format!("{valid}0"),
            valid[..valid.len() - 1].to_string(),
            valid.replace(LOG_INGEST_KEY_PREFIX, "pa_"),
        ] {
            assert!(!is_well_formed(&bad), "accepted {bad:?}");
        }
    }

    fn resolved(key_id: i32) -> AuthenticatedKey {
        AuthenticatedKey {
            key_id,
            project_id: 1,
            environment_id: None,
            default_service: DEFAULT_INGEST_SERVICE.to_string(),
        }
    }

    #[test]
    fn a_lookup_that_overlapped_a_revoke_is_not_cached() {
        let mut cache = KeyCache::default();
        // The lookup notes the count, then awaits its queries ...
        let seen = cache.revocations;
        // ... while the key is revoked and evicted.
        cache.revoke("h");
        assert!(!cache.publish("h".into(), resolved(1), seen));
        assert!(cache.entries.is_empty(), "the revoked key came back");
    }

    #[test]
    fn a_lookup_with_no_revoke_in_between_is_cached() {
        let mut cache = KeyCache::default();
        cache.revoke("other");
        let seen = cache.revocations;
        assert!(cache.publish("h".into(), resolved(1), seen));
        assert!(cache.entries.contains_key("h"));
    }

    #[tokio::test]
    async fn a_malformed_key_never_reaches_the_database() {
        // No query results queued: any query would fail the test.
        let service = LogIngestKeyService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));
        assert!(matches!(
            service.authenticate("not-a-key").await,
            Err(LogAggregatorError::IngestKeyRejected)
        ));
    }

    #[tokio::test]
    async fn an_unknown_key_is_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<Model>::new()])
            .into_connection();
        let service = LogIngestKeyService::new(Arc::new(db));
        assert!(matches!(
            service.authenticate(&generate_key()).await,
            Err(LogAggregatorError::IngestKeyRejected)
        ));
    }

    #[test]
    fn service_labels_are_folded_to_a_safe_alphabet() {
        assert_eq!(sanitize_service("Web API").as_deref(), Some("web-api"));
        assert_eq!(
            sanitize_service("nginx.access").as_deref(),
            Some("nginx.access")
        );
        assert_eq!(sanitize_service("../../etc").as_deref(), Some("..-..-etc"));
        assert_eq!(sanitize_service("  "), None);
        assert_eq!(sanitize_service("///"), None);
        assert_eq!(
            sanitize_service(&"a".repeat(200)).map(|s| s.len()),
            Some(MAX_SERVICE_LEN)
        );
    }

    #[tokio::test]
    async fn create_validates_its_input_before_touching_the_database() {
        let service = LogIngestKeyService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));
        for (name, service_label) in [("", None), ("ok", Some("///"))] {
            let error = service
                .create(
                    7,
                    NewLogIngestKey {
                        name: name.to_string(),
                        environment_id: None,
                        default_service: service_label.map(str::to_string),
                        created_by: None,
                    },
                )
                .await
                .expect_err("invalid input is refused");
            assert!(
                matches!(error, LogAggregatorError::Validation { .. }),
                "unexpected: {error}"
            );
        }
    }

    #[tokio::test]
    async fn usage_is_summed_in_memory_until_flushed() {
        let service = LogIngestKeyService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));
        let now = Utc::now();
        service.record_usage(3, 10, now);
        service.record_usage(3, 5, now);
        service.record_usage(4, 1, now);
        let usage = service.usage.lock().expect("usage lock");
        assert_eq!(usage.get(&3).map(|u| u.lines), Some(15));
        assert_eq!(usage.get(&4).map(|u| u.lines), Some(1));
    }
}
