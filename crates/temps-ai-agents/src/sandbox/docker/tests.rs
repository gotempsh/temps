// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[test]
fn ownership_fix_changes_only_wrongly_owned_files_without_following_links() {
    use super::{ownership_fix_command, OwnershipScope};

    let home = ownership_fix_command("/home/temps", OwnershipScope::OwnFilesystem);
    assert_eq!(
        home,
        [
            "find",
            "/home/temps",
            "-xdev",
            "(",
            "!",
            "-uid",
            "1000",
            "-o",
            "!",
            "-gid",
            "1000",
            ")",
            "-exec",
            "chown",
            "-h",
            "temps:temps",
            "{}",
            "+",
        ]
    );
    assert!(!home.iter().any(|arg| arg == "-R"));

    let work = ownership_fix_command("/home/temps/workspace", OwnershipScope::AcrossMounts);
    assert_eq!(work[1], "/home/temps/workspace");
    assert!(
        !work.iter().any(|arg| arg == "-xdev"),
        "the workspace pass must reach a bind-mounted workspace: {work:?}"
    );
    assert!(work.windows(2).any(|pair| pair == ["chown", "-h"]));
}

#[test]
fn write_probe_uses_a_fresh_file_each_time() {
    let script = super::write_probe_script("/home/temps/workspace");
    assert_eq!(
        script,
        "probe=$(mktemp '/home/temps/workspace/.temps-write-probe.XXXXXX') && rm -f \"$probe\""
    );
}

#[test]
fn runtime_health_requires_harness_recovery_capability() {
    use super::has_harness_recovery_capability;

    assert!(has_harness_recovery_capability(
        r#"{"type":"health","version":1,"capabilities":["retained_runtime","recover_harness"]}"#
    ));
    assert!(!has_harness_recovery_capability(
        r#"{"type":"health","version":1,"capabilities":["retained_runtime"]}"#
    ));
    assert!(!has_harness_recovery_capability(
        r#"{"type":"error","capabilities":["recover_harness"]}"#
    ));
    assert!(!has_harness_recovery_capability("not json"));
}
#[test]
fn retained_runtime_connection_is_unprivileged_binary_duplex_not_a_shell() {
    let config = super::agent_runtime_exec_config();
    assert_eq!(config.user.as_deref(), Some("1000:1000"));
    assert_eq!(config.tty, Some(false));
    assert_eq!(config.attach_stdin, Some(true));
    assert_eq!(config.attach_stdout, Some(true));
    assert_eq!(config.attach_stderr, Some(true));
    assert_eq!(
        config.cmd,
        Some(vec![
            "/usr/local/bin/temps-sandbox-runtime".into(),
            "connect".into()
        ])
    );
    assert!(config.env.is_none());
}

#[test]
fn exec_frames_preserve_split_utf8_and_json_lines() {
    let mut bytes = b"{\"text\":\"".to_vec();
    let mut emitted = 0;
    assert!(super::completed_exec_lines(&bytes, &mut emitted, false).is_empty());
    bytes.extend_from_slice(&[0xc3]);
    assert!(super::completed_exec_lines(&bytes, &mut emitted, false).is_empty());
    bytes.extend_from_slice(b"\xa9\"}\nlast");
    assert_eq!(
        super::completed_exec_lines(&bytes, &mut emitted, false),
        vec!["{\"text\":\"é\"}"]
    );
    assert_eq!(
        super::completed_exec_lines(&bytes, &mut emitted, true),
        vec!["last"]
    );
    assert!(super::completed_exec_lines(&bytes, &mut emitted, true).is_empty());
}

#[test]
fn unbounded_exec_output_keeps_everything() {
    let mut buffer = super::ExecOutputBuffer::new(None);
    let mut lines = Vec::new();
    for i in 0..1000 {
        lines.extend(buffer.push(format!("line {i}\n").as_bytes()));
    }
    lines.extend(buffer.finish());
    assert_eq!(lines.len(), 1000);
    let output = buffer.into_output();
    assert!(output.starts_with("line 0\n"));
    assert!(output.ends_with("line 999\n"));
}

#[test]
fn bounded_exec_output_stays_within_the_limit_and_keeps_the_tail() {
    const LIMIT: usize = 256;
    let mut buffer = super::ExecOutputBuffer::new(Some(LIMIT));
    let mut delivered = 0;
    for i in 0..10_000 {
        delivered += buffer.push(format!("line {i}\n").as_bytes()).len();
        assert!(
            buffer.bytes.len() <= 2 * LIMIT + 16,
            "buffer grew to {} bytes",
            buffer.bytes.len()
        );
    }
    delivered += buffer.finish().len();
    // Every line still reaches the callback, only the kept copy is bounded.
    assert_eq!(delivered, 10_000);
    let output = buffer.into_output();
    assert!(output.len() <= LIMIT, "{} bytes", output.len());
    assert!(output.starts_with('['), "{output}");
    assert!(output.contains("earlier bytes truncated"), "{output}");
    assert!(output.ends_with("line 9999\n"), "{output}");
}

#[test]
fn bounded_exec_output_splits_a_line_without_newlines() {
    const LIMIT: usize = 64;
    let mut buffer = super::ExecOutputBuffer::new(Some(LIMIT));
    let mut pieces = Vec::new();
    // A progress bar that never prints a newline.
    for _ in 0..1000 {
        pieces.extend(buffer.push(&[b'#'; 10]));
        assert!(buffer.bytes.len() - buffer.emitted <= LIMIT);
        assert!(buffer.bytes.len() <= 2 * LIMIT + 10);
    }
    pieces.extend(buffer.finish());
    assert_eq!(pieces.iter().map(String::len).sum::<usize>(), 10_000);
    assert!(buffer.into_output().len() <= LIMIT);
}

#[test]
fn bounded_exec_output_cuts_on_a_char_boundary() {
    const LIMIT: usize = 100;
    let mut buffer = super::ExecOutputBuffer::new(Some(LIMIT));
    for _ in 0..200 {
        buffer.push("é\n".as_bytes());
    }
    buffer.finish();
    let output = buffer.into_output();
    assert!(output.len() <= LIMIT, "{} bytes", output.len());
    assert!(!output.contains('\u{FFFD}'), "{output}");
}

#[test]
fn bounded_exec_output_under_the_limit_is_unchanged() {
    let mut buffer = super::ExecOutputBuffer::new(Some(1024));
    buffer.push(b"ok\n");
    buffer.finish();
    assert_eq!(buffer.into_output(), "ok\n");
}

