// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fixtures shared by the external-service manager tests.

use super::*;
use bollard::Docker;
use chrono::Utc;
use sea_orm::DatabaseConnection;
#[cfg(feature = "docker-tests")]
use sea_orm::{ActiveModelTrait, Set};
#[cfg(feature = "docker-tests")]
use std::collections::HashMap;
#[cfg(feature = "docker-tests")]
use std::net::TcpListener;
use std::sync::Arc;
use temps_core::DockerHandle;
use temps_core::EncryptionService;
#[cfg(feature = "docker-tests")]
use temps_database::test_utils::TestDatabase;
use temps_entities::{external_services, nodes, projects, service_members};

/// Healthy-by-default node states (health = 1, i.e. responding).
pub(super) fn st(pairs: &[(&str, &str)]) -> Vec<ClusterNodeState> {
    pairs
        .iter()
        .map(|(n, s)| ClusterNodeState {
            name: n.to_string(),
            state: s.to_string(),
            health: 1,
        })
        .collect()
}

pub(super) fn service_member_info(
    id: i32,
    name: &str,
    role: &str,
    status: &str,
) -> ServiceMemberInfo {
    ServiceMemberInfo {
        id,
        role: role.to_string(),
        node_id: None,
        container_name: name.to_string(),
        hostname: Some(format!("{name}.cluster.temps.local")),
        port: Some(5432),
        status: status.to_string(),
        ordinal: id,
        compute_ip: Some(format!("172.20.0.{id}")),
        provisioning_step: None,
        provisioning_error: None,
        live_state: None,
    }
}

pub(super) fn cluster_member_health(nodename: &str, reported_state: &str) -> ClusterMemberHealth {
    ClusterMemberHealth {
        nodename: nodename.to_string(),
        nodehost: "10.0.0.2".to_string(),
        nodeport: 5432,
        reported_state: reported_state.to_string(),
        goal_state: reported_state.to_string(),
        health: 1,
        seconds_since_report: 1,
        candidate_priority: 100,
        replication_quorum: true,
        sync_state: None,
        replay_lag_ms: None,
    }
}

pub(super) fn health_report(members: Vec<ClusterMemberHealth>) -> ClusterHealthReport {
    ClusterHealthReport {
        configured_standbys: 0,
        checked_at: chrono::Utc::now(),
        monitor_response_ms: 5,
        monitor_error: None,
        members,
    }
}

pub(super) fn mock_manager(db: Arc<DatabaseConnection>) -> Option<Arc<ExternalServiceManager>> {
    let docker = Docker::connect_with_local_defaults().ok()?;
    Some(Arc::new(ExternalServiceManager::new(
        db.clone(),
        Arc::new(EncryptionService::new_from_password(
            "service-parameter-reveal-test",
        )),
        Arc::new(docker),
        Arc::new(temps_dns::DnsRegistry::new(db)),
    )))
}

pub(super) fn member(role: &str, node_id: Option<i32>) -> ClusterMemberRequest {
    ClusterMemberRequest {
        role: role.to_string(),
        node_id,
    }
}

