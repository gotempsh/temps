// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use super::{
    application_workspace::{
        parse_application_workspace_usage, APPLICATION_GIT_BOOTSTRAP,
        APPLICATION_WORKSPACE_USAGE_COMMAND,
    },
    destroy::EVICTION_UNANSWERED_LIMIT,
    links::merge_runtime_variables,
    node_placement::{
        eviction_owners, node_sandboxes_page, split_eviction_outcomes, MAX_REASON_CHARS,
    },
    reconcile::VM_RECONCILE_REPORTED_IDS,
    source_import::{
        clean_git_environment, sandbox_image_is_trusted_for_credentials,
        sanitize_privileged_import_environment, tarball_extract_command,
        validate_connection_clone_origin, SourceNetworkPin,
    },
};
use async_trait::async_trait;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use temps_ai_agents::error::AgentError;
use temps_ai_agents::sandbox::{
    SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider,
};

/// Minimal provider. Each knob corresponds to a failure the service is
/// supposed to clean up after.
struct FakeProvider {
    /// `create` fails — the provider-failure arm of `create_sandbox`.
    fail_create: bool,
    /// `exec` returns a non-zero exit — the seeding-failure arm.
    fail_exec: bool,
    /// `destroy` errors — the case where the container may still be
    /// running and we delete the work dir anyway.
    fail_destroy: bool,
    /// `start` errors — the wake-on-access failure path.
    fail_start: bool,
    /// `configure_application_network` errors before compute may start.
    fail_network_config: bool,
    /// `destroy` never answers, like a frozen node; counts attempts.
    hang_destroy: Option<Arc<AtomicUsize>>,
    destroys: AtomicUsize,
    destroy_purge_flags: Arc<Mutex<Vec<bool>>>,
    creates: Arc<AtomicUsize>,
    starts: Arc<AtomicUsize>,
    lifecycle_calls: Arc<Mutex<Vec<&'static str>>>,
    /// Provider truth can differ from the database after an out-of-band
    /// container kill. `start` flips this back to true so the complete
    /// wake-and-retry path can be exercised without Docker.
    alive: Arc<AtomicBool>,
    /// `is_alive` errors instead of answering, like the Docker provider
    /// when it cannot stop a container whose isolation policy is stale.
    /// `start` clears it, as a successful start replaces that state.
    alive_check_fails: Arc<AtomicBool>,
    /// Every egress policy applied, in order.
    egress_calls: Arc<Mutex<Vec<temps_ai_agents::sandbox::SandboxEgressPolicy>>>,
    /// Refuse restricted policies, like a backend without allow-lists.
    fail_egress: bool,
    /// `create` takes this long, like a first image build.
    create_delay: Option<Duration>,
    /// The backends this host registered; `None` claims every one.
    backends: Option<Vec<SandboxBackend>>,
}

impl FakeProvider {
    fn new() -> Self {
        Self {
            fail_create: false,
            fail_exec: false,
            fail_destroy: false,
            fail_start: false,
            fail_network_config: false,
            hang_destroy: None,
            destroys: AtomicUsize::new(0),
            destroy_purge_flags: Arc::new(Mutex::new(Vec::new())),
            creates: Arc::new(AtomicUsize::new(0)),
            starts: Arc::new(AtomicUsize::new(0)),
            lifecycle_calls: Arc::new(Mutex::new(Vec::new())),
            alive: Arc::new(AtomicBool::new(true)),
            alive_check_fails: Arc::new(AtomicBool::new(false)),
            egress_calls: Arc::new(Mutex::new(Vec::new())),
            fail_egress: false,
            create_delay: None,
            backends: None,
        }
    }
}

fn handle_for(name: &str) -> SandboxHandle {
    SandboxHandle {
        node_id: None,
        sandbox_id: format!("docker-id-{}", name),
        sandbox_name: format!("temps-sandbox-{}", name),
        work_dir: PathBuf::from("/workspace"),
        backend: SandboxBackend::Docker,
        image: String::new(),
        conditions: Vec::new(),
    }
}

#[async_trait]
impl SandboxProvider for FakeProvider {
    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        self.backends
            .as_ref()
            .is_none_or(|backends| backends.contains(&backend))
    }

    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        if let Some(delay) = self.create_delay {
            tokio::time::sleep(delay).await;
        }
        if self.fail_create {
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "fake".into(),
                reason: "image pull failed".into(),
            });
        }
        self.creates.fetch_add(1, Ordering::SeqCst);
        self.alive.store(true, Ordering::SeqCst);
        let mut handle = handle_for(config.container_name_override.as_deref().unwrap_or("x"));
        handle.image = config.image.unwrap_or_default();
        Ok(handle)
    }

    async fn exec(
        &self,
        _handle: &SandboxHandle,
        _cmd: Vec<String>,
        _env: HashMap<String, String>,
        _on_output: Option<temps_agent_core::OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        Ok(SandboxExecResult {
            exit_code: if self.fail_exec { 1 } else { 0 },
            stdout: String::new(),
            stderr: if self.fail_exec {
                "clone failed".into()
            } else {
                String::new()
            },
        })
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        if self.alive_check_fails.load(Ordering::SeqCst) {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "fake".into(),
                reason: format!(
                    "stop sandbox '{}' after its isolation policy failed validation",
                    handle.sandbox_name
                ),
            });
        }
        Ok(self.alive.load(Ordering::SeqCst))
    }

    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        _network_name: &str,
        _service_containers: &[String],
    ) -> Result<(), AgentError> {
        self.lifecycle_calls
            .lock()
            .expect("lifecycle call mutex")
            .push("configure");
        if self.fail_network_config {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "fake".into(),
                reason: format!("network reconciliation failed for {}", handle.sandbox_name),
            });
        }
        Ok(())
    }

    async fn write_file(
        &self,
        _handle: &SandboxHandle,
        _path: &str,
        _contents: &[u8],
        _mode: u32,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    async fn read_file(&self, _handle: &SandboxHandle, _path: &str) -> Result<Vec<u8>, AgentError> {
        Ok(Vec::new())
    }

    async fn write_directory(
        &self,
        _handle: &SandboxHandle,
        _local_dir: &std::path::Path,
        _target_path: &str,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    async fn kill_processes(
        &self,
        _handle: &SandboxHandle,
        _pattern: &str,
        _signal: temps_ai_agents::sandbox::KillSignal,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    async fn fence_process_trees(
        &self,
        _handle: &SandboxHandle,
        _patterns: &[&str],
    ) -> Result<(), AgentError> {
        // Restart-recovery fencing is orthogonal to the individual
        // lifecycle failure knobs exercised by this service fake.
        Ok(())
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        if let Some(attempts) = &self.hang_destroy {
            attempts.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        self.destroys.fetch_add(1, Ordering::SeqCst);
        self.destroy_purge_flags
            .lock()
            .expect("destroy purge flags mutex")
            .push(purge_volumes);
        if self.fail_destroy {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "fake".into(),
                reason: format!(
                    "daemon unreachable while destroying {}",
                    handle.sandbox_name
                ),
            });
        }
        Ok(())
    }

    /// The trait's default `start` returns "not supported", so the
    /// wake path needs a real implementation here.
    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.lifecycle_calls
            .lock()
            .expect("lifecycle call mutex")
            .push("start");
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self.fail_start {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "fake".into(),
                reason: format!("daemon unreachable while starting {}", handle.sandbox_name),
            });
        }
        self.alive.store(true, Ordering::SeqCst);
        self.alive_check_fails.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self, _handle: &SandboxHandle) -> Result<(), AgentError> {
        self.lifecycle_calls
            .lock()
            .expect("lifecycle call mutex")
            .push("stop");
        Ok(())
    }

    async fn set_egress_policy(
        &self,
        handle: &SandboxHandle,
        policy: &temps_ai_agents::sandbox::SandboxEgressPolicy,
    ) -> Result<(), AgentError> {
        if self.fail_egress && policy.is_restricted() {
            return Err(AgentError::SandboxEgressPolicyUnsupported {
                sandbox: handle.sandbox_name.clone(),
                backend: "firecracker".into(),
            });
        }
        self.egress_calls
            .lock()
            .expect("egress call mutex")
            .push(policy.clone());
        Ok(())
    }

    async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        Ok(None)
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        Ok(Some(handle_for(container_name)))
    }

    fn name(&self) -> &str {
        "fake"
    }

    async fn is_available(&self) -> bool {
        true
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        Ok((true, "fake:latest".into()))
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        Ok("fake:latest".into())
    }
}

/// A `ServerConfig` built by struct literal rather than `ServerConfig::new`,
/// which has filesystem side effects (creates `~/.temps` and writes an
/// auth secret + encryption key). A test must not touch the developer's
/// real data dir.
fn test_server_config(data_dir: PathBuf) -> temps_config::ServerConfig {
    temps_config::ServerConfig {
        address: "127.0.0.1:3000".into(),
        database_url: "postgresql://test".into(),
        tls_address: None,
        console_address: "127.0.0.1:3001".into(),
        console_admin_address: None,
        console_ingress_address: None,
        admin_allowed_ips: Vec::new(),
        admin_allowed_hosts: Vec::new(),
        admin_trust_forwarded_for: false,
        data_dir,
        auth_secret: "test-auth-secret".into(),
        encryption_key: "0123456789abcdef0123456789abcdef\
                             0123456789abcdef0123456789abcdef"
            .into(),
        api_base_url: "http://127.0.0.1:3000".into(),
        postgres_max_connections: None,
        postgres_min_connections: None,
        postgres_connect_timeout_secs: None,
        postgres_acquire_timeout_secs: None,
        postgres_idle_timeout_secs: None,
        postgres_max_lifetime_secs: None,
        clickhouse_url: None,
        clickhouse_database: None,
        clickhouse_user: None,
        clickhouse_password: None,
        docker_extra_networks: Vec::new(),
    }
}

struct PanicOnSendJobQueue;

#[async_trait]
impl temps_core::JobQueue for PanicOnSendJobQueue {
    async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError> {
        panic!("no job should be queued in these tests, got: {job:?}");
    }

    fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
        panic!("subscribe not needed for these tests")
    }
}

/// A unique data root per test so concurrent tests can't collide.
fn unique_data_root(tag: &str) -> PathBuf {
    let uniq = std::process::id() as u64
        + std::sync::atomic::AtomicU64::new(0).fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("temps-sbx-test-{}-{}", tag, uniq))
}

fn build_service(
    db: sea_orm::DatabaseConnection,
    provider: FakeProvider,
    data_root: PathBuf,
) -> (Arc<SandboxService>, Arc<StandaloneSandboxRegistry>) {
    build_service_shared(Arc::new(db), provider, data_root)
}

/// [`build_service`] over a connection the test keeps a handle to, so it
/// can read the statement log back once the service is dropped.
fn build_service_shared(
    db: Arc<sea_orm::DatabaseConnection>,
    provider: FakeProvider,
    data_root: PathBuf,
) -> (Arc<SandboxService>, Arc<StandaloneSandboxRegistry>) {
    let registry = Arc::new(StandaloneSandboxRegistry::local_only(
        Arc::new(provider) as Arc<dyn SandboxProvider>
    ));
    let config = Arc::new(temps_config::ConfigService::new(
        Arc::new(test_server_config(data_root.clone())),
        db.clone(),
    ));
    let encryption = Arc::new(
        temps_core::EncryptionService::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("test encryption key"),
    );
    let git = Arc::new(GitProviderManager::new(
        db.clone(),
        encryption,
        Arc::new(PanicOnSendJobQueue) as Arc<dyn temps_core::JobQueue>,
        config.clone(),
    ));
    let cookie_crypto = Arc::new(
        temps_core::CookieCrypto::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("test cookie key"),
    );
    let service = Arc::new(SandboxService::new(
        db,
        registry.clone(),
        Arc::new(JobTracker::new()),
        config,
        cookie_crypto,
        git,
        data_root,
        Arc::new(crate::services::placement::EveryNodeReady),
    ));
    (service, registry)
}

fn row(public_id: &str, agent_run_id: Option<i32>) -> sandboxes::Model {
    let now = Utc::now();
    sandboxes::Model {
        id: 7,
        node_id: None,
        public_id: public_id.to_string(),
        user_id: Some(1),
        agent_run_id,
        name: "sbx-7".into(),
        status: "running".into(),
        image: None,
        work_dir: "/workspace".into(),
        timeout_secs: 3600,
        metadata: None,
        backend: None,
        created_at: now,
        last_activity_at: now,
        expires_at: now + chrono::Duration::seconds(3600),
        preview_password_hash: None,
        preview_password_hint: None,
        preview_access: Default::default(),
        preview_allowed_cidrs: Vec::new(),
        egress_policy: Default::default(),
        lifecycle: "ephemeral".to_string(),
        project_id: None,
        source_repo_url: None,
    }
}

/// Seed a work dir with nested, non-empty content — a shallow
/// `remove_dir` would fail on this, a recursive delete succeeds.
fn seed_work_dir(data_root: &Path, public_id: &str) -> PathBuf {
    let dir = data_root.join(public_id);
    let nested = dir.join("node_modules").join("pkg");
    std::fs::create_dir_all(&nested).expect("seed work dir");
    std::fs::write(nested.join("index.js"), b"module.exports = {}").expect("seed file");
    dir
}

const PUBLIC_ID: &str = "sbx_deadbeef01234567";

fn attested_application_workspace(status: &str, application_work_dir: &Path) -> sandboxes::Model {
    sandboxes::Model {
        name: "ai-application:app_example".to_string(),
        status: status.to_string(),
        lifecycle: "workspace".to_string(),
        work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
        image: ApplicationWorkspaceConfig::default().image,
        timeout_secs: ApplicationWorkspaceConfig::default().idle_timeout_secs as i32,
        metadata: Some(serde_json::json!({
            "managed_application_id": "app_example",
            "managed_host_work_dir": application_work_dir,
        })),
        ..row(PUBLIC_ID, None)
    }
}