#[test]
fn null_network_inspection_allows_only_docker_none_network() {
    let mut container = bollard::models::ContainerInspectResponse {
        host_config: Some(bollard::models::HostConfig {
            network_mode: Some("none".into()),
            ..Default::default()
        }),
        network_settings: Some(bollard::models::NetworkSettings {
            networks: Some(std::collections::HashMap::from([(
                "none".into(),
                Default::default(),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(super::recovered_container_matches_egress_policy(
        &container, "test"
    ));
    container
        .network_settings
        .as_mut()
        .unwrap()
        .networks
        .as_mut()
        .unwrap()
        .insert("bridge".into(), Default::default());
    assert!(!super::recovered_container_matches_egress_policy(
        &container, "test"
    ));
}

use super::*;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Mutex;

/// Serializes Docker integration tests that mutate the shared sandbox
/// image (`ghcr.io/gotempsh/temps-sandbox-node:<version>`). Without
/// this, `test_pull_fallback_on_missing_hub_image` (which deletes the
/// image, forces a rebuild, then inspects it) can race with
/// `test_docker_sandbox_e2e_lifecycle` (which creates a container
/// from that same image) and produce
///   `unable to find image "sha256:..."`
/// when the delete lands mid-create.
fn docker_image_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn reused_networks_must_preserve_the_isolation_policy() {
    let managed_labels = HashMap::from([(
        "sh.temps.sandbox-data-network".to_string(),
        "true".to_string(),
    )]);
    let compliant = bollard::models::NetworkInspect {
        driver: Some("bridge".to_string()),
        internal: Some(true),
        enable_ipv6: Some(false),
        labels: Some(managed_labels.clone()),
        ..Default::default()
    };
    assert!(docker_network_matches_policy(&compliant, true, true));

    for rejected in [
        bollard::models::NetworkInspect {
            enable_ipv6: Some(true),
            ..compliant.clone()
        },
        bollard::models::NetworkInspect {
            internal: Some(false),
            ..compliant.clone()
        },
        bollard::models::NetworkInspect {
            labels: None,
            ..compliant.clone()
        },
    ] {
        assert!(!docker_network_matches_policy(&rejected, true, true));
    }

    let shared = bollard::models::NetworkInspect {
        driver: Some("bridge".to_string()),
        internal: Some(false),
        enable_ipv6: Some(false),
        ..Default::default()
    };
    assert!(docker_network_matches_policy(&shared, false, false));
}

#[test]
fn per_sandbox_network_request_requires_host_isolation() {
    let request = isolated_sandbox_network_request(
        "sandbox-network",
        "sandbox-container",
        "temps-preview-gateway-test",
    );

    assert_eq!(request.name, "sandbox-network");
    assert_eq!(request.driver.as_deref(), Some("bridge"));
    assert_eq!(request.internal, Some(true));
    assert_eq!(request.enable_ipv6, Some(false));
    assert!(has_host_isolation(request.options.as_ref()));
    assert_eq!(
        request
            .labels
            .as_ref()
            .and_then(|labels| labels.get(SANDBOX_EGRESS_POLICY_LABEL))
            .map(String::as_str),
        Some(SANDBOX_EGRESS_POLICY_VERSION)
    );
    assert_eq!(
        request
            .labels
            .as_ref()
            .and_then(|labels| labels.get(SANDBOX_NETWORK_OWNER_LABEL))
            .map(String::as_str),
        Some("sandbox-container")
    );
    assert_eq!(
        request
            .labels
            .as_ref()
            .and_then(|labels| labels.get(SANDBOX_PREVIEW_GATEWAY_LABEL))
            .map(String::as_str),
        Some("temps-preview-gateway-test")
    );
}

#[test]
fn sandbox_attaches_only_a_verified_preview_gateway_container() {
    let managed = bollard::models::ContainerInspectResponse {
        config: Some(bollard::models::ContainerConfig {
            labels: Some(HashMap::from([(
                crate::preview_gateway::PREVIEW_GATEWAY_LABEL.to_string(),
                "true".to_string(),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };
    let wrong_label = bollard::models::ContainerInspectResponse {
        config: Some(bollard::models::ContainerConfig {
            labels: Some(HashMap::from([(
                crate::preview_gateway::PREVIEW_GATEWAY_LABEL.to_string(),
                "false".to_string(),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };

    assert!(is_managed_preview_gateway(&managed));
    assert!(!is_managed_preview_gateway(&wrong_label));
    assert!(!is_managed_preview_gateway(
        &bollard::models::ContainerInspectResponse::default()
    ));
}

#[test]
fn existing_per_sandbox_network_accepts_only_complete_host_isolation_policy() {
    let request = isolated_sandbox_network_request(
        "sandbox-network",
        "sandbox-container",
        "temps-preview-gateway-test",
    );
    let modern = bollard::models::NetworkInspect {
        name: Some(request.name),
        driver: request.driver,
        internal: request.internal,
        enable_ipv6: request.enable_ipv6,
        labels: request.labels,
        options: request.options,
        ..Default::default()
    };
    assert!(sandbox_network_matches_isolation_policy(
        &modern,
        "sandbox-container",
        "temps-preview-gateway-test",
        false
    ));

    let mut legacy = modern.clone();
    let legacy_options = legacy.options.get_or_insert_with(HashMap::new);
    legacy_options.remove(BRIDGE_GATEWAY_MODE_IPV4_OPTION);
    legacy_options.insert(BRIDGE_INHIBIT_IPV4_OPTION.to_string(), "true".to_string());
    assert!(sandbox_network_matches_isolation_policy(
        &legacy,
        "sandbox-container",
        "temps-preview-gateway-test",
        false
    ));

    let mut wrong_owner = legacy.clone();
    wrong_owner.labels.get_or_insert_with(HashMap::new).insert(
        SANDBOX_NETWORK_OWNER_LABEL.to_string(),
        "another-container".to_string(),
    );
    assert!(!sandbox_network_matches_isolation_policy(
        &wrong_owner,
        "sandbox-container",
        "temps-preview-gateway-test",
        false
    ));

    let mut wrong_gateway = legacy.clone();
    wrong_gateway
        .labels
        .get_or_insert_with(HashMap::new)
        .insert(
            SANDBOX_PREVIEW_GATEWAY_LABEL.to_string(),
            "another-preview-gateway".to_string(),
        );
    assert!(!sandbox_network_matches_isolation_policy(
        &wrong_gateway,
        "sandbox-container",
        "temps-preview-gateway-test",
        true
    ));

    let mut externally_routed = legacy.clone();
    externally_routed.internal = Some(false);
    assert!(!sandbox_network_matches_isolation_policy(
        &externally_routed,
        "sandbox-container",
        "temps-preview-gateway-test",
        false
    ));

    let mut disabled_isolation = legacy;
    disabled_isolation
        .options
        .get_or_insert_with(HashMap::new)
        .insert(BRIDGE_INHIBIT_IPV4_OPTION.to_string(), "false".to_string());
    assert!(!sandbox_network_matches_isolation_policy(
        &disabled_isolation,
        "sandbox-container",
        "temps-preview-gateway-test",
        false
    ));

    let mut legacy_unscoped = modern;
    legacy_unscoped
        .labels
        .get_or_insert_with(HashMap::new)
        .remove(SANDBOX_PREVIEW_GATEWAY_LABEL);
    assert!(sandbox_network_matches_isolation_policy(
        &legacy_unscoped,
        "sandbox-container",
        crate::preview_gateway::PREVIEW_GATEWAY_CONTAINER,
        false
    ));
    assert!(!sandbox_network_matches_isolation_policy(
        &legacy_unscoped,
        "sandbox-container",
        "temps-preview-gateway-custom",
        false
    ));
    assert!(sandbox_network_matches_isolation_policy(
        &legacy_unscoped,
        "sandbox-container",
        "temps-preview-gateway-custom",
        true
    ));
}

#[test]
fn application_network_reconnects_compute_after_services() {
    let services = vec!["postgres-1".to_string(), "redis-2".to_string()];

    assert_eq!(
        application_network_connection_order("sandbox-3", &services),
        vec![
            "postgres-1".to_string(),
            "redis-2".to_string(),
            "sandbox-3".to_string(),
        ]
    );
}

#[test]
fn turn_scoped_chat_capabilities_are_owned_by_the_runtime_user() {
    assert!(is_turn_secret_path(
        "/run/secrets/temps-chat-mcp-random.json"
    ));
    assert!(!is_turn_secret_path("/run/secrets/operator-owned"));
    assert!(runtime_user_owns_uploaded_file(
        "/run/secrets/temps-chat-mcp-random.json"
    ));
    assert!(runtime_user_owns_uploaded_file(
        "/home/temps/.config/claude.json"
    ));
    assert!(!runtime_user_owns_uploaded_file(
        "/run/secrets/operator-owned"
    ));
    assert!(!runtime_user_owns_uploaded_file("/etc/shadow"));
    assert!(!is_turn_secret_path(
        "/run/secrets/temps-chat-mcp-token.json/../operator-owned"
    ));
    assert!(!is_turn_secret_path(
        "/run/secrets/temps-chat-mcp-token.json/child"
    ));
}

#[test]
fn workspace_archive_round_trip_preserves_nested_files() {
    let source = tempfile::tempdir().unwrap();
    let nested = source.path().join("src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("main.rs"), b"fn main() {}\n").unwrap();
    std::fs::write(source.path().join(".gitignore"), b"target\n").unwrap();

    let archive_dir = tempfile::tempdir().unwrap();
    let archive_path = archive_dir.path().join("workspace.tar");
    let (content_digest, size_bytes) =
        archive_workspace(source.path(), &archive_path, u64::MAX).unwrap();
    let artifact = super::super::SnapshotCompanionArtifact {
        content_path: archive_path,
        content_digest,
        size_bytes,
    };
    let restored = tempfile::tempdir().unwrap();

    restore_workspace_archive(&artifact, restored.path()).unwrap();

    assert_eq!(
        std::fs::read(restored.path().join("src/main.rs")).unwrap(),
        b"fn main() {}\n"
    );
    assert_eq!(
        std::fs::read(restored.path().join(".gitignore")).unwrap(),
        b"target\n"
    );
}

#[test]
fn workspace_restore_rejects_digest_mismatch() {
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("state.txt"), b"captured").unwrap();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive_path = archive_dir.path().join("workspace.tar");
    let (_, size_bytes) = archive_workspace(source.path(), &archive_path, u64::MAX).unwrap();
    let artifact = super::super::SnapshotCompanionArtifact {
        content_path: archive_path,
        content_digest: "0".repeat(64),
        size_bytes,
    };
    let restored = tempfile::tempdir().unwrap();

    let error = restore_workspace_archive(&artifact, restored.path()).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("digest mismatch"));
}

#[test]
fn combined_snapshot_digest_changes_with_workspace() {
    let first = combined_snapshot_digest("same-image", "workspace-one");
    let second = combined_snapshot_digest("same-image", "workspace-two");
    assert_ne!(first, second);
}

#[test]
fn workspace_archive_stops_at_size_limit() {
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("large.bin"), vec![7u8; 4096]).unwrap();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive_path = archive_dir.path().join("workspace.tar");

    let error = archive_workspace(source.path(), &archive_path, 1024).unwrap_err();

    assert!(error.to_string().contains("exceeds the 1024 byte limit"));
    assert!(std::fs::metadata(archive_path).unwrap().len() <= 1024);
}

#[tokio::test]
async fn content_addressed_publish_reuses_shared_file_without_replacing_it() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("shared.tar");
    let temporary = directory.path().join("attempt.tar");
    std::fs::write(&destination, b"shared snapshot bytes").unwrap();
    std::fs::write(&temporary, b"shared snapshot bytes").unwrap();
    let (digest, _) = hash_file(&destination).unwrap();

    let created = publish_content_addressed_file(&temporary, &destination, &digest)
        .await
        .unwrap();

    assert!(!created);
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(destination).unwrap(),
        b"shared snapshot bytes"
    );
}

#[tokio::test]
async fn docker_restore_rejects_snapshot_without_immutable_image_id() {
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("snapshot.tar");
    std::fs::write(&primary, b"verified-image-tar").unwrap();
    let (digest, size_bytes) = hash_file(&primary).unwrap();
    let artifact = super::super::SnapshotArtifact {
        content_path: primary,
        content_digest: digest.clone(),
        primary_digest: digest,
        size_bytes,
        backend: super::super::SandboxBackend::Docker,
        image_ref: Some("temps-snapshot/legacy:latest".to_string()),
        image_id: None,
        workspace: None,
    };
    // Refused before Docker is called: a client that needs no socket
    // keeps the test running on a host without one.
    let provider = DockerSandboxProvider::new(
        Arc::new(Docker::connect_with_http_defaults().unwrap()),
        DockerSandboxConfig::default(),
    );
    let config = SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id: 1,
        container_name_override: Some("missing-image-id".to_string()),
        host_work_dir: directory.path().join("workspace"),
        workspace_volume: None,
        image: None,
        cpu_limit: None,
        memory_limit_mb: None,
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: Some("none".to_string()),
        env_vars: HashMap::new(),
        idle_timeout: std::time::Duration::from_secs(60),
        backend: Some(super::super::SandboxBackend::Docker),
    };

    let error = provider
        .create_from_snapshot(&artifact, config)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("no immutable Docker image ID"));
}

#[tokio::test]
async fn docker_workspace_verification_error_identifies_artifact_and_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("snapshot.tar");
    std::fs::write(&primary, b"verified-image-tar").unwrap();
    let (primary_digest, primary_size) = hash_file(&primary).unwrap();
    let missing_workspace = directory.path().join("missing-workspace.tar");
    let logical_digest = "logical-snapshot-digest".to_string();
    let artifact = super::super::SnapshotArtifact {
        content_path: primary,
        content_digest: logical_digest.clone(),
        primary_digest,
        size_bytes: primary_size,
        backend: super::super::SandboxBackend::Docker,
        image_ref: Some("temps-snapshot/context:latest".to_string()),
        image_id: Some("sha256:immutable-image".to_string()),
        workspace: Some(super::super::SnapshotCompanionArtifact {
            content_path: missing_workspace.clone(),
            content_digest: "missing-workspace-digest".to_string(),
            size_bytes: 1,
        }),
    };
    // Refused before Docker is called: a client that needs no socket
    // keeps the test running on a host without one.
    let provider = DockerSandboxProvider::new(
        Arc::new(Docker::connect_with_http_defaults().unwrap()),
        DockerSandboxConfig::default(),
    );
    let config = SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id: 2,
        container_name_override: Some("workspace-context".to_string()),
        host_work_dir: directory.path().join("workspace"),
        workspace_volume: None,
        image: None,
        cpu_limit: None,
        memory_limit_mb: None,
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: Some("none".to_string()),
        env_vars: HashMap::new(),
        idle_timeout: std::time::Duration::from_secs(60),
        backend: Some(super::super::SandboxBackend::Docker),
    };

    let error = provider
        .create_from_snapshot(&artifact, config)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains(&missing_workspace.display().to_string()));
    assert!(error.contains(&logical_digest));
}

/// Root maintenance commands resolve `chown`/`su`/`cp` through PATH. The
/// sandbox image places user-writable bin directories ahead of the system
/// ones, so the PATH handed to a root exec must contain system
/// directories only — otherwise a sandbox user can drop `~/.local/bin/chown`
/// and have it executed as container root on the next recovery.
#[test]
fn root_exec_path_excludes_sandbox_writable_directories() {
    let entries: Vec<&str> = ROOT_EXEC_PATH.split(':').collect();
    assert!(!entries.is_empty());

    for entry in &entries {
        assert!(
            entry.starts_with('/'),
            "PATH entry {entry:?} must be absolute"
        );
        assert!(
            !entry.contains(SANDBOX_HOME),
            "PATH entry {entry:?} is inside the sandbox user's home"
        );
        assert!(
            !entry.contains('~') && !entry.is_empty() && *entry != ".",
            "PATH entry {entry:?} must not be relative or home-relative"
        );
    }

    // The specific directories the sandbox image prepends and the user can write.
    for writable in [
        "/home/temps/.local/bin",
        "/home/temps/.bun/bin",
        "/home/temps/.opencode/bin",
    ] {
        assert!(
            !entries.contains(&writable),
            "{writable} must not be on the root exec PATH"
        );
    }

    // And it must still be able to find the commands we actually run.
    assert!(entries.contains(&"/usr/bin"));
    assert!(entries.contains(&"/bin"));
    assert!(entries.contains(&"/usr/sbin"));
    assert!(entries.contains(&"/sbin"));
}

#[test]
fn runtime_health_probe_uses_sandbox_uid_without_a_tty() {
    let config = runtime_health_exec_config();
    assert_eq!(
        config.user.as_deref(),
        Some(format!("{SANDBOX_UID}:{SANDBOX_GID}").as_str())
    );
    assert_eq!(
        config.cmd,
        Some(vec![
            "/usr/local/bin/temps-sandbox-runtime".to_string(),
            "request".to_string(),
        ])
    );
    assert_ne!(config.tty, Some(true));
}

/// Regression: the PATH pin has to cover *every* root exec, not just
/// `run_root_exec`.
///
/// `exec_as_root` reaches `exec_inner` with the caller's env map, which
/// never sets PATH — so before this, it inherited the image PATH with the
/// sandbox user's writable bin directories in front. Its live caller is the
/// pre-snapshot credential shred, where a planted `sh`/`shred`/`rm` runs as
/// container root *before* the credential file is wiped.
#[test]
fn every_root_exec_form_is_recognised_for_path_pinning() {
    // What `exec_as_root` actually passes, plus the other spellings Docker
    // accepts for the same privilege.
    for root in ["0:0", "0", "root", "root:root", "0:1000", "root:staff"] {
        assert!(
            exec_runs_as_root(Some(root)),
            "{root:?} runs as root and must get the pinned PATH"
        );
    }

    // Non-root execs keep their own environment untouched.
    for non_root in ["1000:1000", "temps", "temps:temps", "rootless", "10"] {
        assert!(
            !exec_runs_as_root(Some(non_root)),
            "{non_root:?} is not root and must not be rewritten"
        );
    }

    // `None` means "the image's own user", which is the sandbox user.
    assert!(!exec_runs_as_root(None));
}

#[test]
fn test_container_name_format() {
    assert_eq!(
        DockerSandboxProvider::container_name(42),
        "temps-sandbox-42"
    );
}

/// Regression: the home volume name must be derivable from the
/// container name alone.
///
/// The leak this replaced: `create` built the volume name from
/// `config.run_id` while `destroy` rebuilt it by stripping the prefix
/// off `handle.sandbox_name`. For agent runs both produce the same
/// string, so the bug was invisible there — but standalone sandboxes
/// override the container suffix with their `public_id` label, so
/// destroy asked Docker to remove a volume that had never existed and
/// the real one stayed on disk for good.
#[test]
fn home_volume_name_round_trips_through_container_name() {
    // Agent-run style: numeric suffix.
    assert_eq!(
        DockerSandboxProvider::home_volume_name(&DockerSandboxProvider::container_name(42)),
        "temps-sandbox-home-v2-42"
    );
    // Standalone style: opaque public_id label suffix. This is the
    // case that used to leak.
    assert_eq!(
        DockerSandboxProvider::home_volume_name("temps-sandbox-a1b2c3d4e5f60718"),
        "temps-sandbox-home-v2-a1b2c3d4e5f60718"
    );
}

/// A standalone sandbox (`container_name_override`) and an agent run
/// that happen to share a numeric id must not share a home volume.
/// Keying on `run_id` meant sandbox row 5 and agent run 5 both mounted
/// `temps-sandbox-home-5` — one sandbox reading another's shell
/// history, `~/.claude` credentials, and project files.
#[test]
fn home_volume_name_does_not_collide_across_naming_schemes() {
    let agent_run =
        DockerSandboxProvider::home_volume_name(&DockerSandboxProvider::container_name(5));
    let standalone = DockerSandboxProvider::home_volume_name("temps-sandbox-abc123");
    assert_ne!(agent_run, standalone);
}

fn create_config_for(run_id: i32, override_label: Option<&str>) -> SandboxCreateConfig {
    SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id,
        container_name_override: override_label.map(|s| s.to_string()),
        host_work_dir: std::path::PathBuf::from("/tmp/does-not-matter"),
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
        idle_timeout: Duration::from_secs(60),
        backend: None,
    }
}

/// The Docker-less guard for the leak. `create` takes both names from
/// `sandbox_names`, and `destroy` re-derives the volume from the
/// container name — so asserting the pair agrees here catches a
/// reintroduction of the original bug on any machine, including CI
/// runners with no Docker daemon (where the e2e below skips).
#[test]
fn sandbox_names_agree_for_standalone_and_agent_run() {
    // Standalone: opaque public_id label. The old code derived the
    // volume from run_id instead, producing temps-sandbox-home-7 here
    // while destroy looked for temps-sandbox-home-a1b2c3d4e5f60718.
    let (container, volume) =
        DockerSandboxProvider::sandbox_names(&create_config_for(7, Some("a1b2c3d4e5f60718")));
    assert_eq!(container, "temps-sandbox-a1b2c3d4e5f60718");
    assert_eq!(volume, "temps-sandbox-home-v2-a1b2c3d4e5f60718");
    assert_eq!(volume, DockerSandboxProvider::home_volume_name(&container));
    assert_ne!(volume, "temps-sandbox-home-7");
    assert_ne!(volume, "temps-sandbox-home-v2-7");

    // Agent run: numeric naming, no override.
    let (container, volume) = DockerSandboxProvider::sandbox_names(&create_config_for(42, None));
    assert_eq!(container, "temps-sandbox-42");
    assert_eq!(volume, "temps-sandbox-home-v2-42");
    assert_eq!(volume, DockerSandboxProvider::home_volume_name(&container));
}

/// The bind the container actually gets must mount the volume `destroy`
/// will remove.
///
/// This is the Docker-less guard for the original bug's *call site*.
/// The naming tests above only prove the helpers agree with each other;
/// re-inlining `format!("temps-sandbox-home-{}", config.run_id)` into
/// `create`'s binds would leave every one of them green and re-ship the
/// leak on any CI runner without a daemon. Asserting on the bind string
/// closes that, because the bind is what `create` actually hands Docker.
#[test]
fn create_binds_mount_the_volume_destroy_will_remove() {
    let cfg = create_config_for(7, Some("a1b2c3d4e5f60718"));
    let (container, volume) = DockerSandboxProvider::sandbox_names(&cfg);
    let binds = DockerSandboxProvider::container_binds("/host/work", &volume);

    // The home bind must name the volume destroy re-derives from the
    // container name — not anything keyed on run_id.
    let home_bind = format!(
        "{}:{}",
        DockerSandboxProvider::home_volume_name(&container),
        SANDBOX_HOME
    );
    assert!(
        binds.contains(&home_bind),
        "binds {:?} must mount {} — otherwise create and destroy \
             disagree about which volume belongs to this sandbox",
        binds,
        home_bind
    );
    assert!(
        !binds.iter().any(|b| b.starts_with("temps-sandbox-home-7:")),
        "binds {:?} must not key the home volume on run_id",
        binds
    );
    // And the work dir is still mounted where the sandbox expects it.
    assert!(binds.contains(&format!("/host/work:{}", CONTAINER_WORK_DIR)));
}

#[test]
fn sandbox_source_import_staging_has_hard_byte_and_inode_limits() {
    let tmpfs = DockerSandboxProvider::container_tmpfs();
    assert_eq!(
        tmpfs.get("/run/temps-source-import").map(String::as_str),
        Some("size=256m,nr_inodes=5001,mode=0700,uid=1000,gid=1000")
    );
    assert!(tmpfs.contains_key("/run/secrets"));
}

/// No name this build generates may land in the pre-fix namespace.
///
/// Legacy volumes are `temps-sandbox-home-<digits>`. On an upgraded host
/// they are stranded but still present, and Docker attaches an existing
/// volume by name — so if we could still generate one of those names, an
/// agent run would mount a previous standalone sandbox's `/home/temps`,
/// i.e. another user's credentials and shell history.
#[test]
fn generated_names_never_reenter_the_pre_fix_namespace() {
    let legacy_shaped = |name: &str| {
        name.strip_prefix(HOME_VOLUME_PREFIX)
            .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false)
    };
    // Sanity: the predicate really does describe the legacy shape.
    assert!(legacy_shaped("temps-sandbox-home-5"));

    for container in [
        DockerSandboxProvider::container_name(5),
        DockerSandboxProvider::container_name(1),
        DockerSandboxProvider::container_name(i32::MAX),
        "temps-sandbox-0123456789012345".to_string(), // all-digit hex label
    ] {
        let volume = DockerSandboxProvider::home_volume_name(&container);
        assert!(
            !legacy_shaped(&volume),
            "{} produced {}, which collides with a pre-fix volume and \
                 could mount another tenant's home",
            container,
            volume
        );
    }
}

/// Every home volume must carry the prefix an operator filters on when
/// reclaiming stranded volumes by hand — a naming change on the create
/// side would otherwise leave volumes nothing can find.
#[test]
fn home_volume_names_carry_the_documented_prefix() {
    for container in ["temps-sandbox-1", "temps-sandbox-deadbeef", "no-prefix"] {
        assert!(
            DockerSandboxProvider::home_volume_name(container).starts_with(HOME_VOLUME_PREFIX),
            "{} produced a volume outside the documented prefix",
            container
        );
    }
}

#[test]
fn test_default_config() {
    let config = DockerSandboxConfig::default();
    assert_eq!(config.runtime, "node");
    assert_eq!(config.custom_image, "");
    assert_eq!(config.default_cpu_limit, 4.0);
    assert_eq!(config.default_memory_limit_mb, 8192);
    assert_eq!(config.network_mode, "full");
}

#[test]
fn test_resolved_image_for_presets_stable_channel() {
    let v = SANDBOX_IMAGE_VERSION;
    // Test against the channel-explicit helper so the result doesn't
    // depend on whatever TEMPS_SANDBOX_CHANNEL happens to be set to in
    // the test runner's environment.
    for (runtime, expected) in [
        ("node", format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")),
        (
            "python",
            format!("ghcr.io/gotempsh/temps-sandbox-python:{v}"),
        ),
        ("rust", format!("ghcr.io/gotempsh/temps-sandbox-rust:{v}")),
        ("bun", format!("ghcr.io/gotempsh/temps-sandbox-bun:{v}")),
        ("go", format!("ghcr.io/gotempsh/temps-sandbox-go:{v}")),
        ("full", format!("ghcr.io/gotempsh/temps-sandbox-full:{v}")),
    ] {
        assert_eq!(
            image_name_for_runtime_in_channel(runtime, SandboxChannel::Stable),
            expected,
            "runtime={}",
            runtime
        );
    }
}

#[test]
fn test_resolved_image_custom() {
    let config = DockerSandboxConfig {
        runtime: "custom".to_string(),
        custom_image: "my-registry/my-agent:v2".to_string(),
        ..Default::default()
    };
    assert_eq!(config.resolved_image(), "my-registry/my-agent:v2");
}

#[test]
fn test_resolved_image_custom_empty_falls_back() {
    // Custom runtime with empty custom_image falls through to the
    // preset path, producing a "custom" preset name. (The actual
    // preset list rejects this at create time; the resolver doesn't
    // validate.) Channel suffix tracks env, so we test the helper
    // explicitly to avoid env-dependence.
    assert_eq!(
        image_name_for_runtime_in_channel("custom", SandboxChannel::Stable),
        format!("ghcr.io/gotempsh/temps-sandbox-custom:{SANDBOX_IMAGE_VERSION}")
    );
}

#[test]
fn immutable_digest_rebuild_is_rejected_before_any_docker_action() {
    for image in [
        "ghcr.io/gotempsh/temps-sandbox-node@sha256:aaaaaaaa",
        "sha256:aaaaaaaa",
    ] {
        assert!(matches!(
            ensure_mutable_rebuild_target(image),
            Err(AgentError::ImmutableSandboxImageRebuild { image: rejected }) if rejected == image
        ));
    }
    assert!(ensure_mutable_rebuild_target("ghcr.io/gotempsh/temps-sandbox-node:dev").is_ok());
}

#[test]
fn test_dockerfile_for_runtime_node() {
    let df = dockerfile_for_runtime("node");
    assert!(df.contains("FROM ubuntu:24.04"));
    assert!(
        df.contains("claude.ai/install.sh"),
        "must use native Claude installer"
    );
    assert!(df.contains("git"));
    assert!(df.contains("jq"), "jq must be installed for memory script");
    assert!(
        df.contains("nodesource"),
        "node runtime must install Node.js via NodeSource"
    );
}

#[test]
fn test_all_runtimes_install_jq() {
    // The memory script requires jq. Every runtime preset must install it.
    for runtime in &["node", "bun", "python", "rust", "go", "full"] {
        let df = dockerfile_for_runtime(runtime);
        assert!(
            df.contains("jq"),
            "runtime {} dockerfile must install jq",
            runtime
        );
    }
}

#[test]
fn test_dockerfile_for_runtime_python() {
    let df = dockerfile_for_runtime("python");
    assert!(df.contains("FROM python:3.12-slim"));
    assert!(df.contains("claude.ai/install.sh"));
    assert!(df.contains("uv"));
}

#[test]
fn test_dockerfile_for_runtime_rust() {
    let df = dockerfile_for_runtime("rust");
    assert!(df.contains("FROM rust:1-slim"));
    assert!(df.contains("claude.ai/install.sh"));
}

#[test]
fn test_dockerfile_for_runtime_bun() {
    let df = dockerfile_for_runtime("bun");
    assert!(df.contains("FROM oven/bun:latest"));
    assert!(df.contains("claude.ai/install.sh"));
}

#[test]
fn test_dockerfile_for_runtime_go() {
    let df = dockerfile_for_runtime("go");
    assert!(df.contains("FROM golang:1.23-bookworm"));
    assert!(df.contains("claude.ai/install.sh"));
}

#[test]
fn test_dockerfile_for_runtime_full() {
    let df = dockerfile_for_runtime("full");
    assert!(df.contains("FROM ubuntu:24.04"));
    assert!(df.contains("claude.ai/install.sh"));
    assert!(df.contains("python3"));
    assert!(df.contains("golang-go"));
    assert!(df.contains("nodejs"));
    assert!(df.contains("uv"));
}

#[test]
fn test_dockerfile_for_unknown_runtime_defaults_to_node() {
    let df = dockerfile_for_runtime("unknown");
    assert!(df.contains("FROM ubuntu:24.04"));
}

#[test]
fn test_image_name_for_runtime_shape() {
    // The env-reading wrapper picks a channel at runtime, so we don't
    // pin the exact tag — just check the structural shape every
    // returned name MUST have. The channel-explicit variants below
    // (test_image_name_for_runtime_*_channel) cover the exact strings.
    for runtime in &["node", "", "python", "bun", "rust", "go", "full"] {
        let img = image_name_for_runtime(runtime);
        assert!(
            img.starts_with("ghcr.io/gotempsh/temps-sandbox-"),
            "image must be GHCR-qualified: {img}"
        );
        assert!(img.contains(':'), "image must carry a tag: {img}");
    }
}

#[tokio::test]
async fn test_docker_provider_recover_no_docker() {
    // If Docker isn't available, connect will fail — we test gracefully
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping test");
            return;
        }
    };

    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping test");
        return;
    }

    let provider = DockerSandboxProvider::new(Arc::new(docker), DockerSandboxConfig::default());

    // Recover a run that doesn't exist
    let result = provider.recover(999999).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_docker_sandbox_e2e_lifecycle() {
    // Serialize against other tests that delete/rebuild the shared
    // sandbox image; see `docker_image_lock`.
    let _guard = docker_image_lock().lock().await;
    // Full lifecycle: create → exec → is_alive → recover → destroy
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping e2e test");
            return;
        }
    };
    let docker = Arc::new(docker);

    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping e2e test");
        return;
    }

    let config = DockerSandboxConfig::default();
    let provider = DockerSandboxProvider::new(docker.clone(), config);

    // Ensure the default image is built
    if let Err(e) = provider.ensure_image().await {
        println!("Cannot build sandbox image, skipping e2e test: {}", e);
        return;
    }

    let run_id = 900_000 + (std::process::id() % 90_000) as i32;
    let work_dir = std::env::temp_dir().join(format!("sandbox-e2e-test-{}", run_id));
    let _ = std::fs::create_dir_all(&work_dir);
    std::fs::write(work_dir.join("test.txt"), "hello from test").unwrap();

    // 1. Create sandbox
    let create_config = SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id,
        container_name_override: None,
        host_work_dir: work_dir.clone(),
        workspace_volume: None,
        image: None,
        cpu_limit: Some(1.0),
        memory_limit_mb: Some(512),
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: Some("none".to_string()),
        env_vars: HashMap::from([("TEST_VAR".to_string(), "test_value".to_string())]),
        idle_timeout: Duration::from_secs(120),
        backend: None,
    };

    // Some dev environments (macOS Docker Desktop's virtiofs / userns-remap
    // on Linux) refuse to honor chown on bind-mounted host directories,
    // which trips the post-create write-probe with "permission denied".
    // That's a real signal in production but expected here, so degrade
    // to skip rather than fail.
    let handle = match provider.create(create_config).await {
        Ok(h) => h,
        Err(AgentError::SandboxCreationFailed { reason, .. }) if reason.contains("write-probe") => {
            println!(
                "Skipping e2e test: bind-mount filesystem doesn't honor chown ({})",
                reason
            );
            return;
        }
        Err(e) => panic!("sandbox creation failed: {:?}", e),
    };
    assert!(handle.sandbox_name.contains("temps-sandbox-"));
    assert!(!handle.sandbox_id.is_empty());

    // 2. Verify it's alive
    assert!(provider.is_alive(&handle).await.unwrap());

    // 3. Execute a command — check the work dir is mounted
    let result = provider
        .exec(
            &handle,
            vec![
                "cat".to_string(),
                format!("{}/test.txt", CONTAINER_WORK_DIR),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("hello from test"));

    // 4. Execute with env vars
    let result = provider
        .exec(
            &handle,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo $MY_VAR".to_string(),
            ],
            HashMap::from([("MY_VAR".to_string(), "injected".to_string())]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("injected"));

    // 5. A turn capability must be visible through the live tmpfs mount
    // to the unprivileged runtime user. Docker's archive upload API can
    // report success while placing the file below that mount, so this is
    // an end-to-end regression check rather than an ownership-only test.
    let turn_secret_path = "/run/secrets/temps-chat-e2e.json";
    provider
        .write_file(&handle, turn_secret_path, b"turn-capability", 0o600)
        .await
        .unwrap();
    let read_turn_secret = provider
        .exec(
            &handle,
            vec!["cat".to_string(), turn_secret_path.to_string()],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let cleanup_turn_secret = provider
        .exec_as_root(
            &handle,
            vec![
                "rm".to_string(),
                "-f".to_string(),
                "--".to_string(),
                turn_secret_path.to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(read_turn_secret.exit_code, 0);
    // The streamed exec adapter is line-oriented and terminates emitted
    // lines with `\n`; assert the capability bytes independently of that
    // transport framing.
    assert_eq!(
        read_turn_secret.stdout.trim_end_matches('\n'),
        "turn-capability"
    );
    assert_eq!(cleanup_turn_secret.exit_code, 0);

    // 6. Verify recovery — simulate finding existing container
    let recovered = provider.recover(run_id).await.unwrap();
    assert!(recovered.is_some());
    let recovered_handle = recovered.unwrap();
    assert_eq!(recovered_handle.sandbox_name, handle.sandbox_name);

    // 7. Add container-layer state, then capture both it and the mounted
    // workspace through the real provider.
    let write_layer = provider
        .exec(
            &handle,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'layer-state' > /tmp/snapshot-layer-state".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(write_layer.exit_code, 0);
    provider.stop(&handle).await.unwrap();
    let artifact = provider
        .take_snapshot(&handle, Some("e2e-round-trip".to_string()), u64::MAX)
        .await
        .unwrap();

    // 8. Destroy the source and remove the daemon tag so restore must load
    // and verify the exported image tar rather than use the cache.
    provider.destroy(&handle, true).await.unwrap();
    let image_ref = artifact
        .image_ref
        .clone()
        .expect("Docker snapshot image reference");
    provider.delete_image(&image_ref).await.unwrap();

    // 9. Restore into a new empty host workspace and verify both captured
    // files through provider exec.
    let restored_work_dir =
        std::env::temp_dir().join(format!("sandbox-e2e-restore-test-{}", run_id));
    let _ = std::fs::remove_dir_all(&restored_work_dir);
    std::fs::create_dir_all(&restored_work_dir).unwrap();
    let restored = provider
        .create_from_snapshot(
            &artifact,
            SandboxCreateConfig {
                node_id: None,
                owner_user_id: None,
                run_id: run_id + 1,
                container_name_override: None,
                host_work_dir: restored_work_dir.clone(),
                workspace_volume: None,
                image: None,
                cpu_limit: Some(1.0),
                memory_limit_mb: Some(512),
                pids_limit: None,
                disk_size_mb: None,
                allow_docker: false,
                egress_policy: Default::default(),
                network_mode: Some("none".to_string()),
                env_vars: HashMap::new(),
                idle_timeout: Duration::from_secs(120),
                backend: Some(crate::sandbox::SandboxBackend::Docker),
            },
        )
        .await
        .unwrap();
    let restored_state = provider
        .exec(
            &restored,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "cat {}/test.txt; cat /tmp/snapshot-layer-state",
                    CONTAINER_WORK_DIR
                ),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();

    // Gather assertions after cleanup so assertion failures cannot leak
    // Docker resources or content-addressed files.
    let source_alive = provider.is_alive(&handle).await.unwrap_or(false);
    let source_recovered = provider.recover(run_id).await.unwrap();
    provider.destroy(&restored, true).await.unwrap();
    let _ = provider.delete_image(&image_ref).await;
    let _ = tokio::fs::remove_file(&artifact.content_path).await;
    if let Some(workspace) = &artifact.workspace {
        let _ = tokio::fs::remove_file(&workspace.content_path).await;
    }
    let _ = std::fs::remove_dir_all(&restored_work_dir);
    let _ = std::fs::remove_dir_all(&work_dir);

    // 10. Verify source destruction and restored state.
    assert!(!source_alive);
    assert!(source_recovered.is_none());
    assert_eq!(restored_state.exit_code, 0);
    assert!(restored_state.stdout.contains("hello from test"));
    assert!(restored_state.stdout.contains("layer-state"));
}

#[cfg(unix)]
#[test]
fn directory_upload_follows_only_links_inside_the_directory() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("token"), b"node token").unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills/a")).unwrap();
    std::fs::write(dir.path().join("skills/a/SKILL.md"), b"skill").unwrap();
    std::os::unix::fs::symlink("skills/a/SKILL.md", dir.path().join("alias.md")).unwrap();
    std::os::unix::fs::symlink("skills", dir.path().join("linked")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("token"), dir.path().join("token")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();
    std::os::unix::fs::symlink("/", dir.path().join("root")).unwrap();
    std::os::unix::fs::symlink("missing", dir.path().join("dangling")).unwrap();
    // A loop back to the root must not recurse forever.
    std::os::unix::fs::symlink(".", dir.path().join("self")).unwrap();

    let plan = directory_upload_files(dir.path(), dir.path()).unwrap();
    let mut relative: Vec<String> = plan
        .files
        .iter()
        .map(|(_, rel)| rel.to_string_lossy().into_owned())
        .collect();
    relative.sort();
    assert_eq!(
        relative,
        vec!["alias.md", "linked/a/SKILL.md", "skills/a/SKILL.md"]
    );
    assert_eq!(
        skipped_set(&plan),
        vec![
            ("dangling".to_string(), UploadSkipReason::Unreadable),
            ("outside".to_string(), UploadSkipReason::OutsideRoot),
            ("root".to_string(), UploadSkipReason::OutsideRoot),
            ("self".to_string(), UploadSkipReason::LinkLoop),
            ("token".to_string(), UploadSkipReason::OutsideRoot),
        ]
    );
}

#[cfg(unix)]
fn skipped_set(plan: &DirectoryUploadPlan) -> Vec<(String, UploadSkipReason)> {
    let mut skipped: Vec<(String, UploadSkipReason)> = plan
        .skipped
        .iter()
        .map(|s| (s.relative_path.to_string_lossy().into_owned(), s.reason))
        .collect();
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    skipped
}

/// A repository that keeps skills at its root and links them into
/// `.claude/`: the wider containment root (the checkout) lets the link
/// through, while a link escaping the checkout is still skipped and
/// reported, and a loop terminates.
#[cfg(unix)]
#[test]
fn directory_upload_follows_links_into_a_wider_containment_root() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("token"), b"node token").unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("skills/foo")).unwrap();
    std::fs::write(repo.path().join("skills/foo/SKILL.md"), b"foo skill").unwrap();
    let claude = repo.path().join(".claude");
    std::fs::create_dir_all(claude.join("skills")).unwrap();
    std::fs::write(claude.join("settings.json"), b"{}").unwrap();
    std::os::unix::fs::symlink("../../skills/foo", claude.join("skills/foo")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("token"), claude.join("token")).unwrap();
    std::os::unix::fs::symlink(outside.path(), claude.join("skills/leak")).unwrap();
    // Loops back to the checkout root, which contains `.claude` itself.
    std::os::unix::fs::symlink("../..", claude.join("skills/up")).unwrap();

    let relative_files = |plan: &DirectoryUploadPlan| -> Vec<String> {
        let mut files: Vec<String> = plan
            .files
            .iter()
            .map(|(_, rel)| rel.to_string_lossy().into_owned())
            .collect();
        files.sort();
        files
    };

    let plan = directory_upload_files(&claude, repo.path()).unwrap();
    let files = relative_files(&plan);
    assert!(
        files.contains(&"skills/foo/SKILL.md".to_string()),
        "{files:?}"
    );
    assert!(files.contains(&"settings.json".to_string()), "{files:?}");
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with("token") || f.contains("leak")),
        "{files:?}"
    );
    let skipped = skipped_set(&plan);
    assert!(
        skipped.contains(&("token".to_string(), UploadSkipReason::OutsideRoot)),
        "{skipped:?}"
    );
    assert!(
        skipped.contains(&("skills/leak".to_string(), UploadSkipReason::OutsideRoot)),
        "{skipped:?}"
    );
    // `skills/up` reaches the checkout root, so the walk sees `.claude`
    // once more beneath it, then stops at the link that would enter
    // `skills/up` again.
    assert!(
        skipped.contains(&(
            "skills/up/.claude/skills/up".to_string(),
            UploadSkipReason::LinkLoop
        )),
        "{skipped:?}"
    );

    // With the uploaded directory as its own root (the default), the
    // same repository link is skipped — and reported, not dropped.
    let narrow = directory_upload_files(&claude, &claude).unwrap();
    assert_eq!(relative_files(&narrow), vec!["settings.json"]);
    let skipped = skipped_set(&narrow);
    assert!(
        skipped.contains(&("skills/foo".to_string(), UploadSkipReason::OutsideRoot)),
        "{skipped:?}"
    );
    assert!(
        skipped.contains(&("skills/up".to_string(), UploadSkipReason::OutsideRoot)),
        "{skipped:?}"
    );
}

#[cfg(unix)]
#[test]
fn directory_upload_rejects_a_directory_outside_its_containment_root() {
    let repo = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), repo.path().join(".claude")).unwrap();

    let err = directory_upload_files(&repo.path().join(".claude"), repo.path()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("containment root"), "{err}");
}

