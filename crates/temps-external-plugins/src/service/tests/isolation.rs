// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How an installation is isolated: what a new one gets, what an existing
//! one keeps across an upgrade of Temps, and what an administrator can change.
//!
//! These servers have no container sandbox (the default configuration is
//! given no Docker client), so "container" must be refused with a reason and
//! "process" must keep working.

use super::*;
use crate::install::{LocalFileReceipt, LocalFileSource};
use crate::sandbox::{IsolationMode, PluginIsolation, SandboxError};
use sea_orm::ConnectionTrait;

struct Fixture {
    database: temps_test_utils::TestDatabase,
    root: tempfile::TempDir,
    config: ExternalPluginConfig,
    service: ExternalPluginsService,
}

async fn fixture(reason: &str) -> Option<Fixture> {
    if !std::path::Path::new("/usr/bin/python3").exists() {
        eprintln!("Skipping {reason}: the plugin fixture needs /usr/bin/python3");
        return None;
    }
    let database = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(database) => database,
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Skipping {reason}: Docker unavailable");
            return None;
        }
        Err(error) => panic!("fixture database: {error}"),
    };
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut config =
        ExternalPluginConfig::new(root.path().into(), "postgres://localhost/test".into());
    config.sockets_dir = root.path().join("sockets");
    std::fs::create_dir_all(&config.sockets_dir).unwrap();
    let service =
        ExternalPluginsService::new_empty(config.clone(), None, database.connection_arc());
    Some(Fixture {
        database,
        root,
        config,
        service,
    })
}

impl Fixture {
    async fn install(
        &self,
        name: &str,
        isolation: Option<PluginIsolation>,
    ) -> Result<(), ExternalPluginsError> {
        let bytes = protocol_v2_fixture(name, "1.0.0");
        let binary = self.root.path().join(format!("{name}-upload"));
        std::fs::write(&binary, &bytes).unwrap();
        let receipt = LocalFileReceipt {
            source: LocalFileSource::File,
            plugin_name: name.into(),
            version: "1.0.0".into(),
            platform: platform_target().unwrap(),
            sha256: hex::encode(Sha256::digest(&bytes)),
        };
        self.service
            .install_file(&receipt, &binary, isolation)
            .await
    }

    fn active_record(&self, name: &str) -> std::path::PathBuf {
        self.config.plugins_dir.join(name).join("active.json")
    }

    async fn stored(&self, name: &str) -> Option<PluginIsolation> {
        crate::grants::PluginGrantService::new(self.database.connection_arc())
            .isolation(name)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_new_installation_is_refused_before_it_runs_when_this_server_has_no_sandbox() {
    let Some(fixture) = fixture("default isolation test").await else {
        return;
    };

    // No isolation named: a new installation is a container.
    let error = fixture.install("contained", None).await.unwrap_err();

    let ExternalPluginsError::Sandbox(SandboxError::Unavailable { plugin, reason }) = &error else {
        panic!("expected the sandbox to be unavailable, got {error:?}");
    };
    assert_eq!(plugin, "contained");
    assert!(!reason.is_empty(), "the refusal must say why");
    assert!(!fixture.service.manager.is_running("contained").await);
    assert!(
        !fixture.config.plugins_dir.join("contained").exists(),
        "a refused installation must leave nothing in the plugins directory"
    );
    assert_eq!(fixture.stored("contained").await, None);
}

#[tokio::test]
async fn an_administrator_can_install_a_plugin_without_a_container_and_the_choice_is_recorded() {
    let Some(fixture) = fixture("process isolation test").await else {
        return;
    };

    fixture
        .install("legacy", Some(PluginIsolation::Process))
        .await
        .unwrap();

    let status = fixture.service.plugin_isolation("legacy").await.unwrap();
    assert_eq!(status.requested, Some(PluginIsolation::Process));
    assert_eq!(status.effective, Some(IsolationMode::Process));
    assert!(status.container_unavailable.is_some());
    assert!(!status.launcher);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.active_record("legacy")).unwrap()).unwrap();
    assert_eq!(record["isolation"], "process");
    let overview = fixture.service.isolation_overview().await.unwrap();
    assert_eq!(overview.len(), 1);
    assert_eq!(overview[0].0, "legacy");
    assert_eq!(overview[0].1.effective, Some(IsolationMode::Process));
    assert_eq!(overview[0].2, None);
    fixture.service.manager.shutdown_plugin("legacy").await;
}

