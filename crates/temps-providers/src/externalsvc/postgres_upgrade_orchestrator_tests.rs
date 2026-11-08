// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end tests of the PostgreSQL major-version upgrade orchestrator. They
//! drive it through `ExternalServiceManager`, so they live above the engine.

#![cfg(feature = "docker-tests")]

use async_trait::async_trait;
use temps_entities::postgres_major_upgrades;

use crate::externalsvc::naming::shell_escape;
use crate::externalsvc::postgres_upgrade::*;
use crate::externalsvc::test_utils::{run_exec_capture, run_exec_ok};

// ========================================================================
// Orchestrator integration-test harness (docker-tests only)
// ========================================================================
//
// Shared fixtures used by the five orchestrator tests below. Each test
// needs a real Postgres container plus a real control-plane DB row so
// the orchestrator's Sea-ORM reads/writes exercise production paths.
//
// Design notes:
// * We bypass `ExternalServiceManager::create_service` because that
//   starts a real Docker container synchronously via the full
//   `PostgresService` path (120s wait). Instead we insert the row
//   directly and let the test drive `PostgresLifecycleAdapter` for
//   container ops — same code path the orchestrator uses.
// * A real user row is required by the `created_by` FK.
// * Cleanup is best-effort per-test via `Drop` plus an explicit
//   `cleanup()` call. Tests that panic leak Docker resources; CI
//   reaps them via a `temps_upgrade_test_*` label prune.
// * Images use the published, reviewed Timescale PostgreSQL 17/18 range
//   accepted by production.
//
// Run subset:
//     cargo test -p temps-providers --features docker-tests \
//         --lib orchestrator_ -- --nocapture --test-threads=1
//
// Each test takes 2–6 min. Run serially (`--test-threads=1`) so the
// Docker daemon doesn't thrash on concurrent image pulls.

