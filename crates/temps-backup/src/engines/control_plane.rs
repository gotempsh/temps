// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `ControlPlaneEngine`: in-process backup of the Temps control-plane
//! PostgreSQL database, implemented against `engine_v2::BackupEngine`.
//!
//! ## Flow
//!
//! 1. Validate S3 source + bucket reachability.
//! 2. Run `pg_dumpall --globals-only` + `pg_dump | gzip` as a one-shot Docker
//!    container (host networking) whose entrypoint is the backup command
//!    itself. The dump stays in the sidecar's own filesystem and is streamed
//!    out through the Docker archive API into an attempt-scoped host dir
//!    under `<data_dir>/backups/tmp` (see `dump_capture`); the sidecar is
//!    removed on every path.
//! 3. Upload the resulting `.sql.gz` to S3 (single-part or multipart).
//! 4. Write a `metadata.json` companion object.
//!
//! ## What is backed up
//!
//! Schema for every table, but data only for critical tables: high-volume
//! observability/analytics tables ([`EXCLUDED_DATA_TABLES`]) are dumped
//! schema-only via `--exclude-table-data`. Most of them are TimescaleDB
//! hypertables whose rows physically live in `_timescaledb_internal` chunk
//! tables, so the exclusion patterns are resolved against
//! `_timescaledb_catalog.hypertable` at backup time to also cover the
//! `_hyper_N_*` (and `compress_hyper_N_*`) chunks.
//!
//! ## Retry semantics
//!
//! Every attempt gets a fresh [`DumpAttempt`]: its own sidecar container
//! name, its own directory inside the sidecar and its own host directory.
//! A container or file left behind by a failed (or crashed) attempt can
//! therefore never block the next one, and an attempt deletes only what it
//! created. The S3 key is per backup, so a retried upload replaces the
//! object instead of adding a second one.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use tracing::{info, warn};

use super::dump_capture::{capture_dump, CaptureRequest, DumpAttempt, DumpCaptureError};
use super::oneshot::OneShotSpec;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "control_plane";
const DUMP_FILE_SUFFIX: &str = "backup.sql.gz";
/// Database name used in user-facing dump errors.
const CONTROL_PLANE_TOOL: &str = "Control-plane PostgreSQL";
/// Prefix of the attempt-scoped sidecar container name.
const CONTAINER_NAME_PREFIX: &str = "temps-cp-backup";
/// File names inside the sidecar's attempt directory.
const SIDECAR_DUMP_SQL: &str = "backup.sql";
const SIDECAR_DUMP_GZ: &str = "backup.sql.gz";
const SIDECAR_STDERR: &str = "pg_dump.stderr";

/// High-volume observability/analytics tables backed up schema-only.
///
/// Their data regenerates from live traffic and would otherwise dominate the
/// dump size; everything needed to bring a control plane back (projects,
/// deployments, domains, certs, users, secrets, settings, audit logs, revenue
/// data) keeps its data.
///
/// This is the seed list only. A table that keeps its data while referencing
/// an excluded parent produces a dump that cannot be restored: pg_dump emits
/// the child's rows and then `ALTER TABLE ... ADD CONSTRAINT`, which fails on
/// the first dangling key (`request_logs.session_id -> request_sessions`
/// did exactly that, and the first isolated restore verification of a
/// control-plane backup caught it). So [`resolve_excluded_data`] closes the
/// set over every foreign key at backup time instead of trusting this list
/// to stay closed by hand.
pub const EXCLUDED_DATA_TABLES: &[&str] = &[
    // Proxy / OTel telemetry (hypertables)
    "proxy_logs",
    "otel_spans",
    "otel_metrics",
    "otel_log_events",
    // Web analytics
    "events",
    "events_ch_outbox",
    "visitor",
    "request_sessions",
    "performance_metrics",
    // Session replay (raw rrweb payloads)
    "session_replay_sessions",
    "session_replay_events",
    // Error tracking (groups reference visitor, so the set is excluded whole)
    "error_events",
    "error_groups",
    "error_alert_fires",
    // Uptime / AI-gateway samples (hypertables)
    "status_checks",
    "ai_usage_logs",
];

// ── Dependencies ─────────────────────────────────────────────────────────────

pub struct ControlPlaneDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub config_service: Arc<temps_config::ConfigService>,
}

// ── Engine ────────────────────────────────────────────────────────────────────

pub struct ControlPlaneEngine {
    deps: Arc<ControlPlaneDeps>,
}