#[tokio::test]
async fn untrusted_application_workspace_is_not_destroyed_or_rebuilt() {
    let data_root = unique_data_root("untrusted-application-workspace");
    std::fs::create_dir_all(&data_root).expect("data root");
    let host_work_dir = data_root.join("app_example");
    let mut forged = attested_application_workspace("running", &host_work_dir);
    forged.metadata = Some(serde_json::json!({
        "managed_application_id": "app_example",
        "managed_host_work_dir": data_root.join("another-app"),
    }));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![forged]])
        .into_connection();
    let provider = FakeProvider::new();
    let creates = provider.creates.clone();
    let purge_flags = provider.destroy_purge_flags.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let error = service
        .get_or_create_application_workspace_with_config(
            1,
            "app_example",
            None,
            host_work_dir,
            ApplicationWorkspaceConfig::default(),
            &[],
        )
        .await
        .expect_err("forged host path must fail closed");
    assert!(matches!(error, SandboxError::InvalidState { .. }));
    assert_eq!(creates.load(Ordering::SeqCst), 0);
    assert!(purge_flags.lock().expect("purge flags").is_empty());
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn managed_image_upgrade_rebuilds_same_workspace_without_purging_home() {
    let data_root = unique_data_root("managed-image-upgrade");
    std::fs::create_dir_all(&data_root).expect("data root");
    let host_work_dir = data_root.join("app_example");
    let mut old = attested_application_workspace("running", &host_work_dir);
    old.image = Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2".into());
    let mut rebuilt = old.clone();
    rebuilt.image = ApplicationWorkspaceConfig::default().image;
    rebuilt.timeout_secs = ApplicationWorkspaceConfig::default().idle_timeout_secs as i32;
    rebuilt.metadata = Some(serde_json::json!({
        "disk_size_mb": ApplicationWorkspaceConfig::default().disk_limit_mb,
        "managed_application_id": "app_example",
        "managed_host_work_dir": host_work_dir,
    }));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![old.clone()]])
        .append_query_results([vec![old]])
        .append_query_results([vec![rebuilt.clone()]])
        .append_query_results([vec![rebuilt.clone()]])
        .append_query_results([vec![rebuilt.clone()]])
        .append_query_results([Vec::<project_services::Model>::new()])
        .append_query_results([vec![rebuilt.clone()]])
        .append_query_results([vec![rebuilt.clone()]])
        .append_query_results([vec![rebuilt]])
        .append_exec_results([sea_orm::MockExecResult {
            last_insert_id: 1,
            rows_affected: 1,
        }])
        .into_connection();
    let provider = FakeProvider::new();
    let creates = provider.creates.clone();
    let purge_flags = provider.destroy_purge_flags.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let result = service
        .get_or_create_application_workspace_with_config(
            1,
            "app_example",
            None,
            host_work_dir,
            ApplicationWorkspaceConfig::default(),
            &[],
        )
        .await
        .expect("managed image change should preserve the workspace identity");
    assert_eq!(result.public_id, PUBLIC_ID);
    assert_eq!(creates.load(Ordering::SeqCst), 1);
    assert_eq!(*purge_flags.lock().expect("purge flags"), vec![false]);
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn stopped_application_workspace_reconciles_network_before_start() {
    let data_root = unique_data_root("application-network-before-start");
    std::fs::create_dir_all(&data_root).expect("data root");
    let application_work_dir = data_root.join("app_example");
    let stopped = attested_application_workspace("stopped", &application_work_dir);
    let running = attested_application_workspace("running", &application_work_dir);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Existing workspace lookup and the re-read used for network
        // preparation, then the authorized project's empty topology.
        .append_query_results([vec![stopped.clone()]])
        .append_query_results([vec![stopped.clone()]])
        .append_query_results([Vec::<project_services::Model>::new()])
        // Resume re-reads the stopped row.
        .append_query_results([vec![stopped]])
        // Resume update returns the running row.
        .append_query_results([vec![running.clone()]])
        // Resume bookkeeping and Git bootstrap re-resolve the now-running
        // workspace through the same durable identity.
        .append_query_results([vec![running.clone()]])
        .append_query_results([vec![running.clone()]])
        .append_query_results([vec![running]])
        .append_exec_results([sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let provider = FakeProvider::new();
    let lifecycle_calls = provider.lifecycle_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    service
        .get_or_create_application_workspace_with_config(
            1,
            "app_example",
            None,
            application_work_dir,
            ApplicationWorkspaceConfig::default(),
            &[42],
        )
        .await
        .expect("network reconciliation must complete before the stopped workspace starts");

    assert_eq!(
        *lifecycle_calls.lock().expect("lifecycle call mutex"),
        vec!["configure", "start"]
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn failed_network_reconciliation_never_starts_stopped_application_workspace() {
    let data_root = unique_data_root("application-network-failure");
    std::fs::create_dir_all(&data_root).expect("data root");
    let application_work_dir = data_root.join("app_example");
    let stopped = attested_application_workspace("stopped", &application_work_dir);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![stopped.clone()]])
        .append_query_results([vec![stopped]])
        .append_query_results([Vec::<project_services::Model>::new()])
        .into_connection();
    let provider = FakeProvider {
        fail_network_config: true,
        ..FakeProvider::new()
    };
    let starts = provider.starts.clone();
    let lifecycle_calls = provider.lifecycle_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let error = service
        .get_or_create_application_workspace_with_config(
            1,
            "app_example",
            None,
            application_work_dir,
            ApplicationWorkspaceConfig::default(),
            &[42],
        )
        .await
        .expect_err("failed network reconciliation must abort workspace preparation");

    assert!(error.to_string().contains("network reconciliation failed"));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        *lifecycle_calls.lock().expect("lifecycle call mutex"),
        vec!["configure"]
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[test]
fn git_bootstrap_trusts_the_workspace_before_probing_the_repository() {
    let trust = APPLICATION_GIT_BOOTSTRAP
        .find("git config --global --add safe.directory")
        .expect("bootstrap must trust the workspace directory");
    let probe = APPLICATION_GIT_BOOTSTRAP
        .find("if ! git rev-parse")
        .expect("bootstrap probes for an existing repository");
    assert!(
        trust < probe,
        "an unowned bind mount must be trusted before git inspects it"
    );
}

#[tokio::test]
async fn application_workspace_fails_when_in_sandbox_git_bootstrap_fails() {
    let data_root = unique_data_root("application-git-bootstrap");
    std::fs::create_dir_all(&data_root).expect("data root");
    let application_work_dir = data_root.join("app_example");
    let existing = sandboxes::Model {
        name: "ai-application:app_example".to_string(),
        lifecycle: "workspace".to_string(),
        work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
        image: ApplicationWorkspaceConfig::default().image,
        timeout_secs: ApplicationWorkspaceConfig::default().idle_timeout_secs as i32,
        metadata: Some(serde_json::json!({
            "managed_application_id": "app_example",
            "managed_host_work_dir": application_work_dir,
        })),
        ..row(PUBLIC_ID, None)
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Existing application sandbox lookup.
        .append_query_results([vec![existing.clone()]])
        // Network preparation re-reads the row and resolves the empty
        // authorized service topology before Git bootstrap may execute.
        .append_query_results([vec![existing.clone()]])
        .append_query_results([Vec::<project_services::Model>::new()])
        // `exec` ownership lookup.
        .append_query_results([vec![existing]])
        // Activity touch performed before provider exec.
        .append_exec_results([sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let mut provider = FakeProvider::new();
    provider.fail_exec = true;
    let (service, _) = build_service(db, provider, data_root.clone());

    let error = service
        .get_or_create_application_workspace(1, "app_example", None, application_work_dir)
        .await
        .expect_err("a failed Git bootstrap must fail workspace preparation");

    assert!(matches!(error, SandboxError::ExecFailed { .. }));
    assert!(error
        .to_string()
        .contains("initialize persistent Git workspace"));
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn application_workspace_rebuilds_killed_provider_before_git_bootstrap() {
    let data_root = unique_data_root("application-provider-recovery");
    std::fs::create_dir_all(&data_root).expect("data root");
    let application_work_dir = data_root.join("app_example");
    let existing = sandboxes::Model {
        name: "ai-application:app_example".to_string(),
        lifecycle: "workspace".to_string(),
        work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
        image: ApplicationWorkspaceConfig::default().image,
        metadata: Some(serde_json::json!({
            "managed_application_id": "app_example",
            "managed_host_work_dir": application_work_dir,
        })),
        ..row(PUBLIC_ID, None)
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Existing application sandbox lookup.
        .append_query_results([vec![existing.clone()]])
        // Rebuild lookup and update return the same durable identity.
        .append_query_results([vec![existing.clone()]])
        .append_query_results([vec![existing.clone()]])
        // Rebuild bookkeeping and network preparation re-read the durable
        // row, then resolve no data services for the empty authorized set.
        .append_query_results([vec![existing.clone()]])
        .append_query_results([vec![existing.clone()]])
        .append_query_results([Vec::<project_services::Model>::new()])
        // Git-bootstrap exec ownership lookup sees the rebuilt row.
        .append_query_results([vec![existing]])
        // Activity touch performed before provider exec.
        .append_exec_results([sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let creates = provider.creates.clone();
    let starts = provider.starts.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let sandbox = service
        .get_or_create_application_workspace(1, "app_example", None, application_work_dir)
        .await
        .expect("application preparation must wake its stopped provider runtime");

    assert_eq!(sandbox.public_id, PUBLIC_ID);
    assert_eq!(
        sandbox
            .handle
            .as_ref()
            .expect("running application workspace handle")
            .sandbox_name,
        "temps-sandbox-deadbeef01234567"
    );
    assert_eq!(
        creates.load(Ordering::SeqCst),
        1,
        "Git bootstrap must be preceded by exactly one provider rebuild"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_dir_all(&data_root);
}

/// A row in the given lifecycle class and status. Used by the
/// workspace wake tests below.
fn row_with(public_id: &str, lifecycle: &str, status: &str) -> sandboxes::Model {
    sandboxes::Model {
        status: status.to_string(),
        lifecycle: lifecycle.to_string(),
        ..row(public_id, None)
    }
}

// ── Lifecycle class (ADR-036) ────────────────────────────────────────

#[test]
fn lifecycle_parses_both_public_values() {
    assert_eq!(
        SandboxLifecycle::parse("ephemeral").expect("ephemeral is valid"),
        SandboxLifecycle::Ephemeral
    );
    assert_eq!(
        SandboxLifecycle::parse("workspace").expect("workspace is valid"),
        SandboxLifecycle::Workspace
    );
}

#[test]
fn lifecycle_rejects_unknown_values_with_a_helpful_message() {
    // Silently falling back to ephemeral would cost the caller their
    // work at the first idle sweep, so a typo must be a 400.
    let err = SandboxLifecycle::parse("workspaces").expect_err("typo must not be accepted");
    let msg = err.to_string();
    assert!(msg.contains("workspaces"), "msg: {}", msg);
    assert!(msg.contains("ephemeral"), "must list valid values: {}", msg);
    assert!(msg.contains("workspace"), "must list valid values: {}", msg);
}

#[test]
fn lifecycle_from_row_treats_unrecognised_values_as_ephemeral() {
    // Forward-compat: a row written by a newer binary and then rolled
    // back must not auto-start containers this binary doesn't
    // understand. Ephemeral is the conservative reading.
    let unknown = row_with(PUBLIC_ID, "hibernating", "stopped");
    assert_eq!(
        SandboxLifecycle::from_row(&unknown),
        SandboxLifecycle::Ephemeral
    );
}

#[test]
fn lifecycle_default_is_ephemeral() {
    // Every existing caller and SDK client omits the field.
    assert_eq!(SandboxLifecycle::default(), SandboxLifecycle::Ephemeral);
    assert_eq!(SandboxLifecycle::default().as_str(), "ephemeral");
}

#[test]
fn idle_deadline_is_now_plus_timeout() {
    let now = Utc::now();
    assert_eq!(idle_deadline(now, 3600), now + chrono::Duration::hours(1));
    // Degenerate but representable: a zero timeout is already expired
    // rather than infinite. The service clamps to >= 60 before this
    // is reached, so this only pins the arithmetic.
    assert_eq!(idle_deadline(now, 0), now);
}

#[tokio::test]
async fn resolve_id_wakes_a_stopped_workspace() {
    let data_root = unique_data_root("wake-workspace");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_public_id → a suspended workspace
        .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "stopped")]])
        // wake_workspace's status update (RETURNING the running row)
        .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "running")]])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let (row, id) = service
        .resolve_id(PUBLIC_ID, 1)
        .await
        .expect("a stopped workspace must wake, not error");

    assert_eq!(id, 7);
    assert_eq!(
        row.status, "running",
        "the caller must receive the woken row, not the stale stopped one"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resolve_id_still_rejects_a_stopped_ephemeral_sandbox() {
    // `@vercel/sandbox` consumers rely on a stopped sandbox staying
    // stopped. Waking one under them would restart billing on a
    // container they deliberately shut down.
    let data_root = unique_data_root("no-wake-ephemeral");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let err = service
        .resolve_id(PUBLIC_ID, 1)
        .await
        .expect_err("ephemeral sandboxes must not auto-wake");

    match err {
        SandboxError::InvalidState { state, .. } => assert_eq!(state, "stopped"),
        other => panic!("expected InvalidState, got {:?}", other),
    }
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resolve_id_passes_through_a_running_workspace_without_starting_it() {
    // A running workspace is the common case and must not pay for the
    // wake path. Proven with a provider whose `start` always fails: if
    // the resolve path touched it, this would error.
    let data_root = unique_data_root("running-workspace");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "running")]])
        .into_connection();

    let provider = FakeProvider {
        fail_start: true,
        ..FakeProvider::new()
    };
    let (service, _) = build_service(db, provider, data_root.clone());
    let (row, _) = service
        .resolve_id(PUBLIC_ID, 1)
        .await
        .expect("a running workspace resolves without a provider start");
    assert_eq!(row.status, "running");
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resolve_id_wakes_workspace_when_row_is_running_but_provider_is_stopped() {
    // Reproduces an application harness hitting its hard deadline: the
    // provider stops the Docker container to kill the orphaned process,
    // but the standalone sandbox row remains `running`. The next Git
    // bootstrap/exec must recover the workspace instead of failing before
    // the harness-level recovery path can run.
    let data_root = unique_data_root("wake-stale-running-workspace");
    let stale_row = row_with(PUBLIC_ID, "workspace", "running");
    let recovered_row = stale_row.clone();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // resolve_id ownership lookup returns the stale DB state.
        .append_query_results([vec![stale_row]])
        // wake_workspace status reconciliation returns a live row.
        .append_query_results([vec![recovered_row]])
        .into_connection();

    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let starts = provider.starts.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let (row, id) = service
        .resolve_id(PUBLIC_ID, 1)
        .await
        .expect("a provider-stopped workspace must wake despite a stale running row");

    assert_eq!(id, 7);
    assert_eq!(row.status, "running");
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "provider start must run exactly once"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resolve_id_wakes_workspace_when_local_liveness_check_fails() {
    // A local Docker liveness check errors when stopping a container
    // whose isolation policy is stale fails. The registry reports that
    // as a missing sandbox for control-plane sandboxes, so the access
    // path still wakes the workspace instead of failing the exec.
    let data_root = unique_data_root("wake-after-liveness-failure");
    let stale_row = row_with(PUBLIC_ID, "workspace", "running");
    let recovered_row = stale_row.clone();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![stale_row]])
        .append_query_results([vec![recovered_row]])
        .into_connection();

    let provider = FakeProvider::new();
    provider.alive_check_fails.store(true, Ordering::SeqCst);
    let starts = provider.starts.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let (row, id) = service
        .resolve_id(PUBLIC_ID, 1)
        .await
        .expect("a failed local liveness check must wake the workspace, not fail");

    assert_eq!(id, 7);
    assert_eq!(row.status, "running");
    assert_eq!(starts.load(Ordering::SeqCst), 1, "the wake must start it");
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resolve_id_surfaces_a_failed_wake_instead_of_swallowing_it() {
    // A workspace that silently fails to wake and then reports
    // "not found" on every later call is exactly the dead end a
    // self-hosted user can't debug. The provider error must reach them.
    let data_root = unique_data_root("wake-fails");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "stopped")]])
        .into_connection();

    let provider = FakeProvider {
        fail_start: true,
        ..FakeProvider::new()
    };
    let (service, _) = build_service(db, provider, data_root.clone());
    let err = service
        .resolve_id(PUBLIC_ID, 1)
        .await
        .expect_err("a provider start failure must not be swallowed");

    let msg = err.to_string();
    assert!(
        msg.contains("daemon unreachable"),
        "the provider's reason must reach the caller: {}",
        msg
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

// ── A VM that is not running is not "not found" ──────────────────────

/// Run `work` against a service on a mock database and return what it
/// produced together with the SQL the service issued.
async fn with_statement_log<T, F, Fut>(
    tag: &str,
    results: impl IntoIterator<Item = Vec<sandboxes::Model>>,
    events: usize,
    provider: FakeProvider,
    work: F,
) -> (T, String)
where
    F: FnOnce(Arc<SandboxService>) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let data_root = unique_data_root(tag);
    let mut mock = MockDatabase::new(DatabaseBackend::Postgres).append_query_results(results);
    for _ in 0..events {
        mock = mock.append_query_results([vec![destroyed_event()]]);
    }
    let db = Arc::new(mock.into_connection());
    let (service, registry) = build_service_shared(db.clone(), provider, data_root.clone());
    let output = work(service.clone()).await;
    drop(service);
    drop(registry);
    let log = Arc::try_unwrap(db)
        .expect("nothing holds the database after the service is dropped")
        .into_transaction_log();
    let _ = std::fs::remove_dir_all(&data_root);
    (output, format!("{log:?}"))
}