pub(super) fn nodes_test_model(id: i32) -> nodes::Model {
    nodes::Model {
        id,
        name: format!("worker-{id}"),
        token_hash: "hash".to_string(),
        token_encrypted: None,
        address: "http://10.0.0.2:3100".to_string(),
        private_address: "10.0.0.2".to_string(),
        public_endpoint: None,
        wg_public_key: None,
        role: "worker".to_string(),
        status: "active".to_string(),
        labels: serde_json::json!({}),
        capacity: serde_json::json!({}),
        last_heartbeat: None,
        edge_public_key: None,
        compute_cidr: None,
        architecture: None,
        underlay_address: None,
        mesh_wg_public_key: None,
        mesh_wg_endpoint: None,
        mesh_wg_address: None,
        failover_at: None,
        dns_resolver_running: None,
        dns_resolver_tasks_alive: None,
        dns_resolver_last_sync_at: None,
        dns_resolver_consecutive_failures: 0,
        dns_resolver_last_error: None,
        dns_resolver_record_count: None,
        deployments_without_resolver: None,
        build_limits: None,
        public_ingress_enabled: false,
        public_ingress_running: None,
        public_ingress_last_error: None,
        public_ingress_certificate_count: None,
        public_ingress_route_count: None,
        public_ingress_unsupported_route_count: None,
        public_ingress_unsupported_reasons: serde_json::json!([]),
        occupied_ranges: None,
        overlay_status: None,
        overlay_last_error: None,
        overlay_ready_cidr: None,
        overlay_remediation: None,
        overlay_network_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

pub(super) fn service_member_model(id: i32, ordinal: i32, role: &str) -> service_members::Model {
    service_members::Model {
        id,
        service_id: 7,
        node_id: None,
        role: role.to_string(),
        container_id: None,
        container_name: format!("cluster-member-{ordinal}"),
        hostname: None,
        port: None,
        compute_ip: None,
        status: "pending".to_string(),
        ordinal,
        config: None,
        provisioning_step: None,
        provisioning_error: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[cfg(feature = "docker-tests")]
pub(super) fn get_unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("Failed to bind to address")
        .local_addr()
        .unwrap()
        .port()
}

#[cfg(feature = "docker-tests")]
pub(super) async fn setup_test_manager(
) -> Result<(Arc<ExternalServiceManager>, TestDatabase), String> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| format!("Docker client is unavailable: {error}"))?;
    docker
        .ping()
        .await
        .map_err(|error| format!("Docker daemon is unavailable: {error}"))?;

    let test_db = TestDatabase::with_migrations()
        .await
        .map_err(|error| format!("test database is unavailable: {error}"))?;
    let db = test_db.db.clone();

    let encryption_key = "test_encryption_key_1234567890ab";
    let encryption_service = Arc::new(
        EncryptionService::new(encryption_key)
            .map_err(|error| format!("test encryption setup failed: {error}"))?,
    );
    let docker = Arc::new(docker);

    let dns_registry = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
    let manager = Arc::new(ExternalServiceManager::new(
        db,
        encryption_service,
        docker.clone(),
        dns_registry,
    ));
    Ok((manager, test_db))
}

#[cfg(feature = "docker-tests")]
macro_rules! setup_test_manager_or_skip {
    () => {
        match setup_test_manager().await {
            Ok(setup) => setup,
            Err(error) => {
                if temps_database::test_utils::is_container_runtime_unavailable(&error) {
                    eprintln!("Skipping Docker-dependent test: {error}");
                    return;
                }
                panic!("Failed to set up provider Docker test: {error}");
            }
        }
    };
}

pub(super) fn mock_service_manager(
    query_results: Vec<Vec<external_services::Model>>,
) -> ExternalServiceManager {
    mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results(query_results)
            .into_connection(),
    ))
}

pub(super) fn mock_service_manager_with_db(db: Arc<DatabaseConnection>) -> ExternalServiceManager {
    ExternalServiceManager::new(
        db.clone(),
        Arc::new(EncryptionService::new_from_password(
            "service-parameter-reveal-test",
        )),
        Arc::new(
            Docker::connect_with_local_defaults()
                .expect("Docker client configuration should be available"),
        ),
        Arc::new(temps_dns::DnsRegistry::new(db)),
    )
}