impl ControlPlaneEngine {
    pub fn new(deps: ControlPlaneDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for ControlPlaneEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        // ── Params + S3 client ───────────────────────────────────────────────
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;
        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "control-plane-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let s3_key =
            v2_common::build_dump_s3_key(&s3_source.bucket_path, &backup_uuid, DUMP_FILE_SUFFIX);

        info!(
            backup_id,
            s3_key = %s3_key,
            bucket = %s3_source.bucket_name,
            "ControlPlaneEngine: S3 validated, starting dump",
        );

        // ── Resolve DB connection params ─────────────────────────────────────
        let target =
            ControlPlaneTarget::from_database_url(&deps.config_service.get_database_url())?;

        // Match the running server's major so pg_dumpall is version-compatible.
        let pg_tag = detect_postgres_version(&deps).await;
        let major = pg_tag.trim_start_matches("pg");
        let image_tag = format!("postgres:{}", major);
        super::image_pull::ensure_image_pulled_v2(&image_tag, ENGINE_KEY).await?;

        // ── Attempt-scoped working dir + container command ───────────────────
        //
        // The sidecar writes into its own filesystem and the dump is streamed
        // out through the Docker archive API (see `dump_capture`): no bind
        // mount, so Docker in a VM or on another host works, and the
        // container name and every path are scoped to this attempt, so a
        // retry never collides with what a failed attempt left behind. The
        // host copy still lives under `<data_dir>/backups/tmp`, which is
        // sized for control-plane dumps, unlike a possibly small `/tmp`.
        let backup_dir = v2_common::ensure_backup_tmpdir(&deps.config_service).await?;
        let attempt =
            DumpAttempt::new_in(&backup_dir, CONTROL_PLANE_TOOL, ENGINE_KEY, &backup_uuid)?;
        let host_dump_path = attempt.host_path(SIDECAR_DUMP_GZ);

        let excluded = resolve_excluded_data(deps.db.as_ref()).await?;
        info!(
            backup_id,
            tables = excluded.tables.len(),
            patterns = excluded.patterns.len(),
            "ControlPlaneEngine: dumping schema-only for high-volume tables",
        );
        let spec =
            control_plane_dump_spec(&attempt, backup_id, &image_tag, &target, &excluded.patterns);

        let docker =
            bollard::Docker::connect_with_local_defaults().map_err(|e| BackupError::Failed {
                reason: format!("failed to connect to Docker: {}", e),
            })?;

        let file_size = capture_dump(
            &docker,
            CaptureRequest {
                tool: CONTROL_PLANE_TOOL,
                spec,
                container_path: attempt.container_path(SIDECAR_DUMP_GZ),
                host_path: host_dump_path.clone(),
                failure_log: Some((
                    attempt.container_path(SIDECAR_STDERR),
                    attempt.host_path(SIDECAR_STDERR),
                )),
            },
            &ctx.cancel,
        )
        .await?;
        let file_size = i64::try_from(file_size).map_err(|_| BackupError::Failed {
            reason: format!(
                "control-plane dump for backup {backup_id} is larger than i64::MAX bytes"
            ),
        })?;
        let host_dump_path_str = host_dump_path.to_string_lossy().into_owned();

        info!(
            backup_id,
            path = %host_dump_path_str,
            size_bytes = file_size,
            "ControlPlaneEngine: dump completed",
        );

        // ── Upload dump ──────────────────────────────────────────────────────
        if ctx.cancel.is_cancelled() {
            return Err(BackupError::Cancelled);
        }
        let tags = v2_common::BackupTags::load_for_backup(&ctx.db, ctx.backup_id).await;
        v2_common::upload_file(
            &s3_client,
            &s3_source.bucket_name,
            &s3_key,
            &host_dump_path_str,
            "application/x-gzip",
            file_size,
            Some(&tags),
            &ctx.cancel,
        )
        .await
        .map_err(|error| {
            DumpCaptureError::upload(CONTROL_PLANE_TOOL, &s3_source.bucket_name, &s3_key, error)
        })?;
        // Deletes this attempt's host dir (and the dump in it); every early
        // return above does the same when `attempt` drops.
        drop(attempt);

        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %s3_key,
            size_bytes = file_size,
            "ControlPlaneEngine: dump uploaded",
        );

        // ── Metadata companion ───────────────────────────────────────────────
        let metadata_key = v2_common::derive_metadata_key(&s3_key);
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &s3_key,
            file_size,
            s3_source_id,
            "gzip",
            Some(serde_json::json!({
                "excluded_table_data": excluded.tables,
            })),
        )
        .await?;
        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %metadata_key,
            "ControlPlaneEngine: metadata.json written",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}

// ── Local helpers ────────────────────────────────────────────────────────────

/// Where the control-plane dump connects, parsed from `DATABASE_URL`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ControlPlaneTarget {
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
}

impl ControlPlaneTarget {
    fn from_database_url(database_url: &str) -> Result<Self, BackupError> {
        let url = url::Url::parse(database_url).map_err(|e| BackupError::PermanentFailure {
            reason: format!("invalid DATABASE_URL: {}", e),
        })?;
        Ok(Self {
            host: url.host_str().unwrap_or("localhost").to_string(),
            port: url.port().unwrap_or(5432),
            database: url.path().trim_start_matches('/').to_string(),
            username: url.username().to_string(),
            password: urlencoding::decode(url.password().unwrap_or(""))
                .map(|s| s.to_string())
                .unwrap_or_default(),
        })
    }
}

/// The control-plane dump sidecar.
///
/// Globals (roles) via `pg_dumpall`, then a single-database `pg_dump` with
/// the heavy tables dumped schema-only. Both restore through the same
/// `psql --dbname=<cp-db> --file=dump.sql` path as the old `pg_dumpall`
/// format. Everything is written into this attempt's directory inside the
/// sidecar's own filesystem; stderr goes to a file next to the dump and is
/// copied out only when the command fails.
fn control_plane_dump_spec(
    attempt: &DumpAttempt,
    backup_id: i32,
    image: &str,
    target: &ControlPlaneTarget,
    exclude_patterns: &[String],
) -> OneShotSpec {
    let exclude_flags = exclude_patterns
        .iter()
        .map(|p| format!("--exclude-table-data={}", v2_common::shell_escape(p)))
        .collect::<Vec<_>>()
        .join(" ");
    let pg_dump_cmd = format!(
        "mkdir -p {dir} && pg_dumpall --globals-only --clean --if-exists --no-password \
         --host={host} --port={port} --username={user} --database={db} \
         2>{stderr} > {out} \
         && pg_dump --clean --if-exists --no-password \
         --host={host} --port={port} --username={user} {excludes} --dbname={db} \
         2>>{stderr} >> {out} && gzip {out}",
        dir = v2_common::shell_escape(&attempt.container_dir()),
        host = v2_common::shell_escape(&target.host),
        port = v2_common::shell_escape(&target.port.to_string()),
        user = v2_common::shell_escape(&target.username),
        db = v2_common::shell_escape(&target.database),
        excludes = exclude_flags,
        stderr = v2_common::shell_escape(&attempt.container_path(SIDECAR_STDERR)),
        out = v2_common::shell_escape(&attempt.container_path(SIDECAR_DUMP_SQL)),
    );
    OneShotSpec {
        image: image.to_string(),
        name: attempt.container_name(CONTAINER_NAME_PREFIX),
        engine: ENGINE_KEY,
        backup_id,
        entrypoint: vec!["sh".to_string(), "-c".to_string()],
        cmd: vec![pg_dump_cmd],
        env: vec![format!("PGPASSWORD={}", target.password)],
        binds: vec![],
        // `host` mode so the container can reach 127.0.0.1:5432 where the
        // control-plane Postgres binds under `temps serve`.
        network_mode: Some("host".to_string()),
        user: Some("root".to_string()),
        stderr_watch: None,
    }
}