#[tokio::test]
async fn a_dead_vm_is_a_409_naming_the_backend_and_the_row_is_marked_stopped() {
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let row = row_with(PUBLIC_ID, "ephemeral", "running");
    let (result, sql) = with_statement_log(
        "dead-vm",
        [vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]],
        1,
        provider,
        |service| async move { service.live_handle(&row).await },
    )
    .await;

    let err = result.expect_err("a dead VM has no handle to give");
    match &err {
        SandboxError::VmNotRunning {
            sandbox_id,
            backend,
            reason,
            recovery,
        } => {
            assert_eq!(*recovery, crate::error::VmRecovery::Resume);
            assert_eq!(sandbox_id, PUBLIC_ID);
            assert_eq!(backend.as_deref(), Some("docker"));
            assert_eq!(reason, "its container is stopped");
        }
        other => panic!("expected VmNotRunning, got {other:?}"),
    }
    let problem = temps_core::problemdetails::Problem::from(err);
    assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
    assert!(sql.contains("UPDATE"), "the row must be reconciled: {sql}");
    assert!(sql.contains("vm_lost"), "the event must be recorded: {sql}");
}

/// A liveness check that errors proves nothing about the VM, so the row
/// is left alone (a Docker stale-policy failure must not stop a sandbox).
#[tokio::test]
async fn a_failed_liveness_check_does_not_touch_the_row() {
    let provider = FakeProvider::new();
    provider.alive_check_fails.store(true, Ordering::SeqCst);
    let row = row_with(PUBLIC_ID, "ephemeral", "running");
    let (result, sql) = with_statement_log(
        "liveness-error",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        provider,
        |service| async move { service.live_handle(&row).await },
    )
    .await;

    assert!(
        matches!(
            result,
            Err(SandboxError::VmNotRunning { backend: None, .. })
        ),
        "{:?}",
        result.err()
    );
    assert!(!sql.contains("UPDATE"), "no write expected: {sql}");
}

#[tokio::test]
async fn a_live_vm_gives_its_handle_and_writes_nothing() {
    let row = row_with(PUBLIC_ID, "ephemeral", "running");
    let (result, sql) = with_statement_log(
        "live-vm",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        FakeProvider::new(),
        |service| async move { service.live_handle(&row).await },
    )
    .await;

    assert!(result.is_ok(), "{:?}", result.err());
    assert!(!sql.contains("UPDATE"), "no write expected: {sql}");
}

/// A pause or destroy that lands between the liveness probe and the
/// reconcile wins: the conditional update matches nothing and no
/// `vm_lost` event is written for a transition that did not happen.
#[tokio::test]
async fn marking_a_vm_lost_does_not_override_a_concurrent_status_change() {
    let row = row_with(PUBLIC_ID, "ephemeral", "running");
    let cause = AgentError::SandboxNotRunning {
        run_id: 7,
        sandbox_id: PUBLIC_ID.into(),
        container: "temps-sandbox-deadbeef01234567".into(),
        backend: "firecracker".into(),
        reason: "its firecracker process exited".into(),
    };
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let (changed, sql) = with_statement_log(
        "mark-race",
        [Vec::<sandboxes::Model>::new()],
        0,
        provider,
        |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
    )
    .await;

    assert!(!changed);
    assert!(sql.contains("UPDATE"), "{sql}");
    assert!(!sql.contains("vm_lost"), "no event without a change: {sql}");
}

/// Snapshot capture, restart and disk resize take the VM down on purpose
/// and start it again. A request that sees it down meanwhile must not
/// rewrite the row to `stopped`, or the sandbox would come back to life
/// unreachable.
#[tokio::test]
async fn a_planned_stop_is_not_a_lost_vm() {
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let row = row_with(PUBLIC_ID, "ephemeral", "running");
    let cause = AgentError::SandboxNotRunning {
        run_id: row.id,
        sandbox_id: PUBLIC_ID.into(),
        container: "temps-sandbox-deadbeef01234567".into(),
        backend: "firecracker".into(),
        reason: "its firecracker process exited".into(),
    };
    let data_root = unique_data_root("planned-stop");
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
            .append_query_results([vec![destroyed_event()]])
            .into_connection(),
    );
    let (service, registry) = build_service_shared(db.clone(), provider, data_root.clone());

    let planned = registry.begin_planned_downtime(row.id).await;
    assert!(
        !service.mark_vm_lost(&row, &cause, "access").await,
        "a VM that is down for a planned stop must not be marked lost"
    );
    drop(planned);
    assert!(
        service.mark_vm_lost(&row, &cause, "access").await,
        "once the planned stop is over a down VM is lost again"
    );

    drop(service);
    drop(registry);
    let log = Arc::try_unwrap(db)
        .expect("nothing holds the database after the service is dropped")
        .into_transaction_log();
    let _ = std::fs::remove_dir_all(&data_root);
    let sql = format!("{log:?}");
    assert_eq!(sql.matches("UPDATE").count(), 1, "{sql}");
}

/// A verdict reached while the control plane had the VM down is stale by
/// the time it is acted on if the planned stop has finished and the VM is
/// back: the claim is only worth something with a fresh look.
#[tokio::test]
async fn a_stale_down_verdict_is_not_acted_on_when_the_vm_is_back() {
    let row = row_with(PUBLIC_ID, "ephemeral", "running");
    let cause = AgentError::SandboxNotRunning {
        run_id: row.id,
        sandbox_id: PUBLIC_ID.into(),
        container: "temps-sandbox-deadbeef01234567".into(),
        backend: "firecracker".into(),
        reason: "its firecracker process exited".into(),
    };
    let (changed, sql) = with_statement_log(
        "stale-verdict",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        FakeProvider::new(),
        |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
    )
    .await;

    assert!(!changed);
    assert!(!sql.contains("UPDATE"), "{sql}");
}

/// The reconciliation reads running VM rows page by page and covers every
/// one of them, however small the page.
#[tokio::test]
async fn startup_reconciliation_walks_every_page() {
    let mut first = row_with(PUBLIC_ID, "ephemeral", "running");
    first.id = 1;
    let mut second = row_with(PUBLIC_ID, "ephemeral", "running");
    second.id = 2;
    let mut third = row_with(PUBLIC_ID, "ephemeral", "running");
    third.id = 3;
    let data_root = unique_data_root("reconcile-pages");
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![first, second], vec![third]])
            .into_connection(),
    );
    let (service, _registry) =
        build_service_shared(db.clone(), FakeProvider::new(), data_root.clone());

    let report = service.reconcile_running_vms_paged(2).await;
    drop(service);
    drop(_registry);
    let log = Arc::try_unwrap(db)
        .expect("nothing holds the database after the service is dropped")
        .into_transaction_log();
    let _ = std::fs::remove_dir_all(&data_root);

    assert_eq!(report.checked, 3);
    assert_eq!(report.alive, 3);
    let sql = format!("{log:?}");
    assert_eq!(sql.matches("LIMIT").count(), 2, "one query per page: {sql}");
    assert!(
        sql.contains("Int(Some(2))"),
        "second page starts after the last id of the first: {sql}"
    );
}

#[tokio::test]
async fn only_running_rows_are_marked_lost() {
    let row = row_with(PUBLIC_ID, "ephemeral", "stopped");
    let cause = AgentError::SandboxNotFound {
        run_id: 7,
        sandbox: "temps-sandbox-deadbeef01234567".into(),
    };
    let (changed, sql) = with_statement_log(
        "mark-stopped-row",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        FakeProvider::new(),
        |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
    )
    .await;

    assert!(!changed);
    assert!(sql.contains("[]"), "no statement expected: {sql}");
}

#[test]
fn startup_reconciliation_only_judges_vmm_backends() {
    let with_backend = |backend: Option<&str>| sandboxes::Model {
        backend: backend.map(str::to_string),
        ..row(PUBLIC_ID, None)
    };
    assert!(reconcilable_at_startup(&with_backend(Some("firecracker"))));
    assert!(reconcilable_at_startup(&with_backend(Some("microsandbox"))));
    // dockerd may still be restarting these after a reboot.
    assert!(!reconcilable_at_startup(&with_backend(Some("docker"))));
    assert!(!reconcilable_at_startup(&with_backend(None)));
}

/// A managed application workspace reports a lost runtime as the derived
/// status `recovering`; stopping its row would hide that from its own
/// recovery and the data-network reconciliation.
#[tokio::test]
async fn managed_application_workspaces_are_never_marked_lost() {
    let row = sandboxes::Model {
        name: "ai-application:app_example".to_string(),
        lifecycle: "workspace".to_string(),
        ..row(PUBLIC_ID, None)
    };
    let cause = AgentError::SandboxNotFound {
        run_id: 7,
        sandbox: "temps-sandbox-deadbeef01234567".into(),
    };
    let (changed, sql) = with_statement_log(
        "mark-app-workspace",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        FakeProvider::new(),
        |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
    )
    .await;

    assert!(!changed);
    assert!(!sql.contains("UPDATE"), "{sql}");
}

