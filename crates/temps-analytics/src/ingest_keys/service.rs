// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `AnalyticsIngestKeyService` — mint, list, update, rotate, revoke and
//! resolve analytics ingest keys (ADR-040 §2).
//!
//! The service lives in the umbrella `temps-analytics` crate so all three
//! ingest crates (`-events`, `-performance`, `-session-replay`) can reach it
//! with a downward-only dependency.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use moka::future::Cache;
use rand::RngExt;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use temps_entities::{analytics_ingest_keys, environments, projects};
use tracing::{debug, warn};

use super::rate_limiter::DEFAULT_RATE_LIMIT_PER_MINUTE;
use super::types::{
    AnalyticsIngestKey, AnalyticsIngestKeyError, ResolvedIngestScope, ANALYTICS_INGEST_KEY_BYTES,
    ANALYTICS_INGEST_KEY_PREFIX, DEFAULT_INGEST_KEY_NAME, MAX_ALLOWED_ORIGINS,
    MAX_ALLOWED_ORIGIN_LEN, MAX_INGEST_KEY_NAME_LEN, MAX_RATE_LIMIT_PER_MINUTE,
};

/// Resolution cache lifetime. Mirrors `temps-otel`'s `AUTH_CACHE_TTL`.
///
/// A revoked or rotated key keeps working for at most this long *only* if the
/// invalidation below is somehow missed — `rotate`/`revoke` evict the entry
/// synchronously, so in practice revocation is immediate rather than
/// eventually-consistent.
const RESOLVE_CACHE_TTL: Duration = Duration::from_secs(5);
const RESOLVE_CACHE_CAPACITY: u64 = 10_000;

/// Exact rendered length of a key: `pa_` + 64 hex characters = 67. Derived
/// from the mint-side constants rather than hard-coded, so the gate below and
/// [`generate_public_key`] can never drift apart.
const PUBLIC_KEY_LEN: usize = ANALYTICS_INGEST_KEY_PREFIX.len() + ANALYTICS_INGEST_KEY_BYTES * 2;

/// At most one `event_count`/`last_used_at` write per key per interval.
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(60);
/// Bound on tracked keys. Cardinality is the number of *active* keys, which is
/// operator-created and therefore small; this is a backstop, not a budget.
const USAGE_TRACKER_CAPACITY: usize = 10_000;
/// Buckets with no pending events and no activity for this long are pruned
/// when the tracker is at capacity.
const USAGE_IDLE_EVICTION: Duration = Duration::from_secs(600);

#[derive(Debug)]
struct PendingUsage {
    /// `None` until the first flush, so a key's very first event is written
    /// through immediately — an operator verifying a fresh key sees
    /// `last_used_at` populate instead of waiting a minute.
    last_flush: Option<Instant>,
    count: i64,
}

/// Throttles `event_count` / `last_used_at` writes so ingest never pays a
/// database write per request. Events are counted in memory and flushed at
/// most once per [`USAGE_FLUSH_INTERVAL`] per key.
#[derive(Default)]
struct UsageTracker {
    pending: Mutex<HashMap<i32, PendingUsage>>,
}

impl UsageTracker {
    /// Record one ingested event for `key_id`.
    ///
    /// Returns `Some(count)` when a flush is due, where `count` is the number
    /// of events accumulated since the previous flush (including this one).
    /// Returns `None` when the batch is still accumulating.
    fn record(&self, key_id: i32, now: Instant) -> Option<i64> {
        let mut pending = match self.pending.lock() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        };

        if !pending.contains_key(&key_id) && pending.len() >= USAGE_TRACKER_CAPACITY {
            pending.retain(|_, usage| {
                usage.count > 0
                    || usage
                        .last_flush
                        .is_none_or(|at| now.duration_since(at) < USAGE_IDLE_EVICTION)
            });
            if pending.len() >= USAGE_TRACKER_CAPACITY {
                // Best-effort counter: drop rather than grow without bound.
                return None;
            }
        }

        let usage = pending.entry(key_id).or_insert(PendingUsage {
            last_flush: None,
            count: 0,
        });
        usage.count += 1;

        if usage
            .last_flush
            .is_some_and(|at| now.duration_since(at) < USAGE_FLUSH_INTERVAL)
        {
            return None;
        }

        usage.last_flush = Some(now);
        Some(std::mem::replace(&mut usage.count, 0))
    }

    fn forget(&self, key_id: i32) {
        let mut pending = match self.pending.lock() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        };
        pending.remove(&key_id);
    }
}

/// Manages `analytics_ingest_keys` rows and resolves them on the ingest path.
pub struct AnalyticsIngestKeyService {
    db: Arc<DatabaseConnection>,
    /// Keyed by the raw public key string. Negative results are cached as
    /// `None` too.
    ///
    /// Scope of that mitigation, stated precisely because it is easy to
    /// overclaim: it only helps against a *repeated* invalid value — one typo'd
    /// key in a deployed bundle costs one query, not one per pageview. It does
    /// nothing against *distinct* forged values, each of which misses the cache
    /// and, at `RESOLVE_CACHE_CAPACITY` distinct misses, also evicts every
    /// legitimately cached positive. [`is_well_formed_public_key`] raises the
    /// cost of producing a plausible-looking candidate; rate-limiting
    /// unresolved-key attempts per client IP *before* the lookup is the real
    /// fix and is not implemented yet.
    resolve_cache: Cache<String, Option<ResolvedIngestScope>>,
    usage_tracker: UsageTracker,
}

