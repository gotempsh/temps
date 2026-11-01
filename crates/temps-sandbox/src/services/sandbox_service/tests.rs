// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg(target_os = "linux")]
use super::source_import::{shell_escape_service, source_import_staging_finalize_script};
use super::*;
use super::{
    application_workspace::{
        application_workspace_config_has_drift, application_workspace_identity_is_attested,
        application_workspace_mutex, pinned_or_fallback,
    },
    create::{parse_requested_backend, validate_allow_docker},
    data_network::sandbox_data_network_name,
    destroy::work_dir_to_remove,
    source_import::{
        git_metadata_cleanup_script, source_import_aggregate_bounds_script,
        source_import_bounds_script, source_import_command_script, source_import_staging_dir,
        source_import_staging_prepare_script, source_target_guard_script,
    },
};
use sea_orm::{DatabaseBackend, MockDatabase};
use temps_core::{Job, JobQueue, JobReceiver, QueueError};

#[tokio::test]
async fn application_workspace_mutations_serialize_only_the_same_application() {
    let mut locks = HashMap::new();
    let first = application_workspace_mutex(&mut locks, "app-a");
    let same = application_workspace_mutex(&mut locks, "app-a");
    let other = application_workspace_mutex(&mut locks, "app-b");
    assert!(Arc::ptr_eq(&first, &same));
    assert!(!Arc::ptr_eq(&first, &other));
    let _held = first.lock_owned().await;
    assert!(same.try_lock_owned().is_err());
    assert!(other.try_lock_owned().is_ok());
}

#[test]
fn release_manifest_pin_wins_over_the_published_beta_fallback() {
    let pinned = "ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let fallback = "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta";
    assert_eq!(pinned_or_fallback(Some(pinned), fallback), pinned);
    assert_eq!(pinned_or_fallback(None, fallback), fallback);
}

#[test]
fn source_build_fallbacks_are_beta_tags_the_allowlist_accepts() {
    for flavor in ["nodejs", "python", "all"] {
        let image = format!("ghcr.io/gotempsh/temps-sandbox-{flavor}:0.3.4-beta");
        assert!(is_managed_application_workspace_image(&image), "{image}");
    }
    if temps_core::release_images::REVISION.is_none() {
        for runtime in ["node", "bun", "python", "rust", "go", "full"] {
            let image = managed_application_workspace_image(runtime)
                .expect("managed runtime resolves to an image");
            assert!(image.ends_with(":0.3.4-beta"), "{runtime} -> {image}");
        }
    }
}

#[test]
fn application_workspace_runtimes_resolve_to_managed_daemon_images() {
    // A candidate can pass runtime preflight but fail persistence if its
    // pinned tag is absent from the database image CHECK constraint.
    let migration = include_str!("../../../../temps-migrations/src/migration/m20260913_000002_managed_daemon_workspace_images_v034.rs");
    for runtime in ["node", "bun", "python", "rust", "go", "full"] {
        let image = managed_application_workspace_image(runtime)
            .expect("managed runtime must resolve to an image");
        if temps_core::release_images::REVISION.is_none() {
            let beta_migration = include_str!("../../../../temps-migrations/src/migration/m20261012_000003_managed_daemon_beta_images.rs");
            assert!(
                migration.contains(&format!("'{image}'"))
                    || (image.ends_with("-beta")
                        && beta_migration.contains(
                            "temps-sandbox-(nodejs|python|all):[0-9]+[.][0-9]+[.][0-9]+-beta"
                        )),
                "no migration allows {image}"
            );
        } else {
            assert!(is_managed_application_workspace_image(image));
        }
    }
    if temps_core::release_images::REVISION.is_some() {
        return;
    }
    assert_eq!(
        managed_application_workspace_image("node"),
        Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")
    );
    assert_eq!(
        managed_application_workspace_image("bun"),
        Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")
    );
    assert_eq!(
        managed_application_workspace_image("python"),
        Some("ghcr.io/gotempsh/temps-sandbox-python:0.3.4-beta")
    );
    for runtime in ["rust", "go", "full"] {
        assert_eq!(
            managed_application_workspace_image(runtime),
            Some("ghcr.io/gotempsh/temps-sandbox-all:0.3.4-beta")
        );
    }
    assert_eq!(managed_application_workspace_image("custom"), None);
    assert_eq!(managed_application_workspace_image("unknown"), None);
    assert!(is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0"
    ));
    assert!(is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-node:0.1.0"
    ));
    assert!(is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-all:0.3.0"
    ));
    assert!(is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-all:0.3.1"
    ));
    assert!(is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-all:0.3.2"
    ));
    assert!(is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-all:0.3.3"
    ));
    assert!(is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-all:0.3.4"
    ));
    for flavor in ["nodejs", "python", "all"] {
        assert!(
            is_managed_application_workspace_image(&format!(
                "ghcr.io/gotempsh/temps-sandbox-{flavor}:0.3.4-beta"
            )),
            "the beta tag published by main builds must stay accepted for {flavor}"
        );
    }
    assert!(!is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-nodejs:9.9.9-beta"
    ));
    assert!(!is_managed_application_workspace_image(
        "ghcr.io/gotempsh/temps-sandbox-nodejs:0.1.0"
    ));
    for flavor in ["nodejs", "python", "all"] {
        let image = format!(
            "ghcr.io/gotempsh/temps-sandbox-{flavor}@sha256:{}",
            "a".repeat(64)
        );
        assert!(is_managed_application_workspace_image(&image));
    }
    for image in [
        format!(
            "ghcr.io/gotempsh/temps-sandbox-bun@sha256:{}",
            "a".repeat(64)
        ),
        format!(
            "ghcr.io/other/temps-sandbox-nodejs@sha256:{}",
            "a".repeat(64)
        ),
        format!(
            "ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:{}",
            "A".repeat(64)
        ),
        "ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:short".into(),
    ] {
        assert!(
            !is_managed_application_workspace_image(&image),
            "accepted {image}"
        );
    }
}