/// The "VM not running" error says to run `resume`; for a row that still
/// says `running` that must boot the VM, not answer "already running".
#[tokio::test]
async fn resume_boots_a_running_row_whose_vm_is_down() {
    let data_root = unique_data_root("resume-dead-running");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_public_id: the stale row.
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
        // mark_vm_lost: conditional running -> stopped update.
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
        // its `vm_lost` event.
        .append_query_results([vec![destroyed_event()]])
        // resume: stopped -> running update.
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
        // its `resumed` event.
        .append_query_results([vec![destroyed_event()]])
        .into_connection();
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let starts = provider.starts.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let row = service
        .resume_sandbox(PUBLIC_ID, 1)
        .await
        .expect("resume must boot a dead VM behind a running row");

    assert_eq!(row.status, "running");
    assert_eq!(starts.load(Ordering::SeqCst), 1, "the VM must be started");
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resume_of_a_running_row_with_a_live_vm_stays_a_no_op() {
    let data_root = unique_data_root("resume-live-running");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
        .into_connection();
    let provider = FakeProvider::new();
    let starts = provider.starts.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let row = service
        .resume_sandbox(PUBLIC_ID, 1)
        .await
        .expect("idempotent on a running sandbox");

    assert_eq!(row.status, "running");
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn startup_reconciliation_stops_rows_whose_vm_is_down() {
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let rows = vec![row_with(PUBLIC_ID, "ephemeral", "running")];
    let (report, sql) = with_statement_log(
        "reconcile-dead",
        [vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]],
        1,
        provider,
        |service| async move { service.reconcile_running_vms(rows).await },
    )
    .await;

    assert_eq!(
        report,
        VmReconcileReport {
            checked: 1,
            alive: 0,
            marked_stopped: vec![PUBLIC_ID.to_string()],
            marked_stopped_total: 1,
            not_marked: 0,
            unverified: 0,
        }
    );
    assert!(sql.contains("vm_lost"), "{sql}");
}

/// A resume that lands after the probe moves `last_activity_at`; the
/// verdict about the old row must not stop the sandbox that came back.
#[tokio::test]
async fn a_stale_verdict_is_pinned_to_the_activity_timestamp_it_was_read_at() {
    let row = row_with(PUBLIC_ID, "ephemeral", "running");
    let cause = AgentError::SandboxNotRunning {
        run_id: 7,
        sandbox_id: PUBLIC_ID.into(),
        container: "temps-sandbox-deadbeef01234567".into(),
        backend: "firecracker".into(),
        reason: "its firecracker process exited".into(),
    };
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let (_, sql) = with_statement_log(
        "mark-pinned",
        [Vec::<sandboxes::Model>::new()],
        0,
        provider,
        |service| async move { service.mark_vm_lost(&row, &cause, "startup").await },
    )
    .await;

    // The statement log is the Debug form of the transaction, so the
    // quotes around identifiers are escaped.
    assert!(sql.contains(r#"last_activity_at\" = $4"#), "{sql}");
    assert!(sql.contains("ChronoDateTimeUtc"), "{sql}");
    assert!(sql.contains(r#"String(Some("running"))"#), "{sql}");
}

/// A down VM whose row is not rewritten (here a managed application
/// workspace) is counted, so the startup log cannot claim every VM is
/// alive.
#[tokio::test]
async fn startup_reconciliation_counts_down_rows_it_did_not_rewrite() {
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let mut managed = row_with(PUBLIC_ID, "ephemeral", "running");
    managed.name = format!("{APPLICATION_WORKSPACE_NAME_PREFIX}app_0123456789abcdef");
    let (report, sql) = with_statement_log(
        "reconcile-managed",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        provider,
        |service| async move { service.reconcile_running_vms(vec![managed]).await },
    )
    .await;

    assert_eq!(report.checked, 1);
    assert_eq!(report.alive, 0);
    assert_eq!(report.not_marked, 1);
    assert!(report.marked_stopped.is_empty());
    assert!(!report.all_alive());
    assert!(!sql.contains("UPDATE"), "{sql}");
}

/// A restart that took down many VMs still yields a small report: every
/// row is moved and counted, but only a bounded sample of ids is kept.
#[tokio::test]
async fn the_startup_report_names_a_bounded_sample_of_ids() {
    let provider = FakeProvider::new();
    provider.alive.store(false, Ordering::SeqCst);
    let total = VM_RECONCILE_REPORTED_IDS + 10;
    let rows: Vec<sandboxes::Model> = (0..total)
        .map(|index| {
            let mut row = row_with(PUBLIC_ID, "ephemeral", "running");
            row.id = i32::try_from(index).unwrap_or(i32::MAX) + 1;
            row
        })
        .collect();
    // Each move is an UPDATE then a `vm_lost` event insert, so the mock's
    // results alternate row, event.
    let data_root = unique_data_root("reconcile-many");
    let mut mock = MockDatabase::new(DatabaseBackend::Postgres);
    for _ in 0..total {
        mock = mock
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
            .append_query_results([vec![destroyed_event()]]);
    }
    let (service, _registry) = build_service_shared(
        Arc::new(mock.into_connection()),
        provider,
        data_root.clone(),
    );
    let report = service.reconcile_running_vms(rows).await;
    let _ = std::fs::remove_dir_all(&data_root);

    assert_eq!(report.checked, total);
    assert_eq!(report.marked_stopped_total, total);
    assert_eq!(report.marked_stopped.len(), VM_RECONCILE_REPORTED_IDS);
    assert_eq!(report.not_marked, 0);
}

#[tokio::test]
async fn startup_reconciliation_leaves_live_vms_alone() {
    let rows = vec![row_with(PUBLIC_ID, "ephemeral", "running")];
    let (report, sql) = with_statement_log(
        "reconcile-alive",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        FakeProvider::new(),
        |service| async move { service.reconcile_running_vms(rows).await },
    )
    .await;

    assert_eq!(report.checked, 1);
    assert_eq!(report.alive, 1);
    assert!(report.marked_stopped.is_empty());
    assert!(!sql.contains("UPDATE"), "{sql}");
}

/// When the liveness check cannot say either way the row stays as it is,
/// and the report counts it so the startup log can say so.
#[tokio::test]
async fn startup_reconciliation_does_not_guess_when_the_check_fails() {
    let provider = FakeProvider::new();
    provider.alive_check_fails.store(true, Ordering::SeqCst);
    let rows = vec![row_with(PUBLIC_ID, "ephemeral", "running")];
    let (report, sql) = with_statement_log(
        "reconcile-unknown",
        Vec::<Vec<sandboxes::Model>>::new(),
        0,
        provider,
        |service| async move { service.reconcile_running_vms(rows).await },
    )
    .await;

    assert_eq!(report.checked, 1);
    assert_eq!(report.unverified, 1);
    assert!(report.marked_stopped.is_empty());
    assert!(!sql.contains("UPDATE"), "{sql}");
}

/// An id with no row at all is still a plain 404: the 409 is only for
/// sandboxes that exist.
#[tokio::test]
async fn an_id_with_no_row_is_still_not_found() {
    let (result, _) = with_statement_log(
        "no-row",
        [Vec::<sandboxes::Model>::new()],
        0,
        FakeProvider::new(),
        |service| async move { service.resolve_id(PUBLIC_ID, 1).await },
    )
    .await;

    let err = result.expect_err("no such sandbox");
    assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
    assert_eq!(
        temps_core::problemdetails::Problem::from(err).status_code,
        axum::http::StatusCode::NOT_FOUND
    );
}

/// An ephemeral sandbox the reconciliation stopped tells the caller how
/// to continue instead of a bare "stopped".
#[tokio::test]
async fn a_stopped_ephemeral_sandbox_says_how_to_resume_it() {
    let (result, _) = with_statement_log(
        "stopped-hint",
        [vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]],
        0,
        FakeProvider::new(),
        |service| async move { service.resolve_id(PUBLIC_ID, 1).await },
    )
    .await;

    let message = result.expect_err("stopped ephemeral").to_string();
    assert!(message.contains("stopped"), "{message}");
    assert!(
        message.contains(&format!("sandbox resume {PUBLIC_ID}")),
        "{message}"
    );
}

// ── Workspace-from-project (ADR-036 §5) ──────────────────────────────

fn project_row(git_url: Option<&str>) -> projects::Model {
    let now = Utc::now();
    projects::Model {
        id: 12,
        name: "acme-api".into(),
        repo_name: "api".into(),
        repo_owner: "acme".into(),
        directory: String::new(),
        pull_only_root_directory: false,
        main_branch: "main".into(),
        preset: temps_entities::preset::Preset::NextJs,
        preset_config: None,
        deployment_config: None,
        created_at: now,
        updated_at: now,
        slug: "acme-api".into(),
        is_deleted: false,
        deleted_at: None,
        last_deployment: None,
        is_public_repo: true,
        git_url: git_url.map(str::to_string),
        git_provider_connection_id: Some(3),
        attack_mode: false,
        shield_mode: "log".to_string(),
        ai_alert_summaries_enabled: None,
        ai_api_traffic_summary_enabled: None,
        allow_alternate_sources: None,
        hosting: Default::default(),
        ai_debug_chat_enabled: None,
        ai_write_actions_enabled: false,
        error_source_context_enabled: false,
        vulnerability_scanning_enabled: false,
        error_source_root: None,
        enable_preview_environments: false,
        preview_envs_on_demand: false,
        preview_envs_idle_timeout_seconds: 0,
        preview_envs_wake_timeout_seconds: 0,
        source_type: Default::default(),
        project_type: temps_entities::types::ProjectType::Server,
        template_slug: None,
        service_template: None,
        gitlab_webhook_id: None,
        gitlab_webhook_signing_token: None,
        gitea_webhook_signing_token: None,
        github_webhook_id: None,
        github_webhook_signing_token: None,
        bitbucket_webhook_token: None,
        bitbucket_webhook_hook_id: None,
        generic_webhook_token: None,
        cross_project_trace_sharing: false,
        serves_own_temps_api: false,
        image_retention_hours: None,
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
        cloud_telemetry_fidelity:
            temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity::Metered,
        cloud_telemetry_write_mode:
            temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode::Local,
        cloud_analytics_write_mode:
            temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode::Local,
        instantiation_token: None,
        cloud_telemetry_attribute_allowlist: Vec::new(),
    }
}

#[tokio::test]
async fn source_from_project_uses_the_projects_repo_branch_and_connection() {
    let data_root = unique_data_root("project-source-ok");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row(Some("https://github.com/acme/api.git"))]])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let source = service
        .source_from_project(12)
        .await
        .expect("a connected project yields a git source");

    match source.expect("a Git project has a seed source") {
        SandboxSource::Git {
            url,
            revision,
            depth,
            git_connection_id,
            username,
            password,
            destination,
            strip_git_metadata,
        } => {
            assert_eq!(url, "https://github.com/acme/api.git");
            assert_eq!(revision.as_deref(), Some("main"));
            assert!(
                depth.is_none(),
                "a workspace gets full history — you cannot rebase in a shallow clone"
            );
            assert_eq!(git_connection_id, Some(3));
            assert!(destination.is_none());
            assert!(!strip_git_metadata);
            assert!(
                username.is_none() && password.is_none(),
                "credentials must resolve server-side from the connection, \
                     never be materialised here"
            );
        }
        other => panic!("expected a git source, got {:?}", other),
    }
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn source_from_project_errors_when_the_project_has_no_repo() {
    let data_root = unique_data_root("project-no-repo");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row(None)]])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let err = service
        .source_from_project(12)
        .await
        .expect_err("a project with no git_url cannot seed a workspace");

    match err {
        SandboxError::ProjectHasNoRepo { project_id, name } => {
            assert_eq!(project_id, 12);
            assert_eq!(name, "acme-api");
        }
        other => panic!("expected ProjectHasNoRepo, got {:?}", other),
    }
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn source_from_project_allows_an_empty_workspace_for_manual_projects() {
    let data_root = unique_data_root("manual-project-no-repo");
    let mut manual_project = project_row(None);
    manual_project.source_type = temps_entities::source_type::SourceType::Manual;
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![manual_project]])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let source = service
        .source_from_project(12)
        .await
        .expect("a manual project may start with an empty workspace");

    assert!(source.is_none());
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn source_from_project_errors_when_the_project_is_missing() {
    let data_root = unique_data_root("project-missing");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<projects::Model>::new()])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let err = service
        .source_from_project(999)
        .await
        .expect_err("a missing project is a 404, not a silent empty workspace");

    assert!(matches!(
        err,
        SandboxError::ProjectNotFound { project_id: 999 }
    ));
    let _ = std::fs::remove_dir_all(&data_root);
}

// ── Resolved-source validation ───────────────────────────────────────

#[test]
fn all_embedded_userinfo_is_rejected() {
    assert!(url_has_embedded_credentials(
        "https://user:secret@github.com/org/repo.git"
    ));
    // A bare user can itself be a token or sensitive account identifier.
    assert!(url_has_embedded_credentials(
        "https://user@github.com/org/repo.git"
    ));
    assert!(!url_has_embedded_credentials(
        "https://github.com/org/repo.git"
    ));
    // An `@` in the *path* must not be mistaken for userinfo, or every
    // pinned-ref URL would be rejected as leaking a password.
    assert!(!url_has_embedded_credentials(
        "https://github.com/org/repo@v1.git"
    ));
    assert!(!url_has_embedded_credentials("git@github.com:org/repo.git"));
    // A `:` and `@` in the query or fragment are not userinfo either —
    // userinfo always precedes the first `/`, `?` or `#`. Splitting on
    // `/` alone read this as `user:password` and rejected a valid URL.
    assert!(!url_has_embedded_credentials(
        "https://github.com/org/repo.git?ref=a:b@c"
    ));
    assert!(!url_has_embedded_credentials("https://host?ref=a:b@c"));
    assert!(!url_has_embedded_credentials("https://host#frag=a:b@c"));
    // ...but real userinfo before a query is still caught.
    assert!(url_has_embedded_credentials(
        "https://user:pw@github.com/org/repo.git?ref=main"
    ));
}

#[test]
fn credentials_only_enter_temps_managed_sandbox_images() {
    let mut managed = handle_for("managed");
    managed.image = "ghcr.io/gotempsh/temps-sandbox-node:v1".to_string();
    assert!(sandbox_image_is_trusted_for_credentials(&managed));

    let mut custom = handle_for("custom");
    custom.image = "registry.example/customer/image:latest".to_string();
    assert!(!sandbox_image_is_trusted_for_credentials(&custom));

    let mut local = handle_for("local");
    local.backend = SandboxBackend::Local;
    local.image = managed.image;
    assert!(!sandbox_image_is_trusted_for_credentials(&local));
}

#[test]
fn privileged_imports_clear_image_startup_hooks() {
    let mut environment = HashMap::from([
        ("LD_PRELOAD".to_string(), "/workspace/steal.so".to_string()),
        ("BASH_ENV".to_string(), "/workspace/startup.sh".to_string()),
        ("GIT_PASS".to_string(), "ephemeral".to_string()),
    ]);

    sanitize_privileged_import_environment(&mut environment);

    assert_eq!(environment.get("LD_PRELOAD").map(String::as_str), Some(""));
    assert_eq!(environment.get("BASH_ENV").map(String::as_str), Some(""));
    assert_eq!(
        environment.get("GIT_PASS").map(String::as_str),
        Some("ephemeral")
    );
    for name in ["HOME", "CURL_HOME", "XDG_CONFIG_HOME"] {
        assert_eq!(
            environment.get(name).map(String::as_str),
            Some("/var/empty/temps-source-import")
        );
    }
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "GIT_CONFIG_PARAMETERS",
        "GIT_SSL_CAINFO",
    ] {
        assert_eq!(environment.get(name).map(String::as_str), Some(""));
    }
    assert_eq!(environment.get("NO_PROXY").map(String::as_str), Some("*"));
    assert_eq!(environment.get("no_proxy").map(String::as_str), Some("*"));
    assert_eq!(
        environment.get("GIT_CONFIG_COUNT").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        environment.get("GIT_SSL_NO_VERIFY").map(String::as_str),
        Some("false")
    );
}

#[test]
fn source_network_pin_formats_curl_resolve_values() {
    let ipv4 = SourceNetworkPin {
        host: "git.example".to_string(),
        port: 443,
        address: "203.0.113.8".parse().expect("IPv4 fixture"),
    };
    assert_eq!(ipv4.curl_resolve_value(), "git.example:443:203.0.113.8");

    let ipv6 = SourceNetworkPin {
        host: "git.example".to_string(),
        port: 443,
        address: "2001:db8::8".parse().expect("IPv6 fixture"),
    };
    assert_eq!(ipv6.curl_resolve_value(), "git.example:443:[2001:db8::8]");
}

#[test]
fn tarball_import_disables_image_curl_configuration_before_other_flags() {
    let command = tarball_extract_command(
        "https://downloads.example/source.tar.gz",
        "/tmp/staging",
        None,
    );

    assert!(
        command.contains(" curl -q "),
        "unexpected command: {command}"
    );
    assert!(command.starts_with("env -i "));
    assert!(command.contains("--noproxy '*' --proxy ''"));
    assert!(command.contains("--max-redirs 0"));
    assert!(command.contains("| env -i "));
}

#[test]
fn git_import_uses_an_allowlisted_environment_without_secret_values() {
    let environment = clean_git_environment(
        "/root/.temps-source-auth-7",
        Some("/root/.temps-source-auth-7/askpass.sh"),
    );

    assert!(environment.starts_with("env -i "));
    assert!(environment.contains("GIT_CONFIG_COUNT=0"));
    assert!(environment.contains("GIT_SSL_NO_VERIFY=false"));
    assert!(environment.contains("GIT_ASKPASS="));
    assert!(!environment.contains("GIT_PASS="));
    assert!(!environment.contains("GIT_USER="));
    assert!(!environment.contains("GIT_TRACE"));
}