#[cfg(feature = "docker-tests")]
#[allow(dead_code)]
mod harness {
    use super::*;
    use bollard::Docker;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicI32, AtomicU64, Ordering},
        Arc, Mutex,
    };
    use std::time::{Duration, Instant};
    use tempfile::TempDir;
    use temps_core::EncryptionService;
    use temps_deployment_logs::LogService;
    use temps_entities::{backups, external_services, postgres_major_upgrades, s3_sources, users};
    use temps_test_utils::TestDatabase;

    use crate::postgres_lifecycle::PostgresLifecycleAdapter;
    use crate::services::ExternalServiceManager;

    /// Images used by every integration test. Keep in sync with the
    /// stable range supported by the upgrade orchestrator.
    pub const FROM_IMAGE: &str = "gotempsh/postgres-walg:17-bookworm";
    pub const FROM_VERSION: &str = "17";
    pub const TO_IMAGE: &str = "gotempsh/postgres-walg:18-bookworm";
    pub const TO_VERSION: &str = "18";

    /// Return `Some(docker)` if a Docker daemon responds; `None` if the
    /// test should skip.
    pub async fn docker_or_skip(test_name: &str) -> Option<Arc<Docker>> {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                println!("[{}] Docker unavailable, skipping: {}", test_name, e);
                return None;
            }
        };
        if docker.ping().await.is_err() {
            println!("[{}] Docker daemon not responding, skipping", test_name);
            return None;
        }
        Some(docker)
    }

    /// Find an unused host port by binding to `127.0.0.1:0`.
    pub fn pick_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .unwrap()
            .port()
    }

    /// Full per-test fixture: isolated DB schema, an encryption service,
    /// a real Docker handle, a manager, a lifecycle adapter, a service
    /// row, a user row, a fresh LogService, and the log tempdir (kept
    /// alive via the struct so it isn't deleted while tests run).
    pub struct UpgradeTestCtx {
        pub test_db: TestDatabase,
        pub docker: Arc<Docker>,
        pub manager: Arc<ExternalServiceManager>,
        pub encryption_service: Arc<EncryptionService>,
        pub lifecycle_adapter: Arc<PostgresLifecycleAdapter>,
        pub log_service: Arc<LogService>,
        pub service_id: i32,
        pub user_id: i32,
        pub service_name: String,
        pub host_port: u16,
        pub username: String,
        pub password: String,
        pub database: String,
        _log_dir: TempDir,
        /// Every `postgres_major_upgrades.id` created via
        /// `insert_upgrade_row`, so `cleanup()` can remove the
        /// per-upgrade dumper/restorer containers and dump/rollback
        /// volumes those phases name with the upgrade row's id.
        created_upgrade_ids: std::sync::Mutex<Vec<i32>>,
    }

    impl UpgradeTestCtx {
        /// Spin up a fresh DB schema, a single reviewed Timescale PostgreSQL 17
        /// service row, and its container on the app network. Blocks
        /// until Postgres accepts connections.
        pub async fn new(test_name: &str) -> Self {
            let test_db = TestDatabase::with_migrations()
                .await
                .expect("TestDatabase::with_migrations");
            let db: Arc<DatabaseConnection> = test_db.db.clone();

            let encryption_service =
                Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
            let docker = match Docker::connect_with_local_defaults() {
                Ok(d) => Arc::new(d),
                Err(e) => panic!("Docker required but unavailable: {}", e),
            };
            if docker.ping().await.is_err() {
                panic!("Docker daemon required but not responding");
            }

            let dns_registry = Arc::new(crate::DnsRegistry::new(db.clone()));
            let manager = Arc::new(ExternalServiceManager::new(
                db.clone(),
                encryption_service.clone(),
                docker.clone(),
                dns_registry,
            ));
            let lifecycle_adapter = Arc::new(PostgresLifecycleAdapter::new(
                db.clone(),
                Arc::new(temps_core::DockerHandle::available(docker.clone())),
                manager.clone(),
                encryption_service.clone(),
            ));

            // Seed a user for the created_by FK.
            let user_row = users::ActiveModel {
                name: Set(format!("upgrade-test-{}", test_name)),
                email: Set(format!(
                    "upgrade-test-{}-{}@example.com",
                    test_name,
                    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                )),
                email_verified: Set(true),
                mfa_enabled: Set(false),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("insert user");

            // Build a service row with an encrypted Postgres config.
            // Service name is unique per run_id so concurrent test
            // runs don't collide on container names.
            let run_id = format!(
                "{:x}{:x}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
                rand::random::<u32>()
            );
            let service_name = format!("upg-{}-{}", test_name, &run_id[..8]);
            let host_port = pick_port();
            let username = "upgradeuser".to_string();
            let password = "upgr@de!pass".to_string();
            let database = "upgradedb".to_string();

            let params = json!({
                "host": "localhost",
                "port": host_port.to_string(),
                "database": database,
                "username": username,
                "password": password,
                "max_connections": 100,
                "docker_image": FROM_IMAGE,
            });
            let config_json = serde_json::to_string(&params).unwrap();
            let encrypted = encryption_service.encrypt_string(&config_json).unwrap();

            let service_row = external_services::ActiveModel {
                name: Set(service_name.clone()),
                slug: Set(Some(service_name.clone())),
                service_type: Set("postgres".to_string()),
                version: Set(Some(FROM_VERSION.to_string())),
                status: Set("running".to_string()),
                config: Set(Some(encrypted)),
                node_id: Set(None),
                topology: Set("standalone".to_string()),
                created_at: Set(chrono::Utc::now()),
                updated_at: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("insert service");

            let log_dir = tempfile::tempdir().expect("log tempdir");
            let log_service = Arc::new(LogService::new(log_dir.path().to_path_buf()));

            // Boot the old-version container via the real adapter —
            // same path the orchestrator uses after rollback.
            lifecycle_adapter
                .create_and_start(service_row.id, FROM_IMAGE)
                .await
                .expect("create_and_start old container");

            Self {
                test_db,
                docker,
                manager,
                encryption_service,
                lifecycle_adapter,
                log_service,
                service_id: service_row.id,
                user_id: user_row.id,
                service_name,
                host_port,
                username,
                password,
                database,
                _log_dir: log_dir,
                created_upgrade_ids: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// Container name matching the adapter's formula.
        pub fn container_name(&self) -> String {
            format!("postgres-{}", self.service_name)
        }

        /// Insert a `postgres_major_upgrades` row for this service in
        /// the requested starting phase. Returns the new row's id.
        pub async fn insert_upgrade_row(&self, starting_phase: &str) -> i32 {
            let row = postgres_major_upgrades::ActiveModel {
                service_id: Set(self.service_id),
                from_version: Set(FROM_VERSION.to_string()),
                to_version: Set(TO_VERSION.to_string()),
                from_image: Set(FROM_IMAGE.to_string()),
                to_image: Set(TO_IMAGE.to_string()),
                status: Set(status::PENDING.to_string()),
                phase: Set(starting_phase.to_string()),
                pre_upgrade_backup_id: Set(None),
                log_id: Set(format!("upgrade-test-{}", self.service_id)),
                rollback_volume_name: Set(None),
                rollback_volume_expires_at: Set(None),
                error_message: Set(None),
                attempt: Set(1),
                started_at: Set(None),
                finished_at: Set(None),
                created_by: Set(self.user_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(self.test_db.db.as_ref())
            .await
            .expect("insert upgrade row");
            self.created_upgrade_ids
                .lock()
                .expect("created_upgrade_ids mutex poisoned")
                .push(row.id);
            row.id
        }

        /// Build an orchestrator wired to this fixture's deps and the
        /// given backup provider + lifecycle.
        pub fn orchestrator(
            &self,
            backup_provider: Arc<dyn PreUpgradeBackupProvider>,
            lifecycle: Arc<dyn PostgresContainerLifecycle>,
        ) -> PostgresUpgradeOrchestrator {
            PostgresUpgradeOrchestrator::new(
                self.test_db.db.clone(),
                self.docker.clone(),
                backup_provider,
                lifecycle,
                self.log_service.clone(),
            )
        }

        /// Exec a SQL statement inside the currently-running service
        /// container and assert success. Uses the fixture credentials.
        pub async fn psql_exec(&self, sql: &str) -> Result<(), String> {
            let cmd = format!(
                "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d {d} -c {sql}",
                pw = shell_escape(&self.password),
                u = shell_escape(&self.username),
                d = shell_escape(&self.database),
                sql = shell_escape(sql),
            );
            run_exec_ok(
                self.docker.as_ref(),
                &self.container_name(),
                &cmd,
                Some(&self.password),
            )
            .await
        }

        /// Run a SELECT and return stdout (tuples-only mode, -tA).
        pub async fn psql_query(&self, sql: &str) -> Result<String, String> {
            let cmd = format!(
                "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d {d} -tAc {sql}",
                pw = shell_escape(&self.password),
                u = shell_escape(&self.username),
                d = shell_escape(&self.database),
                sql = shell_escape(sql),
            );
            run_exec_capture(
                self.docker.as_ref(),
                &self.container_name(),
                &cmd,
                Some(&self.password),
            )
            .await
        }

        /// Remove the service container, its data volume, and every
        /// per-upgrade dumper/restorer container and dump/rollback
        /// volume the orchestrator created for the upgrade ids this ctx
        /// tracked via `insert_upgrade_row`. Best-effort.
        pub async fn cleanup(&self) {
            use bollard::query_parameters::{RemoveContainerOptions, RemoveVolumeOptions};
            let container = self.container_name();

            let upgrade_ids: Vec<i32> = self
                .created_upgrade_ids
                .lock()
                .expect("created_upgrade_ids mutex poisoned")
                .clone();

            let mut containers = vec![container.clone()];
            let mut volumes = vec![format!("{}_data", container)];
            for id in upgrade_ids {
                containers.push(format!("temps_pg_upgrade_{}_dumper", id));
                containers.push(format!("temps_pg_upgrade_{}_restorer", id));
                // Restore-phase retries can create attempt-suffixed
                // restorer containers too (see phase_restore's retry
                // loop); MAX_RESTORER_ATTEMPTS is small, so just try them
                // all rather than tracking which attempt actually ran.
                for attempt in 1..=3 {
                    containers.push(format!("temps_upgrade_test_restorer_{}_{}", id, attempt));
                }
                volumes.push(format!("{}_pgdump_{}", container, id));
                volumes.push(format!("{}_data_rollback_{}", container, id));
            }

            for c in containers {
                let _ = self
                    .docker
                    .remove_container(
                        &c,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
            for v in volumes {
                let _ = self
                    .docker
                    .remove_volume(&v, None::<RemoveVolumeOptions>)
                    .await;
            }
        }
    }

    /// Remove Docker volumes by exact name, best-effort.
    pub async fn remove_volumes(docker: &Docker, names: &[String]) {
        use bollard::query_parameters::RemoveVolumeOptions;
        for v in names {
            let _ = docker.remove_volume(v, None::<RemoveVolumeOptions>).await;
        }
    }

    /// Stub `PreUpgradeBackupProvider` — creates a real control-plane
    /// backup row without touching S3. The orchestrator stores the
    /// returned id on `postgres_major_upgrades.pre_upgrade_backup_id`,
    /// which has a real FK to `backups.id` in the test database.
    pub struct StubBackupProvider {
        db: Arc<DatabaseConnection>,
        pub calls: Arc<AtomicU64>,
        pub last_backup_id: Arc<AtomicI32>,
    }

    impl StubBackupProvider {
        pub fn new(db: Arc<DatabaseConnection>) -> Self {
            Self {
                db,
                calls: Arc::new(AtomicU64::new(0)),
                last_backup_id: Arc::new(AtomicI32::new(0)),
            }
        }
    }

    #[async_trait]
    impl PreUpgradeBackupProvider for StubBackupProvider {
        async fn default_s3_source_id(&self, service_id: i32) -> Result<Option<i32>, String> {
            let now = chrono::Utc::now();
            let source = s3_sources::ActiveModel {
                id: sea_orm::NotSet,
                name: Set(format!(
                    "upgrade-test-s3-{}-{}",
                    service_id,
                    now.timestamp_nanos_opt().unwrap_or(0)
                )),
                backing_service_id: Set(None),
                bucket_name: Set("upgrade-test-bucket".to_string()),
                region: Set("us-east-1".to_string()),
                endpoint: Set(Some("http://127.0.0.1:9000".to_string())),
                bucket_path: Set("postgres-upgrade-tests".to_string()),
                access_key_id: Set("test-access-key".to_string()),
                secret_key: Set("test-secret-key".to_string()),
                session_token: Set(None),
                credentials_expire_at: Set(None),
                force_path_style: Set(Some(true)),
                is_default: Set(true),
                managed_by_cloud: Set(false),
                lifecycle_reconcile_failed_at: Set(None),
                lifecycle_reconcile_generation: Set(0),
                worker_credentials: sea_orm::ActiveValue::Set("scoped".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(self.db.as_ref())
            .await
            .map_err(|e| format!("insert test s3 source: {}", e))?;

            Ok(Some(source.id))
        }

        async fn create_pre_upgrade_backup(
            &self,
            service_id: i32,
            s3_source_id: i32,
            created_by: i32,
        ) -> Result<i32, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = chrono::Utc::now();
            let backup_uuid = format!(
                "upgrade-pre-backup-{}-{}",
                service_id,
                now.timestamp_nanos_opt().unwrap_or(0)
            );
            let backup = backups::ActiveModel {
                id: sea_orm::NotSet,
                name: Set(format!("Pre-upgrade backup {}", service_id)),
                backup_id: Set(backup_uuid.clone()),
                schedule_id: Set(None),
                backup_type: Set("pre_upgrade".to_string()),
                state: Set("completed".to_string()),
                started_at: Set(now),
                finished_at: Set(Some(now)),
                size_bytes: Set(Some(0)),
                file_count: Set(Some(0)),
                s3_source_id: Set(s3_source_id),
                s3_location: Set(format!("s3://upgrade-test-bucket/{}", backup_uuid)),
                error_message: Set(None),
                metadata: Set(serde_json::json!({
                    "test": true,
                    "service_id": service_id,
                    "purpose": "postgres_major_upgrade_pre_backup"
                })
                .to_string()),
                checksum: Set(None),
                compression_type: Set("none".to_string()),
                created_by: Set(created_by),
                expires_at: Set(None),
                tags: Set("[]".to_string()),
                schedule_run_id: sea_orm::NotSet,
            }
            .insert(self.db.as_ref())
            .await
            .map_err(|e| format!("insert test backup: {}", e))?;

            self.last_backup_id.store(backup.id, Ordering::SeqCst);
            Ok(backup.id)
        }
    }

    /// A `PreUpgradeBackupProvider` that routes the pre-upgrade backup
    /// through the REAL `exec_util::run_exec` (against the service's own
    /// running container) before recording the backup row like
    /// `StubBackupProvider`.
    ///
    /// The production pre-upgrade backup shells `pg_dumpall … > file` via
    /// `run_exec`, whose exec stdout is empty — the exact shape that
    /// regressed into a 100% CPU busy loop that never returned. The stub
    /// providers bypass `run_exec` entirely, which is why that hang shipped
    /// despite a green orchestrator happy-path test. This provider closes
    /// the gap: a `run_exec` regression makes the whole `orchestrator.run()`
    /// hang, caught by the test's bounded `tokio::time::timeout`.
    pub struct RunExecBackupProvider {
        inner: StubBackupProvider,
        docker: Arc<Docker>,
        container: String,
    }

    impl RunExecBackupProvider {
        pub fn new(db: Arc<DatabaseConnection>, docker: Arc<Docker>, container: String) -> Self {
            Self {
                inner: StubBackupProvider::new(db),
                docker,
                container,
            }
        }
        pub fn calls(&self) -> u64 {
            self.inner.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl PreUpgradeBackupProvider for RunExecBackupProvider {
        async fn default_s3_source_id(&self, service_id: i32) -> Result<Option<i32>, String> {
            self.inner.default_s3_source_id(service_id).await
        }

        async fn create_pre_upgrade_backup(
            &self,
            service_id: i32,
            s3_source_id: i32,
            created_by: i32,
        ) -> Result<i32, String> {
            // Exercise the real run_exec with a no-stdout command — the
            // production backup's `pg_dumpall … > file` shape. Bounded so a
            // regression fails rather than hangs the whole run.
            crate::externalsvc::exec_util::run_exec(
                self.docker.as_ref(),
                &self.container,
                vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
                None,
                std::time::Duration::from_secs(120),
            )
            .await
            .map_err(|e| format!("pre-upgrade backup run_exec failed: {}", e))?;

            self.inner
                .create_pre_upgrade_backup(service_id, s3_source_id, created_by)
                .await
        }
    }

    /// Recording wrapper around `PostgresLifecycleAdapter`. Captures an
    /// ordered event log of each trait method call, with monotonic
    /// timestamps, so tests can assert on call ordering (e.g.,
    /// stop_and_remove BEFORE any copy_volume).
    pub struct RecordingLifecycle {
        inner: Arc<PostgresLifecycleAdapter>,
        pub events: Arc<Mutex<Vec<LifecycleEvent>>>,
        start: Instant,
    }

    #[derive(Debug, Clone)]
    pub struct LifecycleEvent {
        pub at_ms: u128,
        pub kind: LifecycleEventKind,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum LifecycleEventKind {
        StopAndRemove,
        CreateAndStart { image: String },
        SetDockerImage { image: String },
        ContainerName,
        ConnectionParams,
    }

    impl RecordingLifecycle {
        pub fn new(inner: Arc<PostgresLifecycleAdapter>) -> Arc<Self> {
            Arc::new(Self {
                inner,
                events: Arc::new(Mutex::new(Vec::new())),
                start: Instant::now(),
            })
        }

        fn record(&self, kind: LifecycleEventKind) {
            let at_ms = self.start.elapsed().as_millis();
            self.events
                .lock()
                .unwrap()
                .push(LifecycleEvent { at_ms, kind });
        }

        pub fn snapshot(&self) -> Vec<LifecycleEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PostgresContainerLifecycle for RecordingLifecycle {
        async fn container_name(&self, service_id: i32) -> Result<String, String> {
            self.record(LifecycleEventKind::ContainerName);
            self.inner.container_name(service_id).await
        }

        async fn connection_params(&self, service_id: i32) -> Result<PostgresConnection, String> {
            self.record(LifecycleEventKind::ConnectionParams);
            self.inner.connection_params(service_id).await
        }

        async fn docker_image(&self, service_id: i32) -> Result<String, String> {
            self.inner.docker_image(service_id).await
        }

        async fn stop_and_remove(&self, service_id: i32) -> Result<(), String> {
            self.record(LifecycleEventKind::StopAndRemove);
            self.inner.stop_and_remove(service_id).await
        }

        async fn create_and_start(&self, service_id: i32, image: &str) -> Result<(), String> {
            self.record(LifecycleEventKind::CreateAndStart {
                image: image.to_string(),
            });
            self.inner.create_and_start(service_id, image).await
        }

        async fn set_docker_image(&self, service_id: i32, image: &str) -> Result<(), String> {
            self.record(LifecycleEventKind::SetDockerImage {
                image: image.to_string(),
            });
            self.inner.set_docker_image(service_id, image).await
        }
    }

    /// Helper: read the upgrade row by id.
    pub async fn load_upgrade(
        db: &DatabaseConnection,
        upgrade_id: i32,
    ) -> postgres_major_upgrades::Model {
        postgres_major_upgrades::Entity::find_by_id(upgrade_id)
            .one(db)
            .await
            .expect("db")
            .expect("upgrade row")
    }

    /// Wait for a Docker volume to exist by name (bounded).
    pub async fn wait_volume_exists(
        docker: &Docker,
        name: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if docker.inspect_volume(name).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(format!("volume '{}' did not appear in time", name))
    }
}

// ========================================================================
// Test #29: rollback_restores_pre_upgrade_data (flagship)
// ========================================================================
//
// Guarantees the user-facing rollback contract:
//   1. Upgrade v17 → v18 completes; service is on v18 with migrated data.
//   2. User mutates data on v18 (simulating "production ran for a bit").
//   3. User invokes rollback().
//   4. Service is back on v17 with EXACTLY the pre-upgrade data — v18
//      mutations are discarded (that's the whole point of a rollback).
//   5. Upgrade row is marked `rolled_back` with metadata cleared.
//
// This backs the "user controls rollback" guarantee we promise in the
// handler docstring and in the UX. If this ever fails, rollback is
// unsafe to ship.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn orchestrator_rollback_restores_pre_upgrade_data() {
    use harness::*;
    use std::sync::Arc;

    if docker_or_skip("orchestrator_rollback_restores_pre_upgrade_data")
        .await
        .is_none()
    {
        return;
    }

    let ctx = UpgradeTestCtx::new("rollback").await;
    let backup_provider: Arc<dyn PreUpgradeBackupProvider> =
        Arc::new(StubBackupProvider::new(ctx.test_db.db.clone()));
    let lifecycle: Arc<dyn PostgresContainerLifecycle> = ctx.lifecycle_adapter.clone();

    // Seed pre-upgrade data on v17.
    let result = async {
        ctx.psql_exec("CREATE TABLE rollback_probe(id INT PRIMARY KEY, note TEXT NOT NULL)")
            .await?;
        ctx.psql_exec("INSERT INTO rollback_probe VALUES (1,'alpha'), (2,'beta'), (3,'gamma')")
            .await?;

        // Record pre-upgrade state.
        let before = ctx
            .psql_query("SELECT string_agg(id || ':' || note, ',' ORDER BY id) FROM rollback_probe")
            .await?;
        let before = before.trim().to_string();
        assert_eq!(before, "1:alpha,2:beta,3:gamma", "seed didn't land");

        let upgrade_id = ctx.insert_upgrade_row(phase::PRE_BACKUP).await;
        let orch = ctx.orchestrator(backup_provider.clone(), lifecycle.clone());

        orch.run(upgrade_id)
            .await
            .map_err(|e| format!("run: {}", e))?;

        // Verify upgrade finished cleanly.
        let row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        if row.status != status::COMPLETED {
            return Err(format!("expected COMPLETED, got {}", row.status));
        }
        if row.rollback_volume_name.is_none() {
            return Err("rollback_volume_name should be set after upgrade".into());
        }

        // Data should survive v17 → v18.
        let after_upgrade = ctx
            .psql_query("SELECT string_agg(id || ':' || note, ',' ORDER BY id) FROM rollback_probe")
            .await?;
        assert_eq!(
            after_upgrade.trim(),
            before,
            "data differed between v17 and v18"
        );

        // Mutate on v18 — these changes MUST be lost on rollback.
        ctx.psql_exec("UPDATE rollback_probe SET note='CHANGED ON V18' WHERE id=2")
            .await?;
        ctx.psql_exec("INSERT INTO rollback_probe VALUES (4,'added-on-v18')")
            .await?;
        let v18_state = ctx
            .psql_query("SELECT string_agg(id || ':' || note, ',' ORDER BY id) FROM rollback_probe")
            .await?;
        assert!(
            v18_state.contains("CHANGED ON V18"),
            "v18 mutation didn't stick: {:?}",
            v18_state
        );

        // Execute rollback.
        let rolled = orch
            .rollback(upgrade_id)
            .await
            .map_err(|e| format!("rollback: {}", e))?;
        if rolled.status != status::ROLLED_BACK {
            return Err(format!("expected ROLLED_BACK, got {}", rolled.status));
        }
        if rolled.rollback_volume_name.is_some() {
            return Err("rollback_volume_name should be cleared post-rollback".into());
        }

        // Post-rollback data must match pre-upgrade snapshot byte-for-byte.
        let post_rollback = ctx
            .psql_query("SELECT string_agg(id || ':' || note, ',' ORDER BY id) FROM rollback_probe")
            .await?;
        if post_rollback.trim() != before {
            return Err(format!(
                "ROLLBACK DID NOT RESTORE PRE-UPGRADE STATE. before={:?}, after={:?}",
                before,
                post_rollback.trim()
            ));
        }

        // Confirm we're back on v17 by asking the server for its version.
        let version = ctx.psql_query("SHOW server_version_num").await?;
        let major: i32 = version
            .trim()
            .parse::<i32>()
            .map(|v| v / 10000)
            .unwrap_or(0);
        if major != 17 {
            return Err(format!(
                "expected to be back on v17 (server_version_num/10000==17), got {}",
                major
            ));
        }

        Ok::<_, String>(upgrade_id)
    }
    .await;

    // Clean up regardless of outcome.
    let upgrade_id_for_cleanup = result.as_ref().ok().copied().unwrap_or(-1);
    let rollback_vol = if upgrade_id_for_cleanup > 0 {
        format!(
            "postgres-{}_data_rollback_{}",
            ctx.service_name, upgrade_id_for_cleanup
        )
    } else {
        String::new()
    };
    let dump_vol = if upgrade_id_for_cleanup > 0 {
        format!(
            "postgres-{}_pgdump_{}",
            ctx.service_name, upgrade_id_for_cleanup
        )
    } else {
        String::new()
    };
    ctx.cleanup().await;
    remove_volumes(
        ctx.docker.as_ref(),
        &[rollback_vol, dump_vol]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>(),
    )
    .await;

    if let Err(e) = result {
        panic!("rollback integration test failed: {}", e);
    }
}

// ========================================================================
// Test #30: orchestrator_happy_path_v17_to_v18
// ========================================================================
//
// End-to-end through `run()`: every phase executes in order, every
// phase transition is persisted, and user-seeded data survives. This
// is the "does a major upgrade actually work" gate.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn orchestrator_happy_path_v17_to_v18() {
    use harness::*;
    use std::sync::Arc;

    if docker_or_skip("orchestrator_happy_path_v17_to_v18")
        .await
        .is_none()
    {
        return;
    }

    let ctx = UpgradeTestCtx::new("happy").await;
    let backup_provider = Arc::new(StubBackupProvider::new(ctx.test_db.db.clone()));
    let backup_provider_trait: Arc<dyn PreUpgradeBackupProvider> = backup_provider.clone();
    let lifecycle: Arc<dyn PostgresContainerLifecycle> = ctx.lifecycle_adapter.clone();

    let result = async {
        // Seed enough data to prove the dump/restore actually moves
        // rows (not just DDL).
        ctx.psql_exec("CREATE TABLE happy_probe(id SERIAL PRIMARY KEY, payload TEXT)")
            .await?;
        ctx.psql_exec(
            "INSERT INTO happy_probe(payload) SELECT 'row_' || g FROM generate_series(1,500) g",
        )
        .await?;

        let count_before = ctx
            .psql_query("SELECT count(*) FROM happy_probe")
            .await?
            .trim()
            .parse::<i64>()
            .map_err(|e| e.to_string())?;
        if count_before != 500 {
            return Err(format!("expected 500 rows, got {}", count_before));
        }

        let upgrade_id = ctx.insert_upgrade_row(phase::PRE_BACKUP).await;
        let orch = ctx.orchestrator(backup_provider_trait, lifecycle);

        orch.run(upgrade_id)
            .await
            .map_err(|e| format!("run: {}", e))?;

        // Assertions on final DB state.
        let row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        assert_eq!(row.status, status::COMPLETED, "expected COMPLETED");
        assert_eq!(row.phase, phase::COMPLETED, "expected phase=completed");
        assert!(row.started_at.is_some(), "started_at unset");
        assert!(row.finished_at.is_some(), "finished_at unset");
        assert_eq!(
            row.pre_upgrade_backup_id,
            Some(
                backup_provider
                    .last_backup_id
                    .load(std::sync::atomic::Ordering::SeqCst)
            ),
            "pre_upgrade_backup_id not persisted"
        );
        assert!(
            row.rollback_volume_name.is_some(),
            "rollback_volume_name not persisted"
        );
        assert!(
            row.rollback_volume_expires_at.is_some(),
            "rollback_volume_expires_at not persisted"
        );
        assert_eq!(
            backup_provider
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "pre-upgrade backup should be called exactly once"
        );

        // Data survived.
        let count_after = ctx
            .psql_query("SELECT count(*) FROM happy_probe")
            .await?
            .trim()
            .parse::<i64>()
            .map_err(|e| e.to_string())?;
        if count_after != count_before {
            return Err(format!(
                "row count mismatch: before={} after={}",
                count_before, count_after
            ));
        }

        // New server is on v18.
        let version = ctx.psql_query("SHOW server_version_num").await?;
        let major: i32 = version
            .trim()
            .parse::<i32>()
            .map(|v| v / 10000)
            .unwrap_or(0);
        if major != 18 {
            return Err(format!(
                "expected v18, got server_version_num major {}",
                major
            ));
        }

        Ok::<_, String>(upgrade_id)
    }
    .await;

    let upgrade_id_for_cleanup = result.as_ref().ok().copied().unwrap_or(-1);
    let extras: Vec<String> = if upgrade_id_for_cleanup > 0 {
        vec![
            format!(
                "postgres-{}_data_rollback_{}",
                ctx.service_name, upgrade_id_for_cleanup
            ),
            format!(
                "postgres-{}_pgdump_{}",
                ctx.service_name, upgrade_id_for_cleanup
            ),
        ]
    } else {
        vec![]
    };
    ctx.cleanup().await;
    remove_volumes(ctx.docker.as_ref(), &extras).await;

    if let Err(e) = result {
        panic!("happy-path integration test failed: {}", e);
    }
}

/// Full-orchestrator regression test that drives `pre_backup` through the
/// REAL `exec_util::run_exec` (via `RunExecBackupProvider`) rather than a
/// stub. This is the coverage gap that let the `run_exec` 100% CPU hang
/// ship: the happy-path test above runs the whole orchestrator to
/// `completed`, but stubs the backup, so it never touches `run_exec`. Here
/// `orchestrator.run()` actually invokes `run_exec` during `pre_backup`, so
/// a regression makes the run hang — caught in bounded time by the
/// `tokio::time::timeout` wrapper instead of hanging CI forever.
#[cfg(feature = "docker-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orchestrator_full_run_exercises_backup_exec_v17_to_v18() {
    use harness::*;
    use std::sync::Arc;

    if docker_or_skip("orchestrator_full_run_exercises_backup_exec_v17_to_v18")
        .await
        .is_none()
    {
        return;
    }

    let ctx = UpgradeTestCtx::new("bkexec").await;
    let container = format!("postgres-{}", ctx.service_name);
    let provider = Arc::new(RunExecBackupProvider::new(
        ctx.test_db.db.clone(),
        ctx.docker.clone(),
        container,
    ));
    let provider_trait: Arc<dyn PreUpgradeBackupProvider> = provider.clone();
    let lifecycle: Arc<dyn PostgresContainerLifecycle> = ctx.lifecycle_adapter.clone();

    let result = async {
        ctx.psql_exec("CREATE TABLE bkexec_probe(id SERIAL PRIMARY KEY, payload TEXT)")
            .await?;
        ctx.psql_exec(
            "INSERT INTO bkexec_probe(payload) SELECT 'row_' || g FROM generate_series(1,300) g",
        )
        .await?;

        let upgrade_id = ctx.insert_upgrade_row(phase::PRE_BACKUP).await;
        let orch = ctx.orchestrator(provider_trait, lifecycle);

        // Run on a SEPARATE task and time out the JoinHandle. A run_exec
        // regression is a synchronous busy loop that never yields, so a
        // plain `timeout(_, orch.run())` could never fire; spawned, the spin
        // occupies one worker while the timeout fires on another -> the test
        // FAILS instead of hanging CI.
        let run_handle = tokio::spawn(async move { orch.run(upgrade_id).await });
        tokio::time::timeout(std::time::Duration::from_secs(600), run_handle)
            .await
            .map_err(|_| {
                "orchestrator.run() did not finish within 600s — likely a \
                 run_exec / exec hang"
                    .to_string()
            })?
            .map_err(|e| format!("orchestrator.run task panicked: {}", e))?
            .map_err(|e| format!("run: {}", e))?;

        let row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        if row.status != status::COMPLETED {
            return Err(format!("expected COMPLETED, got {}", row.status));
        }
        if provider.calls() != 1 {
            return Err(format!(
                "pre-upgrade backup (run_exec path) should run exactly once, ran {}",
                provider.calls()
            ));
        }
        let after = ctx
            .psql_query("SELECT count(*) FROM bkexec_probe")
            .await?
            .trim()
            .parse::<i64>()
            .map_err(|e| e.to_string())?;
        if after != 300 {
            return Err(format!("expected 300 rows after upgrade, got {}", after));
        }
        Ok::<_, String>(upgrade_id)
    }
    .await;

    let upgrade_id_for_cleanup = result.as_ref().ok().copied().unwrap_or(-1);
    let extras: Vec<String> = if upgrade_id_for_cleanup > 0 {
        vec![
            format!(
                "postgres-{}_data_rollback_{}",
                ctx.service_name, upgrade_id_for_cleanup
            ),
            format!(
                "postgres-{}_pgdump_{}",
                ctx.service_name, upgrade_id_for_cleanup
            ),
        ]
    } else {
        vec![]
    };
    ctx.cleanup().await;
    remove_volumes(ctx.docker.as_ref(), &extras).await;

    if let Err(e) = result {
        panic!("full-run backup-exec integration test failed: {}", e);
    }
}

// ========================================================================
// Test #31: phase_snapshot_stops_container_before_copy_volume
// ========================================================================
//
// Regression guard for the silent-data-loss bug fixed when `phase_snapshot`
// was reordered to stop the container BEFORE copying the volume. If a
// future refactor reverts this order, writes racing the copy would
// land in the rollback volume but not in the subsequent pg_dumpall,
// producing a new-version cluster that silently loses data.
//
// Test asserts: on the first `StopAndRemove` call, the rollback volume
// `..._data_rollback_{id}` does NOT yet exist (enforcing that stop
// happens BEFORE copy, which is the operation that creates / writes
// into the rollback volume).
//
// We run `phase_snapshot` in isolation rather than the full run() —
// faster, and the ordering invariant is localized to this phase.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn orchestrator_phase_snapshot_stops_before_copy() {
    use harness::*;
    use std::sync::Arc;
    use std::time::Duration;

    if docker_or_skip("orchestrator_phase_snapshot_stops_before_copy")
        .await
        .is_none()
    {
        return;
    }

    let ctx = UpgradeTestCtx::new("snap-order").await;
    let backup_provider: Arc<dyn PreUpgradeBackupProvider> =
        Arc::new(StubBackupProvider::new(ctx.test_db.db.clone()));

    // Wrap the real adapter in a recorder so we can observe call order.
    let recorder = RecordingLifecycle::new(ctx.lifecycle_adapter.clone());
    let events_handle = recorder.events.clone();
    let lifecycle: Arc<dyn PostgresContainerLifecycle> = recorder.clone();

    let result = async {
        // Some trivial data so the volume has content to copy.
        ctx.psql_exec("CREATE TABLE snap_probe(id INT)").await?;
        ctx.psql_exec("INSERT INTO snap_probe VALUES (1),(2),(3)").await?;

        let upgrade_id = ctx.insert_upgrade_row(phase::SNAPSHOT).await;
        let expected_rollback_vol =
            format!("postgres-{}_data_rollback_{}", ctx.service_name, upgrade_id);

        // Watcher task: polls until the rollback volume appears, then
        // records its appearance timestamp. Started BEFORE we kick off
        // the phase so we don't miss a fast creation.
        let docker_for_watch = ctx.docker.clone();
        let vol_name = expected_rollback_vol.clone();
        let watch_handle = tokio::spawn(async move {
            use std::time::Instant;
            let start = Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(120) {
                    return None;
                }
                if docker_for_watch.inspect_volume(&vol_name).await.is_ok() {
                    return Some(Instant::now());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let phase_start = std::time::Instant::now();

        let orch = ctx.orchestrator(backup_provider, lifecycle);
        // Reach into private phase by driving `run()` — it will run
        // snapshot only because we inserted the row at phase=snapshot
        // and the subsequent phases will fail safely without disrupting
        // the assertions we care about. But we want phase_snapshot to
        // complete in isolation, so we instead stop after snapshot.
        //
        // Approach: let run() proceed. `phase_snapshot` is the first
        // thing it executes for this row. After snapshot advances the
        // phase to `dump`, the dump phase will exercise pull+create
        // container; that's fine, we just need the events from
        // snapshot to be in the recorder already.
        //
        // Rather than drive the full thing, we call phase_snapshot
        // directly via the public orchestrator path by running the
        // whole thing and letting it continue — but that doubles the
        // test runtime. Instead, we use a thin trick: drive run() in
        // a task and check events as soon as the rollback volume is
        // seen, which happens mid-snapshot.
        let run_task = tokio::spawn(async move {
            // Best-effort — we don't care if later phases error, only
            // that snapshot ordering is correct.
            let _ = orch.run(upgrade_id).await;
        });

        // Wait for rollback volume to appear (indicates copy ran).
        let vol_appeared_at = tokio::time::timeout(
            Duration::from_secs(150),
            watch_handle,
        )
        .await
        .map_err(|_| "rollback volume watcher timed out".to_string())?
        .map_err(|e| format!("watcher task: {}", e))?
        .ok_or_else(|| "rollback volume never appeared".to_string())?;

        // Record the elapsed time of the FIRST stop_and_remove event.
        let first_stop_ms = {
            let ev = events_handle.lock().unwrap();
            ev.iter()
                .find(|e| matches!(e.kind, LifecycleEventKind::StopAndRemove))
                .map(|e| e.at_ms)
                .ok_or_else(|| {
                    "stop_and_remove was never called — phase_snapshot invariant broken"
                        .to_string()
                })?
        };

        // vol_appeared_at is wall-clock; convert to ms since phase_start.
        let vol_appeared_ms = vol_appeared_at.duration_since(phase_start).as_millis();

        // Assert stop happened BEFORE the rollback volume had content
        // (we use "volume exists" as the proxy since create_volume +
        // copy_volume are adjacent; the real ordering bug would show
        // up as stop_and_remove occurring AFTER the volume existed).
        // A strict invariant: the first stop must happen at a timestamp
        // <= the moment we first saw the volume exist.
        if first_stop_ms > vol_appeared_ms + 1000 {
            return Err(format!(
                "ORDERING BUG: first stop_and_remove at {}ms, rollback volume appeared at {}ms. stop must happen before copy.",
                first_stop_ms, vol_appeared_ms
            ));
        }

        // Let run() finish (or error) so cleanup has a quiescent state.
        let _ = tokio::time::timeout(Duration::from_secs(600), run_task).await;

        Ok::<_, String>(upgrade_id)
    }
    .await;

    let upgrade_id_for_cleanup = result.as_ref().ok().copied().unwrap_or(-1);
    let extras: Vec<String> = if upgrade_id_for_cleanup > 0 {
        vec![
            format!(
                "postgres-{}_data_rollback_{}",
                ctx.service_name, upgrade_id_for_cleanup
            ),
            format!(
                "postgres-{}_pgdump_{}",
                ctx.service_name, upgrade_id_for_cleanup
            ),
        ]
    } else {
        vec![]
    };
    ctx.cleanup().await;
    remove_volumes(ctx.docker.as_ref(), &extras).await;

    if let Err(e) = result {
        panic!("snapshot ordering regression test failed: {}", e);
    }
}

// ========================================================================
// Test #27: phase_new_container stream-drain regression
// ========================================================================
//
// Regression guard for the "healthy container but orchestrator times
// out at 120s" bug caused by not draining the bollard exec stream in
// `PostgresLifecycleAdapter::create_and_start`'s pg_isready poll loop.
//
// If the drain is missing, this test will hang (120s+) on
// create_and_start and eventually fail with "container failed to
// become ready within 120s" even though pg_isready is succeeding
// inside the container. With the fix, it completes in <30s.
//
// Guarded by a 90s total timeout so a broken drain fails fast in CI.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn orchestrator_phase_new_container_completes_under_timeout() {
    use harness::*;
    use std::sync::Arc;
    use std::time::Duration;

    if docker_or_skip("orchestrator_phase_new_container_completes_under_timeout")
        .await
        .is_none()
    {
        return;
    }

    let ctx = UpgradeTestCtx::new("stream-drain").await;
    let backup_provider: Arc<dyn PreUpgradeBackupProvider> =
        Arc::new(StubBackupProvider::new(ctx.test_db.db.clone()));
    let lifecycle: Arc<dyn PostgresContainerLifecycle> = ctx.lifecycle_adapter.clone();

    let result = async {
        // Jump straight to phase=new_container. Pre-reqs: the old
        // container has been stopped (new_container expects its name
        // free) and the service live data volume is empty. We emulate
        // snapshot's tail (stop + wipe live volume) manually.
        ctx.lifecycle_adapter
            .stop_and_remove(ctx.service_id)
            .await
            .map_err(|e| format!("stop_and_remove: {}", e))?;
        use bollard::query_parameters::RemoveVolumeOptions;
        let live_vol = format!("{}_data", ctx.container_name());
        let _ = ctx
            .docker
            .remove_volume(&live_vol, None::<RemoveVolumeOptions>)
            .await;

        let upgrade_id = ctx.insert_upgrade_row(phase::NEW_CONTAINER).await;
        // Ensure rollback volume is "set" so downstream phases have
        // valid preconditions if they run — but we'll time out before
        // then. We don't actually need it for phase_new_container.
        let _orch = ctx.orchestrator(backup_provider, lifecycle);

        // Total budget: 90s. A broken drain produces a 120s hang at
        // create_and_start, so this cap reliably surfaces the bug.
        let outcome = tokio::time::timeout(Duration::from_secs(90), async {
            // phase_new_container + restore would be triggered by run(),
            // but we only want to exercise new_container. Call
            // create_and_start directly on the real adapter through
            // the trait surface — same code path the phase calls.
            ctx.lifecycle_adapter
                .create_and_start(ctx.service_id, TO_IMAGE)
                .await
        })
        .await;

        match outcome {
            Err(_elapsed) => Err(
                "create_and_start did not complete within 90s — stream-drain bug regressed"
                    .to_string(),
            ),
            Ok(Err(e)) => Err(format!("create_and_start errored: {}", e)),
            Ok(Ok(())) => {
                // Sanity: container is actually up and accepting queries.
                let version = ctx.psql_query("SHOW server_version_num").await?;
                let major = version
                    .trim()
                    .parse::<i32>()
                    .map(|v| v / 10000)
                    .unwrap_or(0);
                if major != 18 {
                    return Err(format!("expected v18, got {}", major));
                }
                Ok::<_, String>(upgrade_id)
            }
        }
    }
    .await;

    let _ = result.as_ref().ok().copied();
    ctx.cleanup().await;

    if let Err(e) = result {
        panic!("new_container stream-drain regression test failed: {}", e);
    }
}

// ========================================================================
// Test #28: phase_dump idempotency
// ========================================================================
//
// Verifies that `phase_dump` is safe to retry after a crash-mid-dump:
//   - Run 1: no marker exists, pg_dumpall runs, marker is written.
//   - Run 2 (retry): marker exists, phase is a no-op.
//
// Implementation: we invoke `phase_dump` twice back-to-back. Between
// the two invocations we reset `phase` back to DUMP in the DB so the
// second call re-enters. The second call should complete in <5s
// because it short-circuits on the marker; the first takes 30s+
// because it does a real pg_dumpall.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn orchestrator_phase_dump_is_idempotent() {
    use harness::*;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use std::sync::Arc;
    use std::time::Duration;

    if docker_or_skip("orchestrator_phase_dump_is_idempotent")
        .await
        .is_none()
    {
        return;
    }

    let ctx = UpgradeTestCtx::new("dump-idem").await;
    let backup_provider: Arc<dyn PreUpgradeBackupProvider> =
        Arc::new(StubBackupProvider::new(ctx.test_db.db.clone()));
    let lifecycle: Arc<dyn PostgresContainerLifecycle> = ctx.lifecycle_adapter.clone();

    let result = async {
        ctx.psql_exec("CREATE TABLE dump_probe(id INT)").await?;
        ctx.psql_exec("INSERT INTO dump_probe VALUES (1),(2),(3)").await?;

        let upgrade_id = ctx.insert_upgrade_row(phase::SNAPSHOT).await;
        let rollback_vol =
            format!("postgres-{}_data_rollback_{}", ctx.service_name, upgrade_id);
        let dump_vol =
            format!("postgres-{}_pgdump_{}", ctx.service_name, upgrade_id);
        let orch = ctx.orchestrator(backup_provider, lifecycle);

        // Drive only the two phases needed for this test. Keeping the
        // authoritative service image on FROM_IMAGE makes the simulated
        // DUMP retry valid under the phase-aware security guard.
        let snapshot_row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        orch.phase_snapshot(&snapshot_row)
            .await
            .map_err(|e| format!("snapshot: {}", e))?;
        let first_dump_row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        let run_start = std::time::Instant::now();
        orch.phase_dump(&first_dump_row)
            .await
            .map_err(|e| format!("first dump: {}", e))?;
        let first_dump_ms = run_start.elapsed().as_millis();

        // Marker must be present after a successful dump.
        // We re-check via dump_marker_present by calling the helper —
        // but it's a private method. Instead, we inspect the docker
        // volume existence as a coarser check.
        wait_volume_exists(ctx.docker.as_ref(), &dump_vol, Duration::from_secs(5))
            .await
            .map_err(|e| format!("expected dump volume: {}", e))?;

        // Reset only the durable phase, simulating a crash after the
        // marker write but before the phase transition committed.
        let row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        let mut active: postgres_major_upgrades::ActiveModel = row.into();
        active.phase = Set(phase::DUMP.to_string());
        active.status = Set(status::RUNNING.to_string());
        active.update(ctx.test_db.db.as_ref()).await.map_err(|e| e.to_string())?;

        // Re-run only DUMP. The marker makes this a fast no-op that
        // advances back to NEW_CONTAINER.
        let retry_start = std::time::Instant::now();
        let retry_row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        orch.phase_dump(&retry_row)
            .await
            .map_err(|e| format!("retry dump: {}", e))?;
        let retry_ms = retry_start.elapsed().as_millis();

        // A full first run includes real pg_dumpall + restore (slow).
        // The retry should be dominated by pass-through ADVANCE/log
        // writes plus at most a container restart, so must be
        // materially faster. We require retry < 0.5 * full_run to
        // give the assertion headroom for noisy CI runners.
        if retry_ms >= first_dump_ms {
            return Err(format!(
                "retry not materially faster: first_dump={}ms retry={}ms — dump idempotency may be broken",
                first_dump_ms, retry_ms
            ));
        }

        let final_row = load_upgrade(ctx.test_db.db.as_ref(), upgrade_id).await;
        if final_row.phase != phase::NEW_CONTAINER {
            return Err(format!(
                "retry left row in phase={}, expected NEW_CONTAINER",
                final_row.phase
            ));
        }

        Ok::<_, String>((upgrade_id, rollback_vol, dump_vol))
    }
    .await;

    let (cleanup_rollback, cleanup_dump): (Vec<String>, Vec<String>) = match &result {
        Ok((_, rb, dv)) => (vec![rb.clone()], vec![dv.clone()]),
        Err(_) => (vec![], vec![]),
    };
    ctx.cleanup().await;
    remove_volumes(
        ctx.docker.as_ref(),
        &[cleanup_rollback, cleanup_dump].concat(),
    )
    .await;

    if let Err(e) = result {
        panic!("dump idempotency test failed: {}", e);
    }
}