#[test]
fn application_workspace_config_preserves_explicit_image_and_maps_null_runtime() {
    let now = Utc::now();
    let mut row = ai_application_workspaces::Model {
        id: 1,
        application_id: 2,
        sandbox_public_id: None,
        desired_state: "running".to_string(),
        runtime: "python".to_string(),
        image: None,
        cpu_limit: 1.0,
        memory_limit_mb: 1024,
        pids_limit: 128,
        disk_limit_mb: 2048,
        idle_timeout_secs: 900,
        last_error: None,
        created_at: now,
        updated_at: now,
    };

    let mapped = ApplicationWorkspaceConfig::from(&row);
    assert_eq!(
        mapped.image.as_deref(),
        managed_application_workspace_image("python")
    );

    row.image = Some("ghcr.io/gotempsh/temps-sandbox-all:0.2.0".to_string());
    let explicit = ApplicationWorkspaceConfig::from(&row);
    assert_eq!(
        explicit.image.as_deref(),
        Some("ghcr.io/gotempsh/temps-sandbox-all:0.2.0")
    );
}

#[test]
fn scoped_runtime_debug_output_names_variables_but_never_values() {
    let runtime = ScopedRuntime {
        variables: HashMap::from([("POSTGRES_PASSWORD".to_string(), "s3cr3t-value".to_string())]),
        connected_service_ids: vec![4],
        skipped_service_ids: vec![9],
    };
    let shown = format!("{runtime:?}");
    assert!(shown.contains("POSTGRES_PASSWORD"));
    assert!(!shown.contains("s3cr3t-value"), "{shown}");
    assert!(shown.contains("skipped_service_ids"));
}

#[test]
fn sandbox_data_network_names_are_stable_and_scoped() {
    assert_eq!(
        sandbox_data_network_name("sbx_abc123").expect("valid sandbox id"),
        "temps-sandbox-data-abc123"
    );
}

#[test]
fn source_destinations_are_bounded_relative_paths() {
    for valid in ["project", "projects/web", "packages/api-v2"] {
        assert!(validate_source_destination(Some(valid)).is_ok(), "{valid}");
    }
    for invalid in ["", ".", "../project", "projects/../escape", "/workspace"] {
        assert!(
            validate_source_destination(Some(invalid)).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn stripped_git_metadata_targets_only_the_imported_repository() {
    let cleanup = git_metadata_cleanup_script("/workspace/projects/web", true);
    assert!(cleanup.contains("/workspace/projects/web/.git"));
    assert!(!cleanup.contains("/workspace/.git "));
    assert!(git_metadata_cleanup_script("/workspace/projects/web", false).is_empty());
}

#[test]
fn requested_backend_accepts_public_backends_only() {
    use temps_ai_agents::sandbox::SandboxBackend;
    assert_eq!(parse_requested_backend(None).unwrap(), None);
    assert_eq!(
        parse_requested_backend(Some("docker")).unwrap(),
        Some(SandboxBackend::Docker)
    );
    assert_eq!(
        parse_requested_backend(Some("firecracker")).unwrap(),
        Some(SandboxBackend::Firecracker)
    );
    assert_eq!(
        parse_requested_backend(Some("microsandbox")).unwrap(),
        Some(SandboxBackend::Microsandbox)
    );
    // "local" is a dev fallback, never a caller choice.
    for rejected in ["local", "libkrun", ""] {
        match parse_requested_backend(Some(rejected)) {
            Err(SandboxError::Validation { message }) => {
                assert!(message.contains(&format!("'{rejected}'")), "{message}");
                assert!(message.contains("microsandbox"), "{message}");
            }
            other => panic!("expected validation error for {rejected:?}, got {other:?}"),
        }
    }
}

#[test]
fn unavailable_microsandbox_explains_itself() {
    let message =
        backend_unavailable_message(temps_ai_agents::sandbox::SandboxBackend::Microsandbox);
    assert!(
        message.starts_with("backend 'microsandbox' is not available on this host: "),
        "{message}"
    );
    let firecracker =
        backend_unavailable_message(temps_ai_agents::sandbox::SandboxBackend::Firecracker);
    assert!(
        firecracker.starts_with("backend 'firecracker' is not available on this host: "),
        "{firecracker}"
    );
    assert!(
        firecracker.contains("temps firecracker setup"),
        "{firecracker}"
    );
    assert!(firecracker.contains("restart temps"), "{firecracker}");
    assert_eq!(
        backend_unavailable_message(temps_ai_agents::sandbox::SandboxBackend::Docker),
        "backend 'docker' is not available on this host"
    );
}

#[test]
fn microvm_backends_stage_source_imports_under_root_home() {
    assert_eq!(
        source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Microsandbox, 42),
        "/root/.temps-source-import-42"
    );
    assert_eq!(
        source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Firecracker, 42),
        "/root/.temps-source-import-42"
    );
}

#[test]
fn source_imports_use_bounded_staging_and_process_deadlines() {
    let nested_stage =
        source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Docker, 42);
    assert_eq!(
        nested_stage,
        "/run/temps-source-import/.temps-source-import-42"
    );
    let local_stage =
        source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Local, 42);
    assert_eq!(local_stage, "/tmp/.temps-source-import-42");

    let command = source_import_command_script(&nested_stage, "git clone repo target");
    assert!(command.contains("timeout -k 5 120"));
    assert!(command.contains("find /run/temps-source-import/.temps-source-import-42 -mindepth 1"));
    assert!(command.contains("kill -TERM"));
    assert!(
        command.contains("find /run/temps-source-import/.temps-source-import-42 -depth -delete")
    );

    let prepare = source_import_staging_prepare_script(&nested_stage);
    assert!(prepare.contains(" /run/temps-source-import "));
    assert!(prepare.contains("rebuild this sandbox before importing"));

    let local_prepare = source_import_staging_prepare_script(&local_stage);
    assert!(local_prepare.contains("unavailable for this sandbox backend"));
    assert!(local_prepare.contains("exit 69"));

    let auth_prepare = source_import_staging_prepare_script("/run/secrets/.temps-source-auth-42");
    assert!(auth_prepare.contains(" /run/secrets "));
    assert!(auth_prepare.contains("credential storage is unavailable"));

    let bounds = source_import_bounds_script(&nested_stage);
    assert!(bounds.contains("-mindepth 1"));
    assert!(!bounds.contains("-type f"));
}