#[cfg(unix)]
#[test]
fn staged_directory_upload_holds_plain_files_only() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("token"), b"node token").unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("skills/foo")).unwrap();
    std::fs::write(repo.path().join("skills/foo/SKILL.md"), b"foo skill").unwrap();
    let claude = repo.path().join(".claude");
    std::fs::create_dir_all(claude.join("skills")).unwrap();
    std::os::unix::fs::symlink("../../skills/foo", claude.join("skills/foo")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("token"), claude.join("token")).unwrap();
    let staging = tempfile::tempdir().unwrap();

    let plan = stage_directory_upload(&claude, repo.path(), staging.path()).unwrap();

    let staged = staging.path().join("skills/foo/SKILL.md");
    assert!(!staged.is_symlink());
    assert!(!staging.path().join("skills/foo").is_symlink());
    assert_eq!(std::fs::read(&staged).unwrap(), b"foo skill");
    assert!(!staging.path().join("token").exists());
    assert_eq!(
        skipped_set(&plan),
        vec![("token".to_string(), UploadSkipReason::OutsideRoot)]
    );
    // The staged tree uploads unchanged with its own root.
    let restaged = directory_upload_files(staging.path(), staging.path()).unwrap();
    assert_eq!(restaged.files.len(), 1);
    assert!(restaged.skipped.is_empty());
}

#[tokio::test]
async fn docker_directory_upload_and_bounded_read_stay_inside_their_limits() {
    let _guard = docker_image_lock().lock().await;
    let Ok(docker) = Docker::connect_with_local_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return;
    }
    let docker = Arc::new(docker);
    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
    if let Err(e) = provider.ensure_image().await {
        println!("Cannot build sandbox image, skipping: {}", e);
        return;
    }
    let label = format!("upload-bounds-{}", std::process::id());
    let work_dir = std::env::temp_dir().join(format!("sandbox-{label}"));
    std::fs::create_dir_all(&work_dir).unwrap();
    let handle = match provider
        .create(SandboxCreateConfig {
            node_id: None,
            owner_user_id: None,
            run_id: 0,
            container_name_override: Some(label.clone()),
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(512),
            pids_limit: None,
            disk_size_mb: None,
            allow_docker: false,
            egress_policy: Default::default(),
            network_mode: Some("none".to_string()),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(120),
            backend: None,
        })
        .await
    {
        Ok(h) => h,
        Err(AgentError::SandboxCreationFailed { reason, .. }) if reason.contains("write-probe") => {
            println!("Skipping: bind-mount filesystem doesn't honor chown ({reason})");
            let _ = std::fs::remove_dir_all(&work_dir);
            return;
        }
        Err(e) => panic!("sandbox creation failed: {e:?}"),
    };

    let result = async {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"host secret").unwrap();
        let upload = tempfile::tempdir().unwrap();
        std::fs::write(upload.path().join("inside.txt"), b"inside").unwrap();
        std::os::unix::fs::symlink("inside.txt", upload.path().join("alias.txt")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret"),
            upload.path().join("leak.txt"),
        )
        .unwrap();
        provider
            .write_directory(&handle, upload.path(), "/home/temps/upload")
            .await
            .unwrap();
        assert_eq!(
            provider
                .read_file(&handle, "/home/temps/upload/alias.txt")
                .await
                .unwrap(),
            b"inside"
        );
        assert!(
            provider
                .read_file(&handle, "/home/temps/upload/leak.txt")
                .await
                .is_err(),
            "a link escaping the uploaded directory must not be copied"
        );

        let big = vec![b'x'; 256 * 1024];
        provider
            .write_file(&handle, "/home/temps/big.bin", &big, 0o644)
            .await
            .unwrap();
        let err = provider
            .read_file_bounded(&handle, "/home/temps/big.bin", 64 * 1024)
            .await
            .unwrap_err();
        match &err {
            AgentError::Validation { message } => {
                assert!(message.contains("/home/temps/big.bin"), "{message}");
                assert!(message.contains(&handle.sandbox_name), "{message}");
                assert!(message.contains("65536"), "{message}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
        assert_eq!(
            provider
                .read_file_bounded(&handle, "/home/temps/big.bin", 1024 * 1024)
                .await
                .unwrap(),
            big
        );
    }
    .await;
    provider.destroy(&handle, true).await.unwrap();
    let _ = std::fs::remove_dir_all(&work_dir);
    result
}

#[tokio::test]
async fn test_docker_sandbox_image_status() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let docker = Arc::new(docker);

    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping test");
        return;
    }

    let provider = DockerSandboxProvider::new(docker, DockerSandboxConfig::default());
    assert!(provider.is_available().await);

    let (_, image_name) = provider.image_status().await.unwrap();
    assert!(
        image_name.starts_with("ghcr.io/gotempsh/temps-sandbox-"),
        "got: {image_name}"
    );
}

#[tokio::test]
async fn test_docker_sandbox_custom_runtime() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let docker = Arc::new(docker);

    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping test");
        return;
    }

    // Test that different runtimes produce different images
    let config = DockerSandboxConfig {
        runtime: "python".to_string(),
        ..Default::default()
    };
    let provider = DockerSandboxProvider::new(docker, config);

    let (_, image_name) = provider.image_status().await.unwrap();
    // image_status returns whatever channel the env points at; both
    // stable and beta are valid targets here, so we check the
    // structural shape rather than the exact string.
    assert!(
        image_name.starts_with("ghcr.io/gotempsh/temps-sandbox-python:")
            || image_name.starts_with("ghcr.io/gotempsh/temps-sandbox-python@sha256:"),
        "got: {image_name}"
    );
}

#[test]
fn test_image_name_for_runtime_stable_channel() {
    let v = SANDBOX_IMAGE_VERSION;
    let stable = SandboxChannel::Stable;
    assert_eq!(
        image_name_for_runtime_in_channel("node", stable),
        format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")
    );
    assert_eq!(
        image_name_for_runtime_in_channel("", stable),
        format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")
    );
    assert_eq!(
        image_name_for_runtime_in_channel("python", stable),
        format!("ghcr.io/gotempsh/temps-sandbox-python:{v}")
    );
    assert_eq!(
        image_name_for_runtime_in_channel("bun", stable),
        format!("ghcr.io/gotempsh/temps-sandbox-bun:{v}")
    );
    assert_eq!(
        image_name_for_runtime_in_channel("full", stable),
        format!("ghcr.io/gotempsh/temps-sandbox-full:{v}")
    );
}

#[test]
fn test_image_name_for_runtime_beta_channel() {
    let v = SANDBOX_IMAGE_VERSION;
    let beta = SandboxChannel::Beta;
    assert_eq!(
        image_name_for_runtime_in_channel("node", beta),
        format!("ghcr.io/gotempsh/temps-sandbox-node:{v}-beta")
    );
    assert_eq!(
        image_name_for_runtime_in_channel("python", beta),
        format!("ghcr.io/gotempsh/temps-sandbox-python:{v}-beta")
    );
    assert_eq!(
        image_name_for_runtime_in_channel("full", beta),
        format!("ghcr.io/gotempsh/temps-sandbox-full:{v}-beta")
    );
}

#[test]
fn test_runtime_from_image_name() {
    let v = SANDBOX_IMAGE_VERSION;
    // Round-trip: every runtime preset should be recoverable from the
    // image name we'd publish for it. This is what protects the
    // recovery code path that needs to figure out which Dockerfile to
    // regenerate when a missing image needs rebuilding.
    for runtime in &["node", "python", "rust", "bun", "go", "full"] {
        let stable = image_name_for_runtime_in_channel(runtime, SandboxChannel::Stable);
        assert_eq!(runtime_from_image_name(&stable), Some(*runtime));
        let beta = image_name_for_runtime_in_channel(runtime, SandboxChannel::Beta);
        assert_eq!(runtime_from_image_name(&beta), Some(*runtime));
        assert_eq!(
            runtime_from_image_name(&format!(
                "ghcr.io/gotempsh/temps-sandbox-{runtime}@sha256:{}",
                "a".repeat(64)
            )),
            Some(*runtime)
        );
    }
    // Custom images (anything outside our prefix) return None so the
    // recovery code falls back to a plain `docker pull` instead of
    // trying to materialize a Dockerfile for an unknown "runtime".
    assert_eq!(
        runtime_from_image_name("docker.io/library/alpine:3.19"),
        None
    );
    assert_eq!(runtime_from_image_name("temps-sandbox-node:0.1.0"), None);
    // Tag is optional in the parser — recovery still works even if
    // the input lost its tag somehow.
    assert_eq!(
        runtime_from_image_name(&format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")),
        Some("node")
    );
}

#[test]
fn only_current_preset_reference_can_be_materialized_locally() {
    let selected = image_name_for_runtime("node");
    assert_eq!(selected_preset_runtime(&selected), Some("node"));
    assert_eq!(
        selected_preset_runtime("ghcr.io/gotempsh/temps-sandbox-node:0.0.1"),
        None
    );
    assert_eq!(
        selected_preset_runtime("docker.io/library/node:latest"),
        None
    );
}

#[test]
fn test_sandbox_channel_default_is_stable() {
    // Whatever the developer's env happens to be, an unset / non-`beta`
    // value must always resolve to stable. The matcher only treats the
    // exact string "beta" as opt-in.
    assert_eq!(SandboxChannel::Stable.tag_suffix(), "");
    assert_eq!(SandboxChannel::Beta.tag_suffix(), "-beta");
}

#[tokio::test]
async fn test_pull_fallback_on_missing_hub_image() {
    // Serialize against other tests that build/use the shared sandbox
    // image; this test deletes it mid-suite which would otherwise race
    // with `test_docker_sandbox_e2e_lifecycle` and produce a
    // `unable to find image "sha256:..."` container-create failure.
    let _guard = docker_image_lock().lock().await;
    // Verify that ensure_image_for_runtime succeeds even when the
    // Docker Hub image doesn't exist — it should fall back to a
    // local build. We test by pointing at a non-existent hub image
    // (which is the normal case until images are published).
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping test");
        return;
    }

    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());

    // `ensure_image_for_runtime` tags both the pulled and the
    // locally-built image with the fully-qualified GHCR name including
    // the channel suffix — that's the string we must remove beforehand
    // and inspect afterwards. An earlier version of this test used the
    // short `temps-sandbox-node:{version}` name; that name has never
    // been produced by the provider, so the post-build inspect would
    // always fail.
    let local_image = image_name_for_runtime("node");

    // Delete the local image first (if any) so we exercise the
    // pull→fail→build path. Ignore errors if it doesn't exist.
    let _ = docker
        .remove_image(
            &local_image,
            None::<bollard::query_parameters::RemoveImageOptions>,
            None,
        )
        .await;

    // This should succeed via fallback build even when the registry
    // image doesn't exist (or rate-limits the pull) — the local build
    // path is the safety net.
    let result = provider.ensure_image_for_runtime("node").await;
    assert!(
        result.is_ok(),
        "ensure_image should succeed via fallback build: {:?}",
        result.err()
    );

    // Image should now exist locally under its GHCR-qualified name.
    assert!(docker.inspect_image(&local_image).await.is_ok());
}

