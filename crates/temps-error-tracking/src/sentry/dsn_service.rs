// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use moka::future::Cache;
use rand::RngExt;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use temps_entities::{project_dsns, projects};

use super::types::{ParsedDSN, ProjectDSN, SentryIngesterError};

/// Resolution-cache lifetime for the unauthenticated tunnel lookup. Mirrors
/// `AnalyticsIngestKeyService`'s `RESOLVE_CACHE_TTL` (ADR-040 §2) so both
/// public ingest surfaces age credentials identically.
///
/// Rotation and revocation evict the affected entry synchronously, so this TTL
/// is a backstop for a missed invalidation, not the normal revocation latency.
const RESOLVE_CACHE_TTL: Duration = Duration::from_secs(5);

/// Bound on cached public keys. Cardinality is the number of *minted* DSNs,
/// which is operator-created and small; the cap only matters while a flood of
/// distinct forged keys is being absorbed as negative entries.
const RESOLVE_CACHE_CAPACITY: u64 = 10_000;

/// Shortest and longest public key this service will look up.
///
/// [`DSNService::generate_key`] mints 32 random bytes hex-encoded, i.e. 64
/// lowercase hex characters, and that is what every Temps-issued DSN carries.
/// The lower bound is the 32-character form classic Sentry DSNs use, kept
/// because an operator may have migrated rows minted elsewhere; anything
/// outside `[32, 64]` hex characters cannot be a `project_dsns.public_key`
/// under any generation scheme this codebase has ever had.
const MIN_PUBLIC_KEY_LEN: usize = 32;
const MAX_PUBLIC_KEY_LEN: usize = 64;

/// Whether `public_key` is shaped like a DSN public key at all.
///
/// Checked *before* the cache and *before* the database on the public tunnel
/// route: this is the cheap half of the anti-amplification story, and it is
/// what structurally guarantees a `tk_`/`dt_` secret pasted into `?sentry_key=`
/// never reaches a query. It does not stop a bot generating valid-shaped
/// garbage — that is the job of the global unresolved-credential budget in
/// [`super::rate_limiter`].
pub fn is_well_formed_public_key(public_key: &str) -> bool {
    (MIN_PUBLIC_KEY_LEN..=MAX_PUBLIC_KEY_LEN).contains(&public_key.len())
        && public_key.chars().all(|c| c.is_ascii_hexdigit())
}

/// First characters of a public key, safe to log.
///
/// Enough to correlate a rejection with a specific key an operator is holding,
/// far too little to replay: 6 hex characters is 24 bits of a 256-bit value.
pub(crate) fn public_key_prefix(public_key: &str) -> &str {
    let end = public_key
        .char_indices()
        .nth(6)
        .map(|(idx, _)| idx)
        .unwrap_or(public_key.len());
    &public_key[..end]
}

/// Build a Sentry-compatible DSN string from the instance base URL.
///
/// Sentry SDKs derive the ingest URL (host, **port**, and scheme) from the DSN
/// itself, so the DSN must preserve all three from `base_url`. Earlier code
/// force-`https`'d and stripped `:8080`, which sent events to the wrong
/// scheme/port (e.g. a local instance at `http://host.docker.internal:8080`
/// produced `https://…@host.docker.internal/4` → unreachable). This keeps
/// `{scheme}://{public_key}@{host[:port]}/{project_id}` intact.
fn build_dsn(base_url: &str, public_key: &str, project_id: i32) -> String {
    let (scheme, rest) = if let Some(r) = base_url.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = base_url.strip_prefix("http://") {
        ("http", r)
    } else {
        ("https", base_url)
    };
    // Keep host[:port]; drop any path / trailing slash.
    let host = rest.split('/').next().unwrap_or(rest);
    format!("{}://{}@{}/{}", scheme, public_key, host, project_id)
}