#[test]
fn source_import_quota_includes_existing_workspace_content() {
    let bounds = source_import_aggregate_bounds_script(
        "/workspace",
        "/run/temps-source-import/.temps-source-import-7",
    );
    assert!(bounds.contains("existing_count + staged_count"));
    assert!(bounds.contains("existing_bytes + staged_bytes"));
    assert!(bounds.contains("aggregate 5000 entry or 256 MiB"));
}

#[test]
fn source_destination_guard_rejects_non_empty_targets() {
    let guard = source_target_guard_script("/workspace", "/workspace/projects/web");
    assert!(guard.contains("source destination must be an empty directory"));
    assert!(guard.contains("exec 9< /workspace/projects/web"));
    assert!(guard.contains("find \"$target_fd\" -mindepth 1"));
    assert!(guard.contains("chmod 700 \"$target_fd\""));
}

#[cfg(target_os = "linux")]
#[test]
fn source_publish_uses_retained_destination_after_symlink_swap() {
    use std::process::Command;

    let temp = tempfile::tempdir().expect("temporary source import root");
    let workspace = temp.path().join("workspace");
    let target = workspace.join("projects/web");
    let original = workspace.join("projects/web-original");
    let outside = temp.path().join("outside");
    let staging = temp.path().join("staging");
    std::fs::create_dir_all(&target).expect("source destination");
    std::fs::create_dir_all(&outside).expect("outside directory");
    std::fs::create_dir_all(&staging).expect("staging directory");
    std::fs::write(staging.join("index.js"), b"safe").expect("staged source");

    let guard = source_target_guard_script(
        workspace.to_str().expect("utf-8 workspace"),
        target.to_str().expect("utf-8 target"),
    );
    let finalize =
        source_import_staging_finalize_script(staging.to_str().expect("utf-8 staging directory"));
    let script = format!(
        "set -eu; {guard} mv {target} {original}; ln -s {outside} {target}; {finalize}",
        target = shell_escape_service(target.to_str().expect("utf-8 target")),
        original = shell_escape_service(original.to_str().expect("utf-8 original")),
        outside = shell_escape_service(outside.to_str().expect("utf-8 outside")),
    );
    let status = Command::new("/bin/sh")
        .args(["-c", &script])
        .status()
        .expect("execute source publication script");

    assert!(status.success());
    assert_eq!(
        std::fs::read(original.join("index.js")).expect("published source"),
        b"safe"
    );
    assert!(!outside.join("index.js").exists());
    assert!(std::fs::symlink_metadata(&target)
        .expect("replacement symlink")
        .file_type()
        .is_symlink());
}

#[test]
fn sandbox_data_network_names_reject_docker_name_injection() {
    for invalid in ["", "../shared", "app id", "app/name"] {
        assert!(matches!(
            sandbox_data_network_name(invalid),
            Err(SandboxError::Validation { .. })
        ));
    }
}

#[test]
fn application_workspace_reuse_requires_server_attestation() {
    let host_work_dir = PathBuf::from("/var/lib/temps/ai-applications/app_example");
    let config = ApplicationWorkspaceConfig::default();
    let now = Utc::now();
    let mut managed = sandboxes::Model {
        id: 7,
        node_id: None,
        public_id: "sbx_deadbeef01234567".to_string(),
        user_id: Some(1),
        agent_run_id: None,
        name: "ai-application:app_example".to_string(),
        status: "running".to_string(),
        image: config.image.clone(),
        work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
        timeout_secs: 3600,
        metadata: Some(serde_json::json!({
            "managed_application_id": "app_example",
            "managed_host_work_dir": host_work_dir,
        })),
        backend: Some("docker".to_string()),
        created_at: now,
        last_activity_at: now,
        expires_at: now + chrono::Duration::hours(1),
        preview_password_hash: None,
        preview_password_hint: None,
        preview_access: Default::default(),
        preview_allowed_cidrs: Vec::new(),
        egress_policy: Default::default(),
        lifecycle: "workspace".to_string(),
        project_id: None,
        source_repo_url: None,
    };

    assert!(config.image.is_some(), "the default image must be attested");

    assert!(application_workspace_identity_is_attested(
        &managed,
        1,
        "app_example",
        &host_work_dir,
    ));

    let old_image = "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2";
    managed.image = Some(old_image.to_string());
    assert!(application_workspace_identity_is_attested(
        &managed,
        1,
        "app_example",
        &host_work_dir,
    ));
    assert!(application_workspace_config_has_drift(&managed, &config));

    managed.user_id = Some(2);
    assert!(!application_workspace_identity_is_attested(
        &managed,
        1,
        "app_example",
        &host_work_dir,
    ));
    managed.user_id = Some(1);
    let wrong_path = PathBuf::from("/var/lib/temps/ai-applications/another-app");
    assert!(!application_workspace_identity_is_attested(
        &managed,
        1,
        "app_example",
        &wrong_path,
    ));

    managed.image = Some("attacker/custom-image:latest".to_string());
    assert!(!application_workspace_identity_is_attested(
        &managed,
        1,
        "app_example",
        &host_work_dir,
    ));
    assert_eq!(
        managed_application_id_from_name("ai-application:app_example"),
        Some("app_example")
    );
    assert_eq!(managed_application_id_from_name("ordinary-sandbox"), None);
}