pub(super) fn cluster_member(ordinal: i32, role: &str, status: &str) -> service_members::Model {
    let container_name = if role == "monitor" {
        "postgres-ha-monitor".to_string()
    } else {
        format!("postgres-ha-{ordinal}")
    };
    service_members::Model {
        id: 100 + ordinal,
        service_id: 7,
        node_id: None,
        role: role.to_string(),
        container_id: None,
        container_name,
        hostname: None,
        port: None,
        compute_ip: None,
        status: status.to_string(),
        ordinal,
        config: None,
        provisioning_step: None,
        provisioning_error: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

pub(super) fn encrypted_service_model(
    id: i32,
    parameters: serde_json::Value,
) -> external_services::Model {
    let encryption_service = EncryptionService::new_from_password("service-parameter-reveal-test");
    external_services::Model {
        id,
        name: "postgres-test".to_string(),
        service_type: "postgres".to_string(),
        version: Some("18".to_string()),
        status: "running".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        slug: Some("postgres-test".to_string()),
        config: Some(
            encryption_service
                .encrypt_string(&parameters.to_string())
                .unwrap(),
        ),
        node_id: None,
        topology: "standalone".to_string(),
        error_message: None,
        health_status: None,
        last_health_check_at: None,
        last_health_error: None,
        consecutive_health_failures: 0,
        health_metadata: None,
        metrics_enabled: false,
        default_backup_provisioned: false,
        container_name: None,
        created_by_user_id: None,
        continuous_archive_s3_source_id: None,
        continuous_archive_pinned_at: None,
        workload_mode: Default::default(),
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
    }
}

pub(super) fn environment_preview_project(id: i32) -> projects::Model {
    let now = Utc::now();
    projects::Model {
        id,
        name: "preview-project".to_string(),
        repo_name: "preview-project".to_string(),
        repo_owner: "test".to_string(),
        directory: String::new(),
        pull_only_root_directory: false,
        main_branch: "main".to_string(),
        preset: temps_entities::preset::Preset::NextJs,
        preset_config: None,
        deployment_config: None,
        created_at: now,
        updated_at: now,
        slug: "preview-project".to_string(),
        is_deleted: false,
        deleted_at: None,
        last_deployment: None,
        is_public_repo: false,
        git_url: None,
        git_provider_connection_id: None,
        attack_mode: false,
        shield_mode: "log".to_string(),
        ai_alert_summaries_enabled: None,
        ai_debug_chat_enabled: None,
        ai_write_actions_enabled: false,
        error_source_context_enabled: false,
        vulnerability_scanning_enabled: false,
        error_source_root: None,
        enable_preview_environments: false,
        preview_envs_on_demand: false,
        preview_envs_idle_timeout_seconds: 300,
        preview_envs_wake_timeout_seconds: 30,
        source_type: Default::default(),
        project_type: temps_entities::types::ProjectType::Server,
        allow_alternate_sources: None,
        hosting: Default::default(),
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
        ai_api_traffic_summary_enabled: None,
        image_retention_hours: None,
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
        cloud_telemetry_fidelity: Default::default(),
        cloud_telemetry_attribute_allowlist: Vec::new(),
        cloud_telemetry_write_mode: Default::default(),
        cloud_analytics_write_mode: Default::default(),
        instantiation_token: None,
    }
}

#[cfg(feature = "docker-tests")]
pub(super) async fn insert_test_service(
    db: &DatabaseConnection,
    name: &str,
    service_type: &str,
    topology: &str,
    status: &str,
) -> i32 {
    use sea_orm::ActiveValue::Set;

    let model = external_services::ActiveModel {
        name: Set(name.to_string()),
        service_type: Set(service_type.to_string()),
        version: Set(None),
        status: Set(status.to_string()),
        config: Set(None),
        node_id: Set(None),
        topology: Set(topology.to_string()),
        error_message: Set(None),
        ..Default::default()
    };
    let result = model.insert(db).await.unwrap();
    result.id
}

/// Insert a cluster whose credentials exist, at the given authentication
/// version, with members given as `(role, status)`.
#[cfg(feature = "docker-tests")]
pub(super) async fn insert_cluster_at_auth_version(
    manager: &ExternalServiceManager,
    name: &str,
    version: Option<u64>,
    members: &[(&str, &str)],
) -> i32 {
    let mut parameters = HashMap::new();
    crate::externalsvc::postgres_cluster::ClusterAuthSecrets::generate()
        .insert_into(&mut parameters);
    if let Some(version) = version {
        parameters.insert(
            CLUSTER_AUTH_VERSION_KEY.to_string(),
            serde_json::json!(version),
        );
    }
    let config = manager
        .encryption_service
        .encrypt_string(&serde_json::to_string(&parameters).unwrap())
        .unwrap();
    let service = external_services::ActiveModel {
        name: Set(name.to_string()),
        service_type: Set("postgres".to_string()),
        status: Set("running".to_string()),
        topology: Set("cluster".to_string()),
        config: Set(Some(config)),
        ..Default::default()
    }
    .insert(manager.db.as_ref())
    .await
    .unwrap();
    for (ordinal, (role, status)) in members.iter().enumerate() {
        service_members::ActiveModel {
            service_id: Set(service.id),
            role: Set(role.to_string()),
            container_name: Set(format!("postgres-{name}-{ordinal}")),
            status: Set(status.to_string()),
            ordinal: Set(ordinal as i32),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(manager.db.as_ref())
        .await
        .unwrap();
    }
    service.id
}

/// Build a manager whose `DockerHandle` is disabled, i.e. exactly the
/// state of a `temps serve --profile control-plane` process.
pub(super) fn control_plane_manager() -> ExternalServiceManager {
    use sea_orm::DatabaseBackend;
    use sea_orm::MockDatabase;

    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let enc = Arc::new(EncryptionService::new(&"0".repeat(64)).unwrap());
    let dns = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
    let handle = Arc::new(DockerHandle::disabled(
        temps_core::PROFILE_CONTROL_PLANE,
        temps_core::CONTROL_PLANE_DOCKER_REASON,
    ));

    ExternalServiceManager::new_with_handle(db, enc, handle, false, dns)
}

#[cfg(feature = "docker-tests")]
pub(super) use setup_test_manager_or_skip;