/// Service for managing Data Source Names (DSNs) for error tracking
pub struct DSNService {
    db: Arc<DatabaseConnection>,
    /// Public key -> resolved row, keyed by the raw key string, with negative
    /// results (`None`) cached too.
    ///
    /// Stated precisely, because it is easy to overclaim: this only helps
    /// against a *repeated* value — one typo'd key baked into a deployed
    /// bundle costs one query, not one per pageview — and against a flood of
    /// *distinct* forged values it does nothing except absorb them as
    /// short-lived negative entries. The global unresolved-credential budget
    /// in [`super::rate_limiter`] is what bounds that case.
    resolve_cache: Cache<String, Option<project_dsns::Model>>,
    /// Bumped by every cache invalidation (targeted or global). Read before
    /// issuing the DB query in [`Self::get_project_by_public_key`] and
    /// compared after it returns: if a revoke, rotation, or route-table
    /// reload happened while that query was in flight, the epoch will have
    /// moved and the (possibly now-stale) result is returned to the caller
    /// but never cached.
    ///
    /// Without this, a read that started just before a revoke can still
    /// observe the pre-revoke active row and insert it into the cache
    /// *after* `invalidate_cached_key` already ran — silently undoing the
    /// synchronous invalidation and leaving a revoked key resolving for a
    /// fresh [`RESOLVE_CACHE_TTL`] window.
    cache_epoch: AtomicU64,
}