#[tokio::test]
async fn test_kill_processes_term_and_kill() {
    // Serialize against other tests that build/delete the shared
    // sandbox image; see `docker_image_lock`.
    let _guard = docker_image_lock().lock().await;
    // Integration test: create a sandbox, spawn a `sleep` process,
    // kill it with SIGTERM, verify it's gone. Then spawn another,
    // kill with SIGKILL, verify it's gone.
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping test");
        return;
    }

    // If `temps serve` is running, it periodically cleans up sandbox
    // containers whose run_id isn't in the database. That kills our
    // test containers within ~1s. Skip the test to avoid flakes.
    let containers = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: false,
            filters: Some(HashMap::from([(
                "name".to_string(),
                vec!["temps-sandbox-".to_string()],
            )])),
            ..Default::default()
        }))
        .await
        .unwrap_or_default();
    if !containers.is_empty() {
        println!(
            "temps serve is managing {} sandbox(es) — skipping kill_processes test to avoid flakes",
            containers.len()
        );
        return;
    }

    let config = DockerSandboxConfig::default();
    let provider = DockerSandboxProvider::new(docker.clone(), config);

    if let Err(e) = provider.ensure_image().await {
        println!("Cannot build sandbox image, skipping: {}", e);
        return;
    }

    let run_id = 99992;
    let work_dir = std::env::temp_dir().join(format!("sandbox-kill-test-{}", run_id));
    let _ = std::fs::create_dir_all(&work_dir);

    let create_config = SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id,
        container_name_override: None,
        host_work_dir: work_dir.clone(),
        workspace_volume: None,
        image: None,
        cpu_limit: Some(1.0),
        memory_limit_mb: Some(256),
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: Some("none".to_string()),
        env_vars: HashMap::new(),
        idle_timeout: Duration::from_secs(60),
        backend: None,
    };

    let handle = provider.create(create_config).await.unwrap();

    // Verify the sandbox is actually alive before proceeding.
    // If `temps serve` is running, it may kill test containers whose
    // run_id isn't in the database — skip gracefully rather than fail.
    tokio::time::sleep(Duration::from_millis(500)).await;
    if !provider.is_alive(&handle).await.unwrap_or(false) {
        println!("Container exited immediately after create — skipping kill_processes test");
        let _ = provider.destroy(&handle, true).await;
        let _ = std::fs::remove_dir_all(&work_dir);
        return;
    }

    // --- SIGTERM test ---
    // Write a script that daemonizes itself (double-fork pattern) so
    // the sleep outlives the Docker exec session.
    if let Err(e) = provider
        .write_file(
            &handle,
            "/tmp/spawn_sleep.sh",
            b"#!/bin/sh\n(sleep 9999 &)\nexit 0\n",
            0o755,
        )
        .await
    {
        println!(
            "Container died before write_file (temps serve cleanup?) — skipping: {}",
            e
        );
        let _ = provider.destroy(&handle, true).await;
        let _ = std::fs::remove_dir_all(&work_dir);
        return;
    }

    let _ = provider
        .exec(
            &handle,
            vec!["/tmp/spawn_sleep.sh".to_string()],
            HashMap::new(),
            None,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify sleep is running
    let result = provider
        .exec(
            &handle,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "pgrep -fx 'sleep 9999' | wc -l".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let count: i32 = result.stdout.trim().parse().unwrap_or(0);
    assert!(count > 0, "sleep should be running before kill");

    // Kill with SIGTERM via our typed enum. The pattern names only the sleep
    // this test started: the sandbox keeps itself alive with `sleep infinity`,
    // which must outlive this, so counting every `sleep` never reaches zero.
    provider
        .kill_processes(&handle, "sleep 9999", crate::sandbox::KillSignal::Term)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify sleep is gone
    let result = provider
        .exec(
            &handle,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "pgrep -fx 'sleep 9999' | wc -l".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let count: i32 = result.stdout.trim().parse().unwrap_or(0);
    assert_eq!(count, 0, "sleep should be gone after SIGTERM");

    // --- SIGKILL test ---
    // Spawn a new sleep for the SIGKILL test.
    let _ = provider
        .exec(
            &handle,
            vec!["/tmp/spawn_sleep.sh".to_string()],
            HashMap::new(),
            None,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify it started
    let result = provider
        .exec(
            &handle,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "pgrep -fx 'sleep 9999' | wc -l".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let count: i32 = result.stdout.trim().parse().unwrap_or(0);
    assert!(count > 0, "sleep should be running before SIGKILL");

    // SIGKILL — cannot be trapped
    provider
        .kill_processes(&handle, "sleep 9999", crate::sandbox::KillSignal::Kill)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let result = provider
        .exec(
            &handle,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "pgrep -fx 'sleep 9999' | wc -l".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let count: i32 = result.stdout.trim().parse().unwrap_or(0);
    assert_eq!(count, 0, "sleep should be gone after SIGKILL");

    // Cleanup
    provider.destroy(&handle, true).await.unwrap();
    let _ = std::fs::remove_dir_all(&work_dir);
}

/// Regression test for the "stopped container auto-removed on
/// recovery" bug. The earlier `recover_container` force-removed any
/// container it found in stopped state, destroying the filesystem +
/// volumes of any expired sandbox the user hadn't resumed yet.
///
/// Invariant this pins down: after `stop` + `recover_by_name`, the
/// container must still exist in Docker, a handle must be returned,
/// and a subsequent `start` must succeed. If this test starts failing,
/// the sandbox fleet is silently losing user data on every server
/// restart that happens between stop and resume — fix the registry
/// or provider, not the test.
#[tokio::test]
async fn recover_by_name_preserves_stopped_containers() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping recovery regression test");
            return;
        }
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping recovery regression test");
        return;
    }

    // If `temps serve` is running it will clean up any container whose
    // run_id isn't in its DB. That would kill this test. Skip to avoid
    // flakes — same guard the kill_processes test uses.
    let existing = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: false,
            filters: Some(HashMap::from([(
                "name".to_string(),
                vec!["temps-sandbox-".to_string()],
            )])),
            ..Default::default()
        }))
        .await
        .unwrap_or_default();
    if !existing.is_empty() {
        println!(
            "temps serve is managing {} sandbox(es) — skipping recovery regression test",
            existing.len()
        );
        return;
    }

    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
    if provider.ensure_image().await.is_err() {
        println!("Cannot build sandbox image, skipping recovery regression test");
        return;
    }

    // Use a label-style name (what standalone sandboxes use) so we
    // exercise exactly the recover_by_name path, not recover(run_id).
    let label = "recover-test-abcdef";
    let run_id = 99994;
    let work_dir = std::env::temp_dir().join(format!("sandbox-recover-test-{}", run_id));
    let _ = std::fs::create_dir_all(&work_dir);

    let create_config = SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id,
        container_name_override: Some(label.to_string()),
        host_work_dir: work_dir.clone(),
        workspace_volume: None,
        image: None,
        cpu_limit: Some(1.0),
        memory_limit_mb: Some(256),
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: Some("none".to_string()),
        env_vars: HashMap::new(),
        idle_timeout: Duration::from_secs(60),
        backend: None,
    };

    let handle = provider
        .create(create_config)
        .await
        .expect("create sandbox");

    // Stop the container — this is the state the expiration sweeper
    // leaves a sandbox in when the user doesn't resume before expiry.
    provider.stop(&handle).await.expect("stop container");

    // Sanity check: docker still knows about the container.
    let inspect = docker
        .inspect_container(
            &handle.sandbox_name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("container still exists after stop");
    let running = inspect.state.and_then(|s| s.running).unwrap_or(true);
    assert!(!running, "container should be stopped, not running");

    // NOW the recovery call the bug lived in. Previously this
    // force-removed the stopped container and returned None. Post-fix
    // it must return a handle and leave the container alone.
    let recovered = provider
        .recover_by_name(label)
        .await
        .expect("recover_by_name does not error")
        .expect(
            "recover_by_name must return a handle for stopped \
                 containers — the old behavior silently deleted them, \
                 losing user data on every server restart between stop \
                 and resume",
        );
    assert_eq!(recovered.sandbox_name, handle.sandbox_name);

    // Container is still there AND still stopped (recovery is read-only).
    let inspect2 = docker
        .inspect_container(
            &handle.sandbox_name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("container still exists after recovery");
    assert!(
        !inspect2.state.and_then(|s| s.running).unwrap_or(true),
        "recovery must not start the container — that's the caller's job"
    );

    // And it's actually usable: we can start it back up via the
    // recovered handle, which is exactly what resume_sandbox does.
    provider
        .start(&recovered)
        .await
        .expect("start using recovered handle");

    // Cleanup (only place that should ever call remove_container).
    provider.destroy(&recovered, true).await.unwrap();
    let _ = std::fs::remove_dir_all(&work_dir);
}

/// Docker-level proof that destroying a *standalone* sandbox actually
/// frees its home volume.
///
/// This is the disk-fill regression: standalone sandboxes name their
/// container after the `public_id` label, and destroy used to
/// reconstruct the volume name from that label while create had named
/// it after the numeric `run_id`. Every create/destroy cycle therefore
/// stranded one volume — with `~/.npm`, `~/.cache`, and the sandbox
/// home in it — and a host churning sandboxes filled its disk with
/// storage no API could reach. Asserting on the volume (not the
/// container) is the point: the container was always removed correctly,
/// which is exactly why this went unnoticed.
#[tokio::test]
async fn destroy_frees_the_home_volume_of_a_standalone_sandbox() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping volume purge test");
            return;
        }
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping volume purge test");
        return;
    }

    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
    if provider.ensure_image().await.is_err() {
        println!("Cannot build sandbox image, skipping volume purge test");
        return;
    }

    // Standalone naming: opaque label suffix, numeric run_id. The two
    // deliberately disagree — that divergence is what the bug rode on.
    let label = "purgetestc0ffee01";
    let run_id = 99993;
    let work_dir = std::env::temp_dir().join(format!("sandbox-purge-test-{}", run_id));
    let _ = std::fs::create_dir_all(&work_dir);

    // A `temps serve` on this host reaps sandbox containers whose run
    // isn't in its DB, which would kill this test mid-flight. Same guard
    // the sibling e2e tests use — and this repo's dev workflow runs
    // several slots, so it fires in practice.
    let existing = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: false,
            filters: Some(HashMap::from([(
                "name".to_string(),
                vec!["temps-sandbox-".to_string()],
            )])),
            ..Default::default()
        }))
        .await
        .unwrap_or_default();
    if !existing.is_empty() {
        println!(
            "temps serve is managing {} sandbox(es) — skipping volume purge test",
            existing.len()
        );
        return;
    }

    let volume_name = format!("{}{}{}", HOME_VOLUME_PREFIX, HOME_VOLUME_SCHEME, label);
    let legacy_volume_name = format!("{}{}", HOME_VOLUME_PREFIX, run_id);
    // Clear both names first: a leftover from an older build would make
    // the label assertion below fail for a reason that isn't this code.
    for name in [&volume_name, &legacy_volume_name] {
        let _ = docker
            .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
            .await;
    }

    let handle = provider
        .create(SandboxCreateConfig {
            node_id: None,
            owner_user_id: None,
            run_id,
            container_name_override: Some(label.to_string()),
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(256),
            pids_limit: None,
            disk_size_mb: None,
            allow_docker: false,
            egress_policy: Default::default(),
            network_mode: Some("none".to_string()),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
        })
        .await
        .expect("create sandbox");

    // Gather every observation BEFORE tearing down, then assert after —
    // an assertion that fires between create and destroy would leak the
    // container, its volume, and the work dir onto the host.
    let volume = docker.inspect_volume(&volume_name).await;
    let legacy_volume = docker.inspect_volume(&legacy_volume_name).await;
    let container = docker
        .inspect_container(
            &handle.sandbox_name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await;

    provider.destroy(&handle, true).await.expect("destroy");

    let after_destroy = docker.inspect_volume(&volume_name).await;
    let _ = std::fs::remove_dir_all(&work_dir);

    // 1. The volume exists while the sandbox does, and is labelled so an
    //    operator can find it if a later destroy ever fails to.
    let volume = volume.expect("home volume should exist while the sandbox does");
    assert!(
        volume.labels.contains_key(HOME_VOLUME_LABEL),
        "home volume {} must carry {} — it is the only handle an \
             operator has for reclaiming stranded volumes; labels were {:?}",
        volume_name,
        HOME_VOLUME_LABEL,
        volume.labels
    );

    // 2. The container actually mounts that volume at /home/temps.
    //    Without this, the assertion above passes on its own merits —
    //    `create` pre-creates the volume, so its existence no longer
    //    proves the bind exists, and dropping the bind entirely would
    //    go unnoticed.
    let mounts = container
        .expect("container should exist")
        .mounts
        .unwrap_or_default();
    assert!(
        mounts.iter().any(|m| {
            m.name.as_deref() == Some(volume_name.as_str())
                && m.destination.as_deref() == Some(SANDBOX_HOME)
        }),
        "container must mount {} at {} — mounts were {:?}",
        volume_name,
        SANDBOX_HOME,
        mounts
    );

    // 3. The run_id-keyed name is the one the old code would have made.
    //    Assert it was never created, so this test cannot pass by
    //    accident on a host where both happen to exist.
    assert!(
        legacy_volume.is_err(),
        "home volume must be keyed on the container label, not run_id"
    );

    // 4. And destroy actually frees it — the leak this PR exists for.
    assert!(
        after_destroy.is_err(),
        "destroy must remove the sandbox home volume {} — leaving it \
             behind is the leak that fills the host disk after enough \
             create/destroy cycles",
        volume_name
    );
}

// ---- managed egress policy tests (no Docker required) -------------------

#[test]
fn sandbox_proxy_environment_overrides_caller_routes() {
    let environment = HashMap::from([
        (
            "HTTPS_PROXY".to_string(),
            "http://attacker.invalid:8080".to_string(),
        ),
        ("NO_PROXY".to_string(), "*".to_string()),
        ("SAFE_VALUE".to_string(), "preserved".to_string()),
    ]);

    let network = sandbox_network_name("temps-sandbox-test");
    let result = sandbox_container_environment(environment, &network);
    let expected_proxy = format!("http://{SANDBOX_EGRESS_PROXY_ALIAS}:{SANDBOX_EGRESS_PROXY_PORT}");

    assert!(result.contains(&format!("HTTPS_PROXY={expected_proxy}")));
    assert!(result.contains(&format!("HTTP_PROXY={expected_proxy}")));
    assert!(result.contains(&format!("https_proxy={expected_proxy}")));
    assert!(result.contains(&format!("http_proxy={expected_proxy}")));
    assert!(
        result.contains(&"NO_PROXY=localhost,127.0.0.1,::1,temps-sandbox-egress-proxy".to_string())
    );
    assert!(result.contains(&"SAFE_VALUE=preserved".to_string()));
    assert!(!result
        .iter()
        .any(|entry| entry.contains("attacker.invalid")));
    assert!(!result.iter().any(|entry| entry == "NO_PROXY=*"));
}

#[test]
fn sandbox_proxy_environment_is_not_injected_for_no_network() {
    let result = sandbox_container_environment(HashMap::new(), "none");
    assert!(result.is_empty());
}

#[test]
fn requested_network_mode_overrides_provider_default_for_all_create_steps() {
    let container_name = "temps-sandbox-override-test";

    assert_eq!(
        effective_sandbox_network("none", Some("full"), container_name),
        sandbox_network_name(container_name)
    );
    assert_eq!(
        effective_sandbox_network("full", Some("none"), container_name),
        "none"
    );
    assert_eq!(
        effective_sandbox_network("restricted", None, container_name),
        sandbox_network_name(container_name)
    );
}

fn inspected_sandbox(
    container_name: &str,
    network_names: &[&str],
    environment: Vec<String>,
    network_mode: &str,
) -> bollard::models::ContainerInspectResponse {
    bollard::models::ContainerInspectResponse {
        config: Some(bollard::models::ContainerConfig {
            env: Some(environment),
            ..Default::default()
        }),
        host_config: Some(bollard::models::HostConfig {
            network_mode: Some(network_mode.to_string()),
            ..Default::default()
        }),
        network_settings: Some(bollard::models::NetworkSettings {
            networks: Some(
                network_names
                    .iter()
                    .map(|name| {
                        (
                            (*name).to_string(),
                            bollard::models::EndpointSettings::default(),
                        )
                    })
                    .collect(),
            ),
            ..Default::default()
        }),
        name: Some(container_name.to_string()),
        ..Default::default()
    }
}

#[test]
fn recovered_sandbox_requires_exact_network_and_proxy_policy() {
    let container_name = "temps-sandbox-policy-test";
    let network = sandbox_network_name(container_name);
    let environment = sandbox_container_environment(HashMap::new(), &network);
    let compliant = inspected_sandbox(container_name, &[&network], environment.clone(), &network);
    assert!(recovered_container_matches_egress_policy(
        &compliant,
        container_name
    ));

    let dual_attached = inspected_sandbox(
        container_name,
        &[&network, "temps-sandbox-net"],
        environment.clone(),
        &network,
    );
    assert!(!recovered_container_matches_egress_policy(
        &dual_attached,
        container_name
    ));

    let bad_no_proxy = inspected_sandbox(
        container_name,
        &[&network],
        environment
            .iter()
            .map(|entry| {
                if entry.starts_with("NO_PROXY=") {
                    "NO_PROXY=*".to_string()
                } else {
                    entry.clone()
                }
            })
            .collect(),
        &network,
    );
    assert!(!recovered_container_matches_egress_policy(
        &bad_no_proxy,
        container_name
    ));

    let none_with_attachment = inspected_sandbox(container_name, &["bridge"], Vec::new(), "none");
    assert!(!recovered_container_matches_egress_policy(
        &none_with_attachment,
        container_name
    ));
}

#[test]
fn sandbox_proxy_policy_is_fail_closed_and_blocks_private_destinations() {
    for required_guard in [
        "octets[0] === 10",
        "octets[0] === 127",
        "octets[0] === 169 && octets[1] === 254",
        "octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31",
        "octets[0] === 192 && octets[1] === 168",
        "answers.some(({ address }) => isPrivateAddress(address))",
        "if (policy.mode === \"internet\") return [80, 443].includes(port);",
        "if (!mayResolve(egressPolicy, host, port)) throw new Denied(\"policy\", destination);",
        "const egressPolicy = loadEgressPolicy(process.env.TEMPS_SANDBOX_EGRESS_POLICY);",
        "address = await admit(target.hostname, port);",
        "const address = await admit(hostname, port);",
        "HTTP/1.1 403 Forbidden",
    ] {
        assert!(
            SANDBOX_EGRESS_PROXY_SCRIPT.contains(required_guard),
            "managed proxy is missing security guard: {required_guard}"
        );
    }
    assert!(!SANDBOX_EGRESS_PROXY_SCRIPT.contains("TEMPS_ALLOW_INSECURE"));
    assert!(SANDBOX_NETWORK_PREFIX.starts_with("temps-sandbox-net-v3-"));
    assert_eq!(
        managed_sandbox_network("full", "temps-sandbox-test"),
        sandbox_network_name("temps-sandbox-test")
    );
    assert_eq!(
        managed_sandbox_network("restricted", "temps-sandbox-test"),
        sandbox_network_name("temps-sandbox-test")
    );
    assert_eq!(
        managed_sandbox_network("host", "temps-sandbox-test"),
        sandbox_network_name("temps-sandbox-test")
    );
    assert_eq!(managed_sandbox_network("none", "ignored"), "none");
}