struct NoopJobQueue;

#[async_trait::async_trait]
impl JobQueue for NoopJobQueue {
    async fn send(&self, _job: Job) -> Result<(), QueueError> {
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn JobReceiver> {
        panic!("preview link tests never subscribe to the job queue")
    }
}

fn test_server_config() -> temps_config::ServerConfig {
    temps_config::ServerConfig {
        address: "127.0.0.1:0".into(),
        database_url: "postgres://test".into(),
        tls_address: None,
        console_address: "127.0.0.1:0".into(),
        console_admin_address: None,
        console_ingress_address: None,
        admin_allowed_ips: Vec::new(),
        admin_allowed_hosts: Vec::new(),
        admin_trust_forwarded_for: false,
        data_dir: PathBuf::from("/tmp/temps-sandbox-preview-tests"),
        auth_secret: "default-32-byte-key-for-testing!".into(),
        encryption_key: "another-32-byte-key-for-testing!".into(),
        api_base_url: "/api".into(),
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

fn preview_test_service(
    query_results: Vec<Vec<sandboxes::Model>>,
) -> (SandboxService, Arc<DatabaseConnection>) {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(query_results)
            .into_connection(),
    );
    let service = preview_test_service_with_db(db.clone());
    (service, db)
}

pub(crate) fn preview_test_service_with_db(db: Arc<DatabaseConnection>) -> SandboxService {
    let platform_config = Arc::new(ConfigService::new(
        Arc::new(test_server_config()),
        db.clone(),
    ));
    let git_provider_manager = Arc::new(GitProviderManager::new(
        db.clone(),
        Arc::new(temps_core::EncryptionService::new_from_password("test")),
        Arc::new(NoopJobQueue),
        platform_config.clone(),
    ));
    let registry = Arc::new(StandaloneSandboxRegistry::local_only(Arc::new(
        temps_ai_agents::sandbox::local::LocalSandboxProvider::new(),
    )));
    SandboxService::new(
        db.clone(),
        registry,
        Arc::new(JobTracker::new()),
        platform_config,
        Arc::new(
            temps_core::CookieCrypto::new("default-32-byte-key-for-testing!")
                .expect("valid test cookie key"),
        ),
        git_provider_manager,
        PathBuf::from("/tmp/temps-sandbox-preview-tests"),
        Arc::new(crate::services::placement::EveryNodeReady),
    )
}

#[test]
fn default_request_is_empty() {
    let r = CreateSandboxRequest::default();
    assert!(r.image.is_none());
    assert!(r.env.is_empty());
    assert!(r.timeout_secs.is_none());
    assert!(r.preview_password.is_none());
}

#[test]
fn allow_docker_requires_explicit_microsandbox_backend() {
    use temps_ai_agents::sandbox::SandboxBackend;
    for backend in [
        None,
        Some(SandboxBackend::Docker),
        Some(SandboxBackend::Firecracker),
    ] {
        assert!(validate_allow_docker(true, backend).is_err());
        assert!(validate_allow_docker(false, backend).is_ok());
    }
    assert!(validate_allow_docker(true, Some(SandboxBackend::Microsandbox)).is_ok());
}

#[test]
fn request_carries_preview_password() {
    // The field is plumbed through the service input DTO so handlers
    // don't need to reach around the service to wire it in.
    let r = CreateSandboxRequest {
        preview_password: Some("hunter2secret".to_string()),
        ..Default::default()
    };
    assert_eq!(r.preview_password.as_deref(), Some("hunter2secret"));
}

/// Destroy must actually target the directory create allocated —
/// nothing removed it before, so every create/destroy cycle left the
/// full working tree (node_modules, build output, cloned repos) on the
/// host and the disk filled up with directories no API could reach.
#[test]
fn work_dir_to_remove_targets_the_directory_create_allocated() {
    let root = Path::new("/var/lib/temps/sandboxes");
    let id = "sbx_deadbeef01234567";
    assert_eq!(
        work_dir_to_remove(root, id),
        // Same expression create_sandbox uses to build host_work_dir.
        Some(root.join(id))
    );
}

/// The guard in front of a recursive delete. A public id is only ever
/// generated internally, but if a malformed one ever reached here
/// `data_root.join()` would happily resolve out of the data dir — an
/// absolute id replaces the root outright.
#[test]
fn work_dir_to_remove_refuses_ids_that_escape_the_data_root() {
    let root = Path::new("/var/lib/temps/sandboxes");
    for bad in [
        "../../etc",
        "sbx_../../etc",
        "/etc",
        "sbx_",
        "sbx_short",
        "sbx_zzzzzzzzzzzzzzzz",
        "",
    ] {
        assert_eq!(
            work_dir_to_remove(root, bad),
            None,
            "{:?} must not resolve to a directory we would recursively delete",
            bad
        );
    }
}

/// The cross-crate half of the naming agreement.
///
/// `temps-sandbox` decides the container label; `temps-ai-agents` turns a
/// container name into a volume name. Nothing at the type level forces
/// those to line up, and the leak this PR fixes was exactly one identity
/// derived independently in two places. If either side changes its
/// scheme without the other, this goes red — and it needs no Docker.
#[test]
fn sandbox_service_and_provider_agree_on_volume_naming() {
    let public_id = "sbx_a1b2c3d4e5f60718";
    let label = container_label_for(public_id);
    assert_eq!(label, "a1b2c3d4e5f60718", "label is public_id minus sbx_");

    // What the provider will name the volume for a container built from
    // this label, i.e. what `destroy` will try to remove.
    assert_eq!(
        temps_ai_agents::sandbox::home_volume_name_for_label(label),
        "temps-sandbox-home-v2-a1b2c3d4e5f60718"
    );

    // And it must not be a name from the pre-fix namespace, where a
    // stranded volume from another tenant could still be sitting.
    assert!(!temps_ai_agents::sandbox::home_volume_name_for_label(label)
        .trim_start_matches("temps-sandbox-home-")
        .chars()
        .all(|c| c.is_ascii_digit()));
}

#[test]
fn timeout_constants_are_sane() {
    const _: () = assert!(MIN_TIMEOUT_SECS < DEFAULT_TIMEOUT_SECS);
    const _: () = assert!(DEFAULT_TIMEOUT_SECS < MAX_TIMEOUT_SECS);
    assert_eq!(MAX_TIMEOUT_SECS, 86400);
}

#[test]
fn allow_docker_is_reported_from_persisted_metadata() {
    let mut row = agent_run_row(None);
    assert!(!SandboxSummary::from(&row).allow_docker);
    row.metadata = Some(serde_json::json!({"allow_docker": true}));
    assert!(SandboxSummary::from(&row).allow_docker);
    row.metadata = Some(serde_json::json!({"allow_docker": false}));
    assert!(!SandboxSummary::from(&row).allow_docker);
}

fn agent_run_row(run_id: Option<i32>) -> sandboxes::Model {
    let now = Utc::now();
    sandboxes::Model {
        id: 7,
        node_id: None,
        public_id: "sbx_deadbeef01234567".into(),
        user_id: Some(1),
        agent_run_id: run_id,
        name: match run_id {
            Some(id) => format!("temps-sandbox-{}", id),
            None => "sbx-7".into(),
        },
        status: "running".into(),
        image: None,
        work_dir: "/workspace".into(),
        timeout_secs: 3600,
        metadata: None,
        backend: None,
        created_at: now,
        last_activity_at: now,
        expires_at: now,
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

fn protected_preview_row(user_id: i32) -> sandboxes::Model {
    let mut row = agent_run_row(None);
    row.user_id = Some(user_id);
    row.preview_password_hash = Some("argon2-current".into());
    row.preview_password_hint = Some("rent".into());
    row.preview_access = SandboxPreviewAccess::Password;
    row
}

fn statements_of(service: SandboxService, db: Arc<DatabaseConnection>) -> String {
    drop(service);
    let log = Arc::try_unwrap(db)
        .expect("service owns no database references after drop")
        .into_transaction_log();
    format!("{log:?}")
}

#[tokio::test]
async fn set_preview_access_normalises_the_allow_list_and_records_an_event() {
    let row = agent_run_row(None);
    let mut updated = row.clone();
    updated.preview_access = SandboxPreviewAccess::Public;
    updated.preview_allowed_cidrs = vec!["192.0.2.0/24".into(), "2001:db8::1/128".into()];
    let (service, db) = preview_test_service(vec![vec![row], vec![updated.clone()]]);

    let got = service
        .set_preview_access(
            "sbx_deadbeef01234567",
            1,
            SandboxPreviewAccess::Public,
            &["192.0.2.9/24".into(), "2001:db8::1".into()],
        )
        .await
        .expect("owner may change preview access");
    assert_eq!(got.sandbox, updated);
    assert_eq!(got.previous_access, SandboxPreviewAccess::TempsLogin);
    assert!(got.previous_allowed_cidrs.is_empty());

    let statements = statements_of(service, db);
    assert!(statements.contains("192.0.2.0/24"), "log: {statements}");
    assert!(statements.contains("2001:db8::1/128"), "log: {statements}");
    assert!(
        statements.contains("preview_access_changed"),
        "log: {statements}"
    );
}

#[tokio::test]
async fn set_preview_access_refuses_password_mode_without_a_password() {
    let (service, _) = preview_test_service(vec![vec![agent_run_row(None)]]);
    let error = service
        .set_preview_access(
            "sbx_deadbeef01234567",
            1,
            SandboxPreviewAccess::Password,
            &[],
        )
        .await
        .expect_err("password mode needs a password");
    assert!(matches!(error, SandboxError::Validation { .. }));
    let message = error.to_string();
    assert!(message.contains("sbx_deadbeef01234567"), "{message}");
    assert!(
        message.contains("/v1/sandboxes/sbx_deadbeef01234567/preview-password"),
        "{message}"
    );
}

#[tokio::test]
async fn set_preview_access_rejects_a_bad_cidr_before_touching_the_database() {
    let (service, _) = preview_test_service(Vec::new());
    let error = service
        .set_preview_access(
            "sbx_deadbeef01234567",
            1,
            SandboxPreviewAccess::TempsLogin,
            &["10.0.0.0/8".into(), "10.0.0.0/40".into()],
        )
        .await
        .expect_err("an invalid CIDR is a validation error");
    assert!(matches!(error, SandboxError::Validation { .. }));
    assert!(error.to_string().contains("allowed_cidrs[1]"), "{error}");
}

#[tokio::test]
async fn set_preview_access_hides_other_users_sandboxes() {
    let (service, _) = preview_test_service(vec![vec![protected_preview_row(2)]]);
    let error = service
        .set_preview_access("sbx_deadbeef01234567", 1, SandboxPreviewAccess::Public, &[])
        .await
        .expect_err("a non-owner must not change preview access");
    assert!(matches!(error, SandboxError::NotFound { .. }));
}

#[tokio::test]
async fn setting_a_preview_password_selects_password_access() {
    let row = agent_run_row(None);
    let updated = protected_preview_row(1);
    let (service, db) = preview_test_service(vec![vec![row], vec![updated]]);
    service
        .set_preview_password("sbx_deadbeef01234567", 1, "correct horse battery")
        .await
        .expect("owner may set a password");
    let statements = statements_of(service, db);
    assert!(statements.contains("\"password\""), "log: {statements}");
    assert!(
        statements.contains("preview_password_set"),
        "log: {statements}"
    );
}

#[tokio::test]
async fn clearing_the_preview_password_returns_to_temps_login() {
    let row = protected_preview_row(1);
    let mut updated = agent_run_row(None);
    updated.preview_access = SandboxPreviewAccess::TempsLogin;
    let (service, db) = preview_test_service(vec![vec![row], vec![updated]]);
    let access = service
        .clear_preview_password("sbx_deadbeef01234567", 1)
        .await
        .expect("owner may clear the password");
    assert_eq!(access, SandboxPreviewAccess::TempsLogin);
    let statements = statements_of(service, db);
    assert!(statements.contains("\"temps_login\""), "log: {statements}");
    assert!(
        statements.contains("preview_password_cleared"),
        "log: {statements}"
    );
}

#[tokio::test]
async fn preview_share_link_requires_password_access_even_with_a_hash() {
    let mut row = protected_preview_row(1);
    row.preview_access = SandboxPreviewAccess::TempsLogin;
    let (service, _) = preview_test_service(vec![vec![row]]);
    let error = service
        .preview_share_link(
            "sbx_deadbeef01234567",
            1,
            3000,
            "/",
            Duration::from_secs(60),
        )
        .await
        .expect_err("a share link only exists in password mode");
    assert!(matches!(error, SandboxError::InvalidState { .. }));
}

#[tokio::test]
async fn preview_share_link_uses_fragment_clamps_ttl_and_records_safe_event() {
    let row = protected_preview_row(1);
    let (service, db) = preview_test_service(vec![vec![row]]);

    let before = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("test clock after epoch")
        .as_secs();
    let (url, expires_at) = service
        .preview_share_link(
            "sbx_deadbeef01234567",
            1,
            3000,
            "/\\evil.example",
            temps_core::PREVIEW_SESSION_GRANT_MAX_TTL * 30,
        )
        .await
        .expect("protected sandbox should mint a link");

    assert!(url.starts_with(
            "http://ws-deadbeef01234567-3000.localho.st:0/__temps/preview/login?grant=1&next=%2F#session_grant="
        ));
    assert!(!url.contains("?session_grant="));
    assert!(expires_at >= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs());
    assert!(expires_at <= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs() + 1);

    drop(service);
    let log = Arc::try_unwrap(db)
        .expect("service owns no database references after drop")
        .into_transaction_log();
    let statements = format!("{log:?}");
    assert!(statements.contains("sandbox_events"), "log: {statements}");
    assert!(
        statements.contains("preview_share_link_created"),
        "log: {statements}"
    );
    assert!(!statements.contains("session_grant"), "log: {statements}");
}

#[tokio::test]
async fn preview_renewal_does_not_wake_stopped_workspace() {
    let mut row = protected_preview_row(1);
    row.status = "stopped".to_string();
    let (service, _) = preview_test_service(vec![vec![row]]);
    let error = service
        .preview_ready_sandbox("sbx_deadbeef01234567", 1)
        .await
        .expect_err("stopped preview runtime is unavailable");
    assert!(matches!(error, SandboxError::InvalidState { .. }));
}

#[tokio::test]
async fn preview_renewal_hides_unowned_sandbox() {
    let row = protected_preview_row(2);
    let (service, _) = preview_test_service(vec![vec![row]]);
    let error = service
        .preview_ready_sandbox("sbx_deadbeef01234567", 1)
        .await
        .expect_err("another user's preview sandbox must stay hidden");
    assert!(matches!(error, SandboxError::NotFound { .. }));
}

#[tokio::test]
async fn preview_share_link_requires_a_preview_password() {
    let row = agent_run_row(None);
    let (service, _) = preview_test_service(vec![vec![row]]);

    let error = service
        .preview_share_link(
            "sbx_deadbeef01234567",
            1,
            3000,
            "/pricing",
            Duration::from_secs(60),
        )
        .await
        .expect_err("open sandbox must not receive a non-expiring share link");

    assert!(matches!(error, SandboxError::InvalidState { .. }));
    assert!(error.to_string().contains("set a preview password first"));
}

#[tokio::test]
async fn preview_share_link_hides_cross_user_sandboxes() {
    let row = protected_preview_row(2);
    let (service, _) = preview_test_service(vec![vec![row]]);

    let error = service
        .preview_share_link(
            "sbx_deadbeef01234567",
            1,
            3000,
            "/",
            Duration::from_secs(60),
        )
        .await
        .expect_err("another user's sandbox must stay hidden");

    assert!(matches!(error, SandboxError::NotFound { .. }));
}

#[tokio::test]
async fn preview_share_link_rejects_zero_port_before_database_access() {
    let (service, _) = preview_test_service(Vec::new());

    let error = service
        .preview_share_link("sbx_deadbeef01234567", 1, 0, "/", Duration::from_secs(60))
        .await
        .expect_err("port zero must be rejected");

    assert!(matches!(error, SandboxError::Validation { .. }));
}

#[tokio::test]
async fn preview_share_link_preserves_database_errors() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom(
                "preview lookup unavailable".to_string(),
            )])
            .into_connection(),
    );
    let service = preview_test_service_with_db(db);