impl AnalyticsIngestKeyService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            resolve_cache: Cache::builder()
                .max_capacity(RESOLVE_CACHE_CAPACITY)
                .time_to_live(RESOLVE_CACHE_TTL)
                .build(),
            usage_tracker: UsageTracker::default(),
        }
    }

    // ── Ingest path ──────────────────────────────────────────────────────

    /// Resolve a presented key to the scope its events belong to.
    ///
    /// `Ok(None)` means "no such active key" — the caller must answer 401 and
    /// must **not** fall back to `Host`-based resolution. `Err` is reserved for
    /// genuine storage failures so the caller can distinguish an invalid
    /// credential (401) from a broken database (500).
    pub async fn resolve(
        &self,
        public_key: &str,
    ) -> Result<Option<ResolvedIngestScope>, AnalyticsIngestKeyError> {
        // Reject anything that cannot be one of our keys before the cache and
        // before the database. This structurally guarantees the ADR's negative
        // — a `tk_`/`dt_`/`si_` secret pasted here never even reaches a lookup
        // — and it is the cheap half of the anti-amplification story: the gate
        // is the *exact* minted shape, not a loose length bound, so junk has to
        // be 64 hex characters before it can cost a query or a cache entry.
        //
        // It does not close the hole. A bot can still generate valid-shaped
        // garbage; see `resolve_cache`'s doc comment.
        if !is_well_formed_public_key(public_key) {
            return Ok(None);
        }

        if let Some(cached) = self.resolve_cache.get(public_key).await {
            return Ok(cached);
        }

        let row = resolve_query(public_key).one(self.db.as_ref()).await?;

        let scope = match row {
            Some((key, environment)) => Some(ResolvedIngestScope {
                project_id: key.project_id,
                environment_id: key.environment_id,
                // Derived, never stored: a key is not re-minted per deploy.
                deployment_id: environment.and_then(|env| env.current_deployment_id),
                key_id: key.id,
                allowed_origins: parse_allowed_origins(key.id, key.allowed_origins.as_ref())?,
                rate_limit_per_minute: key.rate_limit_per_minute,
            }),
            None => None,
        };

        self.resolve_cache
            .insert(public_key.to_string(), scope.clone())
            .await;

        Ok(scope)
    }

    /// Account for one ingested event against `key_id`.
    ///
    /// Off the synchronous request path by design: counts accumulate in memory
    /// and are flushed at most once per 60 s per key. Returns `true` when this
    /// call performed a database write.
    ///
    /// Deliberately does **not** bump `updated_at` — usage is not an
    /// operator-visible modification of the key, and churning `updated_at` on
    /// every flush would make the column useless for spotting real changes.
    pub async fn record_usage(&self, key_id: i32) -> Result<bool, AnalyticsIngestKeyError> {
        let Some(count) = self.usage_tracker.record(key_id, Instant::now()) else {
            return Ok(false);
        };

        analytics_ingest_keys::Entity::update_many()
            .col_expr(
                analytics_ingest_keys::Column::EventCount,
                Expr::col(analytics_ingest_keys::Column::EventCount).add(count),
            )
            .col_expr(
                analytics_ingest_keys::Column::LastUsedAt,
                Expr::value(Utc::now()),
            )
            .filter(analytics_ingest_keys::Column::Id.eq(key_id))
            .exec(self.db.as_ref())
            .await?;

        Ok(true)
    }

    // ── Admin path ───────────────────────────────────────────────────────

    /// Mint a new ingest key for `project_id`.
    ///
    /// When `environment_id` is supplied it is verified to belong to
    /// `project_id`. Skipping that check would let anyone with
    /// `AnalyticsWrite` on project A mint a key whose events are attributed to
    /// project B's environment.
    pub async fn create(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        name: Option<String>,
        allowed_origins: Option<Vec<String>>,
        rate_limit_per_minute: Option<i32>,
        created_by_user_id: Option<i32>,
    ) -> Result<AnalyticsIngestKey, AnalyticsIngestKeyError> {
        let name = normalize_name(name)?;
        let allowed_origins = validate_allowed_origins(allowed_origins)?;
        let rate_limit_per_minute = validate_rate_limit(rate_limit_per_minute)?;

        if projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .is_none()
        {
            return Err(AnalyticsIngestKeyError::ProjectNotFound { project_id });
        }

        if let Some(environment_id) = environment_id {
            self.verify_environment_belongs_to_project(project_id, environment_id)
                .await?;
        }

        let public_key = generate_public_key();

        let model = analytics_ingest_keys::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            name: Set(name),
            public_key: Set(public_key),
            is_active: Set(true),
            revoked_at: Set(None),
            rate_limit_per_minute: Set(Some(
                rate_limit_per_minute.unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE),
            )),
            allowed_origins: Set(encode_allowed_origins(allowed_origins.as_ref())),
            event_count: Set(0),
            last_used_at: Set(None),
            created_by_user_id: Set(created_by_user_id),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

        debug!(
            project_id,
            environment_id = ?environment_id,
            key_id = model.id,
            "minted analytics ingest key"
        );

        AnalyticsIngestKey::try_from_model(model)
    }

    /// All keys for a project, active and revoked, newest first.
    ///
    /// Revoked keys are included on purpose: revocation is soft, and an
    /// operator investigating "which key sent this?" needs to see the rows
    /// that are no longer usable.
    /// Fetch a single key, refusing to cross project boundaries.
    pub async fn get(
        &self,
        project_id: i32,
        key_id: i32,
    ) -> Result<AnalyticsIngestKey, AnalyticsIngestKeyError> {
        let model = self.find_scoped(project_id, key_id).await?;
        AnalyticsIngestKey::try_from_model(model)
    }

    pub async fn list(
        &self,
        project_id: i32,
    ) -> Result<Vec<AnalyticsIngestKey>, AnalyticsIngestKeyError> {
        // Unlike events/visitors, these rows are only ever created by an
        // authenticated operator through the admin CRUD above — there is no
        // ingest-triggered growth path — so real per-project cardinality is a
        // handful of rows and true offset/limit pagination would add API
        // surface (and a client-side page-through UI) nobody needs yet. This
        // cap exists purely as a hot-path/memory backstop, not as
        // user-facing pagination: if a project is ever near it, that is a
        // signal to revisit this, not a limit an operator is expected to
        // page through.
        const MAX_KEYS_RETURNED: u64 = 500;

        let models = analytics_ingest_keys::Entity::find()
            .filter(analytics_ingest_keys::Column::ProjectId.eq(project_id))
            .order_by_desc(analytics_ingest_keys::Column::CreatedAt)
            .order_by_desc(analytics_ingest_keys::Column::Id)
            .limit(MAX_KEYS_RETURNED)
            .all(self.db.as_ref())
            .await?;

        models
            .into_iter()
            .map(AnalyticsIngestKey::try_from_model)
            .collect()
    }

    /// Partially update a key's label, origin allowlist, or rate limit.
    ///
    /// Each patch field is three-state: `None` leaves it unchanged,
    /// `Some(None)` clears it, `Some(Some(v))` sets it. `name` cannot be
    /// cleared to NULL (the column is `NOT NULL`); `Some(None)` resets it to
    /// the default label.
    pub async fn update(
        &self,
        project_id: i32,
        key_id: i32,
        name: Option<Option<String>>,
        allowed_origins: Option<Option<Vec<String>>>,
        rate_limit_per_minute: Option<Option<i32>>,
    ) -> Result<AnalyticsIngestKey, AnalyticsIngestKeyError> {
        let existing = self.find_scoped(project_id, key_id).await?;
        let previous_public_key = existing.public_key.clone();

        let mut active: analytics_ingest_keys::ActiveModel = existing.into();

        if let Some(name) = name {
            active.name = Set(normalize_name(name)?);
        }
        if let Some(origins) = allowed_origins {
            let validated = validate_allowed_origins(origins)?;
            active.allowed_origins = Set(encode_allowed_origins(validated.as_ref()));
        }
        if let Some(limit) = rate_limit_per_minute {
            active.rate_limit_per_minute = Set(validate_rate_limit(limit)?);
        }

        let updated = active.update(self.db.as_ref()).await?;

        // `allowed_origins` and `rate_limit_per_minute` are part of the
        // resolved scope, so a cached entry is now stale.
        self.invalidate(&previous_public_key).await;

        AnalyticsIngestKey::try_from_model(updated)
    }

    /// Replace a key's `public_key` in place, keeping the same row, scope,
    /// origin allowlist and rate limit.
    ///
    /// The previous value stops authenticating the moment this returns: the
    /// resolution cache entry for it is evicted synchronously rather than left
    /// to expire, so there is no window in which a rotated-out key still works.
    pub async fn rotate(
        &self,
        project_id: i32,
        key_id: i32,
        created_by_user_id: Option<i32>,
    ) -> Result<AnalyticsIngestKey, AnalyticsIngestKeyError> {
        let existing = self.find_scoped(project_id, key_id).await?;

        if !existing.is_active {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "is_active".to_string(),
                message: format!(
                    "Analytics ingest key {key_id} in project {project_id} is revoked; \
                     rotating it would mint a value that cannot authenticate. \
                     Create a new key instead."
                ),
            });
        }

        let previous_public_key = existing.public_key.clone();
        let public_key = generate_public_key();

        let mut active: analytics_ingest_keys::ActiveModel = existing.into();
        active.public_key = Set(public_key.clone());
        // Record who last handled the credential, so an audit trail exists on
        // the row itself and not only in the audit log.
        if let Some(user_id) = created_by_user_id {
            active.created_by_user_id = Set(Some(user_id));
        }
        let updated = active.update(self.db.as_ref()).await?;

        self.invalidate(&previous_public_key).await;
        // Also drop any cached negative for the freshly minted value.
        self.invalidate(&public_key).await;

        debug!(project_id, key_id, "rotated analytics ingest key");

        AnalyticsIngestKey::try_from_model(updated)
    }

    /// Soft-revoke a key: `is_active = false`, `revoked_at = now()`.
    ///
    /// Never a hard `DELETE` — destroying the row would destroy the record of
    /// which key ingested what.
    ///
    /// This drops the two pieces of in-memory state the service owns: the
    /// resolution cache entry and the usage-counter bucket. It cannot drop the
    /// third — `AnalyticsIngestRateLimiter`'s per-key window lives in a
    /// separate service, so `revoke_analytics_ingest_key` calls
    /// `AnalyticsIngestRateLimiter::forget` after this returns.
    pub async fn revoke(
        &self,
        project_id: i32,
        key_id: i32,
    ) -> Result<AnalyticsIngestKey, AnalyticsIngestKeyError> {
        let existing = self.find_scoped(project_id, key_id).await?;
        let public_key = existing.public_key.clone();

        let mut active: analytics_ingest_keys::ActiveModel = existing.into();
        active.is_active = Set(false);
        active.revoked_at = Set(Some(Utc::now()));
        let updated = active.update(self.db.as_ref()).await?;

        self.invalidate(&public_key).await;
        self.usage_tracker.forget(key_id);

        debug!(project_id, key_id, "revoked analytics ingest key");

        AnalyticsIngestKey::try_from_model(updated)
    }

    // ── Internals ────────────────────────────────────────────────────────

    /// Load a key, refusing to return one that belongs to another project.
    /// This is what stops project A from mutating project B's key by guessing
    /// an integer id.
    async fn find_scoped(
        &self,
        project_id: i32,
        key_id: i32,
    ) -> Result<analytics_ingest_keys::Model, AnalyticsIngestKeyError> {
        analytics_ingest_keys::Entity::find_by_id(key_id)
            .filter(analytics_ingest_keys::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(AnalyticsIngestKeyError::KeyNotFound { key_id, project_id })
    }

    async fn verify_environment_belongs_to_project(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), AnalyticsIngestKeyError> {
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or(AnalyticsIngestKeyError::EnvironmentNotFound {
                environment_id,
                project_id,
            })?;

        if environment.project_id != project_id {
            return Err(AnalyticsIngestKeyError::EnvironmentProjectMismatch {
                environment_id,
                environment_project_id: environment.project_id,
                project_id,
            });
        }

        Ok(())
    }

    async fn invalidate(&self, public_key: &str) {
        self.resolve_cache.invalidate(public_key).await;
    }

    /// Drop every cached resolution.
    ///
    /// `resolve`'s cached [`ResolvedIngestScope`] embeds `deployment_id`,
    /// derived from `environments.current_deployment_id` at cache-fill time.
    /// A deployment transition updates that column without touching
    /// `analytics_ingest_keys` at all, so `rotate`/`revoke`/`update`'s
    /// targeted [`invalidate`](Self::invalidate) never fires for it — an
    /// environment-scoped key would otherwise keep attributing events to the
    /// previous deployment for up to [`RESOLVE_CACHE_TTL`] after a deploy.
    /// The caller (the analytics plugin's `Job::RouteTableUpdated` subscriber)
    /// has no per-key index to target, and deployments are infrequent enough
    /// that clearing the whole cache is cheap — the next resolution per key
    /// just re-reads the current value.
    pub fn invalidate_all_cached_scopes(&self) {
        self.resolve_cache.invalidate_all();
    }
}