#[test]
fn sandbox_proxy_exposes_only_the_capability_scoped_model_relay_route() {
    assert_eq!(
        SANDBOX_MODEL_RELAY_BASE_URL,
        "http://temps-sandbox-egress-proxy:3128/.temps/model-relay"
    );
    for required_guard in [
        "incoming.host !== proxyAuthority",
        "request.method === \"POST\"",
        "request.method === \"GET\"",
        "[a-f0-9]{32}",
        "v1\\/messages(?:\\/count_tokens)?",
        "|responses",
        "|chat\\/completions)$/",
        r"\/(models)",
        "key !== \"client_version\"",
        "[A-Za-z0-9._+-]{1,64}",
        "/api/ai/sandbox-models/",
        "target.search = incoming.search",
        "if (!relayTarget) throw new Error",
    ] {
        assert!(
            SANDBOX_EGRESS_PROXY_SCRIPT.contains(required_guard),
            "model relay reverse proxy is missing guard: {required_guard}"
        );
    }
    assert!(!SANDBOX_EGRESS_PROXY_SCRIPT.contains("allowPrivate = true"));
    assert_eq!(
        sandbox_egress_proxy_extra_hosts(),
        vec!["host.docker.internal:host-gateway"]
    );
    let matching_container = bollard::models::ContainerInspectResponse {
        host_config: Some(bollard::models::HostConfig {
            extra_hosts: Some(sandbox_egress_proxy_extra_hosts()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(container_has_extra_host(
        &matching_container,
        SANDBOX_HOST_GATEWAY
    ));
}

#[test]
fn sandbox_proxy_executes_strict_model_relay_query_policy() {
    let function_start = SANDBOX_EGRESS_PROXY_SCRIPT
        .find("function modelRelayTarget(request) {")
        .expect("model relay policy function");
    let function_end = SANDBOX_EGRESS_PROXY_SCRIPT[function_start..]
        .find("\n\nfunction mcpTarget(request) {")
        .map(|offset| function_start + offset)
        .expect("end of model relay policy function");
    let function = &SANDBOX_EGRESS_PROXY_SCRIPT[function_start..function_end];
    let script = format!(
        r#"
const proxyAuthority = "temps-sandbox-egress-proxy:3128";
const controlPlane = new URL("http://host.docker.internal:8080");
{function}
const relay = "0123456789abcdef0123456789abcdef";
const target = (method, suffix) => modelRelayTarget({{
  method,
  url: `/.temps/model-relay/${{relay}}/${{suffix}}`,
  headers: {{ host: proxyAuthority }},
}});
const accepted = [
  target("POST", "v1/messages?beta=true"),
  target("POST", "v1/messages/count_tokens?beta=true"),
];
if (accepted.some((value) => !value || value.search !== "?beta=true")) process.exit(1);
const chat = target("POST", "chat/completions");
if (!chat || chat.search !== "" || !chat.pathname.endsWith(`/${{relay}}/chat/completions`)) process.exit(3);
const denied = [
  target("POST", "chat/completions?stream=true"),
  target("GET", "chat/completions"),
  target("POST", "chat/completions/extra"),
  target("POST", "responses?beta=true"),
  target("POST", "v1/messages?beta=false"),
  target("POST", "v1/messages?beta=true&beta=true"),
  target("POST", "v1/messages?other=true"),
  target("POST", "v1/messages?beta=true&other=true"),
];
if (denied.some(Boolean)) process.exit(2);
"#
    );
    let output = match std::process::Command::new("node")
        .args(["-e", &script])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping executable proxy policy regression: Node.js is unavailable");
            return;
        }
        Err(error) => panic!("failed to execute Node.js proxy policy regression: {error}"),
    };
    assert!(
        output.status.success(),
        "proxy policy regression failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sandbox_proxy_exposes_only_bounded_git_smart_http_routes() {
    for required_guard in [
        "incoming.host !== proxyAuthority",
        "request.method === \"GET\"",
        "request.method === \"POST\"",
        "([a-f0-9]{32})",
        "([1-9][0-9]*)",
        "git-upload-pack|git-receive-pack",
        "?service=git-upload-pack",
        "?service=git-receive-pack",
        "/api/git/sandbox-relay/",
        "if (!target || !gitRelayAuthorized(request))",
        "await forwardGitRelay(request, response)",
    ] {
        assert!(
            SANDBOX_EGRESS_PROXY_SCRIPT.contains(required_guard),
            "Git relay reverse proxy is missing guard: {required_guard}"
        );
    }
}

#[test]
fn sandbox_proxy_executes_strict_git_smart_http_policy() {
    let function_start = SANDBOX_EGRESS_PROXY_SCRIPT
        .find("function gitRelayTarget(request) {")
        .expect("Git relay policy function");
    let function_end = SANDBOX_EGRESS_PROXY_SCRIPT[function_start..]
        .find("\n\nasync function forward(request, response, target, allowPrivate) {")
        .map(|offset| function_start + offset)
        .expect("end of Git relay policy function");
    let function = &SANDBOX_EGRESS_PROXY_SCRIPT[function_start..function_end];
    let script = format!(
        r#"
const proxyAuthority = "temps-sandbox-egress-proxy:3128";
const controlPlane = new URL("http://host.docker.internal:8080/base");
{function}
const relay = "0123456789abcdef0123456789abcdef";
const target = (method, suffix, host = proxyAuthority) => gitRelayTarget({{
  method,
  url: `/.temps/git/${{relay}}/42/${{suffix}}`,
  headers: {{ host }},
}});
const accepted = [
  target("GET", "info/refs?service=git-upload-pack"),
  target("GET", "info/refs?service=git-receive-pack"),
  target("POST", "git-upload-pack"),
  target("POST", "git-receive-pack"),
];
if (accepted.some((value) => !value ||
    !value.pathname.startsWith("/base/api/git/sandbox-relay/"))) process.exit(1);
const denied = [
  target("PUT", "git-upload-pack"),
  target("GET", "git-upload-pack"),
  target("POST", "info/refs?service=git-upload-pack"),
  target("GET", "info/refs"),
  target("GET", "info/refs?service=git-upload-pack&x=1"),
  target("GET", "info/refs?service=git%2dupload-pack"),
  target("POST", "git-upload-pack?service=git-upload-pack"),
  target("POST", "git%2dupload-pack"),
  gitRelayTarget({{ method: "POST", url: `/.temps/git/${{relay}}/0/git-upload-pack`, headers: {{ host: proxyAuthority }} }}),
  gitRelayTarget({{ method: "POST", url: `/.temps/git/${{relay}}/01/git-upload-pack`, headers: {{ host: proxyAuthority }} }}),
  gitRelayTarget({{ method: "POST", url: `/.temps/git/${{relay.toUpperCase()}}/42/git-upload-pack`, headers: {{ host: proxyAuthority }} }}),
  gitRelayTarget({{ method: "POST", url: `/.temps/git/${{relay}}%2f42/git-upload-pack`, headers: {{ host: proxyAuthority }} }}),
  target("POST", "git-upload-pack", "control-plane.test:8080"),
];
if (denied.some(Boolean)) process.exit(2);
"#
    );
    let output = match std::process::Command::new("node")
        .args(["-e", &script])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping executable Git relay policy regression: Node.js is unavailable");
            return;
        }
        Err(error) => panic!("failed to execute Node.js Git relay policy regression: {error}"),
    };
    assert!(
        output.status.success(),
        "Git relay policy regression failed (status {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sandbox_proxy_never_forwards_git_without_independent_authorization() {
    let authorization_start = SANDBOX_EGRESS_PROXY_SCRIPT
        .find("function gitRelayAuthorized(_request) {")
        .expect("Git relay authorization function");
    let function_end = SANDBOX_EGRESS_PROXY_SCRIPT[authorization_start..]
        .find("\n\nasync function forward(request, response, target, allowPrivate) {")
        .map(|offset| authorization_start + offset)
        .expect("end of Git relay forwarding function");
    let functions = &SANDBOX_EGRESS_PROXY_SCRIPT[authorization_start..function_end];
    let handler_start = SANDBOX_EGRESS_PROXY_SCRIPT
        .find("async function handleRequest(request, response) {")
        .expect("HTTP request handler");
    let handler_end = SANDBOX_EGRESS_PROXY_SCRIPT[handler_start..]
        .find("\n\nconst server = http.createServer(handleRequest);")
        .map(|offset| handler_start + offset)
        .expect("end of HTTP request handler");
    let handler = &SANDBOX_EGRESS_PROXY_SCRIPT[handler_start..handler_end];
    let script = format!(
        r#"
const proxyAuthority = "temps-sandbox-egress-proxy:3128";
let privateForwardCalls = 0;
function modelRelayTarget() {{ return null; }}
function mcpTarget() {{ return null; }}
function gitRelayTarget() {{ return new URL("http://control-plane.test/api/git/sandbox-relay/id/1/git-upload-pack"); }}
async function forward(_request, response, _target, allowPrivate) {{
  if (allowPrivate) privateForwardCalls += 1;
  response.end();
}}
function refuseRequest(_request, response) {{
  response.writeHead(403);
  response.end();
}}
{functions}
{handler}
function response() {{
  return {{ status: 0, writeHead(code) {{ this.status = code; }}, end() {{}} }};
}}
(async () => {{
  const relay = "0123456789abcdef0123456789abcdef";
  const attempts = [
    {{ method: "GET", url: `/.temps/git/${{relay}}/42/info/refs?service=git-upload-pack`, headers: {{ host: proxyAuthority, authorization: "Bearer fake" }} }},
    {{ method: "POST", url: `/.temps/git/${{relay}}/42/git-receive-pack`, headers: {{ host: proxyAuthority, authorization: "Basic fake" }} }},
    {{ method: "POST", url: `http://example.test/.temps/git/${{relay}}/42/git-upload-pack`, headers: {{ host: "example.test", authorization: "Bearer fake" }} }},
  ];
  for (const request of attempts) {{
    const reply = response();
    await handleRequest(request, reply);
    if (request.url.startsWith("/.temps/git/") && reply.status !== 403) process.exit(1);
  }}
  process.exit(privateForwardCalls === 0 ? 0 : 2);
}})();
"#
    );
    let output = match std::process::Command::new("node")
        .args(["-e", &script])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping executable Git authorization regression: Node.js is unavailable");
            return;
        }
        Err(error) => panic!("failed to execute Node.js Git authorization regression: {error}"),
    };
    assert!(
        output.status.success(),
        "unauthorized Git relay reached private forwarding (status {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sandbox_proxy_exposes_only_capability_scoped_mcp_posts() {
    assert_eq!(
        SANDBOX_MCP_RELAY_BASE_URL,
        "http://temps-sandbox-egress-proxy:3128/.temps/mcp"
    );
    for required_guard in [
        "request.method !== \"POST\"",
        "incoming.host !== proxyAuthority",
        "^\\/\\.temps\\/mcp\\/([a-f0-9]{32})$",
        "/api/ai/sandbox-tools/",
        "if (!target) throw new Error(\"MCP target is not allowed\")",
    ] {
        assert!(
            SANDBOX_EGRESS_PROXY_SCRIPT.contains(required_guard),
            "MCP relay is missing security guard: {required_guard}"
        );
    }
}

#[test]
fn sandbox_mcp_relay_rejects_urls_outside_the_registered_capability_route() {
    let bridge_id = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        sandbox_mcp_relay_url(
            "http://host.docker.internal:8221",
            &format!("http://host.docker.internal:8221/api/ai/sandbox-tools/{bridge_id}/mcp")
        )
        .as_deref(),
        Some("http://temps-sandbox-egress-proxy:3128/.temps/mcp/0123456789abcdef0123456789abcdef")
    );
    for rejected in [
            format!("http://attacker.test/api/ai/sandbox-tools/{bridge_id}/mcp"),
            format!("http://host.docker.internal:8221/api/ai/sandbox-tools/{bridge_id}/other"),
            "http://host.docker.internal:8221/api/ai/sandbox-tools/short/mcp".to_string(),
            "http://host.docker.internal:8221/api/ai/sandbox-tools/0123456789ABCDEF0123456789ABCDEF/mcp".to_string(),
            format!("http://host.docker.internal:8221/api/ai/sandbox-tools/{bridge_id}/mcp?x=1"),
        ] {
            assert_eq!(
                sandbox_mcp_relay_url("http://host.docker.internal:8221", &rejected),
                None,
                "unexpectedly relayed {rejected}"
            );
        }
}

/// The proxy script between two markers, `start` included.
fn proxy_script_between(start: &str, end: &str) -> &'static str {
    let from = SANDBOX_EGRESS_PROXY_SCRIPT
        .find(start)
        .unwrap_or_else(|| panic!("proxy script has no {start:?}"));
    let to = SANDBOX_EGRESS_PROXY_SCRIPT[from..]
        .find(end)
        .map(|offset| from + offset)
        .unwrap_or_else(|| panic!("proxy script has no {end:?} after {start:?}"));
    &SANDBOX_EGRESS_PROXY_SCRIPT[from..to]
}

/// Run `script` with Node.js and return its stdout, or `None` when Node
/// is not installed.
fn run_node(script: &str) -> Option<String> {
    let output = match std::process::Command::new("node")
        .args(["-e", script])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping executable proxy egress test: Node.js is unavailable");
            return None;
        }
        Err(error) => panic!("failed to execute Node.js: {error}"),
    };
    assert!(
        output.status.success(),
        "node failed (status {:?}): {}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parity_policy() -> SandboxEgressPolicy {
    SandboxEgressPolicy::restricted(vec![
        temps_workload::EgressRule::host("API.example.com.", &[443]),
        temps_workload::EgressRule::host("*.pkg.example.org", &[]),
        temps_workload::EgressRule::cidr("1.1.1.7/24", &[443, 8443]),
        temps_workload::EgressRule::cidr("2606:4700::/32", &[443]),
        temps_workload::EgressRule::host("internal.example.com", &[443]),
    ])
    .normalized()
    .expect("valid parity policy")
}

/// (requested host, resolved address, port)
const EGRESS_PARITY_CASES: &[(&str, &str, u16)] = &[
    ("api.example.com", "93.184.216.34", 443),
    ("API.Example.com.", "93.184.216.34", 443),
    ("api.example.com", "93.184.216.34", 80),
    ("other.example.com", "93.184.216.34", 443),
    ("registry.pkg.example.org", "93.184.216.35", 443),
    ("a.b.pkg.example.org", "93.184.216.35", 80),
    ("pkg.example.org", "93.184.216.35", 443),
    ("evilpkg.example.org", "93.184.216.35", 443),
    ("registry.pkg.example.org", "93.184.216.35", 22),
    ("1.1.1.1", "1.1.1.1", 443),
    ("1.1.1.1", "1.1.1.1", 8443),
    ("1.1.1.1", "1.1.1.1", 80),
    ("1.1.2.1", "1.1.2.1", 443),
    ("dns.example.net", "1.1.1.9", 443),
    ("2606:4700:4700::1111", "2606:4700:4700::1111", 443),
    ("[2606:4700:4700::1111]", "2606:4700:4700::1111", 443),
    ("2a00:1450::1", "2a00:1450::1", 443),
    ("93.184.216.34", "93.184.216.34", 443),
    ("internal.example.com", "10.0.0.5", 443),
    ("api.example.com", "169.254.169.254", 443),
    ("api.example.com", "::ffff:127.0.0.1", 443),
    ("api.example.com", "fd00::1", 443),
    ("api.example.com", "100.64.0.1", 443),
    ("api.example.com", "203.0.113.9", 443),
    ("api.example.com", "2001:db8::1", 443),
    ("example.net", "93.184.216.34", 443),
    ("example.net", "93.184.216.34", 22),
    ("example.net", "192.168.1.1", 80),
];

/// The JavaScript matcher the proxy runs and the Rust one the API
/// validates against must agree, for both modes: the expected answers
/// come from `SandboxEgressPolicy::decide`, the actual ones from the
/// proxy's own `egressDecision` and `mayResolve`.
#[test]
fn sandbox_proxy_egress_matcher_agrees_with_the_rust_policy() {
    let block = proxy_script_between(
        "function isPrivateAddress(address) {",
        "// ---- egress policy (end) ----",
    );
    let mut checks = Vec::new();
    for (policy, label) in [
        (parity_policy(), "restricted"),
        (SandboxEgressPolicy::internet(), "internet"),
    ] {
        let policy_json = serde_json::to_string(&policy).expect("policy json");
        let cases: Vec<serde_json::Value> = EGRESS_PARITY_CASES
            .iter()
            .map(|(host, address, port)| {
                let parsed: std::net::IpAddr = address.parse().expect("test address");
                let expected = policy
                    .decide(host, parsed, *port)
                    .err()
                    .map(|denial| denial.as_str());
                serde_json::json!({
                    "host": host,
                    "address": address,
                    "family": if parsed.is_ipv4() { 4 } else { 6 },
                    "port": port,
                    "expected": expected,
                })
            })
            .collect();
        checks.push(serde_json::json!({
            "label": label,
            "policy": policy_json,
            "cases": cases,
        }));
    }
    let checks = serde_json::to_string(&checks).expect("checks json");
    let script = format!(
        r#"
const net = require("net");
{block}
const failures = [];
for (const check of {checks}) {{
  const policy = loadEgressPolicy(check.policy);
  for (const c of check.cases) {{
    const decision = egressDecision(policy, c.host, c.port, [{{ address: c.address, family: c.family }}]);
    const got = decision.denied === undefined ? null : decision.denied;
    if (got !== c.expected) failures.push(`${{check.label}} ${{c.host}} -> ${{c.address}}:${{c.port}}: proxy ${{got}}, rust ${{c.expected}}`);
    if (c.expected === null && decision.address !== c.address) failures.push(`${{check.label}} ${{c.host}}: connects to ${{decision.address}}`);
    // The pre-DNS check may only refuse what the full decision refuses.
    if (c.expected === null && !mayResolve(policy, c.host, c.port)) failures.push(`${{check.label}} ${{c.host}}:${{c.port}} refused before resolution`);
  }}
}}
// Names no rule could allow are refused before any lookup. A CIDR rule on
// the port means any name might resolve into it, so only host rules here.
const hostsOnly = loadEgressPolicy({hosts_only_policy});
for (const [host, port] of [["other.example.com", 443], ["api.example.com", 80], ["pkg.example.org", 443]]) {{
  if (mayResolve(hostsOnly, host, port)) failures.push(`resolves ${{host}}:${{port}}`);
}}
// With a CIDR rule, only ports no rule lists are refused unresolved.
const restricted = loadEgressPolicy({checks_policy});
if (!mayResolve(restricted, "other.example.com", 8443)) failures.push("a name that may resolve into a CIDR rule was refused");
if (mayResolve(restricted, "other.example.com", 22)) failures.push("resolves other.example.com:22");
// Any answer being private refuses the whole destination.
if (!egressDecision(restricted, "api.example.com", 443, [
  {{ address: "93.184.216.34", family: 4 }}, {{ address: "10.0.0.1", family: 4 }},
]).denied) failures.push("mixed public/private answers were admitted");
// A malformed policy stops the proxy instead of widening it.
for (const raw of ["{{", JSON.stringify({{ mode: "open" }}), JSON.stringify({{ mode: "internet", rules: [{{ host: "a.example.com" }}] }})]) {{
  let threw = false;
  try {{ loadEgressPolicy(raw); }} catch (_) {{ threw = true; }}
  if (!threw) failures.push(`accepted policy ${{raw}}`);
}}
if (loadEgressPolicy(undefined).mode !== "internet") failures.push("absent policy is not internet");
process.stdout.write(failures.join("\n"));
"#,
        checks_policy =
            serde_json::to_string(&serde_json::to_string(&parity_policy()).expect("policy json"))
                .expect("quoted policy"),
        hosts_only_policy = serde_json::to_string(
            &serde_json::to_string(
                &SandboxEgressPolicy::restricted(vec![
                    temps_workload::EgressRule::host("api.example.com", &[443]),
                    temps_workload::EgressRule::host("*.pkg.example.org", &[]),
                ])
                .normalized()
                .expect("valid policy")
            )
            .expect("policy json")
        )
        .expect("quoted policy"),
    );
    let Some(stdout) = run_node(&script) else {
        return;
    };
    assert!(stdout.trim().is_empty(), "matcher disagreement:\n{stdout}");
}

/// The refusal a sandbox sees names the sandbox, the destination and
/// the policy, and the proxy logs one JSON line for it; CONNECT and
/// absolute-URI authorities accept any port and normalize hosts.
#[test]
fn sandbox_proxy_refusals_explain_themselves() {
    let policy_block = proxy_script_between(
        "function isPrivateAddress(address) {",
        "// ---- egress policy (end) ----",
    );
    let refusal = proxy_script_between(
        "class Denied extends Error {",
        "\n\nfunction modelRelayTarget(request) {",
    );
    let policy =
        serde_json::to_string(&serde_json::to_string(&parity_policy()).expect("policy json"))
            .expect("quoted policy");
    let script = format!(
        r#"
const net = require("net");
const answers = {{
  "api.example.com": [{{ address: "93.184.216.34", family: 4 }}],
  "internal.example.com": [{{ address: "10.0.0.5", family: 4 }}],
}};
let lookups = 0;
const dns = {{ lookup: async (host) => {{
  lookups += 1;
  if (net.isIP(host)) return [{{ address: host, family: net.isIP(host) }}];
  if (!answers[host]) throw new Error("ENOTFOUND");
  return answers[host];
}} }};
const sandboxName = "temps-sandbox-t1";
{policy_block}
const egressPolicy = loadEgressPolicy({policy});
const logged = [];
process.stdout.write = (line) => {{ logged.push(String(line)); return true; }};
{refusal}
const failures = [];
const expectDenied = async (host, port, reason) => {{
  try {{
    await admit(host, port);
    failures.push(`admitted ${{host}}:${{port}}`);
  }} catch (error) {{
    if (!(error instanceof Denied) || error.reason !== reason) failures.push(`${{host}}:${{port}}: ${{error.reason || error}} instead of ${{reason}}`);
  }}
}};
(async () => {{
  if (await admit("API.example.com.", 443) !== "93.184.216.34") failures.push("allowed host not admitted");
  const before = lookups;
  await expectDenied("blocked.example.com", 22, "policy");
  if (lookups !== before) failures.push("a name no rule allows was resolved");
  await expectDenied("internal.example.com", 443, "private");
  await expectDenied("missing.pkg.example.org", 443, "unresolvable");
  if ((await admit("1.1.1.200", 8443)) !== "1.1.1.200") failures.push("cidr literal not admitted");

  const reply = {{ status: 0, headers: {{}}, body: "", headersSent: false,
    writeHead(code, headers) {{ this.status = code; this.headers = headers; }},
    end(body) {{ this.body = body; }} }};
  let error;
  try {{ await admit("blocked.example.com", 9000); }} catch (caught) {{ error = caught; }}
  refuseRequest({{ method: "GET", url: "http://blocked.example.com:9000/x" }}, reply, error);
  if (reply.status !== 403) failures.push(`status ${{reply.status}}`);
  for (const part of ["sandbox temps-sandbox-t1", "blocked.example.com:9000", "mode restricted"]) {{
    if (!reply.body.includes(part)) failures.push(`403 body lacks ${{part}}: ${{reply.body}}`);
  }}
  if (reply.headers["content-length"] !== Buffer.byteLength(reply.body)) failures.push("content-length");
  const entry = JSON.parse(logged[logged.length - 1]);
  if (entry.event !== "sandbox_egress_denied" || entry.sandbox !== "temps-sandbox-t1" ||
      entry.destination !== "blocked.example.com:9000" || entry.reason !== "policy" ||
      entry.policy_mode !== "restricted") failures.push(`log line ${{logged[logged.length - 1]}}`);

  // Rate limited: a flood logs a bounded number of lines.
  const linesBefore = logged.length;
  for (let i = 0; i < 100; i += 1) denial(new Denied("policy", "flood.example.com:443"), "", "CONNECT");
  if (logged.length - linesBefore > 20) failures.push(`logged ${{logged.length - linesBefore}} lines for a flood`);

  const authority = (value, port) => {{ try {{ return parseAuthority(value, port); }} catch (_) {{ return null; }} }};
  const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
  if (!same(authority("API.Example.com:8443", 443), {{ hostname: "api.example.com", port: 8443 }})) failures.push("authority with port");
  if (!same(authority("api.example.com", 443), {{ hostname: "api.example.com", port: 443 }})) failures.push("authority default port");
  if (!same(authority("api.example.com:80", 443), {{ hostname: "api.example.com", port: 80 }})) failures.push("authority :80");
  if (!same(authority("[2606:4700::1]:443", 443), {{ hostname: "2606:4700::1", port: 443 }})) failures.push("authority ipv6");
  for (const bad of ["a.example.com:0", "a.example.com:65536", "user@a.example.com:443", "a.example.com:443/x", "a b:443", ""]) {{
    if (authority(bad, 443) !== null) failures.push(`accepted authority ${{bad}}`);
  }}
  process.stderr.write(failures.join("\n"));
  process.exit(failures.length === 0 ? 0 : 1);
}})();
"#
    );
    run_node(&script);
}