#[tokio::test]
async fn moving_a_plugin_into_a_container_is_refused_here_and_leaves_it_running() {
    let Some(fixture) = fixture("isolation change test").await else {
        return;
    };
    fixture
        .install("reports", Some(PluginIsolation::Process))
        .await
        .unwrap();
    let socket = fixture.service.manager.socket_path_for("reports").await;

    let error = fixture
        .service
        .set_plugin_isolation("reports", PluginIsolation::Container)
        .await
        .unwrap_err();

    assert!(
        matches!(
            &error,
            ExternalPluginsError::Sandbox(SandboxError::Unavailable { plugin, .. })
                if plugin == "reports"
        ),
        "{error:?}"
    );
    assert_eq!(
        fixture.service.manager.socket_path_for("reports").await,
        socket,
        "a refused move must not restart the plugin"
    );
    assert_eq!(
        fixture.stored("reports").await,
        Some(PluginIsolation::Process)
    );

    // Asking for what it already has is not a restart either.
    let status = fixture
        .service
        .set_plugin_isolation("reports", PluginIsolation::Process)
        .await
        .unwrap();
    assert_eq!(status.effective, Some(IsolationMode::Process));
    assert_eq!(
        fixture.service.manager.socket_path_for("reports").await,
        socket
    );
    fixture.service.manager.shutdown_plugin("reports").await;
}

#[tokio::test]
async fn an_installation_from_before_isolation_existed_keeps_running_as_a_process() {
    let Some(fixture) = fixture("upgrade isolation test").await else {
        return;
    };
    fixture
        .install("installed-earlier", Some(PluginIsolation::Process))
        .await
        .unwrap();
    fixture
        .service
        .manager
        .shutdown_plugin("installed-earlier")
        .await;
    // What an upgrade finds for a plugin installed by an older Temps: an
    // activation record that names no isolation, and no actor row.
    let path = fixture.active_record("installed-earlier");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("isolation");
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    fixture
        .database
        .connection_arc()
        .execute_unprepared("DELETE FROM external_plugin_actors")
        .await
        .unwrap();

    fixture
        .service
        .manager
        .restart_from_active("installed-earlier")
        .await
        .expect("an existing installation must keep starting");

    assert_eq!(
        fixture
            .service
            .manager
            .isolation_of("installed-earlier")
            .await,
        Some(IsolationMode::Process)
    );
    assert_eq!(
        fixture.stored("installed-earlier").await,
        Some(PluginIsolation::Process),
        "the first start writes down what the plugin runs with"
    );
    fixture
        .service
        .manager
        .shutdown_plugin("installed-earlier")
        .await;
}

#[tokio::test]
async fn a_recorded_container_choice_is_not_downgraded_when_its_database_row_is_missing() {
    let Some(fixture) = fixture("lost commit isolation test").await else {
        return;
    };
    fixture
        .install("chosen", Some(PluginIsolation::Process))
        .await
        .unwrap();
    fixture.service.manager.shutdown_plugin("chosen").await;
    // An installation activated as a container whose database commit was lost.
    let path = fixture.active_record("chosen");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["isolation"] = "container".into();
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    fixture
        .database
        .connection_arc()
        .execute_unprepared("DELETE FROM external_plugin_actors")
        .await
        .unwrap();

    let failure = fixture
        .service
        .manager
        .restart_from_active("chosen")
        .await
        .expect_err("it must not start without its container");

    assert!(
        matches!(
            failure,
            crate::manager::StartFailure::Sandbox(SandboxError::Unavailable { .. })
        ),
        "{failure:?}"
    );
    assert!(!fixture.service.manager.is_running("chosen").await);
}

#[tokio::test]
async fn changing_isolation_reports_a_plugin_that_is_not_installed_and_a_launcher_host() {
    let Some(fixture) = fixture("isolation change refusal test").await else {
        return;
    };

    let missing = fixture
        .service
        .set_plugin_isolation("absent", PluginIsolation::Process)
        .await
        .unwrap_err();
    assert!(
        matches!(&missing, ExternalPluginsError::NotInstalled { name } if name == "absent"),
        "{missing:?}"
    );
    let unsafe_name = fixture
        .service
        .set_plugin_isolation("../absent", PluginIsolation::Process)
        .await
        .unwrap_err();
    assert!(
        matches!(unsafe_name, ExternalPluginsError::Install(_)),
        "{unsafe_name:?}"
    );

    let launcher_config =
        fixture
            .config
            .clone()
            .with_launcher(crate::launcher::PluginLauncher::new(
                fixture.root.path().join("launcher"),
            ));
    let with_launcher =
        ExternalPluginsService::new_empty(launcher_config, None, fixture.database.connection_arc());
    let managed = with_launcher
        .set_plugin_isolation("reports", PluginIsolation::Process)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &managed,
            ExternalPluginsError::IsolationManagedByLauncher { name } if name == "reports"
        ),
        "{managed:?}"
    );
}