/// The single hot-path lookup: one `LEFT JOIN` from the key to its
/// environment, so `deployment_id` can be derived from
/// `environments.current_deployment_id` without a second round trip.
///
/// Extracted so the generated SQL is directly assertable in a test — the
/// `deleted_at IS NULL` predicate is load-bearing and easy to lose in a
/// refactor.
fn resolve_query(
    public_key: &str,
) -> sea_orm::SelectTwo<analytics_ingest_keys::Entity, environments::Entity> {
    analytics_ingest_keys::Entity::find()
        .find_also_related(environments::Entity)
        .filter(analytics_ingest_keys::Column::PublicKey.eq(public_key))
        .filter(analytics_ingest_keys::Column::IsActive.eq(true))
        // A soft-deleted environment must not resolve. For a project-scoped
        // key the LEFT JOIN yields NULL, which satisfies `IS NULL`, so
        // project-scoped keys are unaffected.
        .filter(environments::Column::DeletedAt.is_null())
}

/// `pa_` + 64 lowercase hex characters from 32 CSPRNG bytes — the same entropy
/// and encoding as `DSNService::generate_key(32)`.
fn generate_public_key() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..ANALYTICS_INGEST_KEY_BYTES)
        .map(|_| rng.random())
        .collect();
    format!("{}{}", ANALYTICS_INGEST_KEY_PREFIX, hex::encode(bytes))
}