/// Run the whole proxy script as the sidecar would, on a loopback port,
/// and send it the raw requests in `requests`. Returns each response and
/// everything the proxy logged, or `None` when Node is not installed.
/// Every destination used here is refused before any DNS or network
/// access, so the test runs offline.
fn run_proxy_process(
    policy: &SandboxEgressPolicy,
    requests: &[&str],
) -> Option<(Vec<String>, String)> {
    let listen = "server.listen(3128, \"0.0.0.0\");";
    assert_eq!(SANDBOX_EGRESS_PROXY_SCRIPT.matches(listen).count(), 1);
    let requests_json = serde_json::to_string(requests).expect("requests json");
    let client = format!(
        r#"server.listen(0, "127.0.0.1", async () => {{
  const {{ port }} = server.address();
  for (const raw of {requests_json}) {{
    const answer = await new Promise((resolve, reject) => {{
      const socket = net.connect(port, "127.0.0.1", () => socket.write(raw));
      const chunks = [];
      socket.on("data", (chunk) => chunks.push(chunk));
      socket.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
      socket.on("error", reject);
      socket.setTimeout(5000, () => reject(new Error("proxy did not answer " + raw)));
    }});
    process.stdout.write("RESPONSE " + JSON.stringify(answer) + "\n");
  }}
  process.exit(0);
}});"#
    );
    let script = SANDBOX_EGRESS_PROXY_SCRIPT.replace(listen, &client);
    let output = match std::process::Command::new("node")
        .args(["-e", &script])
        .env("TEMPS_CONTROL_PLANE_URL", "http://127.0.0.1:9")
        .env("TEMPS_SANDBOX_NAME", "sbx-egress-test")
        .env(
            "TEMPS_SANDBOX_EGRESS_POLICY",
            serde_json::to_string(policy).expect("policy json"),
        )
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping proxy process test: Node.js is unavailable");
            return None;
        }
        Err(error) => panic!("failed to execute Node.js: {error}"),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "proxy process failed (status {:?}): {}\n{stdout}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let mut responses = Vec::new();
    let mut log = String::new();
    for line in stdout.lines() {
        match line.strip_prefix("RESPONSE ") {
            Some(json) => responses.push(serde_json::from_str(json).expect("response json")),
            None => {
                log.push_str(line);
                log.push('\n');
            }
        }
    }
    assert_eq!(responses.len(), requests.len(), "{stdout}");
    Some((responses, log))
}

/// The sidecar process itself, not a fragment of it: a restricted
/// policy refuses what it does not list, over CONNECT and plain HTTP,
/// with a 403 a sandbox user can act on and a JSON log line per refusal.
#[test]
fn the_proxy_process_enforces_a_restricted_policy() {
    let policy = SandboxEgressPolicy::restricted(vec![temps_workload::EgressRule::host(
        "api.example.com",
        &[443],
    )])
    .normalized()
    .expect("valid policy");
    let Some((responses, log)) = run_proxy_process(
            &policy,
            &[
                "CONNECT other.example.net:443 HTTP/1.1\r\nHost: other.example.net:443\r\n\r\n",
                "CONNECT api.example.com:22 HTTP/1.1\r\nHost: api.example.com:22\r\n\r\n",
                "CONNECT 1.1.1.1:443 HTTP/1.1\r\nHost: 1.1.1.1:443\r\n\r\n",
                "GET http://other.example.net/ HTTP/1.1\r\nHost: other.example.net\r\nConnection: close\r\n\r\n",
            ],
        ) else {
            return;
        };

    for (response, destination) in responses.iter().zip([
        "other.example.net:443",
        "api.example.com:22",
        "1.1.1.1:443",
        "other.example.net:80",
    ]) {
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(
            response.contains(&format!(
                "sandbox sbx-egress-test: egress to {destination} denied"
            )),
            "{response}"
        );
        assert!(response.contains("mode restricted"), "{response}");
    }
    let refusals: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("each log line is JSON"))
        .collect();
    assert_eq!(refusals.len(), 4, "{log}");
    assert!(refusals
        .iter()
        .all(|line| line["sandbox"] == "sbx-egress-test" && line["reason"] == "policy"));
}

/// A client that resets its tunnel must not take the proxy down. curl
/// resets the socket right after reading a refusal, and the proxy used
/// to exit on the unhandled ECONNRESET -- cutting every connection the
/// sandbox had, allowed ones included. Resets land both before the
/// refusal is written (while the policy is being checked) and after it.
#[test]
fn the_proxy_process_survives_clients_that_reset_their_tunnel() {
    let listen = "server.listen(3128, \"0.0.0.0\");";
    let client = r#"server.listen(0, "127.0.0.1", async () => {
  const { port } = server.address();
  const refused = "CONNECT other.example.net:443 HTTP/1.1\r\nHost: other.example.net:443\r\n\r\n";
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => {
      const socket = net.connect(port, "127.0.0.1", () => {
        socket.write(refused);
        if (i % 2 === 0) socket.resetAndDestroy();
      });
      socket.on("data", () => socket.resetAndDestroy());
      socket.on("close", resolve);
      socket.on("error", () => {});
    });
  }
  await new Promise((resolve) => setTimeout(resolve, 200));
  const answer = await new Promise((resolve, reject) => {
    const socket = net.connect(port, "127.0.0.1", () => socket.write(refused));
    const chunks = [];
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    socket.on("error", reject);
    socket.setTimeout(5000, () => reject(new Error("proxy stopped answering")));
  });
  process.stdout.write("RESPONSE " + JSON.stringify(answer) + "\n");
  process.exit(0);
});"#;
    let script = SANDBOX_EGRESS_PROXY_SCRIPT.replace(listen, client);
    let policy = SandboxEgressPolicy::restricted(vec![temps_workload::EgressRule::host(
        "api.example.com",
        &[443],
    )])
    .normalized()
    .expect("valid policy");
    let output = match std::process::Command::new("node")
        .args(["-e", &script])
        .env("TEMPS_CONTROL_PLANE_URL", "http://127.0.0.1:9")
        .env("TEMPS_SANDBOX_NAME", "sbx-egress-test")
        .env(
            "TEMPS_SANDBOX_EGRESS_POLICY",
            serde_json::to_string(&policy).expect("policy json"),
        )
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping proxy reset test: Node.js is unavailable");
            return;
        }
        Err(error) => panic!("failed to execute Node.js: {error}"),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "proxy exited after a client reset (status {:?}): {}\n{stdout}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let response = stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESPONSE "))
        .map(|json| serde_json::from_str::<String>(json).expect("response json"))
        .expect("the proxy answered after the resets");
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
}

/// The default policy: public destinations on 80 and 443 only, and the
/// private-address refusal holds whatever the port.
#[test]
fn the_proxy_process_enforces_the_internet_policy() {
    let Some((responses, log)) = run_proxy_process(
        &SandboxEgressPolicy::internet(),
        &[
            "CONNECT 10.0.0.1:443 HTTP/1.1\r\nHost: 10.0.0.1:443\r\n\r\n",
            "CONNECT 169.254.169.254:80 HTTP/1.1\r\nHost: 169.254.169.254:80\r\n\r\n",
            "CONNECT 1.1.1.1:22 HTTP/1.1\r\nHost: 1.1.1.1:22\r\n\r\n",
        ],
    ) else {
        return;
    };

    assert!(responses[0].contains("egress to 10.0.0.1:443 denied: private"));
    assert!(responses[1].contains("egress to 169.254.169.254:80 denied: private"));
    assert!(
        responses[2].contains("egress to 1.1.1.1:22 denied")
            && responses[2].contains("mode internet: ports 80 and 443 only"),
        "{}",
        responses[2]
    );
    assert!(responses.iter().all(|r| r.starts_with("HTTP/1.1 403")));
    assert_eq!(log.lines().count(), 3, "{log}");
}