    let error = service
        .preview_share_link(
            "sbx_deadbeef01234567",
            1,
            3000,
            "/",
            Duration::from_secs(60),
        )
        .await
        .expect_err("database failures must not be collapsed into not-found");

    assert!(matches!(error, SandboxError::Database(_)));
    assert!(error.to_string().contains("preview lookup unavailable"));
}

/// Lifecycle ops on agent-run rows must be rejected with a typed
/// error naming both the sandbox and the run — pre-fix, pause/resume/
/// restart went through the standalone registry, missed the run's
/// `temps-sandbox-<run_id>` container, and let the DB row drift from
/// the real container state.
#[test]
fn ensure_not_agent_run_rejects_attributed_rows() {
    let row = agent_run_row(Some(42));
    let err = ensure_not_agent_run(&row).expect_err("agent-run row must be rejected");
    match err {
        SandboxError::ManagedByAgentRun { sandbox_id, run_id } => {
            assert_eq!(sandbox_id, "sbx_deadbeef01234567");
            assert_eq!(run_id, 42);
        }
        other => panic!("expected ManagedByAgentRun, got {:?}", other),
    }
}

#[test]
fn ensure_not_agent_run_allows_standalone_rows() {
    let row = agent_run_row(None);
    assert!(ensure_not_agent_run(&row).is_ok());
}