fn git_provider(
    provider_type: &str,
    base_url: Option<&str>,
) -> temps_entities::git_providers::Model {
    let now = Utc::now();
    temps_entities::git_providers::Model {
        id: 7,
        name: "fixture".to_string(),
        provider_type: provider_type.to_string(),
        base_url: base_url.map(str::to_string),
        api_url: None,
        auth_method: "pat".to_string(),
        auth_config: serde_json::json!({}),
        webhook_secret: None,
        is_active: true,
        is_default: false,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn stored_git_credentials_are_bound_to_the_provider_origin() {
    let github = git_provider("github", None);
    assert!(validate_connection_clone_origin("https://github.com/acme/repo.git", &github).is_ok());
    assert!(
        validate_connection_clone_origin("https://credentials.example/acme/repo.git", &github)
            .is_err()
    );

    let enterprise = git_provider("github", Some("https://git.example.test:8443"));
    assert!(validate_connection_clone_origin(
        "https://git.example.test:8443/acme/repo.git",
        &enterprise
    )
    .is_ok());
    assert!(validate_connection_clone_origin(
        "https://git.example.test/acme/repo.git",
        &enterprise
    )
    .is_err());
}

#[tokio::test]
async fn resolved_source_rejects_embedded_credentials() {
    // This is the project-derived path's guard: a legacy `projects.git_url`
    // carrying a password would otherwise be persisted to
    // `source_repo_url` and echoed back out of the API.
    let source = SandboxSource::Git {
        url: "https://user:secret@github.com/org/repo.git".into(),
        revision: None,
        depth: None,
        username: None,
        password: None,
        git_connection_id: None,
        destination: None,
        strip_git_metadata: false,
    };
    let err = validate_resolved_source(&source)
        .await
        .expect_err("credentials in the url must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("embedded credentials"), "msg: {msg}");
    // The message must not echo the secret back at the caller.
    assert!(!msg.contains("secret"), "error leaked the password: {msg}");
}

#[tokio::test]
async fn resolved_source_rejects_loopback_and_metadata_hosts() {
    for url in [
        "http://127.0.0.1/repo.git",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/repo.git",
    ] {
        let source = SandboxSource::Git {
            url: url.into(),
            revision: None,
            depth: None,
            username: None,
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        assert!(
            validate_resolved_source(&source).await.is_err(),
            "{url} must be rejected as an SSRF target"
        );
    }
}

#[tokio::test]
async fn resolved_source_rejects_non_http_schemes() {
    let source = SandboxSource::Tarball {
        url: "file:///etc/passwd".into(),
    };
    assert!(validate_resolved_source(&source).await.is_err());
}

#[tokio::test]
async fn destroy_sandbox_removes_the_work_dir() {
    let data_root = unique_data_root("destroy-ok");
    let dir = seed_work_dir(&data_root, PUBLIC_ID);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_public_id
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        // record_event insert (RETURNING)
        .append_query_results([vec![sandbox_events::Model {
            id: 1,
            sandbox_id: 7,
            event_type: "destroyed".into(),
            detail: None,
            created_at: Utc::now(),
        }]])
        // mark_destroyed update (RETURNING)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    service
        .destroy_sandbox(PUBLIC_ID, 1)
        .await
        .expect("destroy should succeed");

    assert!(
        !dir.exists(),
        "destroy must remove the work dir {} — nothing else ever will, \
             so leaving it is a permanent leak",
        dir.display()
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

/// The documented tradeoff: with no background sweep, skipping cleanup
/// when the provider destroy fails would leak the directory forever, so
/// we delete regardless. If someone reintroduces a `container_gone`
/// guard around the call, this goes red.
#[tokio::test]
async fn destroy_sandbox_removes_the_work_dir_even_when_provider_destroy_fails() {
    let data_root = unique_data_root("destroy-fail");
    let dir = seed_work_dir(&data_root, PUBLIC_ID);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .append_query_results([vec![sandbox_events::Model {
            id: 1,
            sandbox_id: 7,
            event_type: "destroyed".into(),
            detail: None,
            created_at: Utc::now(),
        }]])
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    let mut provider = FakeProvider::new();
    provider.fail_destroy = true;
    let (service, _) = build_service(db, provider, data_root.clone());

    // The user must still be able to delete a sandbox when Docker is
    // unhappy — the call succeeds and the row is marked destroyed.
    service
        .destroy_sandbox(PUBLIC_ID, 1)
        .await
        .expect("destroy must succeed even when the provider fails");

    assert!(
        !dir.exists(),
        "work dir {} must be removed even when the provider destroy \
             failed — there is no sweeper to reclaim it later",
        dir.display()
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

/// A node that accepts connections but never answers must not hold an
/// eviction for the provider's lifecycle timeout: the first destroy
/// gives up at the gate's deadline, the rest skip the container call,
/// and every row is still marked destroyed.
#[tokio::test]
async fn eviction_gives_up_on_a_node_that_never_answers() {
    let data_root = unique_data_root("evict-hang");
    let event = |id| sandbox_events::Model {
        id,
        sandbox_id: 7,
        event_type: "destroyed".into(),
        detail: None,
        created_at: Utc::now(),
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![event(1)]])
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .append_query_results([vec![event(2)]])
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut provider = FakeProvider::new();
    provider.hang_destroy = Some(attempts.clone());
    let (service, _) = build_service(db, provider, data_root.clone());
    let gate = EvictionGate::new(std::time::Duration::from_millis(50));

    let started = std::time::Instant::now();
    let first = service
        .destroy_row(row(PUBLIC_ID, None), true, DestroyMode::Evict(&gate))
        .await
        .expect("the row is marked destroyed even though the node hung");
    assert!(gate.is_down(), "a timeout marks the node down");
    let second = service
        .destroy_row(row(PUBLIC_ID, None), true, DestroyMode::Evict(&gate))
        .await
        .expect("later rows are still marked destroyed");

    // Neither container is confirmed gone, and the caller is told so.
    assert!(
        matches!(&first, ContainerOutcome::Unconfirmed(r) if r.contains("did not answer")),
        "{first:?}"
    );
    assert!(
        matches!(&second, ContainerOutcome::Unconfirmed(r) if r.contains("skipped")),
        "{second:?}"
    );

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "once the node is down, later destroys skip the container call"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&data_root);
}

/// A node that answered once is never written off because a later call
/// was slow or unreachable; one that never answered is.
#[test]
fn eviction_gate_writes_off_only_a_node_that_never_answered() {
    let silent = EvictionGate::new(std::time::Duration::from_secs(1));
    silent.unanswered();
    assert!(silent.is_down());

    let slow = EvictionGate::new(std::time::Duration::from_secs(1));
    slow.answered();
    slow.unanswered();
    assert!(!slow.is_down(), "one slow reply must not skip the rest");
}

/// A node that answered at first and then froze part-way through an
/// eviction is written off after one round of unanswered calls, not
/// waited on for every remaining sandbox.
#[test]
fn eviction_gate_writes_off_a_node_that_stops_answering() {
    let gate = EvictionGate::new(std::time::Duration::from_secs(1));
    gate.answered();
    for _ in 1..EVICTION_UNANSWERED_LIMIT {
        gate.unanswered();
    }
    assert!(!gate.is_down(), "fewer than a round of failures");
    gate.answered();
    for _ in 1..EVICTION_UNANSWERED_LIMIT {
        gate.unanswered();
    }
    assert!(!gate.is_down(), "an answer resets the count");
    gate.unanswered();
    assert!(gate.is_down(), "a full round in a row writes it off");
}

fn worker_row(node_id: i32) -> sandboxes::Model {
    sandboxes::Model {
        node_id: Some(node_id),
        ..row(PUBLIC_ID, None)
    }
}

fn destroyed_event() -> sandbox_events::Model {
    sandbox_events::Model {
        id: 1,
        sandbox_id: 7,
        event_type: "destroyed".into(),
        detail: None,
        created_at: Utc::now(),
    }
}

fn single(deadline_ms: u64) -> DestroyMode<'static> {
    DestroyMode::Single {
        worker_deadline: std::time::Duration::from_millis(deadline_ms),
    }
}

/// ADR-048 §7: destroying a sandbox whose worker is unreachable must
/// not mark it destroyed — that would leave its container running with
/// nothing tracking it. The row and its files are kept, and the caller
/// gets a 503 naming the node.
#[tokio::test]
async fn destroying_a_sandbox_on_an_unreachable_worker_keeps_it() {
    let data_root = unique_data_root("destroy-unreachable");
    let dir = seed_work_dir(&data_root, PUBLIC_ID);
    // Only the node-name lookup may run; a mark_destroyed would find
    // no queued result and fail the test with a database error.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::nodes::Model>::new()])
        .into_connection();
    let mut provider = FakeProvider::new();
    provider.fail_destroy = true;
    let (service, _) = build_service(db, provider, data_root.clone());

    let error = service
        .destroy_row(worker_row(3), false, single(1_000))
        .await
        .expect_err("an unreachable worker must not look like a destroy");

    match &error {
        SandboxError::NodeUnreachable {
            sandbox_id,
            node,
            reason,
        } => {
            assert_eq!(sandbox_id, PUBLIC_ID);
            assert_eq!(node, "id 3");
            assert!(reason.contains("daemon unreachable"), "{reason}");
        }
        other => panic!("expected NodeUnreachable, got {other:?}"),
    }
    assert!(error.to_string().contains("sandbox nodes evict"), "{error}");
    assert!(dir.exists(), "a kept sandbox keeps its files");
    let _ = std::fs::remove_dir_all(&data_root);
}

/// A worker that accepts the connection but never answers gives up at
/// the deadline instead of holding the request for the provider's
/// lifecycle timeout, and the sandbox is kept.
#[tokio::test]
async fn destroying_a_sandbox_on_a_frozen_worker_gives_up_and_keeps_it() {
    let data_root = unique_data_root("destroy-frozen");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::nodes::Model>::new()])
        .into_connection();
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut provider = FakeProvider::new();
    provider.hang_destroy = Some(attempts.clone());
    let (service, _) = build_service(db, provider, data_root.clone());

    let started = std::time::Instant::now();
    let error = service
        .destroy_row(worker_row(3), false, single(50))
        .await
        .expect_err("a frozen worker must not look like a destroy");

    assert!(
        matches!(&error, SandboxError::NodeUnreachable { reason, .. } if reason.contains("did not answer")),
        "{error:?}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&data_root);
}

/// A worker that answers destroys the sandbox like the control plane.
#[tokio::test]
async fn destroying_a_sandbox_on_a_reachable_worker_marks_it_destroyed() {
    let data_root = unique_data_root("destroy-worker-ok");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![destroyed_event()]])
        .append_query_results([vec![worker_row(3)]])
        .into_connection();
    let provider = FakeProvider::new();
    let purges = provider.destroy_purge_flags.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let outcome = service
        .destroy_row(worker_row(3), false, single(1_000))
        .await
        .expect("destroyed");

    assert_eq!(outcome, ContainerOutcome::Destroyed);
    assert_eq!(purges.lock().expect("purge flags").len(), 1);
    let _ = std::fs::remove_dir_all(&data_root);
}

fn live_row(status: &str) -> sandboxes::Model {
    sandboxes::Model {
        status: status.to_string(),
        ..worker_row(3)
    }
}

/// The row survived the create: nothing is torn down.
#[tokio::test]
async fn worker_create_confirms_a_row_that_is_still_live() {
    let data_root = unique_data_root("confirm-live");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![live_row("running")]])
        .into_connection();
    let provider = FakeProvider::new();
    let purges = provider.destroy_purge_flags.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    service
        .confirm_worker_create(&worker_row(3), 1)
        .await
        .expect("still live");

    assert!(purges.lock().expect("purge flags").is_empty());
    let _ = std::fs::remove_dir_all(&data_root);
}

/// An eviction destroyed the row while the container was being
/// created: the container is removed and the create fails, telling the
/// user to create it again.
#[tokio::test]
async fn worker_create_removes_the_container_when_its_row_was_evicted() {
    let data_root = unique_data_root("confirm-evicted");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![live_row("destroyed")]])
        .append_query_results([vec![live_row("destroyed")]])
        .into_connection();
    let provider = FakeProvider::new();
    let purges = provider.destroy_purge_flags.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let error = service
        .confirm_worker_create(&worker_row(3), 1)
        .await
        .expect_err("the row is gone");

    assert!(
        matches!(&error, SandboxError::CreateFailed { reason, .. } if reason.contains("destroyed while it was being created")),
        "{error:?}"
    );
    assert_eq!(purges.lock().expect("purge flags").len(), 1);
    let _ = std::fs::remove_dir_all(&data_root);
}

/// An eviction of the node started after placement checked the cordon
/// but before the row existed, so it never listed this row: the create
/// removes its container and fails instead of surviving the eviction.
#[tokio::test]
async fn worker_create_yields_to_an_eviction_that_started_during_it() {
    let data_root = unique_data_root("confirm-cordoned");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![live_row("running")]])
        .append_query_results([vec![live_row("destroyed")]])
        .into_connection();
    let provider = FakeProvider::new();
    let purges = provider.destroy_purge_flags.clone();
    let (service, _) = build_service(db, provider, data_root.clone());
    let row = worker_row(3);
    let node_id = row.node_id.expect("a worker row");
    let _eviction = service
        .evicting_nodes
        .begin(node_id)
        .expect("eviction running");

    let error = service
        .confirm_worker_create(&row, 1)
        .await
        .expect_err("the node is being evicted");

    assert!(
        matches!(&error, SandboxError::CreateFailed { reason, .. } if reason.contains("started evicting")),
        "{error:?}"
    );
    assert_eq!(purges.lock().expect("purge flags").len(), 1);
    let _ = std::fs::remove_dir_all(&data_root);
}

/// If the row can't be read back, the create fails closed and removes
/// the container rather than risk one that nothing tracks.
#[tokio::test]
async fn worker_create_fails_closed_when_the_row_cannot_be_read() {
    let data_root = unique_data_root("confirm-db-error");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())])
        .into_connection();
    let provider = FakeProvider::new();
    let purges = provider.destroy_purge_flags.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let error = service
        .confirm_worker_create(&worker_row(3), 1)
        .await
        .expect_err("fail closed");

    assert!(
        matches!(&error, SandboxError::CreateFailed { reason, .. } if reason.contains("could not confirm")),
        "{error:?}"
    );
    assert_eq!(purges.lock().expect("purge flags").len(), 1);
    let _ = std::fs::remove_dir_all(&data_root);
}

/// A destroy or eviction that lands while a pause waits on the worker
/// wins: the pause reports the sandbox gone and does not write
/// `stopped` over `destroyed` (which would block removing the node).
#[tokio::test]
async fn pause_does_not_resurrect_a_sandbox_destroyed_meanwhile() {
    let data_root = unique_data_root("pause-destroyed");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![worker_row(3)]])
        // The conditional UPDATE matched nothing...
        .append_query_results([Vec::<sandboxes::Model>::new()])
        // ...because the row is destroyed now.
        .append_query_results([vec![live_row("destroyed")]])
        .into_connection();
    let provider = FakeProvider::new();
    let calls = provider.lifecycle_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let err = service
        .pause_sandbox(PUBLIC_ID, 1)
        .await
        .expect_err("a destroyed sandbox is not paused");

    assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
    assert_eq!(*calls.lock().expect("calls"), vec!["stop"]);
    let _ = std::fs::remove_dir_all(&data_root);
}