impl DSNService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            resolve_cache: Cache::builder()
                .max_capacity(RESOLVE_CACHE_CAPACITY)
                .time_to_live(RESOLVE_CACHE_TTL)
                .build(),
            cache_epoch: AtomicU64::new(0),
        }
    }

    /// Drop a cached resolution so a rotated or revoked key stops working on
    /// the very next request rather than after [`RESOLVE_CACHE_TTL`].
    async fn invalidate_cached_key(&self, public_key: &str) {
        // Bump the epoch *before* invalidating: any read already in flight
        // that checks the epoch after this point will see it has moved and
        // skip caching its (possibly stale) result. Ordering the bump first
        // is what closes the race described on `cache_epoch`.
        self.cache_epoch.fetch_add(1, Ordering::SeqCst);
        self.resolve_cache.invalidate(public_key).await;
    }

    /// Drop every cached resolution.
    ///
    /// Revoking or rotating a specific key is handled by the targeted
    /// [`Self::invalidate_cached_key`], but nothing about a DSN row changes
    /// when its *project* is deleted — `active_dsn_query`'s `projects` join
    /// is what stops it resolving, and that only takes effect on the next
    /// uncached lookup. Called from the `Job::RouteTableUpdated` subscriber
    /// in `plugin.rs`, the same in-process signal that fires on project
    /// deletion, mirroring `AnalyticsIngestKeyService::invalidate_all_cached_scopes`
    /// (ADR-040 §2) exactly.
    pub fn invalidate_all_cached_scopes(&self) {
        self.cache_epoch.fetch_add(1, Ordering::SeqCst);
        self.resolve_cache.invalidate_all();
    }

    /// Generate a new DSN for a project
    pub async fn generate_project_dsn(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        name: Option<String>,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::ProjectNotFound)?;

        // Generate secure public key only (secret key is deprecated)
        let public_key = self.generate_key(32);
        let secret_key = String::new(); // Deprecated - kept empty for compatibility

        // Create new DSN record
        let new_dsn = project_dsns::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            deployment_id: Set(deployment_id),
            name: Set(name.unwrap_or_else(|| "Default DSN".to_string())),
            public_key: Set(public_key.clone()),
            secret_key: Set(secret_key.clone()),
            is_active: Set(true),
            rate_limit_per_minute: Set(Some(1000)),
            allowed_origins: Set(None),
            last_used_at: Set(None),
            event_count: Set(0),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let dsn_model = new_dsn.insert(self.db.as_ref()).await?;

        // Build DSN preserving scheme + host + port (see build_dsn).
        let dsn = build_dsn(base_url, &dsn_model.public_key, project_id);

        Ok(ProjectDSN {
            id: dsn_model.id,
            project_id,
            environment_id: dsn_model.environment_id,
            deployment_id: dsn_model.deployment_id,
            name: dsn_model.name,
            public_key: dsn_model.public_key,
            secret_key: dsn_model.secret_key,
            dsn,
            created_at: dsn_model.created_at,
            is_active: dsn_model.is_active,
            event_count: dsn_model.event_count,
            rate_limit_per_minute: dsn_model.rate_limit_per_minute,
        })
    }

    /// Get or create DSN for a project/environment/deployment
    pub async fn get_or_create_project_dsn(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        // Check if DSN already exists
        let mut query = project_dsns::Entity::find()
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .filter(project_dsns::Column::IsActive.eq(true));

        if let Some(env_id) = environment_id {
            query = query.filter(project_dsns::Column::EnvironmentId.eq(env_id));
        } else {
            query = query.filter(project_dsns::Column::EnvironmentId.is_null());
        }

        if let Some(deploy_id) = deployment_id {
            query = query.filter(project_dsns::Column::DeploymentId.eq(deploy_id));
        } else {
            query = query.filter(project_dsns::Column::DeploymentId.is_null());
        }

        if let Some(existing_dsn) = query.one(self.db.as_ref()).await? {
            // Return existing DSN (scheme + host + port preserved, see build_dsn).
            let dsn = build_dsn(base_url, &existing_dsn.public_key, project_id);

            return Ok(ProjectDSN {
                id: existing_dsn.id,
                project_id,
                environment_id: existing_dsn.environment_id,
                deployment_id: existing_dsn.deployment_id,
                name: existing_dsn.name,
                public_key: existing_dsn.public_key,
                secret_key: existing_dsn.secret_key,
                dsn,
                created_at: existing_dsn.created_at,
                is_active: existing_dsn.is_active,
                event_count: existing_dsn.event_count,
                rate_limit_per_minute: existing_dsn.rate_limit_per_minute,
            });
        }

        // Create new DSN if none exists
        let name = match (environment_id, deployment_id) {
            (Some(_), Some(_)) => "Environment-Deployment DSN".to_string(),
            (Some(_), None) => "Environment DSN".to_string(),
            (None, Some(_)) => "Deployment DSN".to_string(),
            (None, None) => "Project DSN".to_string(),
        };

        self.generate_project_dsn(
            project_id,
            environment_id,
            deployment_id,
            Some(name),
            base_url,
        )
        .await
    }

    /// Create a new DSN without checking for duplicates
    pub async fn create_project_dsn(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        name: Option<String>,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        let name = name.unwrap_or_else(|| match (environment_id, deployment_id) {
            (Some(_), Some(_)) => "Environment-Deployment DSN".to_string(),
            (Some(_), None) => "Environment DSN".to_string(),
            (None, Some(_)) => "Deployment DSN".to_string(),
            (None, None) => "Project DSN".to_string(),
        });

        self.generate_project_dsn(
            project_id,
            environment_id,
            deployment_id,
            Some(name),
            base_url,
        )
        .await
    }

    /// Parse a DSN string
    pub fn parse_dsn(&self, dsn: &str) -> Result<ParsedDSN, SentryIngesterError> {
        let url = url::Url::parse(dsn).map_err(|_| SentryIngesterError::InvalidDSN)?;

        let protocol = url.scheme().to_string();
        let host = url
            .host_str()
            .ok_or(SentryIngesterError::InvalidDSN)?
            .to_string();

        let public_key = url.username().to_string();
        if public_key.is_empty() {
            return Err(SentryIngesterError::InvalidDSN);
        }

        let project_id = url
            .path()
            .trim_start_matches('/')
            .parse::<i32>()
            .map_err(|_| SentryIngesterError::InvalidDSN)?;

        // Never log the key itself: a DSN public key is low-value but it is
        // still a credential on the tunnel route, and application logs are
        // shipped/retained far more widely than the one request that carried
        // it. The prefix is enough to correlate.
        tracing::debug!(
            project_id = project_id,
            public_key_prefix = public_key_prefix(&public_key),
            "Parsed DSN"
        );

        Ok(ParsedDSN {
            public_key,
            project_id,
            host,
            protocol,
        })
    }

    /// Validate DSN authentication
    pub async fn validate_dsn_auth(
        &self,
        parsed_dsn: &ParsedDSN,
    ) -> Result<(bool, Option<project_dsns::Model>), SentryIngesterError> {
        tracing::debug!(
            project_id = parsed_dsn.project_id,
            public_key_prefix = public_key_prefix(&parsed_dsn.public_key),
            "Validating DSN auth"
        );

        let dsn = active_dsn_query()
            .filter(project_dsns::Column::ProjectId.eq(parsed_dsn.project_id))
            .filter(project_dsns::Column::PublicKey.eq(&parsed_dsn.public_key))
            .one(self.db.as_ref())
            .await?;

        tracing::debug!(found = dsn.is_some(), "DSN lookup result");

        match dsn {
            Some(dsn_record) => Ok((true, Some(dsn_record))),
            None => Ok((false, None)),
        }
    }

    /// Validate DSN and return ProjectDSN if valid
    pub async fn validate_dsn(
        &self,
        project_id: i32,
        public_key: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        let dsn_record = active_dsn_query()
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .filter(project_dsns::Column::PublicKey.eq(public_key))
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::InvalidDSN)?;

        let dsn_string = format!(
            "https://{}@sentry.io/{}",
            dsn_record.public_key, dsn_record.project_id
        );

        Ok(ProjectDSN {
            id: dsn_record.id,
            project_id: dsn_record.project_id,
            environment_id: dsn_record.environment_id,
            deployment_id: dsn_record.deployment_id,
            name: dsn_record.name,
            public_key: dsn_record.public_key,
            secret_key: dsn_record.secret_key,
            dsn: dsn_string,
            created_at: dsn_record.created_at,
            is_active: dsn_record.is_active,
            event_count: dsn_record.event_count,
            rate_limit_per_minute: dsn_record.rate_limit_per_minute,
        })
    }

    /// Resolve a DSN public key to its row, without a project id to check it
    /// against — `public_key` is globally unique (`idx_project_dsns_public_key`),
    /// so it identifies its project on its own.
    ///
    /// This is the lookup the unauthenticated browser tunnel route performs, so
    /// it is layered like the analytics ingest-key equivalent: shape gate, then
    /// cache (negative results included), then the database. `Ok(None)` means
    /// "no such active DSN on a live project" and the caller must answer 401;
    /// `Err` is reserved for genuine storage failures so a broken database is
    /// never reported as a bad credential.
    pub async fn get_project_by_public_key(
        &self,
        public_key: &str,
    ) -> Result<Option<project_dsns::Model>, SentryIngesterError> {
        if !is_well_formed_public_key(public_key) {
            return Ok(None);
        }

        if let Some(cached) = self.resolve_cache.get(public_key).await {
            return Ok(cached);
        }

        // Recorded before the query so a concurrent revoke/rotate/route-table
        // reload that lands while it is in flight is detectable afterwards —
        // see the doc comment on `cache_epoch`.
        let epoch_before_query = self.cache_epoch.load(Ordering::SeqCst);

        let dsn = active_dsn_query()
            .filter(project_dsns::Column::PublicKey.eq(public_key))
            .one(self.db.as_ref())
            .await?;

        self.cache_if_epoch_unchanged(epoch_before_query, public_key, dsn.clone())
            .await;

        Ok(dsn)
    }

    /// Cache `dsn` for `public_key` unless [`cache_epoch`](Self::cache_epoch)
    /// moved since `epoch_before_query` was captured — i.e. unless a revoke,
    /// rotation, or route-table reload landed while the read that produced
    /// `dsn` was still in flight.
    ///
    /// Split out from [`Self::get_project_by_public_key`] purely so the
    /// epoch comparison can be exercised directly with controlled `u64`s
    /// rather than by racing a real query against a real invalidation, which
    /// would make the regression test for this non-deterministic.
    async fn cache_if_epoch_unchanged(
        &self,
        epoch_before_query: u64,
        public_key: &str,
        dsn: Option<project_dsns::Model>,
    ) {
        // If it moved, this result may already be stale (e.g. a revoke that
        // committed after our snapshot but before our read returned) — the
        // caller still gets a correct-as-of-read answer for this one
        // request, it just isn't cached, so the next request re-reads.
        if self.cache_epoch.load(Ordering::SeqCst) == epoch_before_query {
            self.resolve_cache.insert(public_key.to_string(), dsn).await;
        }
    }

    /// Regenerate DSN (rotate keys)
    pub async fn regenerate_project_dsn(
        &self,
        dsn_id: i32,
        project_id: i32,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        // Get existing DSN
        let existing_dsn = project_dsns::Entity::find_by_id(dsn_id)
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::InvalidDSN)?;

        // Generate new keys
        let new_public_key = self.generate_key(32);
        let new_secret_key = String::new(); // Deprecated

        let previous_public_key = existing_dsn.public_key.clone();

        // Update DSN
        let mut dsn_update: project_dsns::ActiveModel = existing_dsn.into();
        dsn_update.public_key = Set(new_public_key.clone());
        dsn_update.secret_key = Set(new_secret_key.clone());
        dsn_update.updated_at = Set(Utc::now());

        let updated_dsn = dsn_update.update(self.db.as_ref()).await?;

        // Evict synchronously so the retired key stops resolving immediately
        // rather than after RESOLVE_CACHE_TTL. Rotation is a security action;
        // "eventually" is the wrong latency for it.
        self.invalidate_cached_key(&previous_public_key).await;
        self.invalidate_cached_key(&updated_dsn.public_key).await;

        // Build new DSN string preserving scheme + host + port (see build_dsn).
        let dsn = build_dsn(base_url, &updated_dsn.public_key, project_id);

        Ok(ProjectDSN {
            id: updated_dsn.id,
            project_id,
            environment_id: updated_dsn.environment_id,
            deployment_id: updated_dsn.deployment_id,
            name: updated_dsn.name,
            public_key: updated_dsn.public_key,
            secret_key: updated_dsn.secret_key,
            dsn,
            created_at: updated_dsn.created_at,
            is_active: updated_dsn.is_active,
            event_count: updated_dsn.event_count,
            rate_limit_per_minute: updated_dsn.rate_limit_per_minute,
        })
    }

    /// List all DSNs for a project
    pub async fn list_project_dsns(
        &self,
        project_id: i32,
        base_url: &str,
    ) -> Result<Vec<ProjectDSN>, SentryIngesterError> {
        let dsns = project_dsns::Entity::find()
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;

        // Parse base URL to get host
        let (protocol, host_with_port) = if base_url.starts_with("https://") {
            ("https", base_url.strip_prefix("https://").unwrap())
        } else if base_url.starts_with("http://") {
            ("http", base_url.strip_prefix("http://").unwrap())
        } else {
            ("https", base_url)
        };

        Ok(dsns
            .into_iter()
            .map(|dsn| ProjectDSN {
                id: dsn.id,
                project_id: dsn.project_id,
                environment_id: dsn.environment_id,
                deployment_id: dsn.deployment_id,
                name: dsn.name.clone(),
                public_key: dsn.public_key.clone(),
                secret_key: dsn.secret_key.clone(),
                dsn: format!(
                    "{}://{}@{}/{}",
                    protocol, dsn.public_key, host_with_port, dsn.project_id
                ),
                created_at: dsn.created_at,
                is_active: dsn.is_active,
                event_count: dsn.event_count,
                rate_limit_per_minute: dsn.rate_limit_per_minute,
            })
            .collect())
    }

    /// Revoke (deactivate) a DSN
    pub async fn revoke_dsn(
        &self,
        dsn_id: i32,
        project_id: i32,
    ) -> Result<(), SentryIngesterError> {
        let dsn = project_dsns::Entity::find_by_id(dsn_id)
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::InvalidDSN)?;

        let revoked_public_key = dsn.public_key.clone();

        let mut dsn_update: project_dsns::ActiveModel = dsn.into();
        dsn_update.is_active = Set(false);
        dsn_update.updated_at = Set(Utc::now());
        dsn_update.update(self.db.as_ref()).await?;

        // Revocation must take effect on the next request, not at the end of
        // the cache window.
        self.invalidate_cached_key(&revoked_public_key).await;

        Ok(())
    }

    /// Generate a random key
    fn generate_key(&self, length: usize) -> String {
        let mut rng = rand::rng();
        let bytes: Vec<u8> = (0..length).map(|_| rng.random()).collect();
        hex::encode(bytes)
    }
}