/// Terminal decision shared by `destroy_sandbox` (agent-run branch)
/// and `release_orphaned_agent_run_sandboxes`: every terminal status
/// allows cleanup, every active status keeps the run's ownership, and
/// a missing run row (deleted run) counts as terminal so the sandbox
/// isn't orphaned forever.
#[test]
fn run_status_is_terminal_matches_run_lifecycle() {
    for s in TERMINAL_RUN_STATUSES {
        assert!(run_status_is_terminal(Some(s)), "{} must be terminal", s);
    }
    for s in temps_ai_agents::services::run_service::ACTIVE_RUN_STATUSES {
        assert!(
            !run_status_is_terminal(Some(s)),
            "{} must NOT be terminal",
            s
        );
    }
    assert!(
        run_status_is_terminal(None),
        "missing run row must count as terminal so cleanup can proceed"
    );
}

#[test]
fn summary_from_model_copies_fields() {
    let now = Utc::now();
    let m = sandboxes::Model {
        id: 1_000_042,
        node_id: None,
        public_id: "sbx_abc1234567890def".into(),
        user_id: Some(7),
        agent_run_id: Some(42),
        name: "my-sbx".into(),
        status: "running".into(),
        image: Some("node:20".into()),
        work_dir: "/workspace".into(),
        timeout_secs: 3600,
        metadata: None,
        backend: None,
        created_at: now,
        last_activity_at: now,
        expires_at: now,
        preview_password_hash: None,
        preview_password_hint: None,
        preview_access: Default::default(),
        preview_allowed_cidrs: Vec::new(),
        egress_policy: Default::default(),
        lifecycle: "ephemeral".to_string(),
        project_id: None,
        source_repo_url: None,
    };
    let s = SandboxSummary::from(&m);
    assert_eq!(s.public_id, "sbx_abc1234567890def");
    assert_eq!(s.status, "running");
    assert_eq!(s.image.as_deref(), Some("node:20"));
    assert_eq!(s.agent_run_id, Some(42));
}