/// What a control-plane dump leaves schema-only: the table names, and the
/// `--exclude-table-data` patterns that implement them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedData {
    /// Every `public` table dumped schema-only, seeds and foreign-key
    /// dependants alike, sorted. Recorded in the backup's `metadata.json`.
    pub tables: Vec<String>,
    /// `schema.table` patterns for `--exclude-table-data`, one per table plus
    /// the chunk patterns of every hypertable among them.
    pub patterns: Vec<String>,
}

/// Resolve [`EXCLUDED_DATA_TABLES`] against the live database.
///
/// 1. Close the set over foreign keys: any `public` table with a foreign key
///    to an excluded table is excluded too, recursively. A kept child whose
///    rows point at parent rows the dump does not carry restores up to the
///    `ADD CONSTRAINT` and then fails; excluding it is the only dump that
///    restores. The tables added this way are logged by name so a new
///    reference to telemetry data is noticed, not silently dropped.
/// 2. For every table that is a TimescaleDB hypertable, additionally exclude
///    its chunk tables (`_timescaledb_internal._hyper_<id>_*`) and, when
///    compression is enabled, the compressed chunks
///    (`_timescaledb_internal.compress_hyper_<cid>_*`): hypertable rows live
///    in chunks, so root-table exclusion alone would keep all the data.
///    Continuous-aggregate materializations are intentionally NOT excluded:
///    they are small and let dashboards keep aggregate history.
///
/// A failed closure lookup fails the backup: without it the engine would
/// write a dump it already knows may not restore, and a backup that fails
/// loudly is worth more than one that only fails at restore time. A failed
/// hypertable lookup only costs dump size, so it is logged and the dump
/// proceeds.
pub async fn resolve_excluded_data(db: &DatabaseConnection) -> Result<ExcludedData, BackupError> {
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

    // EXCLUDED_DATA_TABLES are compile-time identifiers, safe to inline.
    let seed_list = EXCLUDED_DATA_TABLES
        .iter()
        .map(|t| format!("'{}'", t))
        .collect::<Vec<_>>()
        .join(",");

    #[derive(FromQueryResult)]
    struct TableRow {
        relname: String,
    }
    let closure_sql = format!(
        "WITH RECURSIVE excluded(relid) AS (              SELECT c.oid FROM pg_class c              JOIN pg_namespace n ON n.oid = c.relnamespace              WHERE n.nspname = 'public' AND c.relkind IN ('r','p')                AND c.relname IN ({seed_list})            UNION              SELECT f.conrelid FROM pg_constraint f              JOIN excluded e ON f.confrelid = e.relid              JOIN pg_class c ON c.oid = f.conrelid              JOIN pg_namespace n ON n.oid = c.relnamespace              WHERE f.contype = 'f' AND f.conrelid <> f.confrelid                AND n.nspname = 'public'          )          SELECT DISTINCT c.relname FROM excluded e          JOIN pg_class c ON c.oid = e.relid          ORDER BY c.relname"
    );
    let mut tables: Vec<String> = match TableRow::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        closure_sql,
    ))
    .all(db)
    .await
    {
        Ok(rows) => rows.into_iter().map(|row| row.relname).collect(),
        Err(e) => {
            return Err(BackupError::Failed {
                reason: format!(
                    "could not close the control-plane dump's schema-only tables over \
                     foreign keys (a kept table referencing excluded data would make the \
                     dump unrestorable): {e}"
                ),
            });
        }
    };
    // Seeds that do not exist on this schema version still get a pattern:
    // pg_dump ignores a pattern that matches nothing, and the list stays the
    // documented intent rather than a per-instance surprise.
    for seed in EXCLUDED_DATA_TABLES {
        if !tables.iter().any(|t| t == seed) {
            tables.push((*seed).to_string());
        }
    }
    tables.sort();
    tables.dedup();
    let dependants: Vec<&str> = tables
        .iter()
        .map(String::as_str)
        .filter(|t| !EXCLUDED_DATA_TABLES.contains(t))
        .collect();
    if !dependants.is_empty() {
        info!(
            added = ?dependants,
            "ControlPlaneEngine: also dumping schema-only tables that reference excluded data",
        );
    }

    let mut patterns: Vec<String> = tables.iter().map(|t| format!("public.{}", t)).collect();

    #[derive(FromQueryResult)]
    struct HypertableRow {
        id: i32,
        compressed_hypertable_id: Option<i32>,
    }
    // Names come from pg_class; quote them as literals regardless.
    let table_list = tables
        .iter()
        .map(|t| format!("'{}'", t.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let hypertable_sql = format!(
        "SELECT id, compressed_hypertable_id          FROM _timescaledb_catalog.hypertable          WHERE schema_name = 'public' AND table_name IN ({})",
        table_list
    );
    match HypertableRow::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        hypertable_sql,
    ))
    .all(db)
    .await
    {
        Ok(rows) => {
            for row in rows {
                patterns.push(format!("_timescaledb_internal._hyper_{}_*", row.id));
                if let Some(cid) = row.compressed_hypertable_id {
                    patterns.push(format!("_timescaledb_internal.compress_hyper_{}_*", cid));
                }
            }
        }
        Err(e) => {
            warn!(
                "ControlPlaneEngine: could not resolve TimescaleDB chunks for data                  exclusion, hypertable data will be included in the dump: {}",
                e
            );
        }
    }

    Ok(ExcludedData { tables, patterns })
}