#[test]
fn a_sidecar_carries_its_egress_policy_and_fails_closed_on_garbage() {
    let restricted = parity_policy();
    let egress = SidecarEgressPolicy::new("temps-sandbox-t1", &restricted).expect("valid");
    let internet = SidecarEgressPolicy::new("temps-sandbox-t1", &SandboxEgressPolicy::internet())
        .expect("valid");
    assert_ne!(
        egress.digest, internet.digest,
        "a policy change must be drift"
    );
    assert_eq!(egress.digest.len(), 64);
    // Normalization first: the same policy spelled differently is the
    // same sidecar.
    let respelled = SidecarEgressPolicy::new(
        "temps-sandbox-t1",
        &SandboxEgressPolicy::restricted(vec![
            temps_workload::EgressRule::host("api.example.com", &[443]),
            temps_workload::EgressRule::host("*.PKG.example.org", &[443, 80]),
            temps_workload::EgressRule::cidr("1.1.1.0/24", &[8443, 443]),
            temps_workload::EgressRule::cidr("2606:4700::1/32", &[443]),
            temps_workload::EgressRule::host("internal.example.com.", &[443]),
        ]),
    )
    .expect("valid");
    assert_eq!(respelled, egress);

    let sidecar = |env: Vec<String>, digest: &str| bollard::models::ContainerInspectResponse {
        config: Some(bollard::models::ContainerConfig {
            env: Some(env),
            labels: Some(HashMap::from([(
                SANDBOX_EGRESS_RULES_LABEL.to_string(),
                digest.to_string(),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };
    let current = sidecar(
        vec![
            format!("{SANDBOX_EGRESS_RULES_ENV}={}", egress.json),
            format!("{SANDBOX_NAME_ENV}=temps-sandbox-t1"),
        ],
        &egress.digest,
    );
    assert!(egress.is_carried_by(&current, "temps-sandbox-t1"));
    assert!(!internet.is_carried_by(&current, "temps-sandbox-t1"));
    assert!(!egress.is_carried_by(&current, "temps-sandbox-other"));
    assert_eq!(
        egress_policy_of_sidecar("proxy", &current).expect("readable"),
        restricted
    );

    // A sidecar from before egress policies enforces `internet`.
    let legacy = sidecar(vec!["TEMPS_CONTROL_PLANE_URL=http://cp".into()], "");
    assert_eq!(
        egress_policy_of_sidecar("proxy", &legacy).expect("readable"),
        SandboxEgressPolicy::internet()
    );
    assert!(!internet.is_carried_by(&legacy, "temps-sandbox-t1"));

    // Unreadable is an error, never `internet`.
    for garbage in [
        "{",
        r#"{"mode":"open"}"#,
        r#"{"mode":"restricted","rules":[{"cidr":"10.0.0.0/8"}]}"#,
    ] {
        let broken = sidecar(
            vec![format!("{SANDBOX_EGRESS_RULES_ENV}={garbage}")],
            &egress.digest,
        );
        let error = egress_policy_of_sidecar("temps-sandbox-egress-proxy-v2-x", &broken)
            .expect_err("garbage policy");
        assert!(
            matches!(error, AgentError::SandboxProviderUnavailable { .. }),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("temps-sandbox-egress-proxy-v2-x"),
            "{error}"
        );
    }

    // An invalid policy is refused naming the sandbox.
    let error = SidecarEgressPolicy::new(
        "temps-sandbox-t1",
        &SandboxEgressPolicy::restricted(vec![temps_workload::EgressRule::host("*", &[])]),
    )
    .expect_err("invalid");
    assert!(matches!(error, AgentError::Validation { .. }), "{error:?}");
    assert!(error.to_string().contains("temps-sandbox-t1"), "{error}");
}

// ── Credential-scrubbing tests (ADR-037 §4) ───────────────────────────────
//
// These tests verify the scrubbing helpers in isolation — no Docker daemon
// is required. The security invariant is: after applying the change
// instructions produced by `build_env_scrub_changes`, every sensitive key
// in the committed image has an empty value, and
// `find_surviving_sensitive_keys` returns an empty list.

#[test]
fn is_sensitive_env_key_detects_anthropic_api_key() {
    assert!(is_sensitive_env_key("ANTHROPIC_API_KEY"));
    assert!(is_sensitive_env_key("anthropic_api_key")); // case-insensitive
    assert!(is_sensitive_env_key("MY_ANTHROPIC_API_KEY")); // substring match
}

#[test]
fn is_sensitive_env_key_detects_github_token() {
    assert!(is_sensitive_env_key("GITHUB_TOKEN"));
    assert!(is_sensitive_env_key("GITHUB_TOKEN_READONLY")); // variant
}

#[test]
fn is_sensitive_env_key_detects_temps_prefix() {
    // Every TEMPS_ var is treated as potentially sensitive — the daemon
    // env file, internal tokens, and credential paths all use this namespace.
    assert!(is_sensitive_env_key("TEMPS_DATA_DIR"));
    assert!(is_sensitive_env_key("TEMPS_CREDENTIAL_TOKEN"));
}

#[test]
fn is_sensitive_env_key_passes_safe_keys() {
    assert!(!is_sensitive_env_key("PATH"));
    assert!(!is_sensitive_env_key("HOME"));
    assert!(!is_sensitive_env_key("USER"));
    assert!(!is_sensitive_env_key("LANG"));
    assert!(!is_sensitive_env_key("DEBIAN_FRONTEND"));
    assert!(!is_sensitive_env_key("NODE_VERSION"));
}

#[test]
fn build_env_scrub_changes_returns_change_instructions_for_sensitive_keys() {
    // build_env_scrub_changes must produce `ENV KEY=` Dockerfile-style
    // change instructions for each sensitive key — these are passed to
    // `docker commit --change` which zeroes the values in the committed
    // image. Non-sensitive keys must not appear in the output.
    let env = vec![
        "PATH=/usr/bin:/bin".to_string(),
        "ANTHROPIC_API_KEY=sk-ant-secret".to_string(),
        "HOME=/home/temps".to_string(),
        "GITHUB_TOKEN=ghp_supersecret".to_string(),
        "NODE_VERSION=20".to_string(),
    ];

    let changes = build_env_scrub_changes(&env);

    // Must produce exactly one change instruction per sensitive key.
    assert!(
        changes.contains(&"ENV ANTHROPIC_API_KEY=".to_string()),
        "expected ENV ANTHROPIC_API_KEY= change instruction; got: {:?}",
        changes
    );
    assert!(
        changes.contains(&"ENV GITHUB_TOKEN=".to_string()),
        "expected ENV GITHUB_TOKEN= change instruction; got: {:?}",
        changes
    );

    // Safe keys must NOT appear in the changes list.
    assert!(
        !changes.iter().any(|c| c.contains("PATH")),
        "PATH must not appear in change instructions: {:?}",
        changes
    );
    assert!(
        !changes.iter().any(|c| c.contains("HOME")),
        "HOME must not appear in change instructions: {:?}",
        changes
    );
    assert!(
        !changes.iter().any(|c| c.contains("NODE_VERSION")),
        "NODE_VERSION must not appear in change instructions: {:?}",
        changes
    );

    // Exactly 2 sensitive keys → 2 change instructions.
    assert_eq!(
        changes.len(),
        2,
        "expected 2 change instructions; got {:?}",
        changes
    );
}

#[test]
fn build_env_scrub_changes_empty_env_returns_no_changes() {
    let changes = build_env_scrub_changes(&[]);
    assert!(changes.is_empty());
}

#[test]
fn build_env_scrub_changes_no_sensitive_keys_returns_no_changes() {
    let env = vec!["PATH=/usr/bin".to_string(), "HOME=/home/temps".to_string()];
    let changes = build_env_scrub_changes(&env);
    assert!(
        changes.is_empty(),
        "no sensitive keys → no change instructions; got: {:?}",
        changes
    );
}

#[test]
fn find_surviving_sensitive_keys_returns_empty_for_clean_env() {
    // A committed image with no sensitive keys at all — clean.
    let env = vec![
        "PATH=/usr/bin:/bin".to_string(),
        "HOME=/home/temps".to_string(),
    ];

    let survivors = find_surviving_sensitive_keys(&env);
    assert!(
        survivors.is_empty(),
        "expected no surviving keys, got: {:?}",
        survivors
    );
}

#[test]
fn find_surviving_sensitive_keys_zeroed_key_is_not_a_survivor() {
    // A zeroed entry (KEY=) has been successfully scrubbed by the Docker
    // `--change "ENV KEY="` mechanism. An empty value is the expected
    // post-commit state and must NOT be flagged as a survivor.
    let env = vec![
        "PATH=/usr/bin".to_string(),
        "GITHUB_TOKEN=".to_string(), // zeroed — successfully scrubbed
        "ANTHROPIC_API_KEY=".to_string(), // zeroed — successfully scrubbed
    ];

    let survivors = find_surviving_sensitive_keys(&env);
    assert!(
        survivors.is_empty(),
        "zeroed sensitive keys (KEY=) must not be flagged as survivors; got: {:?}",
        survivors
    );
}

#[test]
fn find_surviving_sensitive_keys_catches_non_empty_secret_value() {
    // A committed image where scrubbing failed — the key has a real value.
    let env = vec![
        "PATH=/usr/bin".to_string(),
        "ANTHROPIC_API_KEY=sk-ant-secret".to_string(), // not zeroed
    ];

    let survivors = find_surviving_sensitive_keys(&env);
    assert_eq!(
        survivors,
        vec!["ANTHROPIC_API_KEY"],
        "a sensitive key with a non-empty value must be flagged as a survivor"
    );
}

#[test]
fn find_surviving_sensitive_keys_mixed_zeroed_and_live() {
    // Mix of zeroed (scrubbed) and still-live (failure case) sensitive keys.
    let env = vec![
        "PATH=/usr/bin".to_string(),
        "GITHUB_TOKEN=".to_string(),                   // zeroed — OK
        "ANTHROPIC_API_KEY=sk-ant-secret".to_string(), // live — failure
    ];

    let survivors = find_surviving_sensitive_keys(&env);
    assert!(
        survivors.contains(&"ANTHROPIC_API_KEY".to_string()),
        "live sensitive key must be flagged; got: {:?}",
        survivors
    );
    assert!(
        !survivors.contains(&"GITHUB_TOKEN".to_string()),
        "zeroed sensitive key must not be flagged; got: {:?}",
        survivors
    );
}

#[test]
fn scrub_then_verify_leaves_no_survivors() {
    // End-to-end property: after applying the change instructions produced
    // by `build_env_scrub_changes`, simulating Docker zeroing the values,
    // `find_surviving_sensitive_keys` must return an empty list.
    let env = vec![
        "PATH=/usr/bin:/bin".to_string(),
        "ANTHROPIC_API_KEY=sk-ant-secret".to_string(),
        "OPENAI_API_KEY=sk-openai-value".to_string(),
        "GITHUB_TOKEN=ghp_supersecret".to_string(),
        "MY_DB_PASSWORD=hunter2".to_string(),
        "HOME=/home/temps".to_string(),
        "TEMPS_CREDENTIAL_TOKEN=tok_internal".to_string(),
    ];

    // Get the change instructions that would be passed to docker commit.
    let changes = build_env_scrub_changes(&env);

    // Simulate what Docker does when applying `ENV KEY=` change instructions:
    // it overwrites the matching env entry's value to an empty string.
    let mut simulated_env = env.clone();
    for change in &changes {
        // Each change is "ENV KEY=" — strip the "ENV " prefix and the
        // trailing "=" to get the key name.
        if let Some(rest) = change.strip_prefix("ENV ") {
            let key = rest.trim_end_matches('=');
            for entry in simulated_env.iter_mut() {
                if entry.starts_with(&format!("{}=", key)) {
                    *entry = format!("{}=", key);
                }
            }
        }
    }

    let survivors = find_surviving_sensitive_keys(&simulated_env);
    assert!(
        survivors.is_empty(),
        "after applying change instructions, find_surviving_sensitive_keys \
             must return empty; found survivors: {:?}",
        survivors
    );
}

// ── C2 regression: CLAUDE_CODE_OAUTH_TOKEN coverage ──────────────────────

/// Regression test for C2: CLAUDE_CODE_OAUTH_TOKEN must be classified
/// sensitive. It is injected by executor.rs (~line 470) and trigger.rs
/// (~line 768) for Claude subscription auth and would have been committed
/// into every subscription user's snapshot without this fix.
#[test]
fn is_sensitive_env_key_detects_claude_code_oauth_token() {
    assert!(
        is_sensitive_env_key("CLAUDE_CODE_OAUTH_TOKEN"),
        "CLAUDE_CODE_OAUTH_TOKEN must be classified sensitive (regression for C2)"
    );
}

#[test]
fn is_sensitive_env_key_detects_oauth_token_variants() {
    // The pattern "OAUTH_TOKEN" catches any OAuth token variant.
    assert!(is_sensitive_env_key("CLAUDE_CODE_OAUTH_TOKEN"));
    assert!(is_sensitive_env_key("MY_OAUTH_TOKEN"));
    assert!(is_sensitive_env_key("oauth_token")); // case-insensitive
}

/// Regression: all env var names that are known to be injected into sandboxes
/// at create time must be classified as sensitive. Keep this list in sync with:
///   - crates/temps-ai-agents/src/services/executor.rs  (CLAUDE_CODE_OAUTH_TOKEN)
///   - crates/temps-ai-agents/src/handlers/trigger.rs   (CLAUDE_CODE_OAUTH_TOKEN)
///
/// If a new credential injection site is added without updating
/// SENSITIVE_ENV_PATTERNS, this test fails and blocks the merge.
#[test]
fn all_known_injected_credential_env_vars_are_classified_sensitive() {
    let injected_credentials = [
        // Injected by executor.rs for Claude subscription auth
        "CLAUDE_CODE_OAUTH_TOKEN",
        // Injected by executor.rs / sandbox_injector for API keys
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "OPENCODE_API_KEY",
        // Injected for git provider tokens
        "GITHUB_TOKEN",
        "GITLAB_TOKEN",
        "BITBUCKET_TOKEN",
        // Injected via TEMPS_ prefix (credential daemon, internal tokens)
        "TEMPS_GIT_CREDENTIAL_TOKEN",
    ];

    for key in &injected_credentials {
        assert!(
            is_sensitive_env_key(key),
            "injected credential env var '{}' is NOT classified as sensitive — \
                 add a matching pattern to SENSITIVE_ENV_PATTERNS",
            key
        );
    }
}

#[test]
fn sensitive_patterns_list_covers_required_keys() {
    // Ensure the pattern list includes every key the ADR explicitly names.
    // This is a static test — adding a required pattern to the ADR but
    // forgetting to add it to the list will fail here.
    let required = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GITHUB_TOKEN",
        "GITLAB_TOKEN",
        "BITBUCKET_TOKEN",
        "API_KEY",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "AWS_SECRET",
        "AWS_ACCESS_KEY",
        "AZURE_CLIENT_SECRET",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "OAUTH_TOKEN", // catches CLAUDE_CODE_OAUTH_TOKEN and any future OAuth tokens
        "TEMPS_",
    ];
    for required_key in required {
        assert!(
            SENSITIVE_ENV_PATTERNS.contains(&required_key),
            "SENSITIVE_ENV_PATTERNS is missing required key '{}'",
            required_key
        );
    }
}

// ── Real-Docker regression: credential-scrub mechanism ────────────────────
//
// This test exercises `take_snapshot` end-to-end against a real running
// Docker daemon. It would have caught the bug that shipped undetected
// through multiple review rounds: the old implementation passed sensitive
// env vars to `docker commit` via the `ContainerConfig` body's `env`
// field, which the Docker Engine silently ignores — the committed image
// retained the original secret values unchanged. The fix switches to
// `CommitContainerOptionsBuilder::changes()` ("ENV KEY=" Dockerfile
// instructions), which is the only API mechanism that actually zeroes
// values in the committed image's `Config.Env`.
//
// The absence of this test (only unit tests on standalone functions, no
// real Docker integration) was the root reason the bug shipped.

/// Regression test: `take_snapshot` must zero all sensitive env-var values
/// in the committed Docker image via `--change 'ENV KEY='`.
///
/// This test creates a real container with credential-shaped env vars
/// (`ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `SAFE_VAR`), calls
/// the real `DockerSandboxProvider::take_snapshot`, then inspects the
/// committed image's `Config.Env` to assert that sensitive keys are
/// present with an **empty** value (`KEY=`) and the safe key survived
/// unchanged.
///
/// The exact assertion (`ANTHROPIC_API_KEY=`) is what the old broken code
/// failed: it produced `ANTHROPIC_API_KEY=sk-ant-...` in the committed
/// image, meaning the secret was baked into every snapshot. A test that
/// only checked "the key isn't present at all" would have missed the bug
/// because Docker's commit never removes keys — it can only overwrite them.
///
/// Skips gracefully (prints a message and returns) when Docker is not
/// available in the test environment, per CLAUDE.md convention.
#[tokio::test]
async fn test_take_snapshot_scrubs_credentials_against_real_docker() {
    // Connect to Docker — skip gracefully if unavailable (CI without Docker,
    // macOS without Docker Desktop running, etc.).
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping test");
        return;
    }

    // alpine:3.20 — small image that is always available. No provisioned
    // `temps` user or AI CLI needed: this test only exercises the env-var
    // scrubbing path, not the full workspace boot sequence.
    let base_image = "alpine:3.20";
    if docker.inspect_image(base_image).await.is_err() {
        let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
            .from_image(base_image)
            .build();
        let mut stream = docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            if let Err(e) = result {
                println!("Cannot pull {}, skipping test: {}", base_image, e);
                return;
            }
        }
    }

    // Use a fixed container name so stale containers from a previous
    // interrupted run are cleaned up automatically.
    let container_name = "temps-snapshot-scrub-regression-test";

    // Best-effort removal of any leftover from a previous test run.
    let _ = docker
        .remove_container(
            container_name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    // Create a container with env vars that mimic real injected credentials.
    // The values are synthetic but structurally real: the assertion below
    // checks that these specific non-empty values are replaced with "".
    let workspace = tempfile::tempdir().expect("create mounted workspace");
    std::fs::write(workspace.path().join("workspace-state.txt"), b"preserved")
        .expect("write workspace marker");
    let container_config = bollard::models::ContainerCreateBody {
        image: Some(base_image.to_string()),
        cmd: Some(vec!["sleep".to_string(), "60".to_string()]),
        env: Some(vec![
            // Sensitive: injected by executor.rs for Anthropic API auth
            "ANTHROPIC_API_KEY=sk-ant-real-secret-must-be-scrubbed".to_string(),
            // Sensitive: injected by executor.rs/trigger.rs for Claude subscription auth
            "CLAUDE_CODE_OAUTH_TOKEN=tok-oauth-real-secret-must-be-scrubbed".to_string(),
            // Non-sensitive: must survive in the snapshot unchanged
            "SAFE_VAR=keep-me".to_string(),
        ]),
        host_config: Some(bollard::models::HostConfig {
            binds: Some(vec![format!(
                "{}:{}",
                workspace.path().display(),
                CONTAINER_WORK_DIR
            )]),
            ..Default::default()
        }),
        ..Default::default()
    };

    let container = docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(container_name)
                    .build(),
            ),
            container_config,
        )
        .await
        .expect("failed to create regression test container");

    let container_id = container.id.clone();

    // Start the container so it has a fully-initialized state that Docker
    // can commit (an un-started container has no `Config.Env` snapshot).
    docker
        .start_container(
            &container_id,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .expect("failed to start regression test container");

    // Stop it — take_snapshot documents that the container must be stopped
    // before commit so the filesystem is in a consistent state.
    docker
        .stop_container(
            &container_id,
            Some(bollard::query_parameters::StopContainerOptions {
                t: Some(5),
                signal: None,
            }),
        )
        .await
        .expect("failed to stop regression test container");

    // Construct a SandboxHandle pointing at the container.
    // The same construction the throwaway harness used (which caught the
    // original bug on first manual run and motivated this permanent test).
    let handle = SandboxHandle {
        node_id: None,
        sandbox_id: container_id.clone(),
        sandbox_name: container_name.to_string(),
        work_dir: "/home/temps/workspace".into(),
        backend: crate::sandbox::SandboxBackend::Docker,
        image: base_image.to_string(),
        conditions: Vec::new(),
    };

    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());

    // Call the real take_snapshot — the path that was broken by the
    // ContainerConfig.env body (silently ignored by the Docker Engine)
    // and fixed by switching to CommitContainerOptionsBuilder::changes().
    let artifact = provider
        .take_snapshot(
            &handle,
            Some("scrub-regression-test".to_string()),
            1024 * 1024 * 1024,
        )
        .await
        .expect("take_snapshot failed");

    // Capture what we need to clean up before asserting, so we know what
    // to remove even if an assertion panics.
    let snapshot_image_ref = artifact.image_ref.clone();
    let snapshot_path = artifact.content_path.clone();
    let workspace_artifact = artifact
        .workspace
        .clone()
        .expect("Docker snapshot must include its bind-mounted workspace");

    let restored_workspace = tempfile::tempdir().expect("create restore destination");
    restore_workspace_archive(&workspace_artifact, restored_workspace.path())
        .expect("restore captured workspace");
    assert_eq!(
        std::fs::read(restored_workspace.path().join("workspace-state.txt"))
            .expect("read restored workspace marker"),
        b"preserved"
    );

    // ── Core assertion: inspect the committed image's Config.Env ─────────
    // This is the exact check that would have caught the bug. The old
    // code using ContainerConfig.env left ANTHROPIC_API_KEY with its
    // original "sk-ant-..." value. The fix produces "ANTHROPIC_API_KEY="
    // (zeroed, empty value) via `ENV KEY=` Dockerfile change instructions.

    let image_ref_str = snapshot_image_ref
        .as_deref()
        .expect("take_snapshot must populate image_ref for Docker backend");

    let inspect = docker
        .inspect_image(image_ref_str)
        .await
        .expect("failed to inspect committed snapshot image");

    let committed_env: Vec<String> = inspect
        .config
        .as_ref()
        .and_then(|c| c.env.as_ref())
        .map(|env| env.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();

    // ANTHROPIC_API_KEY must be present with an EMPTY value — this is
    // the "zeroed" post-commit state that the `--change 'ENV KEY='`
    // mechanism produces. The original value must not appear anywhere.
    assert!(
        committed_env.contains(&"ANTHROPIC_API_KEY=".to_string()),
        "ANTHROPIC_API_KEY must be zeroed ('KEY=') in the committed image — \
             the old ContainerConfig.env path left the original secret intact. \
             committed_env = {:?}",
        committed_env
    );

    // CLAUDE_CODE_OAUTH_TOKEN must be zeroed — this key is what subscription
    // users have injected, making the scrub coverage especially critical.
    assert!(
        committed_env.contains(&"CLAUDE_CODE_OAUTH_TOKEN=".to_string()),
        "CLAUDE_CODE_OAUTH_TOKEN must be zeroed ('KEY=') in the committed image. \
             committed_env = {:?}",
        committed_env
    );

    // SAFE_VAR must survive unchanged — the scrubber must not strip safe keys.
    assert!(
        committed_env.contains(&"SAFE_VAR=keep-me".to_string()),
        "SAFE_VAR=keep-me must survive in the committed image unchanged. \
             committed_env = {:?}",
        committed_env
    );

    // Belt-and-suspenders: the literal original secret values must not
    // appear anywhere in the committed env (as a substring of any entry).
    let leaked: Vec<_> = committed_env
        .iter()
        .filter(|e| {
            e.contains("sk-ant-real-secret-must-be-scrubbed")
                || e.contains("tok-oauth-real-secret-must-be-scrubbed")
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "original secret values must not appear in the committed image; leaked: {:?}",
        leaked
    );

    // ── Cleanup ───────────────────────────────────────────────────────────
    // Best-effort: don't panic on cleanup failure, but do attempt it so
    // repeated test runs don't accumulate test images and containers.
    let _ = docker
        .remove_container(
            &container_id,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    if let Some(ref img) = snapshot_image_ref {
        let _ = docker
            .remove_image(
                img,
                Some(
                    bollard::query_parameters::RemoveImageOptionsBuilder::new()
                        .force(true)
                        .build(),
                ),
                None,
            )
            .await;
    }

    // Remove the snapshot tarball written by take_snapshot to avoid
    // accumulating test artifacts in ~/.temps/snapshots/.
    let _ = tokio::fs::remove_file(&snapshot_path).await;
    let _ = tokio::fs::remove_file(&workspace_artifact.content_path).await;
}

#[test]
fn a_sandbox_network_labelled_for_another_gateway_is_named_in_the_error() {
    let labelled = |gateway: Option<&str>| bollard::models::NetworkInspect {
        labels: Some(
            gateway
                .map(|gateway| {
                    HashMap::from([(
                        SANDBOX_PREVIEW_GATEWAY_LABEL.to_string(),
                        gateway.to_string(),
                    )])
                })
                .unwrap_or_default(),
        ),
        ..Default::default()
    };

    assert_eq!(
        network_labelled_for_another_gateway(
            &labelled(Some("temps-preview-gateway")),
            "temps-preview-gateway-2"
        ),
        Some("temps-preview-gateway")
    );
    assert_eq!(
        network_labelled_for_another_gateway(
            &labelled(Some("temps-preview-gateway-2")),
            "temps-preview-gateway-2"
        ),
        None
    );
    // A network from before the label existed is left to the isolation
    // policy, which adopts it only for the historical default gateway.
    assert_eq!(
        network_labelled_for_another_gateway(&labelled(None), "temps-preview-gateway-2"),
        None
    );
}

#[tokio::test]
async fn sandbox_networks_are_recorded_for_gateway_renames_until_removed() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => Arc::new(docker),
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return;
    }
    let gateway = crate::preview_gateway::GatewayName::new(format!(
        "temps-preview-gateway-record-test-{:016x}",
        rand::random::<u64>()
    ));
    let provider = DockerSandboxProvider::new(
        docker.clone(),
        DockerSandboxConfig {
            preview_gateway_container_name: gateway.clone(),
            ..Default::default()
        },
    );
    let container = format!("network-record-test-{:016x}", rand::random::<u64>());
    let network = sandbox_network_name(&container);
    let gateway_name = gateway.read().await.clone();

    let ensured = provider
        .ensure_isolated_sandbox_bridge(&network, &container, &gateway_name)
        .await;
    let recorded = gateway.networks_set_up().await;
    let cleanup_errors = provider.remove_isolated_sandbox_network(&container).await;

    ensured.expect("set up an isolated sandbox network");
    assert_eq!(recorded, vec![network.clone()]);
    assert!(cleanup_errors.is_empty(), "{cleanup_errors:?}");
    assert!(
        gateway.networks_set_up().await.is_empty(),
        "a removed network still blocks gateway renames"
    );
}

const TEST_GATEWAY: &str = "temps-preview-gateway-test";

fn labelled_network(
    name: &str,
    owner: Option<&str>,
    created_unix: Option<i64>,
) -> bollard::models::Network {
    bollard::models::Network {
        name: Some(name.to_string()),
        created: created_unix.map(|secs| {
            bollard::models::BollardDate::from_unix_timestamp(secs).expect("valid timestamp")
        }),
        labels: owner.map(|owner| {
            HashMap::from([
                (SANDBOX_NETWORK_OWNER_LABEL.to_string(), owner.to_string()),
                (
                    SANDBOX_PREVIEW_GATEWAY_LABEL.to_string(),
                    TEST_GATEWAY.to_string(),
                ),
            ])
        }),
        ..Default::default()
    }
}

#[test]
fn only_old_networks_with_a_matching_owner_are_orphan_candidates() {
    let now = 1_800_000_000_i64;
    let old = Some(now - 2 * 3600);
    let owner = "temps-sandbox-abc123";
    let name = sandbox_network_name(owner);
    let min_age = ORPHANED_NETWORK_MIN_AGE;

    // A leftover network of a long-gone sandbox is reapable.
    assert_eq!(
        orphan_candidate_owner(
            &labelled_network(&name, Some(owner), old),
            TEST_GATEWAY,
            now,
            min_age
        ),
        Some(owner)
    );
    // A legacy network with no gateway label cannot be attributed to
    // this instance, so it is never reaped.
    let mut unlabelled = labelled_network(&name, Some(owner), old);
    if let Some(labels) = unlabelled.labels.as_mut() {
        labels.remove(SANDBOX_PREVIEW_GATEWAY_LABEL);
    }
    assert_eq!(
        orphan_candidate_owner(&unlabelled, TEST_GATEWAY, now, min_age),
        None
    );
    // Labelled for another Temps instance's preview gateway: that
    // instance may be recreating the sandbox, and this one cannot see it.
    assert_eq!(
        orphan_candidate_owner(
            &labelled_network(&name, Some(owner), old),
            "another-instances-gateway",
            now,
            min_age
        ),
        None
    );
    // Younger than the grace period: its container may be mid-create.
    let fresh = Some(now - 30);
    assert_eq!(
        orphan_candidate_owner(
            &labelled_network(&name, Some(owner), fresh),
            TEST_GATEWAY,
            now,
            min_age
        ),
        None
    );
    // No creation time reported: cannot prove it is old, so keep it.
    assert_eq!(
        orphan_candidate_owner(
            &labelled_network(&name, Some(owner), None),
            TEST_GATEWAY,
            now,
            min_age
        ),
        None
    );
    // Not carrying the ownership label: not one of ours.
    assert_eq!(
        orphan_candidate_owner(
            &labelled_network(&name, None, old),
            TEST_GATEWAY,
            now,
            min_age
        ),
        None
    );
    // Name does not match what this provider derives for that owner.
    assert_eq!(
        orphan_candidate_owner(
            &labelled_network("someone-elses-network", Some(owner), old),
            TEST_GATEWAY,
            now,
            min_age
        ),
        None
    );
    assert_eq!(
        orphan_candidate_owner(
            &labelled_network(&name, Some(""), old),
            TEST_GATEWAY,
            now,
            min_age
        ),
        None
    );
}

#[test]
fn networks_with_attached_workloads_are_not_reapable() {
    let owner = "temps-sandbox-abc123";
    let gateway = "temps-preview-gateway";
    let endpoint = |name: &str| bollard::models::EndpointResource {
        name: Some(name.to_string()),
        ..Default::default()
    };
    let network = |members: &[&str]| bollard::models::NetworkInspect {
        containers: Some(
            members
                .iter()
                .enumerate()
                .map(|(index, name)| (format!("id{index}"), endpoint(name)))
                .collect(),
        ),
        ..Default::default()
    };
    let proxy = sandbox_egress_proxy_name(owner);

    // Only its own proxy and the gateway: a leftover.
    assert!(!network_has_foreign_endpoints(
        &network(&[]),
        owner,
        gateway
    ));
    assert!(!network_has_foreign_endpoints(
        &network(&[proxy.as_str(), gateway]),
        owner,
        gateway
    ));
    // The sandbox's own container (a create that finished meanwhile) or
    // anything else attached means the network is in use.
    assert!(network_has_foreign_endpoints(
        &network(&[proxy.as_str(), owner]),
        owner,
        gateway
    ));
    assert!(network_has_foreign_endpoints(
        &network(&["some-other-container"]),
        owner,
        gateway
    ));
}

#[test]
fn a_create_stays_in_flight_until_the_last_guard_for_its_name_drops() {
    let registry = LifecycleRegistry::default();
    let in_flight = |name: &str| lock_lifecycle(&registry).creating.contains_key(name);

    let first = CreateInFlight::try_new(&registry, "temps-sandbox-a").unwrap();
    let second = CreateInFlight::try_new(&registry, "temps-sandbox-a").unwrap();
    let other = CreateInFlight::try_new(&registry, "temps-sandbox-b").unwrap();
    assert!(in_flight("temps-sandbox-a") && in_flight("temps-sandbox-b"));

    // Two overlapping creates of one sandbox: the first to finish must
    // not unprotect the network the second is still about to attach.
    drop(first);
    assert!(in_flight("temps-sandbox-a"));
    drop(second);
    assert!(!in_flight("temps-sandbox-a"));
    assert!(in_flight("temps-sandbox-b"));
    drop(other);
    assert!(lock_lifecycle(&registry).creating.is_empty());
}

#[test]
fn a_reap_and_a_create_of_the_same_sandbox_exclude_each_other() {
    let registry = LifecycleRegistry::default();

    // A running create blocks the reaper from claiming the name.
    let create = CreateInFlight::try_new(&registry, "temps-sandbox-a").unwrap();
    assert!(ReapClaim::try_new(&registry, "temps-sandbox-a").is_none());
    // Other sandboxes are unaffected.
    assert!(ReapClaim::try_new(&registry, "temps-sandbox-b").is_some());
    drop(create);

    // A held reap claim blocks a create (and a second reap) of the name,
    // until it is released.
    let claim = ReapClaim::try_new(&registry, "temps-sandbox-a").unwrap();
    assert!(CreateInFlight::try_new(&registry, "temps-sandbox-a").is_none());
    assert!(ReapClaim::try_new(&registry, "temps-sandbox-a").is_none());
    drop(claim);
    assert!(CreateInFlight::try_new(&registry, "temps-sandbox-a").is_some());
    assert!(lock_lifecycle(&registry).reaping.is_empty());
}

#[tokio::test]
async fn release_orphaned_resources_removes_a_leftover_network_and_is_idempotent() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => Arc::new(docker),
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return;
    }
    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
    // The label a standalone sandbox's container is named after.
    let label = format!("orphan-test-{:016x}", rand::random::<u64>());
    let container = format!("{SANDBOX_NAME_PREFIX}{label}");
    let network = sandbox_network_name(&container);

    // The state a failed create or a manual `docker rm` leaves: the
    // network is there, its container is not. The subnet is pinned so
    // the test does not depend on the daemon having address pools left,
    // which is precisely the condition this cleanup exists to relieve.
    let mut request = isolated_sandbox_network_request(
        &network,
        &container,
        crate::preview_gateway::PREVIEW_GATEWAY_CONTAINER,
    );
    let octet = 16 + rand::random::<u8>() % 200;
    request.ipam = Some(bollard::models::Ipam {
        config: Some(vec![bollard::models::IpamConfig {
            subnet: Some(format!("10.{octet}.{}.0/24", rand::random::<u8>())),
            ..Default::default()
        }]),
        ..Default::default()
    });
    if let Err(error) = create_host_isolated_network(&docker, request).await {
        println!("Could not create a test network ({error}), skipping");
        return;
    }
    assert!(docker
        .inspect_network(
            &network,
            None::<bollard::query_parameters::InspectNetworkOptions>
        )
        .await
        .is_ok());

    // A create of this sandbox is running: it may have reused the
    // network, so the release must refuse and touch nothing.
    let busy = {
        let _create = CreateInFlight::try_new(&provider.creates_in_flight, &container)
            .expect("no reap is running");
        provider.release_orphaned_resources(&label).await
    };
    assert!(
        matches!(busy, Err(AgentError::SandboxConflictOnNode { .. })),
        "release must refuse while a create is in flight: {busy:?}"
    );
    assert!(
        docker
            .inspect_network(
                &network,
                None::<bollard::query_parameters::InspectNetworkOptions>
            )
            .await
            .is_ok(),
        "the network of a sandbox being created must survive a release"
    );

    let released = provider.release_orphaned_resources(&label).await;
    let after = docker
        .inspect_network(
            &network,
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await;
    let again = provider.release_orphaned_resources(&label).await;
    // Never leave the test network behind, whatever the assertions say.
    let _ = provider.remove_isolated_sandbox_network(&container).await;

    released.expect("release the leftover network");
    assert!(
        matches!(&after, Err(error) if docker_error_is_not_found(error)),
        "the leftover network must be gone: {after:?}"
    );
    again.expect("releasing twice is a no-op");
}
/// Starts a throwaway container named `name` from any image the daemon
/// already has, so the test never depends on a pull. `None` when none of
/// them can run `sleep`.
async fn start_throwaway_container(docker: &Docker, name: &str) -> Option<()> {
    start_throwaway_container_on(docker, name, None).await
}

/// [`start_throwaway_container`] joined to `network` instead of the
/// default bridge.
async fn start_throwaway_container_on(
    docker: &Docker,
    name: &str,
    network: Option<&str>,
) -> Option<()> {
    let images = docker
        .list_images(None::<bollard::query_parameters::ListImagesOptions>)
        .await
        .ok()?;
    for image in images
        .iter()
        .flat_map(|image| image.repo_tags.iter())
        .filter(|tag| !tag.contains("<none>"))
        .take(5)
    {
        let created = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(image.clone()),
                    entrypoint: Some(vec!["sleep".to_string()]),
                    cmd: Some(vec!["120".to_string()]),
                    host_config: network.map(|network| bollard::models::HostConfig {
                        network_mode: Some(network.to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await;
        if created.is_err() {
            continue;
        }
        let started = docker
            .start_container(
                name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await;
        if started.is_ok() {
            return Some(());
        }
        let _ = docker
            .remove_container(
                name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }
    None
}

#[test]
fn only_a_stopped_container_counts_as_parked() {
    let with_state = |running, restarting, paused| bollard::models::ContainerInspectResponse {
        state: Some(bollard::models::ContainerState {
            running: Some(running),
            restarting: Some(restarting),
            paused: Some(paused),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(container_is_parked(&with_state(false, false, false)));
    assert!(!container_is_parked(&with_state(true, false, false)));
    assert!(!container_is_parked(&with_state(false, true, false)));
    assert!(!container_is_parked(&with_state(true, false, true)));
    // Without a state Docker has said nothing, so the network is kept.
    assert!(!container_is_parked(
        &bollard::models::ContainerInspectResponse::default()
    ));
}

/// An isolated sandbox network with an explicit /28, so the test runs on a
/// daemon whose default address pools are already exhausted.
fn test_sandbox_network_request(
    network: &str,
    owner: &str,
    gateway: &str,
    subnet: &str,
) -> bollard::models::NetworkCreateRequest {
    let mut request = isolated_sandbox_network_request(network, owner, gateway);
    request.ipam = Some(bollard::models::Ipam {
        config: Some(vec![bollard::models::IpamConfig {
            subnet: Some(subnet.to_string()),
            ..Default::default()
        }]),
        ..Default::default()
    });
    request
}

fn random_test_subnet() -> String {
    format!(
        "10.{}.{}.{}/28",
        16 + rand::random::<u8>() % 200,
        rand::random::<u8>(),
        (rand::random::<u8>() % 16) * 16
    )
}

async fn force_remove_container(docker: &Docker, name: &str) {
    let _ = docker
        .remove_container(
            name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
}

#[tokio::test]
async fn a_stopped_sandbox_gives_back_its_network_and_starts_on_a_new_one() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => Arc::new(docker),
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return;
    }
    let suffix = format!("{:016x}", rand::random::<u64>());
    let gateway = crate::preview_gateway::GatewayName::new(format!("park-test-gateway-{suffix}"));
    let provider = DockerSandboxProvider::new(
        docker.clone(),
        DockerSandboxConfig {
            preview_gateway_container_name: gateway.clone(),
            ..Default::default()
        },
    );
    let gateway_name = gateway.read().await.clone();
    let container = format!("{SANDBOX_NAME_PREFIX}park-test-{suffix}");
    let network = sandbox_network_name(&container);
    let subnet = random_test_subnet();
    if let Err(error) = create_host_isolated_network(
        &docker,
        test_sandbox_network_request(&network, &container, &gateway_name, &subnet),
    )
    .await
    {
        println!("Could not create a test network ({error}), skipping");
        return;
    }
    if start_throwaway_container_on(&docker, &container, Some(&network))
        .await
        .is_none()
    {
        println!("No local image can run sleep, skipping");
        let _ = provider.remove_isolated_sandbox_network(&container).await;
        return;
    }
    let stopped = docker
        .stop_container(
            &container,
            Some(bollard::query_parameters::StopContainerOptions {
                t: Some(0),
                signal: None,
            }),
        )
        .await;

    provider.release_parked_sandbox_network(&container).await;
    let released = docker
        .inspect_network(
            &network,
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await;
    // The parked container still names its network, so recovering it
    // after a server restart does not quarantine it.
    let parked = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await;

    // `start` recreates the network; recreate it the same way here,
    // without the egress proxy image, and reattach.
    let recreated = create_host_isolated_network(
        &docker,
        test_sandbox_network_request(&network, &container, &gateway_name, &subnet),
    )
    .await;
    let handle = SandboxHandle {
        node_id: None,
        sandbox_id: container.clone(),
        sandbox_name: container.clone(),
        work_dir: PathBuf::from(CONTAINER_WORK_DIR),
        backend: super::super::SandboxBackend::Docker,
        image: String::new(),
        conditions: Vec::new(),
    };
    let reattached = match &parked {
        Ok(info) => Some(
            provider
                .reattach_sandbox_network(&handle, info, &network)
                .await,
        ),
        Err(_) => None,
    };
    let started = docker
        .start_container(
            &container,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await;
    let running = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await;
    let again = match &running {
        Ok(info) => Some(
            provider
                .reattach_sandbox_network(&handle, info, &network)
                .await,
        ),
        Err(_) => None,
    };

    force_remove_container(&docker, &container).await;
    let _ = provider.remove_isolated_sandbox_network(&container).await;

    stopped.expect("stop the test sandbox");
    assert!(
        matches!(&released, Err(error) if docker_error_is_not_found(error)),
        "a stopped sandbox must give its network back: {released:?}"
    );
    // The isolation policy check reads the networks a container names;
    // the throwaway container lacks the proxy variables it also checks.
    let networks_of = |info: &bollard::models::ContainerInspectResponse| {
        info.network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .map(|networks| networks.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let parked = parked.expect("inspect the parked sandbox");
    assert_eq!(
        networks_of(&parked),
        vec![network.clone()],
        "a parked sandbox must still name its network, or recovery quarantines it"
    );
    recreated.expect("recreate the sandbox network");
    reattached
        .expect("parked sandbox inspected")
        .expect("reattach the sandbox to its recreated network");
    started.expect("a reattached sandbox starts");
    let running = running.expect("inspect the restarted sandbox");
    assert_eq!(
        running.state.as_ref().and_then(|state| state.running),
        Some(true)
    );
    assert_eq!(
        networks_of(&running),
        vec![network.clone()],
        "the restarted sandbox must be on its isolated network only"
    );
    again
        .expect("restarted sandbox inspected")
        .expect("reattaching a sandbox already on its network is a no-op");
}

#[tokio::test]
async fn the_reaper_frees_the_network_of_a_stopped_sandbox_but_not_a_running_one() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => Arc::new(docker),
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return;
    }
    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
    let suffix = format!("{:016x}", rand::random::<u64>());
    // Unique per run, so the reaper only ever sees this test's networks.
    let gateway = format!("reaper-park-test-gateway-{suffix}");
    let stopped = format!("{SANDBOX_NAME_PREFIX}reaper-stopped-{suffix}");
    let running = format!("{SANDBOX_NAME_PREFIX}reaper-running-{suffix}");

    let mut ready = true;
    for owner in [&stopped, &running] {
        let network = sandbox_network_name(owner);
        if let Err(error) = create_host_isolated_network(
            &docker,
            test_sandbox_network_request(&network, owner, &gateway, &random_test_subnet()),
        )
        .await
        {
            println!("Could not create a test network ({error}), skipping");
            ready = false;
            break;
        }
        if start_throwaway_container_on(&docker, owner, Some(&network))
            .await
            .is_none()
        {
            println!("No local image can run sleep, skipping");
            ready = false;
            break;
        }
    }
    let cleanup = || async {
        for owner in [&stopped, &running] {
            force_remove_container(&docker, owner).await;
            let _ = provider.remove_isolated_sandbox_network(owner).await;
        }
    };
    if !ready {
        cleanup().await;
        return;
    }
    let stop = docker
        .stop_container(
            &stopped,
            Some(bollard::query_parameters::StopContainerOptions {
                t: Some(0),
                signal: None,
            }),
        )
        .await;

    let reaped = provider
        .reap_orphaned_sandbox_networks(&gateway, std::time::Duration::ZERO)
        .await;
    let stopped_network = docker
        .inspect_network(
            &sandbox_network_name(&stopped),
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await;
    let running_network = docker
        .inspect_network(
            &sandbox_network_name(&running),
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await;
    cleanup().await;

    stop.expect("stop the test sandbox");
    assert_eq!(reaped, 1, "only the stopped sandbox's network is reaped");
    assert!(
        matches!(&stopped_network, Err(error) if docker_error_is_not_found(error)),
        "the stopped sandbox's network must be freed: {stopped_network:?}"
    );
    assert!(
        running_network.is_ok(),
        "a running sandbox keeps its network: {running_network:?}"
    );
}

#[tokio::test]
async fn the_reaper_detaches_the_gateway_but_never_a_workload() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => Arc::new(docker),
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return;
    }
    let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
    let suffix = format!("{:016x}", rand::random::<u64>());
    // Unique per run, so the reaper only ever sees this test's networks.
    let gateway = format!("reaper-test-gateway-{suffix}");
    let workload = format!("reaper-test-workload-{suffix}");
    let leftover = format!("{SANDBOX_NAME_PREFIX}reaper-leftover-{suffix}");
    let busy = format!("{SANDBOX_NAME_PREFIX}reaper-busy-{suffix}");

    let mut networks: Vec<(String, String)> = Vec::new();
    for owner in [&leftover, &busy] {
        let network = sandbox_network_name(owner);
        let mut request = isolated_sandbox_network_request(&network, owner, &gateway);
        let octet = 16 + rand::random::<u8>() % 200;
        request.ipam = Some(bollard::models::Ipam {
            config: Some(vec![bollard::models::IpamConfig {
                subnet: Some(format!("10.{octet}.{}.0/24", rand::random::<u8>())),
                ..Default::default()
            }]),
            ..Default::default()
        });
        if let Err(error) = create_host_isolated_network(&docker, request).await {
            println!("Could not create a test network ({error}), skipping");
            for (_, owner) in &networks {
                let _ = provider.remove_isolated_sandbox_network(owner).await;
            }
            return;
        }
        networks.push((network, owner.clone()));
    }

    let cleanup = |docker: Arc<Docker>, names: Vec<String>| async move {
        for name in names {
            let _ = docker
                .remove_container(
                    &name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
    };
    let names = vec![gateway.clone(), workload.clone()];
    if start_throwaway_container(&docker, &gateway).await.is_none()
        || start_throwaway_container(&docker, &workload)
            .await
            .is_none()
    {
        println!("No local image can run a throwaway container, skipping");
        cleanup(docker.clone(), names).await;
        for (_, owner) in &networks {
            let _ = provider.remove_isolated_sandbox_network(owner).await;
        }
        return;
    }
    let attach = |network: String, container: String| {
        let docker = docker.clone();
        async move {
            docker
                .connect_network(
                    &network,
                    bollard::models::NetworkConnectRequest {
                        container,
                        ..Default::default()
                    },
                )
                .await
        }
    };
    // The leftover has only the gateway attached; the busy one also has a
    // workload, which is what a sandbox started elsewhere looks like.
    let attached = [
        attach(sandbox_network_name(&leftover), gateway.clone()).await,
        attach(sandbox_network_name(&busy), gateway.clone()).await,
        attach(sandbox_network_name(&busy), workload.clone()).await,
    ];

    let reaped = if attached.iter().all(Result::is_ok) {
        Some(
            provider
                .reap_orphaned_sandbox_networks(&gateway, std::time::Duration::ZERO)
                .await,
        )
    } else {
        None
    };
    let inspect = |network: String| {
        let docker = docker.clone();
        async move {
            docker
                .inspect_network(
                    &network,
                    None::<bollard::query_parameters::InspectNetworkOptions>,
                )
                .await
        }
    };
    let leftover_after = inspect(sandbox_network_name(&leftover)).await;
    let busy_after = inspect(sandbox_network_name(&busy)).await;

    // Never leave anything behind, whatever the assertions say.
    cleanup(docker.clone(), names).await;
    for (_, owner) in &networks {
        let _ = provider.remove_isolated_sandbox_network(owner).await;
    }

    assert!(
        attached.iter().all(Result::is_ok),
        "attach the test containers: {attached:?}"
    );
    assert_eq!(reaped, Some(1), "only the leftover network is reaped");
    assert!(
        matches!(&leftover_after, Err(error) if docker_error_is_not_found(error)),
        "a network with only the gateway attached must be removed: {leftover_after:?}"
    );
    assert!(
        busy_after.is_ok(),
        "a network with a workload attached must be left alone"
    );
}

// ── Entry points of the split modules that no other test reached ────────────

/// A handle to a container that does not exist, for exercising the error
/// paths of exec and snapshot against a real daemon without the sandbox image.
fn missing_container_handle() -> SandboxHandle {
    SandboxHandle {
        node_id: None,
        sandbox_id: format!("temps-test-missing-{:016x}", rand::random::<u64>()),
        sandbox_name: "temps-sandbox-test-missing".to_string(),
        work_dir: std::path::PathBuf::from(CONTAINER_WORK_DIR),
        backend: crate::sandbox::SandboxBackend::Docker,
        image: String::new(),
        conditions: Vec::new(),
    }
}

async fn reachable_docker() -> Option<Arc<Docker>> {
    let docker = Docker::connect_with_local_defaults().ok()?;
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return None;
    }
    Some(Arc::new(docker))
}

#[tokio::test]
async fn create_refuses_allow_docker_before_touching_docker() {
    // Refused before Docker is called: a client that needs no socket keeps
    // the test running on a host without one.
    let provider = DockerSandboxProvider::new(
        Arc::new(Docker::connect_with_http_defaults().unwrap()),
        DockerSandboxConfig::default(),
    );
    let refused_config = || SandboxCreateConfig {
        allow_docker: true,
        ..create_config_for(1, Some("allow-docker-refused"))
    };

    let created = provider.create(refused_config()).await;
    assert!(
        matches!(&created, Err(AgentError::Validation { message }) if message.contains("allow_docker")),
        "{created:?}"
    );

    let artifact = super::super::SnapshotArtifact {
        content_path: std::path::PathBuf::from("/does/not/exist.tar"),
        content_digest: "0".repeat(64),
        primary_digest: "0".repeat(64),
        size_bytes: 0,
        backend: super::super::SandboxBackend::Docker,
        image_ref: Some("temps-snapshot/missing:latest".to_string()),
        image_id: Some("sha256:missing".to_string()),
        workspace: None,
    };
    let restored = provider
        .create_from_snapshot(&artifact, refused_config())
        .await;
    assert!(
        matches!(&restored, Err(AgentError::Validation { message }) if message.contains("allow_docker")),
        "{restored:?}"
    );
}

#[tokio::test]
async fn every_exec_entry_point_names_a_missing_sandbox() {
    let Some(docker) = reachable_docker().await else {
        return;
    };
    let provider = DockerSandboxProvider::new(docker, DockerSandboxConfig::default());
    let handle = missing_container_handle();
    let cmd = || vec!["true".to_string()];

    let results = [
        (
            "exec",
            provider.exec(&handle, cmd(), HashMap::new(), None).await,
        ),
        (
            "exec_as_root",
            provider
                .exec_as_root(&handle, cmd(), HashMap::new(), None)
                .await,
        ),
        (
            "exec_as_user",
            provider
                .exec_as_user(&handle, SANDBOX_USER, cmd(), HashMap::new(), None)
                .await,
        ),
        (
            "exec_streamed",
            provider
                .exec_streamed(&handle, cmd(), HashMap::new(), None)
                .await,
        ),
    ];
    for (entry_point, result) in results {
        match result {
            Err(AgentError::SandboxExecFailed { sandbox_id, .. }) => {
                assert_eq!(sandbox_id, handle.sandbox_id, "{entry_point}");
            }
            Err(other) => panic!("{entry_point}: expected SandboxExecFailed, got {other:?}"),
            Ok(_) => panic!("{entry_point}: exec in a missing container succeeded"),
        }
    }
}

#[tokio::test]
async fn snapshotting_a_missing_container_names_it() {
    let Some(docker) = reachable_docker().await else {
        return;
    };
    let provider = DockerSandboxProvider::new(docker, DockerSandboxConfig::default());
    let handle = missing_container_handle();

    match provider.take_snapshot(&handle, None, 1024 * 1024).await {
        Err(AgentError::SandboxExecFailed {
            sandbox_id, reason, ..
        }) => {
            assert_eq!(sandbox_id, handle.sandbox_id);
            assert!(reason.contains("snapshot"), "{reason}");
        }
        other => panic!("expected SandboxExecFailed, got {other:?}"),
    }
}