/// `extend_timeout` deliberately pushes `expires_at` past the idle window
/// and leaves `timeout_secs` alone. `touch` then runs on the very next
/// exec — the long operation the extension was bought for — so if it
/// assigned the idle deadline unconditionally it would silently cancel the
/// extension the caller just paid for. The guard has to live in the SQL,
/// because `touch` must stay a single statement with no read-modify-write
/// on the exec hot path.
#[tokio::test]
async fn touch_never_moves_the_deadline_backwards() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection(),
    );
    let service = preview_test_service_with_db(db.clone());

    service.touch(42, 3600).await;

    drop(service);
    let log = Arc::try_unwrap(db)
        .expect("service owns no database references after drop")
        .into_transaction_log();
    let statements = format!("{log:?}");

    assert!(
        statements.contains("CASE WHEN"),
        "touch must guard the deadline in SQL so an extendTimeout() is \
             never silently undone; log: {statements}"
    );
    assert!(statements.contains("expires_at"), "log: {statements}");
    assert!(
        statements.contains("last_activity_at"),
        "activity is always recorded, only the deadline is guarded; \
             log: {statements}"
    );
}

// ── Entry points of the split modules that no other test reached ────────────

#[tokio::test]
async fn extend_timeout_rejects_zero_and_oversized_extensions_before_database_access() {
    for extra_secs in [0, MAX_TIMEOUT_SECS + 1] {
        let (service, db) = preview_test_service(Vec::new());
        let error = service
            .extend_timeout("sbx_deadbeef01234567", 1, extra_secs)
            .await
            .expect_err("an out-of-range extension must be refused");
        assert!(
            matches!(error, SandboxError::Validation { .. }),
            "{extra_secs}: {error:?}"
        );
        assert_eq!(
            statements_of(service, db),
            "[]",
            "{extra_secs}: validation must run before any query"
        );
    }
}

#[tokio::test]
async fn extend_timeout_pushes_the_deadline_forward_and_records_an_event() {
    let row = agent_run_row(None);
    let mut updated = row.clone();
    updated.expires_at = row.expires_at + chrono::Duration::seconds(600);
    let (service, db) = preview_test_service(vec![vec![row], vec![updated.clone()]]);

    let got = service
        .extend_timeout("sbx_deadbeef01234567", 1, 600)
        .await
        .expect("the owner may extend a live sandbox");

    assert_eq!(got, updated);
    let statements = statements_of(service, db);
    assert!(statements.contains("expires_at"), "log: {statements}");
    assert!(statements.contains("timeout_extended"), "log: {statements}");
}