/// The expiry sweep stopped the sandbox while the pause was in flight:
/// the pause is still a success (it is idempotent on stopped sandboxes).
#[tokio::test]
async fn pause_accepts_a_sandbox_the_sweep_stopped_meanwhile() {
    let data_root = unique_data_root("pause-swept");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![worker_row(3)]])
        .append_query_results([Vec::<sandboxes::Model>::new()])
        .append_query_results([vec![live_row("stopped")]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let row = service
        .pause_sandbox(PUBLIC_ID, 1)
        .await
        .expect("already stopped");

    assert_eq!(row.status, "stopped");
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn pause_writes_stopped_only_over_a_running_row() {
    let data_root = unique_data_root("pause-running");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![worker_row(3)]])
        .append_query_results([vec![live_row("stopped")]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let row = service.pause_sandbox(PUBLIC_ID, 1).await.expect("paused");

    assert_eq!(row.status, "stopped");
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resume_does_not_resurrect_a_sandbox_destroyed_meanwhile() {
    let data_root = unique_data_root("resume-destroyed");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![live_row("stopped")]])
        .append_query_results([Vec::<sandboxes::Model>::new()])
        .append_query_results([vec![live_row("destroyed")]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let err = service
        .resume_sandbox(PUBLIC_ID, 1)
        .await
        .expect_err("a destroyed sandbox is not resumed");

    assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
    let _ = std::fs::remove_dir_all(&data_root);
}

fn restricted_egress() -> temps_ai_agents::sandbox::SandboxEgressPolicy {
    temps_ai_agents::sandbox::SandboxEgressPolicy::restricted(vec![
        temps_ai_agents::sandbox::EgressRule::host("API.example.com", &[443]),
        temps_ai_agents::sandbox::EgressRule::cidr("1.1.1.0/24", &[]),
    ])
}

fn with_egress(
    row: sandboxes::Model,
    policy: &temps_ai_agents::sandbox::SandboxEgressPolicy,
) -> sandboxes::Model {
    sandboxes::Model {
        egress_policy: sandboxes::StoredEgressPolicy(policy.normalized().expect("valid")),
        ..row
    }
}

fn egress_event() -> sandbox_events::Model {
    sandbox_events::Model {
        event_type: "egress_policy_updated".into(),
        ..destroyed_event()
    }
}

#[tokio::test]
async fn an_egress_policy_is_applied_before_it_is_stored() {
    let data_root = unique_data_root("egress-set");
    let policy = restricted_egress();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
        .append_query_results([vec![with_egress(
            row_with(PUBLIC_ID, "ephemeral", "running"),
            &policy,
        )]])
        .append_query_results([vec![egress_event()]])
        .into_connection();
    let db = Arc::new(db);
    let provider = FakeProvider::new();
    let calls = provider.egress_calls.clone();
    let (service, _) = build_service_shared(db.clone(), provider, data_root.clone());

    let updated = service
        .set_egress_policy(PUBLIC_ID, 1, policy.clone())
        .await
        .expect("applied and stored");

    let normalized = policy.normalized().expect("valid");
    assert_eq!(updated.egress_policy.0, normalized);
    // The backend got the normalized policy, once.
    assert_eq!(*calls.lock().expect("calls"), vec![normalized]);
    drop(service);
    let log = Arc::try_unwrap(db)
        .expect("only handle")
        .into_transaction_log();
    let sql = format!("{log:?}");
    assert!(sql.contains("egress_policy"), "the row is written: {sql}");
    assert!(
        sql.contains("egress_policy_updated"),
        "an event is recorded: {sql}"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn an_invalid_egress_policy_never_reaches_the_backend() {
    let data_root = unique_data_root("egress-invalid");
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let provider = FakeProvider::new();
    let calls = provider.egress_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let err = service
        .set_egress_policy(
            PUBLIC_ID,
            1,
            temps_ai_agents::sandbox::SandboxEgressPolicy::restricted(vec![
                temps_ai_agents::sandbox::EgressRule::cidr("10.0.0.0/8", &[]),
            ]),
        )
        .await
        .expect_err("a private range can never be allowed");

    assert!(matches!(err, SandboxError::Validation { .. }), "{err:?}");
    let text = err.to_string();
    assert!(
        text.contains(PUBLIC_ID) && text.contains("10.0.0.0/8"),
        "{text}"
    );
    assert!(calls.lock().expect("calls").is_empty());
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn a_backend_that_cannot_enforce_a_restricted_policy_is_a_typed_refusal() {
    let data_root = unique_data_root("egress-unsupported");
    // Only the lookup: no write may follow a refusal.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
        .into_connection();
    let provider = FakeProvider {
        fail_egress: true,
        ..FakeProvider::new()
    };
    let (service, _) = build_service(db, provider, data_root.clone());

    let err = service
        .set_egress_policy(PUBLIC_ID, 1, restricted_egress())
        .await
        .expect_err("refused");

    assert!(
        matches!(&err, SandboxError::EgressPolicyUnsupported { sandbox_id, backend }
                if sandbox_id == PUBLIC_ID && backend == "firecracker"),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn a_policy_that_cannot_be_stored_is_rolled_back_on_the_backend() {
    let data_root = unique_data_root("egress-rollback");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
        // The conditional write matches nothing: destroyed meanwhile.
        .append_query_results([Vec::<sandboxes::Model>::new()])
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "destroyed")]])
        .into_connection();
    let provider = FakeProvider::new();
    let calls = provider.egress_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let err = service
        .set_egress_policy(PUBLIC_ID, 1, restricted_egress())
        .await
        .expect_err("destroyed meanwhile");

    assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
    assert_eq!(
        *calls.lock().expect("calls"),
        vec![
            restricted_egress().normalized().expect("valid"),
            temps_ai_agents::sandbox::SandboxEgressPolicy::internet(),
        ],
        "the stored policy is put back"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn the_egress_policy_of_an_agent_run_sandbox_is_not_changed_here() {
    let data_root = unique_data_root("egress-agent-run");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row(PUBLIC_ID, Some(42))]])
        .into_connection();
    let provider = FakeProvider::new();
    let calls = provider.egress_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let err = service
        .set_egress_policy(PUBLIC_ID, 1, restricted_egress())
        .await
        .expect_err("managed by the run");

    assert!(
        matches!(err, SandboxError::ManagedByAgentRun { run_id: 42, .. }),
        "{err:?}"
    );
    assert!(calls.lock().expect("calls").is_empty());
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resume_reapplies_a_stored_restricted_policy() {
    let data_root = unique_data_root("egress-resume");
    let policy = restricted_egress();
    let stopped = with_egress(row_with(PUBLIC_ID, "ephemeral", "stopped"), &policy);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![stopped.clone()]])
        // enforce_stored_egress_policy re-reads the row.
        .append_query_results([vec![stopped.clone()]])
        .append_query_results([vec![with_egress(
            row_with(PUBLIC_ID, "ephemeral", "running"),
            &policy,
        )]])
        .append_query_results([vec![destroyed_event()]])
        .into_connection();
    let provider = FakeProvider::new();
    let calls = provider.egress_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    let row = service.resume_sandbox(PUBLIC_ID, 1).await.expect("resumed");

    assert_eq!(row.status, "running");
    assert_eq!(
        *calls.lock().expect("calls"),
        vec![policy.normalized().expect("valid")]
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resume_stops_a_sandbox_whose_restricted_policy_cannot_be_reapplied() {
    let data_root = unique_data_root("egress-resume-fail");
    let stopped = with_egress(
        row_with(PUBLIC_ID, "ephemeral", "stopped"),
        &restricted_egress(),
    );
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([vec![stopped.clone()]])
            // mark_stopped_for_egress_failure: the row and its event.
            .append_query_results([vec![stopped]])
            .append_query_results([vec![destroyed_event()]])
            .into_connection(),
    );
    let provider = FakeProvider {
        fail_egress: true,
        ..FakeProvider::new()
    };
    let lifecycle = provider.lifecycle_calls.clone();
    let (service, _) = build_service_shared(db.clone(), provider, data_root.clone());

    let err = service
        .resume_sandbox(PUBLIC_ID, 1)
        .await
        .expect_err("never runs with wider egress");

    assert!(
        matches!(err, SandboxError::EgressPolicyUnsupported { .. }),
        "{err:?}"
    );
    assert_eq!(*lifecycle.lock().expect("calls"), vec!["start", "stop"]);
    drop(service);
    let sql = format!(
        "{:?}",
        Arc::try_unwrap(db)
            .expect("only handle")
            .into_transaction_log()
    );
    assert!(!sql.contains("\"running\""), "never marked running: {sql}");
    assert!(sql.contains(EGRESS_POLICY_REAPPLY_FAILED), "{sql}");
    let _ = std::fs::remove_dir_all(&data_root);
}

/// The row must not keep saying `running` for a sandbox the service
/// stopped because its egress policy could not be re-applied after a
/// restart; the `stopped` event names why.
#[tokio::test]
async fn restart_marks_stopped_a_sandbox_whose_restricted_policy_cannot_be_reapplied() {
    let data_root = unique_data_root("egress-restart-fail");
    let running = with_egress(
        row_with(PUBLIC_ID, "ephemeral", "running"),
        &restricted_egress(),
    );
    let stopped = with_egress(
        row_with(PUBLIC_ID, "ephemeral", "stopped"),
        &restricted_egress(),
    );
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![running.clone()]])
            // enforce_stored_egress_policy re-reads the row.
            .append_query_results([vec![running]])
            // mark_stopped_for_egress_failure: running -> stopped.
            .append_query_results([vec![stopped]])
            .append_query_results([vec![destroyed_event()]])
            .into_connection(),
    );
    let provider = FakeProvider {
        fail_egress: true,
        ..FakeProvider::new()
    };
    let lifecycle = provider.lifecycle_calls.clone();
    let (service, _) = build_service_shared(db.clone(), provider, data_root.clone());

    let err = service
        .restart_sandbox(PUBLIC_ID, 1)
        .await
        .expect_err("never runs with wider egress");

    assert!(
        matches!(err, SandboxError::EgressPolicyUnsupported { .. }),
        "{err:?}"
    );
    assert_eq!(
        lifecycle.lock().expect("calls").last().copied(),
        Some("stop"),
        "the container is stopped"
    );
    drop(service);
    let log = Arc::try_unwrap(db)
        .expect("only handle")
        .into_transaction_log();
    let sql = format!("{log:?}");
    assert!(
        sql.contains("UPDATE") && sql.contains("\"stopped\""),
        "the row is marked stopped: {sql}"
    );
    assert!(
        sql.contains(EGRESS_POLICY_REAPPLY_FAILED) && sql.contains("egress policy"),
        "the stopped event names the egress policy failure: {sql}"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn resume_of_an_internet_sandbox_does_not_touch_its_policy() {
    let data_root = unique_data_root("egress-resume-internet");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
        .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
        .append_query_results([vec![destroyed_event()]])
        .into_connection();
    let provider = FakeProvider::new();
    let calls = provider.egress_calls.clone();
    let (service, _) = build_service(db, provider, data_root.clone());

    service.resume_sandbox(PUBLIC_ID, 1).await.expect("resumed");

    assert!(calls.lock().expect("calls").is_empty());
    let _ = std::fs::remove_dir_all(&data_root);
}

#[test]
fn eviction_guard_serializes_evictions_of_one_node() {
    let nodes = Arc::new(EvictingNodes::default());
    let first = nodes.begin(3).expect("first eviction");
    assert!(nodes.begin(3).is_none(), "a second eviction is refused");
    let other = nodes.begin(4).expect("other nodes are independent");
    assert_eq!(nodes.snapshot(), std::collections::HashSet::from([3, 4]));
    drop(first);
    assert_eq!(nodes.snapshot(), std::collections::HashSet::from([4]));
    assert!(nodes.begin(3).is_some(), "available again once finished");
    drop(other);
}

fn settings_and_worker(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<temps_entities::settings::Model>::new()])
        .append_query_results([vec![crate::services::placement::tests::node_row(
            "worker",
            "active",
            Some(1),
        )]])
}

/// While a node is being evicted, a second eviction of it is a 409 and
/// an explicit create on it is refused naming the node.
#[tokio::test]
async fn a_node_being_evicted_takes_no_sandboxes_and_no_second_eviction() {
    let data_root = unique_data_root("evicting");
    let db = settings_and_worker(MockDatabase::new(DatabaseBackend::Postgres))
        // live counts for the eviction's node lookup
        .append_query_results([Vec::<sandboxes::Model>::new()])
        // the create's allow-list read (never served from the settings
        // cache, so a malformed row fails placement closed)
        .append_query_results([Vec::<temps_entities::settings::Model>::new()])
        // the create's worker listing
        .append_query_results([vec![crate::services::placement::tests::node_row(
            "worker",
            "active",
            Some(1),
        )]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let held = service.evicting_nodes.begin(7).expect("eviction running");

    let err = service
        .evict_node("worker-7")
        .await
        .expect_err("second eviction");
    assert!(
        matches!(&err, SandboxError::NodeEvictionInProgress { node } if node == "worker-7"),
        "{err:?}"
    );

    let req = CreateSandboxRequest {
        node: Some("worker-7".into()),
        ..Default::default()
    };
    let err = service.place_sandbox(&req).await.expect_err("cordoned");
    assert!(
        matches!(&err, SandboxError::NodeNotReady { node, status }
                if node == "worker-7" && status.contains("evicted")),
        "{err:?}"
    );
    drop(held);
    let _ = std::fs::remove_dir_all(&data_root);
}

/// The per-user cap counts what the user created through the API and
/// nothing else: not destroyed rows, not agent-run sandboxes, not managed
/// application workspaces.
#[tokio::test]
async fn the_user_cap_counts_only_live_standalone_sandboxes() {
    let data_root = unique_data_root("cap-count");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![std::collections::BTreeMap::from([(
            "num_items".to_string(),
            sea_orm::Value::from(4i64),
        )])]])
        .into_connection();
    let db = Arc::new(db);
    let (service, registry) = build_service_shared(db.clone(), FakeProvider::new(), data_root);

    let live = SandboxService::live_standalone_sandboxes(service.db.as_ref(), 7)
        .await
        .expect("count query");

    assert_eq!(live, 4);
    drop((service, registry));
    let log = Arc::try_unwrap(db)
        .unwrap_or_else(|_| panic!("service still holds the connection"))
        .into_transaction_log();
    let sql = format!("{log:?}").replace("\\\"", "\"");
    assert!(sql.contains("\"sandboxes\".\"user_id\" = $1"), "{sql}");
    assert!(sql.contains("\"sandboxes\".\"status\" <> $2"), "{sql}");
    assert!(
        sql.contains("\"sandboxes\".\"metadata\"->>'source' = $3"),
        "{sql}"
    );
}