/// Detect the PostgreSQL major version via `current_setting('server_version')`.
/// Falls back to `"pg18"` if detection fails — pg_dumpall is
/// backwards-compatible so the worst case is a slightly-wrong sidecar tag.
async fn detect_postgres_version(deps: &ControlPlaneDeps) -> String {
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

    #[derive(FromQueryResult)]
    struct VersionRow {
        server_version: String,
    }

    let row = VersionRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT current_setting('server_version') AS server_version",
        vec![],
    ))
    .one(deps.db.as_ref())
    .await;

    match row {
        Ok(Some(r)) => {
            let major: u32 = r
                .server_version
                .split('.')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(18);
            format!("pg{}", major)
        }
        Ok(None) | Err(_) => {
            warn!("ControlPlaneEngine: could not detect PG version, defaulting to pg18");
            "pg18".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    //! The dump leaves high-volume tables schema-only. A kept table that
    //! references an excluded one makes the dump unrestorable (its rows are
    //! dumped, the parent's are not, and `ADD CONSTRAINT` fails on restore),
    //! so the exclusion set must be closed over foreign keys against the real
    //! schema. This runs the resolver against a freshly migrated database.
    //!
    //! Skips (and passes) only when no Docker daemon answers; a container that
    //! fails to start once Docker is reachable is a real failure.

    use std::collections::HashSet;
    use std::time::Duration;

    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
    use sea_orm_migration::MigratorTrait;
    use temps_migrations::Migrator;
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    use super::{resolve_excluded_data, EXCLUDED_DATA_TABLES};

    /// `true` when a Docker daemon answers on the local defaults. Kept apart
    /// from container startup so an image or startup problem surfaces as a
    /// failure instead of being mistaken for "no Docker here".
    async fn docker_available() -> bool {
        match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        }
    }

    async fn connect_with_retries(url: &str) -> Result<DatabaseConnection, sea_orm::DbErr> {
        let mut last = None;
        for _ in 0..20 {
            match Database::connect(url).await {
                Ok(db) => return Ok(db),
                Err(error) => {
                    last = Some(error);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
        Err(last.expect("at least one attempt"))
    }

    #[tokio::test]
    async fn excluded_data_is_closed_over_foreign_keys_on_the_real_schema(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !docker_available().await {
            eprintln!("Skipping control-plane exclusion test: no Docker daemon reachable");
            return Ok(());
        }
        let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .with_cmd(vec![
                "postgres",
                "-c",
                "timescaledb.max_background_workers=0",
            ])
            .with_startup_timeout(Duration::from_secs(120))
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let url = format!("postgresql://postgres:postgres@localhost:{port}/postgres");
        tokio::time::sleep(Duration::from_secs(3)).await;
        let db = connect_with_retries(&url).await?;
        Migrator::up(&db, None).await?;

        let excluded = resolve_excluded_data(&db).await?;
        let set: HashSet<&str> = excluded.tables.iter().map(String::as_str).collect();

        for seed in EXCLUDED_DATA_TABLES {
            assert!(set.contains(seed), "seed {seed} must stay excluded");
            assert!(
                excluded
                    .patterns
                    .iter()
                    .any(|p| p == &format!("public.{seed}")),
                "seed {seed} must have its --exclude-table-data pattern"
            );
        }

        // Every foreign key from a kept table into an excluded table is a
        // dump that cannot be restored; there must be none left.
        let rows = db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT c.conname, child.relname AS child, parent.relname AS parent \
                 FROM pg_constraint c \
                 JOIN pg_class child ON child.oid = c.conrelid \
                 JOIN pg_class parent ON parent.oid = c.confrelid \
                 JOIN pg_namespace n ON n.oid = child.relnamespace \
                 WHERE c.contype = 'f' AND n.nspname = 'public' AND c.conrelid <> c.confrelid"
                    .to_string(),
            ))
            .await?;
        let mut dangling = Vec::new();
        for row in rows {
            let child: String = row.try_get("", "child")?;
            let parent: String = row.try_get("", "parent")?;
            let name: String = row.try_get("", "conname")?;
            if set.contains(parent.as_str()) && !set.contains(child.as_str()) {
                dangling.push(format!("{name}: {child} -> {parent}"));
            }
        }
        assert!(
            dangling.is_empty(),
            "kept tables still reference excluded data: {dangling:?}"
        );

        // The case that reached production: request_logs keeps its rows while
        // request_sessions (a seed) does not, and its FK is ON DELETE SET
        // NULL, so the dumped rows carry session ids the dump never restores.
        assert!(
            set.contains("request_logs"),
            "request_logs references request_sessions and must be excluded with it: {:?}",
            excluded.tables
        );
        assert!(excluded.patterns.iter().any(|p| p == "public.request_logs"));

        // Hypertables among the excluded tables get their chunk patterns.
        assert!(
            excluded
                .patterns
                .iter()
                .any(|p| p.starts_with("_timescaledb_internal._hyper_")),
            "hypertable chunk patterns must be present: {:?}",
            excluded.patterns
        );
        Ok(())
    }
}

#[cfg(test)]
mod dump_tests {
    //! The control-plane dump runs in an attempt-scoped sidecar with host
    //! networking and is read back through the Docker archive API. The unit
    //! tests pin the spec and the error classification; the Docker-backed
    //! tests run the real `pg_dumpall` + `pg_dump` command against a real
    //! PostgreSQL reached over host networking, and skip (with a message)
    //! only when no Docker daemon answers or the images cannot be pulled.

    use super::*;
    use std::time::Duration;

    const TEST_PASSWORD: &str = "test-pass";

    fn target(host: &str, port: u16, password: &str) -> ControlPlaneTarget {
        ControlPlaneTarget {
            host: host.to_string(),
            port,
            database: "temps".to_string(),
            username: "postgres".to_string(),
            password: password.to_string(),
        }
    }

    // ── Spec + attempt scoping ───────────────────────────────────────────

    #[test]
    fn dump_spec_is_attempt_scoped_and_has_no_bind_mount() {
        let parent = tempfile::tempdir().expect("parent");
        let first = DumpAttempt::new_in(parent.path(), CONTROL_PLANE_TOOL, ENGINE_KEY, "b-uuid")
            .expect("first attempt");
        let retry = DumpAttempt::new_in(parent.path(), CONTROL_PLANE_TOOL, ENGINE_KEY, "b-uuid")
            .expect("retry attempt");
        let excludes = vec![
            "public.proxy_logs".to_string(),
            "_timescaledb_internal._hyper_3_*".to_string(),
        ];
        let target = target("127.0.0.1", 5432, "pa's$word");

        let spec = control_plane_dump_spec(&first, 7, "postgres:18", &target, &excludes);
        let retry_spec = control_plane_dump_spec(&retry, 7, "postgres:18", &target, &excludes);

        assert!(
            spec.binds.is_empty(),
            "output must not depend on a host bind: {:?}",
            spec.binds
        );
        assert_eq!(spec.network_mode.as_deref(), Some("host"));
        assert_eq!(spec.image, "postgres:18");
        assert_eq!(spec.backup_id, 7);
        assert_ne!(spec.name, retry_spec.name, "a retry gets its own container");
        assert!(
            spec.name.starts_with("temps-cp-backup-b-uuid-"),
            "{}",
            spec.name
        );
        assert_ne!(
            spec.name, "temps-cp-backup-b-uuid",
            "the per-backup name an older binary used must never be reused"
        );

        let cmd = &spec.cmd[0];
        for path in [
            first.container_path(SIDECAR_DUMP_SQL),
            first.container_path(SIDECAR_STDERR),
            first.container_dir(),
        ] {
            assert!(cmd.contains(&path), "missing {path}: {cmd}");
        }
        assert!(!cmd.contains(&retry.container_dir()), "{cmd}");
        assert!(!cmd.contains("/backup/"), "no bind-mount path: {cmd}");

        // Dump semantics: globals first, then the single-database dump with
        // the exclusions, appended to the same file, then gzip.
        let globals = cmd.find("pg_dumpall --globals-only").expect("globals");
        let database = cmd.find("&& pg_dump --clean").expect("pg_dump");
        assert!(globals < database, "{cmd}");
        assert!(cmd.contains("--exclude-table-data='public.proxy_logs'"));
        assert!(cmd.contains("--exclude-table-data='_timescaledb_internal._hyper_3_*'"));
        assert!(cmd.contains("--host='127.0.0.1' --port='5432' --username='postgres'"));
        assert!(cmd.contains("--dbname='temps'"));
        assert!(cmd.contains(&format!(
            "2>>'{}' >> '{}'",
            first.container_path(SIDECAR_STDERR),
            first.container_path(SIDECAR_DUMP_SQL)
        )));
        assert!(cmd.ends_with(&format!(
            "gzip '{}'",
            first.container_path(SIDECAR_DUMP_SQL)
        )));
        assert!(
            !cmd.contains("pa's"),
            "the password must stay out of argv: {cmd}"
        );
        assert_eq!(spec.env, vec!["PGPASSWORD=pa's$word".to_string()]);
    }

    #[test]
    fn leftovers_of_earlier_attempts_in_the_backup_tmpdir_are_neither_reused_nor_deleted() {
        // `<data_dir>/backups/tmp` as an older binary (bind-mount layout) and
        // an earlier attempt of this binary left it.
        let backup_dir = tempfile::tempdir().expect("backup tmpdir");
        let legacy_dump = backup_dir.path().join("b-uuid.sql.gz");
        let legacy_stderr = backup_dir.path().join("b-uuid.stderr");
        std::fs::write(&legacy_dump, b"stale dump").expect("legacy dump");
        std::fs::write(&legacy_stderr, b"stale stderr").expect("legacy stderr");
        let earlier =
            DumpAttempt::new_in(backup_dir.path(), CONTROL_PLANE_TOOL, ENGINE_KEY, "b-uuid")
                .expect("earlier attempt");
        std::fs::write(earlier.host_path(SIDECAR_DUMP_GZ), b"earlier").expect("earlier dump");

        let retry =
            DumpAttempt::new_in(backup_dir.path(), CONTROL_PLANE_TOOL, ENGINE_KEY, "b-uuid")
                .expect("retry attempt");
        let retry_dump = retry.host_path(SIDECAR_DUMP_GZ);
        assert!(retry.host_dir().starts_with(backup_dir.path()));
        assert!(!retry_dump.exists(), "a retry starts from a clean path");
        assert_ne!(retry_dump, legacy_dump);
        assert_ne!(retry_dump, earlier.host_path(SIDECAR_DUMP_GZ));
        std::fs::write(&retry_dump, b"retry").expect("retry dump");
        let retry_dir = retry.host_dir().to_path_buf();
        drop(retry);

        assert!(!retry_dir.exists(), "the attempt removes its own directory");
        assert_eq!(std::fs::read(&legacy_dump).expect("kept"), b"stale dump");
        assert_eq!(
            std::fs::read(&legacy_stderr).expect("kept"),
            b"stale stderr"
        );
        assert_eq!(
            std::fs::read(earlier.host_path(SIDECAR_DUMP_GZ)).expect("kept"),
            b"earlier"
        );
    }

    #[test]
    fn target_is_parsed_from_the_database_url_with_the_old_defaults() {
        let parsed = ControlPlaneTarget::from_database_url(
            "postgres://temps_user:p%40ss%2Fw@db.internal:6543/temps_cp",
        )
        .expect("valid url");
        assert_eq!(
            parsed,
            ControlPlaneTarget {
                host: "db.internal".into(),
                port: 6543,
                database: "temps_cp".into(),
                username: "temps_user".into(),
                password: "p@ss/w".into(),
            }
        );

        let defaults =
            ControlPlaneTarget::from_database_url("postgres://u@localhost/db").expect("valid");
        assert_eq!(defaults.port, 5432);
        assert_eq!(defaults.password, "");

        let error = ControlPlaneTarget::from_database_url("not a url").expect_err("invalid");
        assert!(
            matches!(&error, BackupError::PermanentFailure { reason } if reason.starts_with("invalid DATABASE_URL")),
            "{error:?}"
        );
    }

    // ── Error classification ─────────────────────────────────────────────

    #[test]
    fn dump_failures_are_classified_by_where_they_happened() {
        let export = DumpCaptureError::Export {
            tool: CONTROL_PLANE_TOOL,
            container: "temps-cp-backup-b-a".into(),
            exit_code: 1,
            stderr: "pg_dumpall: error: connection to server failed".into(),
        };
        let message = export.to_string();
        assert!(
            message.starts_with("Control-plane PostgreSQL export failed in backup container 'temps-cp-backup-b-a' with exit code 1"),
            "{message}"
        );
        assert!(matches!(
            BackupError::from(export),
            BackupError::Failed { .. }
        ));

        let unreadable = DumpCaptureError::DumpUnreadable {
            tool: CONTROL_PLANE_TOOL,
            container: "temps-cp-backup-b-a".into(),
            container_path: "/tmp/temps-backup/a/backup.sql.gz".into(),
            host_path: "/data/backups/tmp/x/backup.sql.gz".into(),
            reason: "the Docker archive download failed: 404".into(),
        };
        assert!(unreadable.to_string().starts_with(
            "Control-plane PostgreSQL exported the backup, but Temps could not read the temporary file"
        ));
        assert!(matches!(
            BackupError::from(unreadable),
            BackupError::Failed { .. }
        ));

        let transient_upload = DumpCaptureError::upload(
            CONTROL_PLANE_TOOL,
            "bucket",
            "cp/backup.sql.gz",
            BackupError::Failed {
                reason: "503 Slow Down".into(),
            },
        );
        assert!(transient_upload
            .to_string()
            .contains("uploading it to s3://bucket/cp/backup.sql.gz failed: 503 Slow Down"));
        assert!(matches!(
            BackupError::from(transient_upload),
            BackupError::Failed { .. }
        ));
        let permanent_upload = DumpCaptureError::upload(
            CONTROL_PLANE_TOOL,
            "bucket",
            "key",
            BackupError::PermanentFailure {
                reason: "AccessDenied".into(),
            },
        );
        assert!(matches!(
            BackupError::from(permanent_upload),
            BackupError::PermanentFailure { .. }
        ));
    }

    // ── Docker-backed tests ──────────────────────────────────────────────

    const TARGET_IMAGE: &str = "postgres:18-alpine";
    const SIDECAR_IMAGE: &str = "postgres:18";

    async fn docker_or_skip(test: &str) -> Option<bollard::Docker> {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                println!("Docker not available, skipping {test}: {e}");
                return None;
            }
        };
        if let Err(e) = docker.ping().await {
            println!("Docker daemon not reachable, skipping {test}: {e}");
            return None;
        }
        for image in [TARGET_IMAGE, SIDECAR_IMAGE] {
            if let Err(e) =
                super::super::image_pull::ensure_image_pulled_v2(image, ENGINE_KEY).await
            {
                println!("Could not pull {image}, skipping {test}: {e}");
                return None;
            }
        }
        Some(docker)
    }

    /// Run a command in a container; return its exit code and output.
    async fn exec_in(docker: &bollard::Docker, container: &str, cmd: &[&str]) -> (i64, String) {
        use bollard::exec::{CreateExecOptions, StartExecResults};
        use futures::StreamExt;
        let exec = docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd.iter().map(|part| part.to_string()).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("create exec");
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, None).await.expect("start exec")
        {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&chunk.to_string());
            }
        }
        let code = docker
            .inspect_exec(&exec.id)
            .await
            .expect("inspect exec")
            .exit_code
            .unwrap_or(-1);
        (code, text)
    }

    async fn container_is_gone(docker: &bollard::Docker, name: &str) -> bool {
        for _ in 0..60 {
            match docker
                .inspect_container(
                    name,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => return true,
                _ => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        false
    }

    async fn container_exists(docker: &bollard::Docker, name: &str) -> bool {
        docker
            .inspect_container(
                name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .is_ok()
    }

    /// Containers a test created, force-removed by name on drop (also on
    /// panic) from a dedicated thread with its own runtime.
    struct DockerLeftovers {
        containers: Vec<String>,
    }

    impl Drop for DockerLeftovers {
        fn drop(&mut self) {
            let containers = std::mem::take(&mut self.containers);
            let cleanup = std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
                        return;
                    };
                    for name in containers {
                        let _ = docker
                            .remove_container(
                                &name,
                                Some(bollard::query_parameters::RemoveContainerOptions {
                                    force: true,
                                    v: true,
                                    ..Default::default()
                                }),
                            )
                            .await;
                    }
                });
            });
            let _ = cleanup.join();
        }
    }

    /// A password-protected PostgreSQL standing in for the control plane,
    /// published on the Docker host's loopback so a host-network sidecar
    /// reaches it at `127.0.0.1:<port>` exactly as it reaches the real
    /// control plane under `temps serve`. Seeded with a role, a kept table
    /// and a table whose data the dump excludes.
    struct SeededControlPlane {
        port: u16,
        _leftovers: DockerLeftovers,
    }

    async fn seeded_control_plane(docker: &bollard::Docker, run_id: &str) -> SeededControlPlane {
        use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
        let name = format!("temps-test-cp-pg-{run_id}");
        let leftovers = DockerLeftovers {
            containers: vec![name.clone()],
        };
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(TARGET_IMAGE.to_string()),
                    env: Some(vec![
                        format!("POSTGRES_PASSWORD={TEST_PASSWORD}"),
                        "POSTGRES_DB=temps".to_string(),
                    ]),
                    exposed_ports: Some(vec!["5432/tcp".to_string()]),
                    host_config: Some(HostConfig {
                        port_bindings: Some(std::collections::HashMap::from([(
                            "5432/tcp".to_string(),
                            Some(vec![PortBinding {
                                host_ip: Some("127.0.0.1".to_string()),
                                host_port: Some(String::new()),
                            }]),
                        )])),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create test control-plane postgres");
        docker
            .start_container(
                &name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start test control-plane postgres");

        let inspect = docker
            .inspect_container(
                &name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("inspect test postgres");
        let port = inspect
            .network_settings
            .and_then(|settings| settings.ports)
            .and_then(|ports| ports.get("5432/tcp").cloned().flatten())
            .and_then(|bindings| bindings.into_iter().find_map(|b| b.host_port))
            .and_then(|port| port.parse::<u16>().ok())
            .expect("published port");

        // TCP readiness: the init-time temporary server listens on the unix
        // socket only, so this succeeds once the real server is up.
        let mut ready = false;
        for _ in 0..120 {
            let (code, _) = exec_in(
                docker,
                &name,
                &["pg_isready", "-h", "127.0.0.1", "-U", "postgres"],
            )
            .await;
            if code == 0 {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(ready, "test postgres {name} never became ready");

        let (code, output) = exec_in(
            docker,
            &name,
            &[
                "psql",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
                "-d",
                "temps",
                "-v",
                "ON_ERROR_STOP=1",
                "-c",
                "CREATE ROLE temps_test_reader LOGIN; \
                 CREATE TABLE projects (id int PRIMARY KEY, name text); \
                 INSERT INTO projects VALUES (1, 'kept-project-row'); \
                 CREATE TABLE proxy_logs (id int, path text); \
                 INSERT INTO proxy_logs VALUES (1, '/excluded-telemetry-row');",
            ],
        )
        .await;
        assert_eq!(code, 0, "seeding failed: {output}");

        SeededControlPlane {
            port,
            _leftovers: leftovers,
        }
    }

    fn gunzip_to_string(path: &std::path::Path) -> String {
        use std::io::Read;
        let mut sql = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).expect("open dump"))
            .read_to_string(&mut sql)
            .expect("gunzip dump");
        sql
    }

    fn request(attempt: &DumpAttempt, spec: OneShotSpec) -> CaptureRequest {
        CaptureRequest {
            tool: CONTROL_PLANE_TOOL,
            spec,
            container_path: attempt.container_path(SIDECAR_DUMP_GZ),
            host_path: attempt.host_path(SIDECAR_DUMP_GZ),
            failure_log: Some((
                attempt.container_path(SIDECAR_STDERR),
                attempt.host_path(SIDECAR_STDERR),
            )),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_plane_dump_round_trips_over_host_networking_without_a_bind_mount() {
        let Some(docker) = docker_or_skip(
            "control_plane_dump_round_trips_over_host_networking_without_a_bind_mount",
        )
        .await
        else {
            return;
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let cp = seeded_control_plane(&docker, &run_id).await;

        // The OS temp dir, whatever TMPDIR says: on macOS it is not shared
        // with the Colima / Docker Desktop VM, which the old bind mount
        // needed. Nothing here depends on it being shared.
        let backup_dir = tempfile::tempdir().expect("backup tmpdir");
        println!(
            "control-plane attempt parent dir: {}",
            backup_dir.path().display()
        );
        let attempt = DumpAttempt::new_in(
            backup_dir.path(),
            CONTROL_PLANE_TOOL,
            ENGINE_KEY,
            &format!("roundtrip-{run_id}"),
        )
        .expect("attempt");
        let spec = control_plane_dump_spec(
            &attempt,
            0,
            SIDECAR_IMAGE,
            &target("127.0.0.1", cp.port, TEST_PASSWORD),
            &["public.proxy_logs".to_string()],
        );
        let sidecar = spec.name.clone();
        let _sidecar_leftover = DockerLeftovers {
            containers: vec![sidecar.clone()],
        };

        let size = capture_dump(
            &docker,
            request(&attempt, spec),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("capture the control-plane dump through the Docker API");

        let host_path = attempt.host_path(SIDECAR_DUMP_GZ);
        assert!(size > 0);
        assert_eq!(std::fs::metadata(&host_path).expect("dump").len(), size);
        assert!(
            container_is_gone(&docker, &sidecar).await,
            "sidecar {sidecar} was not removed"
        );

        let sql = gunzip_to_string(&host_path);
        assert!(
            sql.contains("CREATE ROLE temps_test_reader"),
            "globals (roles) missing from the dump"
        );
        assert!(sql.contains("CREATE TABLE public.projects"));
        assert!(sql.contains("kept-project-row"), "kept table data missing");
        assert!(
            sql.contains("CREATE TABLE public.proxy_logs"),
            "excluded tables keep their schema"
        );
        assert!(
            !sql.contains("/excluded-telemetry-row"),
            "excluded table data must not be dumped"
        );
        let globals = sql
            .find("PostgreSQL database cluster dump")
            .expect("pg_dumpall header");
        let database = sql
            .find("CREATE TABLE public.projects")
            .expect("pg_dump body");
        assert!(globals < database, "globals must come first");

        let host_dir = attempt.host_dir().to_path_buf();
        drop(attempt);
        assert!(!host_dir.exists(), "attempt dir not cleaned up");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_plane_retry_after_a_failed_attempt_ignores_its_leftovers() {
        let Some(docker) =
            docker_or_skip("control_plane_retry_after_a_failed_attempt_ignores_its_leftovers")
                .await
        else {
            return;
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let cp = seeded_control_plane(&docker, &run_id).await;
        let backup_uuid = format!("retry-{run_id}");
        let cancel = tokio_util::sync::CancellationToken::new();
        let backup_dir = tempfile::tempdir().expect("backup tmpdir");

        // What an older binary left after a failed run: the per-backup
        // container name and the per-backup files in the shared tmpdir.
        let legacy_container = format!("temps-cp-backup-{backup_uuid}");
        let mut leftovers = DockerLeftovers {
            containers: vec![legacy_container.clone()],
        };
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&legacy_container)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(SIDECAR_IMAGE.to_string()),
                    cmd: Some(vec!["true".to_string()]),
                    ..Default::default()
                },
            )
            .await
            .expect("create legacy leftover container");
        let legacy_dump = backup_dir.path().join(format!("{backup_uuid}.sql.gz"));
        let legacy_stderr = backup_dir.path().join(format!("{backup_uuid}.stderr"));
        std::fs::write(&legacy_dump, b"stale dump").expect("legacy dump");
        std::fs::write(&legacy_stderr, b"stale stderr").expect("legacy stderr");

        // Attempt 1: the database is unreachable (nothing listens on port 1).
        let first = DumpAttempt::new_in(
            backup_dir.path(),
            CONTROL_PLANE_TOOL,
            ENGINE_KEY,
            &backup_uuid,
        )
        .expect("attempt 1");
        let spec = control_plane_dump_spec(
            &first,
            0,
            SIDECAR_IMAGE,
            &target("127.0.0.1", 1, TEST_PASSWORD),
            &[],
        );
        let first_sidecar = spec.name.clone();
        leftovers.containers.push(first_sidecar.clone());
        let error = capture_dump(&docker, request(&first, spec), &cancel)
            .await
            .expect_err("unreachable database");
        match &error {
            DumpCaptureError::Export {
                exit_code, stderr, ..
            } => {
                assert_ne!(*exit_code, 0);
                assert!(
                    stderr.contains("pg_dumpall: error: could not connect"),
                    "the redirected pg_dumpall stderr must reach the error: {stderr}"
                );
            }
            other => panic!("expected an export failure, got {other}"),
        }
        assert!(matches!(
            BackupError::from(error),
            BackupError::Failed { .. }
        ));
        assert!(container_is_gone(&docker, &first_sidecar).await);
        let first_dir = first.host_dir().to_path_buf();
        drop(first);
        assert!(!first_dir.exists(), "failed attempt dir not cleaned up");

        // Attempt 2: same backup, reachable database. Neither the legacy
        // container name nor the legacy files get in the way.
        let second = DumpAttempt::new_in(
            backup_dir.path(),
            CONTROL_PLANE_TOOL,
            ENGINE_KEY,
            &backup_uuid,
        )
        .expect("attempt 2");
        let spec = control_plane_dump_spec(
            &second,
            0,
            SIDECAR_IMAGE,
            &target("127.0.0.1", cp.port, TEST_PASSWORD),
            &[],
        );
        let second_sidecar = spec.name.clone();
        leftovers.containers.push(second_sidecar.clone());
        let size = capture_dump(&docker, request(&second, spec), &cancel)
            .await
            .expect("retry succeeds");
        assert!(size > 0);
        assert!(gunzip_to_string(&second.host_path(SIDECAR_DUMP_GZ)).contains("kept-project-row"));
        assert!(container_is_gone(&docker, &second_sidecar).await);
        drop(second);

        assert!(
            container_exists(&docker, &legacy_container).await,
            "an attempt must not remove a container it did not create"
        );
        assert_eq!(std::fs::read(&legacy_dump).expect("kept"), b"stale dump");
        assert_eq!(
            std::fs::read(&legacy_stderr).expect("kept"),
            b"stale stderr"
        );
    }
}