/// Is `candidate` shaped exactly like something [`generate_public_key`] could
/// have produced — `pa_` followed by exactly 64 lowercase hex characters?
///
/// Deliberately exact rather than "starts with `pa_` and isn't absurdly long".
/// This runs before the resolution cache and before the database on a public,
/// unauthenticated endpoint, so every character it rejects is a query and a
/// cache slot an attacker cannot spend. Uppercase hex is rejected on purpose:
/// keys are minted lowercase, so accepting both cases would double the number
/// of distinct strings that map to one real key and make the cache trivially
/// dilutable with case permutations of a key the attacker already has.
fn is_well_formed_public_key(candidate: &str) -> bool {
    if candidate.len() != PUBLIC_KEY_LEN {
        return false;
    }
    let Some(hex) = candidate.strip_prefix(ANALYTICS_INGEST_KEY_PREFIX) else {
        return false;
    };
    hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn normalize_name(name: Option<String>) -> Result<String, AnalyticsIngestKeyError> {
    let Some(name) = name else {
        return Ok(DEFAULT_INGEST_KEY_NAME.to_string());
    };

    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AnalyticsIngestKeyError::Validation {
            field: "name".to_string(),
            message: "Ingest key name cannot be empty or whitespace-only".to_string(),
        });
    }
    if trimmed.chars().count() > MAX_INGEST_KEY_NAME_LEN {
        return Err(AnalyticsIngestKeyError::Validation {
            field: "name".to_string(),
            message: format!(
                "Ingest key name is {} characters; the maximum is {MAX_INGEST_KEY_NAME_LEN}",
                trimmed.chars().count()
            ),
        });
    }

    Ok(trimmed.to_string())
}

/// Bound a key's requests-per-minute.
///
/// `None` and non-positive values are the documented "unlimited" encoding and
/// pass through untouched — an operator asking for no limit gets no limit. The
/// ceiling only applies to positive values, where an absurd number would
/// silently mean "unlimited" while looking like a limit on the row and in the
/// Console.
fn validate_rate_limit(
    rate_limit_per_minute: Option<i32>,
) -> Result<Option<i32>, AnalyticsIngestKeyError> {
    if let Some(limit) = rate_limit_per_minute {
        if limit > MAX_RATE_LIMIT_PER_MINUTE {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "rate_limit_per_minute".to_string(),
                message: format!(
                    "{limit} requests per minute exceeds the maximum of \
                     {MAX_RATE_LIMIT_PER_MINUTE}; send null or a non-positive \
                     value if the key should be unlimited"
                ),
            });
        }
    }

    Ok(rate_limit_per_minute)
}

/// Validate an origin allowlist. Entries must be exact origins
/// (`scheme://host[:port]`) because that is what a browser sends in `Origin`
/// and what the ingest handlers compare against; a path or query in an entry
/// can never match and would silently block every request.
fn validate_allowed_origins(
    origins: Option<Vec<String>>,
) -> Result<Option<Vec<String>>, AnalyticsIngestKeyError> {
    let Some(origins) = origins else {
        return Ok(None);
    };

    if origins.is_empty() {
        // `[]` and NULL both mean "any origin"; normalize to NULL so the two
        // encodings cannot drift apart in the resolver.
        return Ok(None);
    }

    if origins.len() > MAX_ALLOWED_ORIGINS {
        return Err(AnalyticsIngestKeyError::Validation {
            field: "allowed_origins".to_string(),
            message: format!(
                "{} origins supplied; the maximum is {MAX_ALLOWED_ORIGINS}",
                origins.len()
            ),
        });
    }

    let mut normalized = Vec::with_capacity(origins.len());
    for origin in origins {
        let trimmed = origin.trim();
        if trimmed.is_empty() {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "allowed_origins".to_string(),
                message: "Origin entries cannot be empty".to_string(),
            });
        }
        if trimmed.len() > MAX_ALLOWED_ORIGIN_LEN {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "allowed_origins".to_string(),
                message: format!(
                    "Origin '{trimmed}' is {} characters; the maximum is {MAX_ALLOWED_ORIGIN_LEN}",
                    trimmed.len()
                ),
            });
        }

        let parsed = url::Url::parse(trimmed).map_err(|e| AnalyticsIngestKeyError::Validation {
            field: "allowed_origins".to_string(),
            message: format!(
                "Origin '{trimmed}' is not a valid absolute URL ({e}); \
                 expected a bare origin such as https://app.example.com"
            ),
        })?;

        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "allowed_origins".to_string(),
                message: format!(
                    "Origin '{trimmed}' uses scheme '{}'; only http and https are valid browser origins",
                    parsed.scheme()
                ),
            });
        }
        if parsed.host_str().is_none() {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "allowed_origins".to_string(),
                message: format!("Origin '{trimmed}' has no host"),
            });
        }
        if parsed.path() != "/" && !parsed.path().is_empty() {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "allowed_origins".to_string(),
                message: format!(
                    "Origin '{trimmed}' contains a path; a browser Origin header is \
                     scheme://host[:port] only and this entry could never match"
                ),
            });
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(AnalyticsIngestKeyError::Validation {
                field: "allowed_origins".to_string(),
                message: format!(
                    "Origin '{trimmed}' contains a query or fragment; a browser Origin \
                     header is scheme://host[:port] only and this entry could never match"
                ),
            });
        }

        // `Url::origin().ascii_serialization()` gives exactly the string a
        // browser puts in the `Origin` header (lowercased host, default port
        // elided), so the stored value is directly comparable at ingest time.
        let canonical = parsed.origin().ascii_serialization();
        if !normalized.contains(&canonical) {
            normalized.push(canonical);
        }
    }

    Ok(Some(normalized))
}

fn encode_allowed_origins(origins: Option<&Vec<String>>) -> Option<serde_json::Value> {
    origins.map(|origins| {
        serde_json::Value::Array(
            origins
                .iter()
                .map(|origin| serde_json::Value::String(origin.clone()))
                .collect(),
        )
    })
}