/// A malformed allow-list must stop placement, not read as "every node
/// is allowed" the way a defaulted settings document would.
#[tokio::test]
async fn a_malformed_allow_list_fails_placement_closed() {
    let data_root = unique_data_root("malformed-allow-list");
    let row = temps_entities::settings::Model {
        id: 1,
        data: serde_json::json!({
            "preview_domain": false,
            "agent_sandbox": {"allowed_node_ids": "3,7"}
        }),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let err = service
        .place_sandbox(&CreateSandboxRequest::default())
        .await
        .expect_err("placement must fail closed");
    assert!(
        matches!(
            &err,
            SandboxError::PlacementSettings {
                operation: "load",
                ..
            }
        ),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

/// Explicit placement asks the chosen worker whether it can run a
/// sandbox, and refuses it by name when it cannot.
#[tokio::test]
async fn explicit_placement_refuses_a_worker_that_cannot_run_sandboxes() {
    struct NoDocker;
    #[async_trait::async_trait]
    impl placement::NodeProbe for NoDocker {
        async fn check(&self, _node_id: i32) -> Result<(), String> {
            Err("Docker is not available on the node".into())
        }
    }
    let db = Arc::new(
        settings_and_worker(MockDatabase::new(DatabaseBackend::Postgres)).into_connection(),
    );
    let mut service = super::tests::preview_test_service_with_db(db);
    service.node_probe = Arc::new(NoDocker);

    let req = CreateSandboxRequest {
        node: Some("7".into()),
        ..Default::default()
    };
    let err = service.place_sandbox(&req).await.expect_err("no Docker");
    assert!(
        matches!(&err, SandboxError::NodeNotReady { node, status }
                if node == "worker-7" && status.contains("Docker is not available")),
        "{err:?}"
    );
}

/// The single-node default (no node requested, control plane allowed)
/// asks no worker and lists no nodes.
#[tokio::test]
async fn default_placement_on_the_control_plane_makes_no_node_calls() {
    struct Panics;
    #[async_trait::async_trait]
    impl placement::NodeProbe for Panics {
        async fn check(&self, node_id: i32) -> Result<(), String> {
            panic!("node {node_id} probed on the control-plane path")
        }
    }
    // Only the settings read is queued: a node query would fail.
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .into_connection(),
    );
    let mut service = super::tests::preview_test_service_with_db(db);
    service.node_probe = Arc::new(Panics);

    let placed = service
        .place_sandbox(&CreateSandboxRequest::default())
        .await
        .expect("control plane");
    assert_eq!(placed, None);
}

/// ADR 063: on a control plane that runs no workloads, a create with no
/// node goes to a worker (the control plane is never a candidate).
#[tokio::test]
async fn without_local_workloads_default_placement_picks_a_worker() {
    let db = Arc::new(
        settings_and_worker(MockDatabase::new(DatabaseBackend::Postgres))
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection(),
    );
    let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
    assert!(!service.runs_local_sandboxes());

    let placed = service
        .place_sandbox(&CreateSandboxRequest::default())
        .await
        .expect("a worker");
    assert_eq!(placed, Some(7));

    // Naming the control plane is refused rather than honoured.
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .into_connection(),
    );
    let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
    let req = CreateSandboxRequest {
        node: Some("control-plane".into()),
        ..Default::default()
    };
    let err = service
        .place_sandbox(&req)
        .await
        .expect_err("no local host");
    assert!(matches!(err, SandboxError::NoLocalHost { .. }), "{err:?}");
}

/// ADR 063: with no worker at all, the create fails with the
/// worker-node-required error, not a 404 or a local fallback.
#[tokio::test]
async fn without_local_workloads_and_no_worker_placement_needs_a_worker_node() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            // only the control plane's own row
            .append_query_results([vec![crate::services::placement::tests::node_row(
                "control-plane",
                "active",
                Some(1),
            )]])
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection(),
    );
    let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
    let err = service
        .place_sandbox(&CreateSandboxRequest::default())
        .await
        .expect_err("no worker");
    assert!(
        matches!(&err, SandboxError::NoWorkerNodeForSandboxes { detail }
                if detail == "no worker node has joined it yet"),
        "{err:?}"
    );
}

/// ADR 063: an application workspace needs a directory on this host,
/// which runs nothing; refused before any database work.
#[tokio::test]
async fn without_local_workloads_application_workspaces_are_refused() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
    let req = CreateSandboxRequest {
        host_work_dir_override: Some(PathBuf::from("/tmp/app-workspace")),
        ..Default::default()
    };
    let err = service
        .place_sandbox(&req)
        .await
        .expect_err("no local host");
    let SandboxError::NoLocalHost { message } = &err else {
        panic!("expected NoLocalHost, got {err:?}");
    };
    assert!(
        message.starts_with("An application workspace needs a sandbox"),
        "{message}"
    );
}

/// The node page asks for the control plane by its `nodes` row id; that
/// resolves to the control plane instead of "node does not exist".
#[tokio::test]
async fn the_control_planes_own_row_lists_the_control_planes_sandboxes() {
    let mut cp_row =
        crate::services::placement::tests::node_row("control-plane", "active", Some(1));
    cp_row.id = 1;
    cp_row.name = "cp-1".into();
    for local_workloads in [true, false] {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                .append_query_results([vec![cp_row.clone()]])
                .append_query_results([Vec::<sandboxes::Model>::new()])
                // the miss path: is node 1 the control plane's row?
                .append_query_results([vec![cp_row.clone()]])
                .append_query_results([[std::collections::BTreeMap::from([(
                    "num_items",
                    sea_orm::Value::BigInt(Some(0)),
                )])]])
                .append_query_results([Vec::<sandboxes::Model>::new()])
                .into_connection(),
        );
        let service =
            super::tests::preview_test_service_with_db(db).with_local_workloads(local_workloads);
        let listing = service
            .node_sandboxes("1", None, None)
            .await
            .expect("the control plane");
        assert!(listing.node.is_control_plane);
        assert_eq!(listing.node.eligible, local_workloads);
        if !local_workloads {
            assert_eq!(
                listing.node.reason.as_deref(),
                Some(placement::CONTROL_PLANE_RUNS_NO_WORKLOADS_REASON)
            );
        }
        assert_eq!(listing.total, 0);
    }
}

#[test]
fn eviction_owners_are_the_destroyed_sandboxes_owners() {
    let owners = HashMap::from([
        ("sbx_a".to_string(), Some(4)),
        ("sbx_b".to_string(), Some(2)),
        ("sbx_c".to_string(), Some(4)),
        ("sbx_d".to_string(), None),
        ("sbx_failed".to_string(), Some(9)),
    ]);
    let destroyed = vec![
        "sbx_a".to_string(),
        "sbx_b".to_string(),
        "sbx_c".to_string(),
        "sbx_d".to_string(),
    ];
    assert_eq!(eviction_owners(&destroyed, &owners), vec![2, 4]);
}

#[test]
fn eviction_outcomes_are_split_and_sorted() {
    let (destroyed, unconfirmed, failed) = split_eviction_outcomes(vec![
        ("sbx_c".into(), Ok(ContainerOutcome::Destroyed)),
        (
            "sbx_b".into(),
            Ok(ContainerOutcome::Unconfirmed("did not answer".into())),
        ),
        (
            "sbx_d".into(),
            Err(SandboxError::Validation {
                message: "bad\u{1b}[31m".into(),
            }),
        ),
        ("sbx_a".into(), Ok(ContainerOutcome::Destroyed)),
    ]);

    assert_eq!(destroyed, vec!["sbx_a", "sbx_b", "sbx_c"]);
    assert_eq!(
        unconfirmed,
        vec![("sbx_b".to_string(), "did not answer".to_string())]
    );
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].0, "sbx_d");
    assert!(!failed[0].1.contains('\u{1b}'), "{:?}", failed[0].1);
}

#[test]
fn node_sandboxes_page_is_clamped() {
    assert_eq!(
        node_sandboxes_page(None, None),
        (1, NODE_SANDBOXES_DEFAULT_PAGE_SIZE)
    );
    assert_eq!(
        node_sandboxes_page(Some(0), Some(1_000)),
        (1, NODE_SANDBOXES_MAX_PAGE_SIZE)
    );
    assert_eq!(node_sandboxes_page(Some(u64::MAX), Some(0)), (MAX_PAGE, 1));
}

#[test]
fn reasons_from_a_node_are_bounded_and_printable() {
    let reason = bounded_reason("line one\n\u{1b}]0;owned\u{7}line two");
    assert!(!reason.chars().any(char::is_control), "{reason:?}");
    assert!(reason.contains("line one") && reason.contains("line two"));

    let long = "é".repeat(MAX_REASON_CHARS * 2);
    let reason = bounded_reason(&long);
    assert_eq!(reason.chars().count(), MAX_REASON_CHARS + 1);
    assert!(reason.ends_with('…'));
    assert_eq!(bounded_reason("short"), "short");
}

#[test]
fn cleanup_command_targets_the_sandbox_and_its_proxy() {
    assert_eq!(
        node_cleanup_command("sbx_0b507bb997ea14d0"),
        "docker ps -aq --filter name=temps-sandbox-0b507bb997ea14d0 | xargs -r docker rm -f"
    );
}