#[tokio::test]
async fn extend_timeout_hides_other_users_sandboxes() {
    let (service, _) = preview_test_service(vec![vec![protected_preview_row(2)]]);
    let error = service
        .extend_timeout("sbx_deadbeef01234567", 1, 600)
        .await
        .expect_err("another user's sandbox must stay hidden");
    assert!(matches!(error, SandboxError::NotFound { .. }), "{error:?}");
}

#[tokio::test]
async fn resize_sandbox_rejects_sizes_outside_the_disk_bounds_before_database_access() {
    for new_size_mb in [0, 255, 64 * 1024 + 1] {
        let (service, db) = preview_test_service(Vec::new());
        let error = service
            .resize_sandbox("sbx_deadbeef01234567", 1, new_size_mb)
            .await
            .expect_err("an out-of-range disk size must be refused");
        assert!(
            matches!(error, SandboxError::Validation { .. }),
            "{new_size_mb}: {error:?}"
        );
        assert_eq!(statements_of(service, db), "[]", "{new_size_mb}");
    }
}

#[tokio::test]
async fn resize_sandbox_is_refused_for_a_non_firecracker_sandbox() {
    let (service, db) = preview_test_service(vec![vec![agent_run_row(None)]]);
    let error = service
        .resize_sandbox("sbx_deadbeef01234567", 1, 2048)
        .await
        .expect_err("only Firecracker sandboxes can be resized");
    assert!(
        matches!(&error, SandboxError::Validation { message } if message.contains("Firecracker")),
        "{error:?}"
    );
    let statements = statements_of(service, db);
    assert!(
        !statements.contains("UPDATE"),
        "a refused resize writes nothing; log: {statements}"
    );
}

#[tokio::test]
async fn preview_domain_rejects_port_zero_before_database_access() {
    let (service, db) = preview_test_service(Vec::new());
    let error = service
        .domain("sbx_deadbeef01234567", 1, 0)
        .await
        .expect_err("port zero must be rejected");
    assert!(
        matches!(error, SandboxError::Validation { .. }),
        "{error:?}"
    );
    assert_eq!(statements_of(service, db), "[]");
}

#[tokio::test]
async fn preview_domain_hides_other_users_sandboxes() {
    let (service, _) = preview_test_service(vec![vec![protected_preview_row(2)]]);
    let error = service
        .domain("sbx_deadbeef01234567", 1, 3000)
        .await
        .expect_err("another user's sandbox must stay hidden");
    assert!(matches!(error, SandboxError::NotFound { .. }), "{error:?}");
}

#[tokio::test]
async fn placement_allow_list_rejects_an_unknown_node_without_saving() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<nodes::Model>::new()])
            .into_connection(),
    );
    let service = preview_test_service_with_db(db.clone());

    let error = service
        .set_allowed_node_ids(Some(vec![42]))
        .await
        .expect_err("a node that does not exist cannot be allowed");

    assert!(
        matches!(&error, SandboxError::Validation { message } if message.contains("node id 42")),
        "{error:?}"
    );
    let statements = statements_of(service, db);
    assert!(
        !statements.contains("INSERT") && !statements.contains("UPDATE"),
        "a refused allow-list is not saved; log: {statements}"
    );
}

#[tokio::test]
async fn startup_vm_reconciliation_with_no_running_rows_checks_nothing() {
    let (service, _) = preview_test_service(vec![Vec::new()]);
    let report = service.reconcile_all_running_vms().await;
    assert_eq!(report.checked, 0);
    assert_eq!(report.alive, 0);
    assert_eq!(report.marked_stopped_total, 0);
    assert!(report.marked_stopped.is_empty());
}

#[tokio::test]
async fn startup_vm_reconciliation_stops_early_when_a_page_cannot_be_read() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("sandboxes unavailable".to_string())])
            .into_connection(),
    );
    let service = preview_test_service_with_db(db);
    let report = service.reconcile_all_running_vms().await;
    assert_eq!(report.checked, 0, "an unreadable page is not guessed at");
    assert_eq!(report.marked_stopped_total, 0);
}

#[tokio::test]
async fn scoped_runtime_variables_of_an_unlinked_sandbox_are_empty() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![agent_run_row(None)]])
            .append_query_results([Vec::<sandbox_services::Model>::new()])
            .into_connection(),
    );
    let service = preview_test_service_with_db(db);
    let runtime = service
        .scoped_runtime_variables(1, "sbx_deadbeef01234567")
        .await
        .expect("a sandbox without links has an empty runtime");
    assert!(runtime.variables.is_empty());
    assert!(runtime.connected_service_ids.is_empty());
    assert!(runtime.skipped_service_ids.is_empty());
}

#[tokio::test]
async fn scoped_runtime_variables_hide_other_users_sandboxes() {
    let (service, _) = preview_test_service(vec![vec![protected_preview_row(2)]]);
    let error = service
        .scoped_runtime_variables(1, "sbx_deadbeef01234567")
        .await
        .expect_err("another user's sandbox must stay hidden");
    assert!(matches!(error, SandboxError::NotFound { .. }), "{error:?}");
}

#[tokio::test]
async fn a_project_without_services_puts_no_containers_on_the_data_network() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<project_services::Model>::new()])
            .into_connection(),
    );
    let service = preview_test_service_with_db(db);
    let count = service
        .application_data_service_count(&[7])
        .await
        .expect("an empty topology is not an error");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn synchronizing_a_data_network_hides_other_users_sandboxes() {
    let (service, _) = preview_test_service(vec![vec![protected_preview_row(2)]]);
    let error = service
        .synchronize_data_network(1, "sbx_deadbeef01234567", &[7])
        .await
        .expect_err("another user's sandbox must stay hidden");
    assert!(matches!(error, SandboxError::NotFound { .. }), "{error:?}");
}