fn parse_allowed_origins(
    key_id: i32,
    raw: Option<&serde_json::Value>,
) -> Result<Option<Vec<String>>, AnalyticsIngestKeyError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }

    let origins: Vec<String> = serde_json::from_value(raw.clone()).map_err(|e| {
        // Fail closed and loudly: silently treating a corrupt allowlist as
        // "any origin" would relax a restriction the operator set.
        warn!(key_id, error = %e, "analytics ingest key has a malformed allowed_origins column");
        AnalyticsIngestKeyError::MalformedAllowedOrigins {
            key_id,
            reason: e.to_string(),
        }
    })?;

    if origins.is_empty() {
        Ok(None)
    } else {
        Ok(Some(origins))
    }
}

impl AnalyticsIngestKey {
    fn try_from_model(
        model: analytics_ingest_keys::Model,
    ) -> Result<Self, AnalyticsIngestKeyError> {
        let allowed_origins = parse_allowed_origins(model.id, model.allowed_origins.as_ref())?;
        Ok(Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            name: model.name,
            public_key: model.public_key,
            is_active: model.is_active,
            revoked_at: model.revoked_at,
            rate_limit_per_minute: model.rate_limit_per_minute,
            allowed_origins,
            event_count: model.event_count,
            last_used_at: model.last_used_at,
            created_by_user_id: model.created_by_user_id,
            created_at: model.created_at,
            updated_at: model.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_keys::test_fixtures::{environment_model, key_model, project_model};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn service_with(db: MockDatabase) -> AnalyticsIngestKeyService {
        AnalyticsIngestKeyService::new(Arc::new(db.into_connection()))
    }

    // ── generate_public_key ──────────────────────────────────────────────

    #[test]
    fn generated_keys_are_prefixed_64_hex_and_unique() {
        let a = generate_public_key();
        let b = generate_public_key();
        assert_ne!(a, b, "two mints must not collide");
        for key in [&a, &b] {
            assert!(key.starts_with("pa_"), "{key}");
            assert_eq!(key.len(), 67, "{key}");
            let hex_part = &key[3..];
            assert!(
                hex_part
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{key}"
            );
        }
    }

    // ── validation helpers ───────────────────────────────────────────────

    #[test]
    fn name_defaults_trims_and_rejects_blank() {
        assert_eq!(
            normalize_name(None).expect("default"),
            DEFAULT_INGEST_KEY_NAME
        );
        assert_eq!(
            normalize_name(Some("  Marketing site  ".into())).expect("trimmed"),
            "Marketing site"
        );
        assert!(matches!(
            normalize_name(Some("   ".into())),
            Err(AnalyticsIngestKeyError::Validation { .. })
        ));
        assert!(matches!(
            normalize_name(Some("x".repeat(MAX_INGEST_KEY_NAME_LEN + 1))),
            Err(AnalyticsIngestKeyError::Validation { .. })
        ));
    }

    #[test]
    fn allowed_origins_are_canonicalized_and_validated() {
        assert_eq!(validate_allowed_origins(None).expect("none"), None);
        assert_eq!(validate_allowed_origins(Some(vec![])).expect("empty"), None);

        let ok = validate_allowed_origins(Some(vec![
            "https://App.Example.com".into(),
            "http://localhost:3000".into(),
            "https://app.example.com/".into(),
        ]))
        .expect("valid origins");
        assert_eq!(
            ok,
            Some(vec![
                "https://app.example.com".to_string(),
                "http://localhost:3000".to_string(),
            ]),
            "host case is normalized and duplicates collapse"
        );

        for bad in [
            "not-a-url",
            "ftp://example.com",
            "https://example.com/path",
            "https://example.com/?a=b",
            "",
        ] {
            assert!(
                matches!(
                    validate_allowed_origins(Some(vec![bad.to_string()])),
                    Err(AnalyticsIngestKeyError::Validation { .. })
                ),
                "expected {bad:?} to be rejected"
            );
        }

        let too_many: Vec<String> = (0..(MAX_ALLOWED_ORIGINS + 1))
            .map(|i| format!("https://h{i}.example.com"))
            .collect();
        assert!(matches!(
            validate_allowed_origins(Some(too_many)),
            Err(AnalyticsIngestKeyError::Validation { .. })
        ));
    }

    #[test]
    fn well_formed_gate_accepts_only_the_exact_minted_shape() {
        assert!(is_well_formed_public_key(&generate_public_key()));
        assert!(is_well_formed_public_key(&format!("pa_{}", "0".repeat(64))));
        assert!(is_well_formed_public_key(&format!(
            "pa_{}",
            "abcdef0123456789".repeat(4)
        )));

        for rejected in [
            "",
            "pa_",
            "tk_deadbeef",
            // Right prefix, wrong length — one short, one long.
            &format!("pa_{}", "a".repeat(63)),
            &format!("pa_{}", "a".repeat(65)),
            &format!("pa_{}", "a".repeat(200)),
            // Right length, wrong alphabet. `g` and `-` are not hex; uppercase
            // is not what we mint, and accepting it would let one real key be
            // spelled 2^64 ways in the cache.
            &format!("pa_{}g", "a".repeat(63)),
            &format!("pa_{}-", "a".repeat(63)),
            &format!("pa_{}", "A".repeat(64)),
            // Padded to the right length but not our prefix.
            &format!("tk_{}", "a".repeat(64)),
        ] {
            assert!(
                !is_well_formed_public_key(rejected),
                "expected {rejected:?} to be rejected"
            );
        }
    }

    #[test]
    fn rate_limit_ceiling_bounds_positive_values_only() {
        // "Unlimited" keeps its explicit encodings.
        assert_eq!(validate_rate_limit(None).expect("none"), None);
        assert_eq!(validate_rate_limit(Some(0)).expect("zero"), Some(0));
        assert_eq!(validate_rate_limit(Some(-1)).expect("negative"), Some(-1));

        assert_eq!(
            validate_rate_limit(Some(MAX_RATE_LIMIT_PER_MINUTE)).expect("at the ceiling"),
            Some(MAX_RATE_LIMIT_PER_MINUTE)
        );
        for over in [MAX_RATE_LIMIT_PER_MINUTE + 1, i32::MAX] {
            assert!(
                matches!(
                    validate_rate_limit(Some(over)),
                    Err(AnalyticsIngestKeyError::Validation {
                        ref field,
                        ..
                    }) if field == "rate_limit_per_minute"
                ),
                "expected {over} to be rejected"
            );
        }
    }

    #[test]
    fn malformed_allowed_origins_column_fails_closed() {
        let raw = serde_json::json!({"not": "an array"});
        assert!(matches!(
            parse_allowed_origins(7, Some(&raw)),
            Err(AnalyticsIngestKeyError::MalformedAllowedOrigins { key_id: 7, .. })
        ));
    }

    // ── create ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_project_scoped_key_succeeds() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model(1)]])
            .append_query_results([vec![key_model(10, 1, None)]]);
        let service = service_with(db);

        let key = service
            .create(1, None, None, None, None, Some(9))
            .await
            .expect("create should succeed");

        assert_eq!(key.id, 10);
        assert_eq!(key.project_id, 1);
        assert_eq!(key.environment_id, None);
        assert!(key.public_key.starts_with("pa_"));
        assert!(key.is_active);
    }

    #[tokio::test]
    async fn create_rejects_unknown_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()]);
        let service = service_with(db);

        let err = service
            .create(404, None, None, None, None, None)
            .await
            .expect_err("unknown project must not mint a key");
        assert!(matches!(
            err,
            AnalyticsIngestKeyError::ProjectNotFound { project_id: 404 }
        ));
    }

    #[tokio::test]
    async fn create_rejects_environment_from_another_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model(1)]])
            // Environment 5 belongs to project 2, not project 1.
            .append_query_results([vec![environment_model(5, 2, None)]]);
        let service = service_with(db);

        let err = service
            .create(1, Some(5), None, None, None, None)
            .await
            .expect_err("cross-project environment scoping must be refused");
        assert!(
            matches!(
                err,
                AnalyticsIngestKeyError::EnvironmentProjectMismatch {
                    environment_id: 5,
                    environment_project_id: 2,
                    project_id: 1,
                }
            ),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn create_rejects_missing_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model(1)]])
            .append_query_results([Vec::<environments::Model>::new()]);
        let service = service_with(db);

        let err = service
            .create(1, Some(77), None, None, None, None)
            .await
            .expect_err("unknown environment must be refused");
        assert!(matches!(
            err,
            AnalyticsIngestKeyError::EnvironmentNotFound {
                environment_id: 77,
                project_id: 1,
            }
        ));
    }

    #[tokio::test]
    async fn create_rejects_invalid_origin_before_touching_the_database() {
        // No query results appended: any DB access would surface as an error
        // other than Validation.
        let service = service_with(MockDatabase::new(DatabaseBackend::Postgres));

        let err = service
            .create(
                1,
                None,
                None,
                Some(vec!["javascript:alert(1)".into()]),
                None,
                None,
            )
            .await
            .expect_err("an invalid origin must be rejected");
        assert!(matches!(err, AnalyticsIngestKeyError::Validation { .. }));
    }

    #[tokio::test]
    async fn create_rejects_an_over_the_ceiling_rate_limit_before_touching_the_database() {
        // No query results appended: any DB access would surface as an error
        // other than Validation.
        let service = service_with(MockDatabase::new(DatabaseBackend::Postgres));

        let err = service
            .create(
                1,
                None,
                None,
                None,
                Some(MAX_RATE_LIMIT_PER_MINUTE + 1),
                None,
            )
            .await
            .expect_err("an absurd rate limit must be rejected");
        assert!(
            matches!(
                err,
                AnalyticsIngestKeyError::Validation { ref field, .. } if field == "rate_limit_per_minute"
            ),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn update_rejects_an_over_the_ceiling_rate_limit() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key_model(10, 1, None)]]);
        let service = service_with(db);

        let err = service
            .update(1, 10, None, None, Some(Some(i32::MAX)))
            .await
            .expect_err("an absurd rate limit must be rejected");
        assert!(
            matches!(
                err,
                AnalyticsIngestKeyError::Validation { ref field, .. } if field == "rate_limit_per_minute"
            ),
            "unexpected error: {err}"
        );
    }

    // ── list ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_returns_active_and_revoked_keys() {
        let mut revoked = key_model(2, 1, None);
        revoked.is_active = false;
        revoked.revoked_at = Some(Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key_model(3, 1, Some(4)), revoked]]);
        let service = service_with(db);

        let keys = service.list(1).await.expect("list should succeed");
        assert_eq!(keys.len(), 2);
        assert!(keys[0].is_active);
        assert!(!keys[1].is_active);
    }

    #[tokio::test]
    async fn list_of_a_project_without_keys_is_empty_not_an_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<analytics_ingest_keys::Model>::new()]);
        let service = service_with(db);

        assert!(service
            .list(1)
            .await
            .expect("list should succeed")
            .is_empty());
    }

    // ── update ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_applies_a_partial_patch() {
        let mut updated = key_model(10, 1, None);
        updated.name = "Marketing site".to_string();
        updated.rate_limit_per_minute = None;
        updated.allowed_origins = Some(serde_json::json!(["https://app.example.com"]));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key_model(10, 1, None)]])
            .append_query_results([vec![updated]]);
        let service = service_with(db);

        let key = service
            .update(
                1,
                10,
                Some(Some("Marketing site".into())),
                Some(Some(vec!["https://app.example.com".into()])),
                Some(None),
            )
            .await
            .expect("update should succeed");

        assert_eq!(key.name, "Marketing site");
        assert_eq!(
            key.allowed_origins,
            Some(vec!["https://app.example.com".to_string()])
        );
        assert_eq!(key.rate_limit_per_minute, None);
    }

    #[tokio::test]
    async fn update_refuses_a_key_owned_by_another_project() {
        // `find_scoped` filters on project_id, so the row simply is not found.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<analytics_ingest_keys::Model>::new()]);
        let service = service_with(db);

        let err = service
            .update(999, 10, Some(Some("hijacked".into())), None, None)
            .await
            .expect_err("cross-project update must be refused");
        assert!(matches!(
            err,
            AnalyticsIngestKeyError::KeyNotFound {
                key_id: 10,
                project_id: 999,
            }
        ));
    }

    #[tokio::test]
    async fn update_rejects_a_blank_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key_model(10, 1, None)]]);
        let service = service_with(db);

        let err = service
            .update(1, 10, Some(Some("  ".into())), None, None)
            .await
            .expect_err("a blank name must be refused");
        assert!(matches!(err, AnalyticsIngestKeyError::Validation { .. }));
    }

    // ── rotate ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn rotate_mints_a_new_value_on_the_same_row() {
        let original = key_model(10, 1, Some(4));
        let mut rotated = original.clone();
        rotated.public_key = format!("pa_{}", "a".repeat(64));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![original.clone()]])
            .append_query_results([vec![rotated]]);
        let service = service_with(db);

        let key = service
            .rotate(1, 10, Some(9))
            .await
            .expect("rotate should succeed");

        assert_eq!(key.id, 10, "same row");
        assert_eq!(key.environment_id, Some(4), "same scope");
        assert_ne!(key.public_key, original.public_key, "new value");
    }

    #[tokio::test]
    async fn rotate_refuses_a_revoked_key() {
        let mut revoked = key_model(10, 1, None);
        revoked.is_active = false;

        let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![revoked]]);
        let service = service_with(db);

        let err = service
            .rotate(1, 10, None)
            .await
            .expect_err("rotating a revoked key must be refused");
        assert!(matches!(err, AnalyticsIngestKeyError::Validation { .. }));
    }

    #[tokio::test]
    async fn rotate_refuses_a_key_owned_by_another_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<analytics_ingest_keys::Model>::new()]);
        let service = service_with(db);

        let err = service
            .rotate(999, 10, None)
            .await
            .expect_err("cross-project rotate must be refused");
        assert!(matches!(
            err,
            AnalyticsIngestKeyError::KeyNotFound {
                key_id: 10,
                project_id: 999,
            }
        ));
    }

    // ── revoke ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn revoke_is_soft() {
        let mut revoked = key_model(10, 1, None);
        revoked.is_active = false;
        revoked.revoked_at = Some(Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key_model(10, 1, None)]])
            .append_query_results([vec![revoked]]);
        let service = service_with(db);

        let key = service.revoke(1, 10).await.expect("revoke should succeed");
        assert!(!key.is_active);
        assert!(key.revoked_at.is_some());
    }

    #[tokio::test]
    async fn revoke_refuses_a_key_owned_by_another_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<analytics_ingest_keys::Model>::new()]);
        let service = service_with(db);

        let err = service
            .revoke(999, 10)
            .await
            .expect_err("cross-project revoke must be refused");
        assert!(matches!(
            err,
            AnalyticsIngestKeyError::KeyNotFound {
                key_id: 10,
                project_id: 999,
            }
        ));
    }

    // ── get ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_returns_a_key_and_refuses_another_projects() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key_model(10, 1, None)]])
            .append_query_results([Vec::<analytics_ingest_keys::Model>::new()]);
        let service = service_with(db);

        assert_eq!(service.get(1, 10).await.expect("get should succeed").id, 10);
        assert!(matches!(
            service
                .get(999, 10)
                .await
                .expect_err("cross-project get must be refused"),
            AnalyticsIngestKeyError::KeyNotFound {
                key_id: 10,
                project_id: 999,
            }
        ));
    }

    // ── resolve ──────────────────────────────────────────────────────────

    #[test]
    fn resolve_query_left_joins_environments_and_excludes_soft_deleted_ones() {
        use sea_orm::QueryTrait;

        let sql = resolve_query("pa_example")
            .build(DatabaseBackend::Postgres)
            .to_string();

        assert!(sql.contains("LEFT JOIN \"environments\""), "{sql}");
        assert!(
            sql.contains("\"environments\".\"deleted_at\" IS NULL"),
            "a soft-deleted environment must not resolve: {sql}"
        );
        assert!(
            sql.contains("\"analytics_ingest_keys\".\"is_active\" = TRUE"),
            "{sql}"
        );
        assert!(
            sql.contains("\"analytics_ingest_keys\".\"public_key\" = 'pa_example'"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn resolve_rejects_anything_but_the_exact_key_shape_without_a_query() {
        // No query results appended, so any DB access would error out — which
        // is the assertion: none of these may reach Postgres, and none may
        // occupy a cache slot. This is what stops an attacker turning a public,
        // unauthenticated endpoint into an index-lookup amplifier with a stream
        // of random junk.
        let service = service_with(MockDatabase::new(DatabaseBackend::Postgres));

        for rejected in [
            "".to_string(),
            "tk_deadbeef".to_string(),
            "pa_".to_string(),
            format!("pa_{}", "a".repeat(63)),
            format!("pa_{}", "a".repeat(65)),
            format!("pa_{}", "a".repeat(200)),
            format!("pa_{}", "A".repeat(64)),
            format!("pa_{}z", "a".repeat(63)),
            format!("pa_{}'", "a".repeat(63)),
        ] {
            assert_eq!(
                service
                    .resolve(&rejected)
                    .await
                    .expect("a malformed value must not error"),
                None,
                "expected {rejected:?} to be rejected without a query"
            );
        }
    }

    #[tokio::test]
    async fn resolve_derives_deployment_id_from_the_environment() {
        let key = key_model(10, 1, Some(4));
        let public_key = key.public_key.clone();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![(key, Some(environment_model(4, 1, Some(77))))]]);
        let service = service_with(db);

        let scope = service
            .resolve(&public_key)
            .await
            .expect("resolve should succeed")
            .expect("key should resolve");

        assert_eq!(
            scope,
            ResolvedIngestScope {
                project_id: 1,
                environment_id: Some(4),
                deployment_id: Some(77),
                key_id: 10,
                allowed_origins: None,
                rate_limit_per_minute: Some(600),
            }
        );
    }

    #[tokio::test]
    async fn resolve_of_a_project_scoped_key_has_no_deployment() {
        let key = key_model(11, 1, None);
        let public_key = key.public_key.clone();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![(key, None::<environments::Model>)]]);
        let service = service_with(db);

        let scope = service
            .resolve(&public_key)
            .await
            .expect("resolve should succeed")
            .expect("key should resolve");

        assert_eq!(scope.environment_id, None);
        assert_eq!(scope.deployment_id, None);
    }

    #[tokio::test]
    async fn resolve_caches_negative_results() {
        let missing = format!("pa_{}", "b".repeat(64));
        // Exactly one empty result set: a second query would fail, proving the
        // negative was served from cache.
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([Vec::<(
            analytics_ingest_keys::Model,
            Option<environments::Model>,
        )>::new(
        )]);
        let service = service_with(db);

        assert_eq!(service.resolve(&missing).await.expect("first"), None);
        assert_eq!(
            service.resolve(&missing).await.expect("second"),
            None,
            "a missing key must be served from cache, not re-queried"
        );
    }

    #[tokio::test]
    async fn resolve_caches_positive_results() {
        let key = key_model(10, 1, None);
        let public_key = key.public_key.clone();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![(key, None::<environments::Model>)]]);
        let service = service_with(db);

        let first = service.resolve(&public_key).await.expect("first");
        let second = service.resolve(&public_key).await.expect("second");
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    /// A deployment transition changes `environments.current_deployment_id`
    /// without touching `analytics_ingest_keys`, so nothing in `rotate`,
    /// `revoke` or `update` would ever evict this key's cached scope. The
    /// analytics plugin's `Job::RouteTableUpdated` subscriber closes that gap
    /// by calling `invalidate_all_cached_scopes` on every route reload —
    /// this proves the method it calls actually forces a re-query rather
    /// than continuing to serve the stale `deployment_id`.
    #[tokio::test]
    async fn invalidate_all_cached_scopes_forces_a_fresh_deployment_id() {
        let key = key_model(10, 1, Some(5));
        let public_key = key.public_key.clone();
        let env_before_deploy = environment_model(5, 1, Some(100));
        let env_after_deploy = environment_model(5, 1, Some(200));
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
            vec![(key.clone(), Some(env_before_deploy))],
            vec![(key, Some(env_after_deploy))],
        ]);
        let service = service_with(db);

        let before = service
            .resolve(&public_key)
            .await
            .expect("first resolve")
            .expect("key should resolve");
        assert_eq!(before.deployment_id, Some(100));

        // Without invalidation, the cache would answer this from memory and
        // the mock's second result set would never be consumed.
        service.invalidate_all_cached_scopes();

        let after = service
            .resolve(&public_key)
            .await
            .expect("second resolve")
            .expect("key should still resolve");
        assert_eq!(
            after.deployment_id,
            Some(200),
            "invalidate_all_cached_scopes must force a fresh lookup of the new deployment"
        );
    }

    #[tokio::test]
    async fn revoke_invalidates_the_cached_scope_immediately() {
        let key = key_model(10, 1, None);
        let public_key = key.public_key.clone();
        let mut revoked = key.clone();
        revoked.is_active = false;
        revoked.revoked_at = Some(Utc::now());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. resolve -> hit
            .append_query_results([vec![(key.clone(), None::<environments::Model>)]])
            // 2. revoke: find_scoped
            .append_query_results([vec![key]])
            // 3. revoke: update
            .append_query_results([vec![revoked]])
            // 4. resolve again -> must re-query, and now finds nothing
            .append_query_results([Vec::<(
                analytics_ingest_keys::Model,
                Option<environments::Model>,
            )>::new()]);
        let service = service_with(db);

        assert!(service.resolve(&public_key).await.expect("first").is_some());
        service.revoke(1, 10).await.expect("revoke should succeed");
        assert_eq!(
            service.resolve(&public_key).await.expect("after revoke"),
            None,
            "a revoked key must stop resolving without waiting for the TTL"
        );
    }

    #[tokio::test]
    async fn rotate_invalidates_the_cached_scope_for_the_old_value() {
        let key = key_model(10, 1, None);
        let old_public_key = key.public_key.clone();
        let mut rotated = key.clone();
        rotated.public_key = format!("pa_{}", "c".repeat(64));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![(key.clone(), None::<environments::Model>)]])
            .append_query_results([vec![key]])
            .append_query_results([vec![rotated]])
            .append_query_results([Vec::<(
                analytics_ingest_keys::Model,
                Option<environments::Model>,
            )>::new()]);
        let service = service_with(db);

        assert!(service
            .resolve(&old_public_key)
            .await
            .expect("first")
            .is_some());
        service
            .rotate(1, 10, Some(9))
            .await
            .expect("rotate should succeed");
        assert_eq!(
            service
                .resolve(&old_public_key)
                .await
                .expect("after rotate"),
            None,
            "a rotated-out value must stop resolving without waiting for the TTL"
        );
    }

    #[tokio::test]
    async fn update_invalidates_the_cached_scope() {
        let key = key_model(10, 1, None);
        let public_key = key.public_key.clone();
        let mut updated = key.clone();
        updated.allowed_origins = Some(serde_json::json!(["https://app.example.com"]));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![(key.clone(), None::<environments::Model>)]])
            .append_query_results([vec![key]])
            .append_query_results([vec![updated.clone()]])
            .append_query_results([vec![(updated, None::<environments::Model>)]]);
        let service = service_with(db);

        let before = service
            .resolve(&public_key)
            .await
            .expect("first")
            .expect("resolves");
        assert_eq!(before.allowed_origins, None);

        service
            .update(
                1,
                10,
                None,
                Some(Some(vec!["https://app.example.com".into()])),
                None,
            )
            .await
            .expect("update should succeed");

        let after = service
            .resolve(&public_key)
            .await
            .expect("after update")
            .expect("still resolves");
        assert_eq!(
            after.allowed_origins,
            Some(vec!["https://app.example.com".to_string()]),
            "a tightened origin allowlist must take effect immediately"
        );
    }

    #[tokio::test]
    async fn resolve_surfaces_database_failures_as_errors_not_as_invalid_keys() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_errors([
            sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
                "connection reset".to_string(),
            )),
        ]);
        let service = service_with(db);

        let err = service
            .resolve(&format!("pa_{}", "d".repeat(64)))
            .await
            .expect_err("a storage failure must not look like an invalid key");
        assert!(matches!(err, AnalyticsIngestKeyError::Database(_)));
    }

    // ── record_usage ─────────────────────────────────────────────────────

    #[test]
    fn usage_tracker_flushes_the_first_event_then_throttles() {
        let tracker = UsageTracker::default();
        let start = Instant::now();

        assert_eq!(
            tracker.record(1, start),
            Some(1),
            "the first event flushes immediately so last_used_at appears at once"
        );
        assert_eq!(tracker.record(1, start), None);
        assert_eq!(tracker.record(1, start), None);

        // Still inside the window.
        assert_eq!(tracker.record(1, start + Duration::from_secs(59)), None);

        // Past the window: the buffered events flush as one write.
        assert_eq!(
            tracker.record(1, start + Duration::from_secs(61)),
            Some(4),
            "buffered events are counted, not dropped"
        );
    }

    #[test]
    fn usage_tracker_keys_are_independent_and_forgettable() {
        let tracker = UsageTracker::default();
        let start = Instant::now();

        assert_eq!(tracker.record(1, start), Some(1));
        assert_eq!(tracker.record(2, start), Some(1));
        assert_eq!(tracker.record(1, start), None);

        tracker.forget(1);
        assert_eq!(
            tracker.record(1, start),
            Some(1),
            "a forgotten key starts from a clean slate"
        );
    }

    #[tokio::test]
    async fn record_usage_writes_once_then_throttles() {
        let db =
            MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);
        let service = service_with(db);

        assert!(
            service.record_usage(10).await.expect("first usage"),
            "the first event is written through"
        );
        // A second write would exhaust the mock's exec results and error.
        assert!(
            !service.record_usage(10).await.expect("second usage"),
            "subsequent events inside the window must not hit the database"
        );
    }
}