/// Agent-run sandboxes take an early return through
/// `release_for_agent_run`, and their work dir belongs to the executor,
/// not to `data_root`. If that early return is ever lost, this test
/// catches it before it recursively deletes a live run's workspace.
#[tokio::test]
async fn destroy_sandbox_for_agent_run_leaves_the_work_dir_alone() {
    let data_root = unique_data_root("agent-run");
    let dir = seed_work_dir(&data_root, PUBLIC_ID);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_public_id → row owned by agent run 42
        .append_query_results([vec![row(PUBLIC_ID, Some(42))]])
        // agent_runs lookup → empty, i.e. run is gone ⇒ terminal
        .append_query_results([Vec::<agent_runs::Model>::new()])
        // release_for_agent_run: rows for this run
        .append_query_results([Vec::<sandboxes::Model>::new()])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let _ = service.destroy_sandbox(PUBLIC_ID, 1).await;

    assert!(
        dir.exists(),
        "an agent-run sandbox must not have {} deleted — that directory \
             is owned by the executor, and this path is only for standalone \
             sandboxes",
        dir.display()
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn create_sandbox_provider_failure_leaves_no_work_dir() {
    let data_root = unique_data_root("create-fail");
    std::fs::create_dir_all(&data_root).expect("data root");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // placement settings (ADR-048) → no row, i.e. defaults: every node allowed
        .append_query_results([Vec::<temps_entities::settings::Model>::new()])
        // insert of the new row (RETURNING)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        // mark_destroyed update
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    let mut provider = FakeProvider::new();
    provider.fail_create = true;
    let (service, _) = build_service(db, provider, data_root.clone());

    let err = service
        .create_sandbox(1, CreateSandboxRequest::default())
        .await
        .expect_err("provider create failed, so create_sandbox must fail");
    assert!(
        matches!(err, SandboxError::CreateFailed { .. }),
        "{:?}",
        err
    );

    let leftovers: Vec<_> = std::fs::read_dir(&data_root)
        .expect("read data root")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a failed create must not strand a work dir; found {:?}",
        leftovers
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

/// A caller that stops waiting (a proxy timing out during a first image
/// build) must not cancel the create: it still reaches its cleanup path,
/// so no work dir or "running" row is stranded.
#[tokio::test]
async fn spawned_create_finishes_after_the_caller_stops_waiting() {
    let data_root = unique_data_root("create-detached");
    std::fs::create_dir_all(&data_root).expect("data root");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::settings::Model>::new()])
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        // mark_destroyed update, reached only if the create was not cancelled
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    let mut provider = FakeProvider::new();
    provider.fail_create = true;
    provider.create_delay = Some(Duration::from_millis(200));
    let (service, _) = build_service(db, provider, data_root.clone());

    let (held, hold) = tokio::sync::oneshot::channel::<()>();
    let waiting = service.spawn_create(1, CreateSandboxRequest::default(), held);
    // The caller gives up while the provider is still creating.
    let gave_up = tokio::time::timeout(Duration::from_millis(20), waiting).await;
    assert!(gave_up.is_err(), "the create should still be running");
    // `hold` is released only when the create has ended.
    tokio::time::timeout(Duration::from_secs(5), hold)
        .await
        .expect("the create must finish without a caller")
        .expect_err("the held value is dropped, never sent");

    let leftovers: Vec<_> = std::fs::read_dir(&data_root)
        .expect("read data root")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a create nobody waited for must still clean up; found {:?}",
        leftovers
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

/// While a create runs, the row already says `running`. Reads must say
/// `starting` instead, and the mark must go away when the create ends.
#[tokio::test]
async fn a_create_in_progress_reads_as_starting_until_it_ends() {
    let data_root = unique_data_root("create-starting");
    std::fs::create_dir_all(&data_root).expect("data root");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::settings::Model>::new()])
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    let mut provider = FakeProvider::new();
    provider.fail_create = true;
    provider.create_delay = Some(Duration::from_millis(300));
    let (service, _) = build_service(db, provider, data_root.clone());

    let (held, hold) = tokio::sync::oneshot::channel::<()>();
    let _waiting = service.spawn_create(1, CreateSandboxRequest::default(), held);
    let creating = {
        let mut found = None;
        for _ in 0..100 {
            if let Some(id) = service.creates_in_progress.set().iter().next().cloned() {
                found = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        found.expect("the create should be marked in progress")
    };
    let mut summaries = [
        SandboxSummary::from(&row(&creating, None)),
        SandboxSummary::from(&row(PUBLIC_ID, None)),
    ];
    service.mark_creates_in_progress(&mut summaries);
    assert_eq!(summaries[0].status, "starting");
    assert_eq!(
        summaries[1].status, "running",
        "other sandboxes keep their status"
    );

    tokio::time::timeout(Duration::from_secs(5), hold)
        .await
        .expect("the create must finish")
        .expect_err("the held value is dropped, never sent");
    assert!(
        !service.creates_in_progress.contains(&creating),
        "the mark must be cleared when the create ends, even when it fails"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

/// Exec and filesystem calls on a sandbox still being created get a
/// clear 409 instead of a wake attempt on a container that does not exist.
#[tokio::test]
async fn resolve_id_refuses_a_sandbox_whose_create_is_still_running() {
    let data_root = unique_data_root("resolve-starting");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
    let _mark = service.creates_in_progress.begin(PUBLIC_ID);

    let error = service
        .resolve_id(PUBLIC_ID, 1)
        .await
        .expect_err("a sandbox being created cannot run commands yet");
    match error {
        SandboxError::InvalidState {
            sandbox_id, state, ..
        } => {
            assert_eq!(sandbox_id, PUBLIC_ID);
            assert_eq!(state, "starting");
        }
        other => panic!("expected InvalidState(starting), got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&data_root);
}

/// The costly one: seeding runs after the clone, so by the time it fails
/// the directory can hold an entire repository, and the row is already
/// destroyed so no later `destroy_sandbox` can reach it.
#[tokio::test]
async fn create_sandbox_seed_failure_leaves_no_work_dir() {
    let data_root = unique_data_root("seed-fail");
    std::fs::create_dir_all(&data_root).expect("data root");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // placement settings (ADR-048) → no row, i.e. defaults: every node allowed
        .append_query_results([Vec::<temps_entities::settings::Model>::new()])
        // insert of the new row
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        // update after create (status/metadata write-back)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        // mark_destroyed
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    let mut provider = FakeProvider::new();
    provider.fail_exec = true;
    let (service, _) = build_service(db, provider, data_root.clone());

    let req = CreateSandboxRequest {
        source: Some(SandboxSource::Tarball {
            // A public IP literal keeps this cleanup regression test
            // deterministic: source validation succeeds without DNS and
            // the fake provider remains the intended seed failure.
            url: "https://93.184.216.34/src.tar.gz".into(),
        }),
        ..Default::default()
    };
    let err = service
        .create_sandbox(1, req)
        .await
        .expect_err("seeding failed, so create_sandbox must fail");
    assert!(matches!(err, SandboxError::ExecFailed { .. }), "{:?}", err);

    let leftovers: Vec<_> = std::fs::read_dir(&data_root)
        .expect("read data root")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a failed seed must not strand a work dir holding a cloned \
             repository; found {:?}",
        leftovers
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

// ── Major 5: from_snapshot routing ───────────────────────────────────────

/// When `CreateSandboxRequest.from_snapshot_artifact` is `Some`, the
/// service routes to `registry.create_from_snapshot` instead of
/// `registry.create`. FakeProvider doesn't override `create_from_snapshot`
/// so it returns the trait-default "not supported" error — the test
/// confirms the routing by observing that error rather than a normal
/// create-failure reason.
#[tokio::test]
async fn create_sandbox_from_snapshot_routes_to_create_from_snapshot() {
    let data_root = unique_data_root("from-snapshot-route");
    std::fs::create_dir_all(&data_root).expect("data root");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // placement settings (ADR-048) → no row, i.e. defaults: every node allowed
        .append_query_results([Vec::<temps_entities::settings::Model>::new()])
        // insert of the new row (RETURNING)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        // mark_destroyed update (RETURNING) — cleanup after create fails
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let artifact = temps_ai_agents::sandbox::SnapshotArtifact {
        content_path: std::path::PathBuf::from("/tmp/test.tar"),
        content_digest: "sha256fake".to_string(),
        primary_digest: "sha256fake".to_string(),
        size_bytes: 1024,
        backend: temps_ai_agents::sandbox::SandboxBackend::Docker,
        image_ref: Some("temps-snapshot/test:v1".to_string()),
        image_id: Some("sha256:fake".to_string()),
        workspace: None,
    };

    let req = CreateSandboxRequest {
        from_snapshot_artifact: Some(artifact),
        project_id: Some(42),
        ..Default::default()
    };

    let err = service
        .create_sandbox(1, req)
        .await
        .expect_err("FakeProvider::create_from_snapshot returns NotSupported, must fail");

    // The error message proves we went through the from-snapshot path, not
    // the normal create path.
    let msg = err.to_string();
    assert!(
        msg.contains("not supported") || msg.contains("from_snapshot"),
        "error must reference the snapshot create path, got: {:?}",
        err
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

// ── Major 5: non-Docker provider trait defaults ───────────────────────────

/// Providers that don't override `take_snapshot` return a typed "not
/// supported" error rather than silently succeeding. Regression guard for
/// any future provider that inherits the trait without implementing it.
#[tokio::test]
async fn non_docker_provider_take_snapshot_default_returns_not_supported() {
    let handle = handle_for("test");
    let result = FakeProvider::new().take_snapshot(&handle, None, 1024).await;
    assert!(
        result.is_err(),
        "default take_snapshot must return an error, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not supported"),
        "error must say 'not supported', got: {}",
        msg
    );
}

/// Providers that don't override `create_from_snapshot` return a typed
/// "not supported" error. Ensures the from-snapshot routing fails loudly
/// on backends that don't implement it, rather than silently no-oping.
#[tokio::test]
async fn non_docker_provider_create_from_snapshot_default_returns_not_supported() {
    let artifact = temps_ai_agents::sandbox::SnapshotArtifact {
        content_path: std::path::PathBuf::from("/tmp/test.tar"),
        content_digest: "sha256fake".to_string(),
        primary_digest: "sha256fake".to_string(),
        size_bytes: 0,
        backend: temps_ai_agents::sandbox::SandboxBackend::Docker,
        image_ref: None,
        image_id: None,
        workspace: None,
    };
    let config = SandboxCreateConfig {
        node_id: None,
        run_id: 0,
        container_name_override: None,
        host_work_dir: std::path::PathBuf::from("/tmp"),
        workspace_volume: None,
        image: None,
        cpu_limit: None,
        memory_limit_mb: None,
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: None,
        env_vars: HashMap::new(),
        idle_timeout: std::time::Duration::from_secs(3600),
        backend: None,
        owner_user_id: None,
    };
    let result = FakeProvider::new()
        .create_from_snapshot(&artifact, config)
        .await;
    assert!(
        result.is_err(),
        "default create_from_snapshot must return an error, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not supported"),
        "error must say 'not supported', got: {}",
        msg
    );
}

/// Providers that don't override `delete_image` should no-op successfully.
/// The default is intentionally a no-op for backends (Local, Firecracker)
/// that have no image store concept.
#[tokio::test]
async fn non_docker_provider_delete_image_default_is_noop() {
    let result = FakeProvider::new()
        .delete_image("temps-snapshot/test:v1")
        .await;
    assert!(
        result.is_ok(),
        "default delete_image must succeed (no-op), got: {:?}",
        result
    );
}

// ── Major 5: nullify_source_sandbox failure isolation ─────────────────────

/// `destroy_sandbox` calls `nullify_source_sandbox` best-effort. When the
/// snapshot DB fails (e.g. transient network error), the destroy must still
/// complete and return `Ok(())`. A DB error in nullify must never strand the
/// user with an un-destroyable sandbox.
///
/// We attach a SnapshotService backed by a MockDatabase that errors on the
/// `update_many` query, then verify destroy_sandbox still returns Ok.
#[tokio::test]
async fn destroy_sandbox_nullify_failure_does_not_fail_destroy() {
    use crate::services::snapshot_service::SnapshotService;
    use temps_ai_agents::sandbox::local::LocalSandboxProvider;

    let data_root = unique_data_root("nullify-fail");
    let dir = seed_work_dir(&data_root, PUBLIC_ID);

    // SandboxService DB: three queries for destroy path
    let sandbox_db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .append_query_results([vec![sandbox_events::Model {
            id: 1,
            sandbox_id: 7,
            event_type: "destroyed".into(),
            detail: None,
            created_at: Utc::now(),
        }]])
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();

    // SnapshotService DB: update_many for nullify fails with a DB error
    let snap_db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom(
                "simulated nullify transient failure".to_string(),
            )])
            .into_connection(),
    );

    let snap_provider = Arc::new(LocalSandboxProvider::new()) as Arc<dyn SandboxProvider>;
    let snap_registry = Arc::new(StandaloneSandboxRegistry::local_only(snap_provider.clone()));
    let snapshot_service = Arc::new(SnapshotService::new(snap_db, snap_registry, snap_provider));

    // Build the full SandboxService with the snapshot_service injected,
    // then wrap in Arc (note: with_snapshot_service must be called before Arc::new).
    let db = Arc::new(sandbox_db);
    let registry = Arc::new(StandaloneSandboxRegistry::local_only(
        Arc::new(FakeProvider::new()) as Arc<dyn SandboxProvider>,
    ));
    let config = Arc::new(temps_config::ConfigService::new(
        Arc::new(test_server_config(data_root.clone())),
        db.clone(),
    ));
    let encryption = Arc::new(
        temps_core::EncryptionService::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("test encryption key"),
    );
    let git = Arc::new(GitProviderManager::new(
        db.clone(),
        encryption,
        Arc::new(PanicOnSendJobQueue) as Arc<dyn temps_core::JobQueue>,
        config.clone(),
    ));
    let cookie_crypto = Arc::new(
        temps_core::CookieCrypto::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("test cookie key"),
    );
    let service = Arc::new(
        SandboxService::new(
            db,
            registry,
            Arc::new(JobTracker::new()),
            config,
            cookie_crypto,
            git,
            data_root.clone(),
            Arc::new(crate::services::placement::EveryNodeReady),
        )
        .with_snapshot_service(snapshot_service),
    );

    // destroy_sandbox must succeed even though nullify_source_sandbox failed.
    let result = service.destroy_sandbox(PUBLIC_ID, 1).await;
    assert!(
        result.is_ok(),
        "destroy_sandbox must succeed even when nullify_source_sandbox fails, got: {:?}",
        result
    );

    // Work dir still cleaned up
    assert!(
        !dir.exists(),
        "work dir must be removed even when nullify failed"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[test]
fn application_workspace_usage_parses_atomic_metrics_and_ports() {
    let usage = parse_application_workspace_usage(
        "memory=1048576\npids=17\ncpu=332401\ndisk=26376\nports=3000,8080\n",
    );

    assert_eq!(usage.memory_used_bytes, Some(1_048_576));
    assert_eq!(usage.pids_used, Some(17));
    assert_eq!(usage.cpu_usage_usec, Some(332_401));
    assert_eq!(usage.disk_used_bytes, Some(26_376));
    assert_eq!(usage.open_ports, vec![3000, 8080]);
}

#[test]
fn application_workspace_usage_command_has_proc_socket_fallback() {
    assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("/proc/net/tcp"));
    assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("/proc/net/tcp6"));
    assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("du -sb /home/temps/workspace"));
}

#[test]
fn runtime_variables_merge_without_losing_identical_shared_values() {
    let mut variables = HashMap::from([("SHARED_HOST".to_string(), "database".to_string())]);

    merge_runtime_variables(
        &mut variables,
        HashMap::from([
            ("SHARED_HOST".to_string(), "database".to_string()),
            ("REDIS_URL".to_string(), "redis://database:6379".to_string()),
        ]),
    )
    .expect("identical shared values should be accepted");

    assert_eq!(variables.len(), 2);
    assert_eq!(variables["REDIS_URL"], "redis://database:6379");
}

#[test]
fn runtime_variables_reject_ambiguous_service_credentials() {
    let mut variables = HashMap::from([("DATABASE_URL".to_string(), "postgres://one".to_string())]);

    let conflict = merge_runtime_variables(
        &mut variables,
        HashMap::from([("DATABASE_URL".to_string(), "postgres://two".to_string())]),
    )
    .expect_err("different values for one runtime name must be rejected");

    assert_eq!(conflict, "DATABASE_URL");
    assert_eq!(variables["DATABASE_URL"], "postgres://one");
}

fn link_row(id: i32, service_id: i32, project_id: Option<i32>) -> sandbox_services::Model {
    sandbox_services::Model {
        id,
        sandbox_id: 7,
        service_id,
        project_id,
        created_by: Some(1),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn linked_services_lists_the_sandboxs_links_with_their_projects() {
    let data_root = unique_data_root("linked-services");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .append_query_results([vec![link_row(1, 4, Some(9)), link_row(2, 6, None)]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let links = service
        .linked_services(1, PUBLIC_ID)
        .await
        .expect("owner reads the links");

    assert_eq!(
        links,
        vec![
            LinkedService {
                service_id: 4,
                project_id: Some(9)
            },
            LinkedService {
                service_id: 6,
                project_id: None
            },
        ]
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn another_users_sandbox_has_no_links_to_read() {
    let data_root = unique_data_root("linked-services-foreign");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let error = service
        .linked_services(2, PUBLIC_ID)
        .await
        .expect_err("a non-owner must not see the sandbox");

    assert!(
        matches!(error, SandboxError::NotFound { ref sandbox_id } if sandbox_id == PUBLIC_ID),
        "got {error:?}"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn a_link_that_cannot_be_given_credentials_is_removed_again() {
    let data_root = unique_data_root("link-rollback");
    // No credential provider is registered, so building the sandbox's
    // access fails after the row was added; the row must be taken back
    // (the delete's exec result is consumed) rather than left linked.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row(PUBLIC_ID, None)]])
        .append_query_results([Vec::<sandbox_services::Model>::new()])
        .append_query_results([vec![link_row(1, 4, Some(9))]])
        .append_query_results([vec![link_row(1, 4, Some(9))]])
        .append_exec_results([sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let error = service
        .link_services(
            1,
            PUBLIC_ID,
            &[LinkedService {
                service_id: 4,
                project_id: Some(9),
            }],
        )
        .await
        .expect_err("no credential provider means no scoped access");

    assert!(
        matches!(error, SandboxError::Unavailable { .. }),
        "got {error:?}"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

#[tokio::test]
async fn service_support_needs_a_credential_provider() {
    let data_root = unique_data_root("service-support");
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

    let error = service
        .service_supports_sandbox(4)
        .await
        .expect_err("without a provider the answer is an error, not a quiet false");

    assert!(
        matches!(error, SandboxError::Unavailable { .. }),
        "got {error:?}"
    );
    let _ = std::fs::remove_dir_all(&data_root);
}

/// The control plane offers every selectable backend, says which it can
/// run, and names the setup step for each one it cannot, so the console
/// can show the choice instead of hiding it.
#[test]
fn the_control_plane_lists_its_backends_with_the_fix_for_missing_ones() {
    let mut provider = FakeProvider::new();
    provider.backends = Some(vec![SandboxBackend::Docker]);
    let (service, _) = build_service(
        MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        provider,
        unique_data_root("cp-backends"),
    );

    let backends = service.control_plane_backends();

    let offered: Vec<_> = backends.iter().map(|b| b.backend).collect();
    assert_eq!(
        offered,
        temps_ai_agents::sandbox::SELECTABLE_SANDBOX_BACKENDS.to_vec()
    );
    let docker = &backends[0];
    assert!(docker.available && docker.reason.is_none(), "{docker:?}");
    let firecracker = &backends[1];
    assert!(!firecracker.available);
    assert!(
        firecracker
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("temps firecracker setup")),
        "an unavailable backend says how to provision it: {firecracker:?}"
    );
    assert!(!backends[2].available && backends[2].reason.is_some());
}
