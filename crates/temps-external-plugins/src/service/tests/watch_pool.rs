// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::ai_tools::UpdatePluginAiTools;
use crate::install::{LocalFileReceipt, LocalFileSource};
use crate::watches::{WatchChange, WatchMode};
use axum::{routing::get, Extension, Json};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, EntityTrait};
use serde_json::json;
use std::time::Duration;
use temps_auth::{context::AuthContext, permissions::Role, UserService};

#[tokio::test]
async fn create_and_resume_authorize_a_resource_with_one_database_connection() {
    let fixture = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(fixture) => fixture,
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Skipping watch pool test: Docker unavailable");
            return;
        }
        Err(error) => panic!("watch pool database: {error}"),
    };
    let mut options = ConnectOptions::new(fixture.database_url.clone());
    options
        .max_connections(1)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(2));
    let db = Arc::new(Database::connect(options).await.unwrap());
    db.execute_unprepared(
        "
        INSERT INTO users(id,name,email,created_at,updated_at) VALUES(9911,'Owner','owner@example.test',now(),now());
        INSERT INTO sessions(id,user_id,session_token,expires_at,mfa_pending)
          VALUES(9911,9911,'fixture-session',now()+interval '1 day',false);
        INSERT INTO ai_conversations(id,public_id,context_type,context_id,created_by)
          VALUES(9911,'watch-chat','assistant','assistant',9911);
    ",
    )
    .await
    .unwrap();
    let owner = temps_entities::users::Entity::find_by_id(9911)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let auth = AuthContext::new_persisted_session(owner, Role::Admin, 9911);

    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut config =
        ExternalPluginConfig::new(root.path().into(), "postgres://localhost/test".into());
    config.sockets_dir = root.path().join("sockets");
    std::fs::create_dir_all(&config.sockets_dir).unwrap();
    let service = Arc::new(ExternalPluginsService::new_empty(config, None, db.clone()));
    let manifest = PluginManifest::builder("reports", "1.0.0").published_event(
        serde_json::from_value(json!({
            "type":"report.changed","title":"Report changed","filterable":["resource"],
            "attributes_schema":{"type":"object","properties":{"resource":{"type":"integer"}},"required":["resource"]}
        })).unwrap()).build();
    let bytes = protocol_fixture_manifest(manifest);
    let receipt = LocalFileReceipt {
        source: LocalFileSource::File,
        plugin_name: "reports".into(),
        version: "1.0.0".into(),
        platform: platform_target().unwrap(),
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let binary = root.path().join("upload");
    std::fs::write(&binary, bytes).unwrap();
    service.install_file(&receipt, &binary).await.unwrap();
    let settings = service.plugin_ai_tools("reports").await.unwrap();
    service
        .update_plugin_ai_tools(
            "reports",
            UpdatePluginAiTools {
                actor_id: settings.actor_id,
                installation_revision: receipt.sha256.clone(),
                enabled: true,
            },
        )
        .await
        .unwrap();

    let access_db = db.clone();
    let router = Router::new()
        .route(
            "/api/ai/conversations/watch-chat",
            get(|| async { axum::http::StatusCode::OK }),
        )
        .route(
            "/api/x/reports/watch-subjects",
            get(
                move |Extension(caller): Extension<AuthContext>| async move {
                    assert_eq!(caller.user_id(), 9911);
                    // A plugin resource check can itself require the host's only connection.
                    let allowed = temps_entities::users::Entity::find_by_id(caller.user_id())
                        .one(access_db.as_ref())
                        .await
                        .unwrap()
                        .is_some();
                    Json(json!({"allowed":allowed,"label":"Monthly report"}))
                },
            ),
        );
    let key = "7a".repeat(32);
    service.start_watch_delivery(
        router,
        Arc::new(UserService::new(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new(&key).unwrap()),
        )),
        Arc::new(temps_core::CookieCrypto::new(&key).unwrap()),
    );

    service
        .change_watch(
            &auth,
            "watch-chat",
            &WatchChange::Create {
                plugin: "reports".into(),
                source: "report.changed".into(),
                subject: "+4".into(),
                predicate: None,
                mode: WatchMode::Notify,
                digest: receipt.sha256,
            },
        )
        .await
        .unwrap();
    let panel = service.watch_panel(&auth, "watch-chat").await.unwrap();
    assert_eq!(panel.watches.len(), 1);
    let watch = &panel.watches[0];
    assert_eq!(watch.subject, "4");
    assert_eq!(watch.label.as_deref(), Some("Monthly report"));
    let id = uuid::Uuid::parse_str(&watch.id).unwrap();
    service
        .change_watch(&auth, "watch-chat", &WatchChange::Pause { id })
        .await
        .unwrap();
    service
        .change_watch(&auth, "watch-chat", &WatchChange::Resume { id })
        .await
        .unwrap();
    assert!(
        !service
            .watch_panel(&auth, "watch-chat")
            .await
            .unwrap()
            .watches[0]
            .paused
    );
    service.shutdown_all().await;
}
