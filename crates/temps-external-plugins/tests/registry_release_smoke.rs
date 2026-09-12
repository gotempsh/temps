// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;
use temps_external_plugins::catalog::{RegistryClient, RegistryConfig};
use temps_external_plugins::install::{discover_active, PluginInstaller};
use temps_external_plugins::manager::{ExternalPluginConfig, ExternalPluginManager};

#[tokio::test]
async fn published_deployment_pulse_installs_and_serves_overview() {
    if std::env::var("TEMPS_LIVE_REGISTRY_SMOKE").as_deref() != Ok("1") {
        return;
    }
    let config = RegistryConfig::default();
    let registry = RegistryClient::new(config.clone())
        .expect("registry client")
        .fetch()
        .await
        .expect("verify live root quorum and signed catalog");
    let plugin = registry
        .document
        .plugins
        .iter()
        .find(|plugin| plugin.name == "deployment-pulse")
        .expect("Deployment Pulse is published");
    let temp = tempfile::tempdir().expect("isolated installation directory");
    let plugins = temp.path().join("plugins");
    let installer = PluginInstaller::new(config.clone()).expect("installer");
    installer
        .accept_registry_revision(&plugins, &registry)
        .await
        .expect("persist rollback protection");
    let candidate = installer
        .prepare(&plugins, &registry, plugin)
        .await
        .expect("download and hash-verify binary");
    installer
        .activate(&candidate)
        .await
        .expect("activate verified binary");
    let active = discover_active(&plugins, &config).await;
    assert!(matches!(&active[..], [Ok(found)] if found.version == plugin.version));
    let database = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::projects::Model>::new()])
        .into_connection();
    let mut host = ExternalPluginConfig::new(temp.path().to_path_buf(), String::new());
    host.sockets_dir = temp.path().join("sockets");
    let manager = ExternalPluginManager::new(host, Arc::new(database));
    let report = manager.reload_all().await;
    if report.manifests.len() != 1 {
        manager.shutdown_all().await;
        panic!("plugin startup failed: {:?}", report.failures);
    }
    assert_eq!(report.manifests[0].name, "deployment-pulse");
    let proxy = manager
        .proxy_for("deployment-pulse")
        .await
        .expect("running plugin");
    let client = reqwest::Client::builder()
        .unix_socket(proxy.socket_path)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("socket client");
    for role in ["reader", "admin"] {
        let denied = client
            .get("http://localhost/overview")
            .header("x-temps-auth-signature", &proxy.auth_secret)
            .header("x-temps-user-id", "1")
            .header("x-temps-user-email", "smoke@example.test")
            .header("x-temps-user-role", role)
            .header("x-temps-user-permissions", "projects:read")
            .send()
            .await
            .expect("permission check response");
        if denied.status() != reqwest::StatusCode::FORBIDDEN {
            manager.shutdown_all().await;
            panic!("{role} without system:admin received {}", denied.status());
        }
    }
    let response = client
        .get("http://localhost/overview")
        .header("x-temps-auth-signature", &proxy.auth_secret)
        .header("x-temps-user-id", "1")
        .header("x-temps-user-email", "smoke@example.test")
        .header("x-temps-user-role", "admin")
        .header("x-temps-user-permissions", "system:admin")
        .send()
        .await;
    manager.shutdown_all().await;
    let response = response.expect("request plugin overview");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("overview JSON");
    assert!(body["projects"]
        .as_array()
        .expect("project array")
        .is_empty());
    println!("Verified live catalog revision {}, installed {} {}, started protocol-v2 plugin, denied reader/restricted admin (403), and served system admin (200)", registry.document.revision, plugin.name, plugin.version);
}