/// Base query for every credential lookup: active DSN rows whose project is
/// still live.
///
/// The `projects` join is not cosmetic. Project deletion is *soft*
/// (`projects.is_deleted`), and it does not cascade to `project_dsns`, so
/// without this an operator who deletes a project keeps ingesting into it
/// forever through a DSN they can no longer see in the console — data
/// accumulating under a project that, as far as every read path is concerned,
/// does not exist.
fn active_dsn_query() -> sea_orm::Select<project_dsns::Entity> {
    project_dsns::Entity::find()
        .filter(project_dsns::Column::IsActive.eq(true))
        .inner_join(projects::Entity)
        .filter(projects::Column::IsDeleted.eq(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{preset::Preset, projects};

    async fn setup_test_db() -> TestDatabase {
        TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database")
    }

    #[test]
    fn build_dsn_preserves_scheme_host_and_port() {
        // Local dev: http + explicit port must survive (Sentry SDK derives the
        // ingest host/port/scheme from the DSN).
        assert_eq!(
            build_dsn("http://host.docker.internal:8080", "pk", 4),
            "http://pk@host.docker.internal:8080/4"
        );
        // Production: https + default port → no port, https preserved.
        assert_eq!(
            build_dsn("https://temps.sh", "pk", 7),
            "https://pk@temps.sh/7"
        );
        // Trailing slash / path is dropped, port kept.
        assert_eq!(
            build_dsn("http://localho.st:8443/", "pk", 1),
            "http://pk@localho.st:8443/1"
        );
        // Scheme-less base falls back to https.
        assert_eq!(
            build_dsn("example.com:9000", "pk", 2),
            "https://pk@example.com:9000/2"
        );
    }

    async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
        use uuid::Uuid;

        let unique_slug = format!("test-project-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(unique_slug),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        project
            .insert(db.as_ref())
            .await
            .expect("Failed to create project")
            .id
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_generate_project_dsn() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db.clone());

        let project_id = create_test_project(&db).await;

        let dsn = service
            .generate_project_dsn(
                project_id,
                None,
                None,
                Some("Test DSN".to_string()),
                "https://example.com",
            )
            .await
            .expect("Failed to generate DSN");

        assert_eq!(dsn.project_id, project_id);
        assert_eq!(dsn.name, "Test DSN");
        assert!(!dsn.public_key.is_empty());
        assert!(dsn.dsn.contains(&dsn.public_key));
        assert!(dsn.is_active);
    }

    #[tokio::test]
    async fn test_parse_dsn() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db);

        let dsn_str = "https://abc123@example.com/42";
        let parsed = service.parse_dsn(dsn_str).expect("Failed to parse DSN");

        assert_eq!(parsed.public_key, "abc123");
        assert_eq!(parsed.project_id, 42);
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.protocol, "https");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_validate_dsn_auth() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db.clone());

        let project_id = create_test_project(&db).await;

        // Generate a DSN
        let dsn = service
            .generate_project_dsn(project_id, None, None, None, "https://example.com")
            .await
            .expect("Failed to generate DSN");

        // Parse it
        let parsed = service.parse_dsn(&dsn.dsn).expect("Failed to parse DSN");

        // Validate it
        let (is_valid, record) = service
            .validate_dsn_auth(&parsed)
            .await
            .expect("Failed to validate DSN");

        assert!(is_valid);
        assert!(record.is_some());
        assert_eq!(record.unwrap().project_id, project_id);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_revoke_dsn() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db.clone());

        let project_id = create_test_project(&db).await;

        let dsn = service
            .generate_project_dsn(project_id, None, None, None, "https://example.com")
            .await
            .expect("Failed to generate DSN");

        // Revoke it
        service
            .revoke_dsn(dsn.id, project_id)
            .await
            .expect("Failed to revoke DSN");

        // Try to validate - should fail
        let parsed = service.parse_dsn(&dsn.dsn).expect("Failed to parse DSN");
        let (is_valid, _) = service
            .validate_dsn_auth(&parsed)
            .await
            .expect("Failed to validate DSN");

        assert!(!is_valid);
    }

    /// The revocation-race regression: a lookup that read an active row
    /// *before* a concurrent revoke/rotate must not be allowed to cache that
    /// row *after* the revoke's invalidation already ran, or the revoke would
    /// be silently undone for up to `RESOLVE_CACHE_TTL`.
    ///
    /// Exercises `cache_if_epoch_unchanged` directly with a controlled epoch
    /// rather than racing a real query against a real invalidation — see its
    /// doc comment for why.
    #[tokio::test]
    #[serial_test::serial]
    async fn cache_if_epoch_unchanged_skips_insert_when_epoch_moved_during_the_query() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db);

        let epoch_before_query = service.cache_epoch.load(Ordering::SeqCst);

        // Simulate a revoke/rotate/route-table reload landing while our
        // (hypothetical) database read was in flight.
        service.cache_epoch.fetch_add(1, Ordering::SeqCst);

        service
            .cache_if_epoch_unchanged(epoch_before_query, "deadbeefcafe", None)
            .await;

        assert!(
            service.resolve_cache.get("deadbeefcafe").await.is_none(),
            "a result read under a since-moved epoch must never be cached"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn cache_if_epoch_unchanged_caches_when_nothing_raced_it() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db);

        let epoch_before_query = service.cache_epoch.load(Ordering::SeqCst);

        service
            .cache_if_epoch_unchanged(epoch_before_query, "cafebabe0000", None)
            .await;

        assert!(
            service.resolve_cache.get("cafebabe0000").await.is_some(),
            "epoch unchanged: the (negative) result should be cached as normal"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn revoke_and_rotate_bump_the_cache_epoch() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db.clone());
        let project_id = create_test_project(&db).await;

        let dsn = service
            .generate_project_dsn(project_id, None, None, None, "https://example.com")
            .await
            .expect("Failed to generate DSN");

        let epoch_after_create = service.cache_epoch.load(Ordering::SeqCst);

        service
            .regenerate_project_dsn(dsn.id, project_id, "https://example.com")
            .await
            .expect("Failed to rotate DSN");
        let epoch_after_rotate = service.cache_epoch.load(Ordering::SeqCst);
        assert!(
            epoch_after_rotate > epoch_after_create,
            "rotating a DSN must bump the cache epoch"
        );

        service
            .revoke_dsn(dsn.id, project_id)
            .await
            .expect("Failed to revoke DSN");
        let epoch_after_revoke = service.cache_epoch.load(Ordering::SeqCst);
        assert!(
            epoch_after_revoke > epoch_after_rotate,
            "revoking a DSN must bump the cache epoch"
        );
    }

    #[tokio::test]
    async fn invalidate_all_cached_scopes_bumps_the_epoch_and_clears_the_cache() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db);

        let epoch_before_query = service.cache_epoch.load(Ordering::SeqCst);
        service
            .cache_if_epoch_unchanged(epoch_before_query, "feedfacecafe", None)
            .await;
        assert!(service.resolve_cache.get("feedfacecafe").await.is_some());

        service.invalidate_all_cached_scopes();

        assert!(
            service.cache_epoch.load(Ordering::SeqCst) > epoch_before_query,
            "invalidate_all_cached_scopes must bump the epoch"
        );
        assert!(
            service.resolve_cache.get("feedfacecafe").await.is_none(),
            "invalidate_all_cached_scopes must clear every cached entry"
        );
    }
}
