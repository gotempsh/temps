// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Real-PostgreSQL coverage for durable external-plugin actors and grants.

use std::sync::Arc;

use temps_external_plugins::grants::{GrantError, PluginGrantConfig, PluginGrantService};
use temps_plugin_protocol::channel::PluginHostPermission;
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

async fn boot_database() -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(database) => Some(database),
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Docker unavailable; skipping plugin grants integration test: {error}");
            None
        }
        Err(error) => panic!("failed to prepare plugin grants integration database: {error}"),
    }
}

fn configured_grants() -> PluginGrantConfig {
    PluginGrantConfig {
        permissions: vec![
            PluginHostPermission::AiGenerate,
            PluginHostPermission::ProjectsRead,
        ],
        ai_daily_call_limit: 3,
        ai_max_output_tokens: 2_048,
    }
}

#[tokio::test]
async fn test_ensure_actor_new_plugin_persists_default_deny_grants() {
    // Arrange
    let Some(database) = boot_database().await else {
        return;
    };
    let db = database.connection_arc();
    let service = PluginGrantService::new(db.clone());

    // Act
    let created = service
        .ensure_actor(
            "default-deny-plugin",
            "sha256:first",
            "repository:trusted/default-deny",
        )
        .await
        .expect("create plugin actor");
    let reloaded = PluginGrantService::new(db)
        .get("default-deny-plugin")
        .await
        .expect("reload plugin grants from PostgreSQL");

    // Assert
    assert_eq!(reloaded.actor.id, created.actor.id);
    assert!(reloaded.actor.active);
    assert!(reloaded.config.permissions.is_empty());
    assert_eq!(
        reloaded.config.ai_daily_call_limit,
        PluginGrantConfig::default().ai_daily_call_limit
    );
    assert_eq!(
        reloaded.config.ai_max_output_tokens,
        PluginGrantConfig::default().ai_max_output_tokens
    );
    assert!(
        !service
            .consume_ai_call(&created, 1)
            .await
            .expect("default-deny actor quota check"),
        "an actor without ai_generate must not consume AI quota"
    );
}

#[tokio::test]
async fn test_update_existing_actor_persists_grants_and_revoke_denies_access() {
    // Arrange
    let Some(database) = boot_database().await else {
        return;
    };
    let db = database.connection_arc();
    let service = PluginGrantService::new(db.clone());
    let actor = service
        .ensure_actor(
            "updated-plugin",
            "sha256:first",
            "repository:trusted/updated",
        )
        .await
        .expect("create plugin actor");
    let config = configured_grants();

    // Act
    let updated = service
        .update("updated-plugin", config.clone())
        .await
        .expect("persist plugin grants");
    let reloaded = PluginGrantService::new(db)
        .get("updated-plugin")
        .await
        .expect("reload persisted grants");

    // Assert
    assert_eq!(updated.actor.id, actor.actor.id);
    assert_eq!(reloaded.config.permissions, config.permissions);
    assert_eq!(reloaded.config.ai_daily_call_limit, 3);
    assert_eq!(reloaded.config.ai_max_output_tokens, 2_048);

    service
        .revoke_actor("updated-plugin")
        .await
        .expect("revoke plugin actor");
    assert!(matches!(
        service.get("updated-plugin").await,
        Err(GrantError::NotFound { plugin_name }) if plugin_name == "updated-plugin"
    ));
    assert!(
        !service
            .consume_ai_call(&updated, 1)
            .await
            .expect("revoked actor quota check"),
        "a revoked actor must not consume AI quota"
    );
}

#[tokio::test]
async fn test_ensure_actor_authenticated_same_source_upgrade_preserves_identity_and_grants() {
    // Arrange
    let Some(database) = boot_database().await else {
        return;
    };
    let service = PluginGrantService::new(database.connection_arc());
    let original = service
        .ensure_actor(
            "upgraded-plugin",
            "sha256:old",
            "repository:trusted/upgraded",
        )
        .await
        .expect("create original actor");
    service
        .update("upgraded-plugin", configured_grants())
        .await
        .expect("grant original actor");

    // Act
    let upgraded = service
        .ensure_actor(
            "upgraded-plugin",
            "sha256:new",
            "repository:trusted/upgraded",
        )
        .await
        .expect("replace authenticated plugin source");

    // Assert
    assert_eq!(upgraded.actor.id, original.actor.id);
    assert_eq!(upgraded.config.permissions, configured_grants().permissions);
    assert_eq!(upgraded.config.ai_daily_call_limit, 3);
    assert_eq!(upgraded.config.ai_max_output_tokens, 2_048);
}

#[tokio::test]
async fn test_ensure_actor_different_source_rotates_identity_clears_grants_and_denies_old_actor() {
    // Arrange
    let Some(database) = boot_database().await else {
        return;
    };
    let service = PluginGrantService::new(database.connection_arc());
    let old_actor = service
        .ensure_actor(
            "replaced-plugin",
            "sha256:trusted",
            "repository:trusted/plugin",
        )
        .await
        .expect("create trusted actor");
    let old_grants = service
        .update("replaced-plugin", configured_grants())
        .await
        .expect("grant trusted actor");

    // Act
    let replacement = service
        .ensure_actor(
            "replaced-plugin",
            "sha256:unrelated",
            "repository:unrelated/plugin",
        )
        .await
        .expect("bind unrelated replacement with default-deny authority");

    // Assert
    assert_ne!(replacement.actor.id, old_actor.actor.id);
    assert!(replacement.config.permissions.is_empty());
    assert_eq!(
        replacement.config.ai_daily_call_limit,
        PluginGrantConfig::default().ai_daily_call_limit
    );
    assert_eq!(
        replacement.config.ai_max_output_tokens,
        PluginGrantConfig::default().ai_max_output_tokens
    );
    assert!(
        !service
            .consume_ai_call(&old_grants, 1)
            .await
            .expect("stale actor quota check"),
        "the replaced actor ID must not redeem its stale AI grant snapshot"
    );
}

#[tokio::test]
async fn test_ensure_actor_revoked_reinstall_rotates_identity_and_clears_grants() {
    // Arrange
    let Some(database) = boot_database().await else {
        return;
    };
    let service = PluginGrantService::new(database.connection_arc());
    let original = service
        .ensure_actor(
            "reinstalled-plugin",
            "sha256:first",
            "repository:trusted/reinstalled",
        )
        .await
        .expect("create original actor");
    service
        .update("reinstalled-plugin", configured_grants())
        .await
        .expect("grant original actor");
    service
        .revoke_actor("reinstalled-plugin")
        .await
        .expect("revoke original actor");

    // Act
    let reinstalled = service
        .ensure_actor(
            "reinstalled-plugin",
            "sha256:first",
            "repository:trusted/reinstalled",
        )
        .await
        .expect("reinstall plugin");

    // Assert
    assert_ne!(reinstalled.actor.id, original.actor.id);
    assert!(reinstalled.actor.active);
    assert!(reinstalled.config.permissions.is_empty());
    assert_eq!(
        reinstalled.config.ai_daily_call_limit,
        PluginGrantConfig::default().ai_daily_call_limit
    );
    assert_eq!(
        reinstalled.config.ai_max_output_tokens,
        PluginGrantConfig::default().ai_max_output_tokens
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_consume_ai_call_concurrent_requests_allow_exact_daily_limit() {
    // Arrange
    let Some(database) = boot_database().await else {
        return;
    };
    let service = PluginGrantService::new(database.connection_arc());
    service
        .ensure_actor("quota-plugin", "sha256:quota", "repository:trusted/quota")
        .await
        .expect("create quota actor");
    let grants = Arc::new(
        service
            .update("quota-plugin", configured_grants())
            .await
            .expect("configure quota actor"),
    );

    // Act
    let mut requests = Vec::new();
    for _ in 0..24 {
        let service = service.clone();
        let grants = grants.clone();
        requests.push(tokio::spawn(async move {
            service.consume_ai_call(grants.as_ref(), 1).await
        }));
    }
    let mut allowed = 0;
    for request in requests {
        if request
            .await
            .expect("quota task must not panic")
            .expect("quota database operation")
        {
            allowed += 1;
        }
    }

    // Assert
    assert_eq!(allowed, 3, "concurrent requests must not overspend quota");
    assert!(!service
        .consume_ai_call(grants.as_ref(), 1)
        .await
        .expect("exhausted quota check"));
}

#[tokio::test]
async fn test_actor_migration_rolls_back_populated_tables_and_reapplies() {
    use sea_orm::EntityTrait;
    use temps_entities::external_plugin_actors;
    use temps_migrations::{MigrationTrait, Migrator, MigratorTrait, SchemaManager};

    const CONSENT: &str = "m20261108_000001_external_plugin_ai_tools";

    // Exercise rollback at the schema version under test. Later migrations
    // legitimately refer to the actor tables and must keep their foreign keys.
    struct ActorConsentSchema;
    impl MigratorTrait for ActorConsentSchema {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            let mut history = Migrator::migrations();
            let consent = history
                .iter()
                .position(|migration| migration.name() == CONSENT)
                .expect("AI consent migration must be registered");
            history.truncate(consent + 1);
            history
        }
    }
    let database = match TestDatabase::with_custom_migrations::<ActorConsentSchema>().await {
        Ok(database) => database,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Docker unavailable; skipping actor migration rollback: {error}");
            return;
        }
        Err(error) => panic!("historical actor migration database: {error}"),
    };
    let db = database.connection_arc();
    let service = PluginGrantService::new(db.clone());
    service
        .ensure_actor(
            "rollback-plugin",
            "sha256:rollback",
            "repository:test/rollback",
        )
        .await
        .expect("create actor before rollback");
    let grants = service
        .update("rollback-plugin", configured_grants())
        .await
        .expect("populate grants before rollback");
    assert!(service
        .consume_ai_call(&grants, 1)
        .await
        .expect("populate usage"));
    let migration = Migrator::migrations()
        .into_iter()
        .find(|migration| migration.name() == "m20260916_000001_external_plugin_actors")
        .expect("actor migration must be registered");
    let consent_migration = Migrator::migrations()
        .into_iter()
        .find(|migration| migration.name() == CONSENT)
        .expect("AI consent migration must be registered");
    let manager = SchemaManager::new(db.as_ref());
    // Roll back dependent columns before dropping their owning table.
    consent_migration
        .down(&manager)
        .await
        .expect("rollback AI consent before actor tables");
    migration
        .down(&manager)
        .await
        .expect("rollback populated actor migration");
    for table in [
        "external_plugin_actors",
        "external_plugin_grants",
        "external_plugin_ai_usage",
    ] {
        assert!(
            !manager
                .has_table(table)
                .await
                .expect("inspect rolled-back table"),
            "{table} remains after rollback"
        );
    }
    assert!(manager
        .has_table("users")
        .await
        .expect("inspect unrelated table"));
    migration
        .up(&manager)
        .await
        .expect("reapply actor migration");
    consent_migration
        .up(&manager)
        .await
        .expect("reapply AI consent before using the current actor service");
    let recreated = service
        .ensure_actor(
            "rollback-plugin",
            "sha256:rollback",
            "repository:test/rollback",
        )
        .await
        .expect("create actor after migration reapplication");
    assert_ne!(recreated.actor.id, grants.actor.id);
    let recreated_id =
        uuid::Uuid::parse_str(&recreated.actor.id).expect("recreated actor has a valid UUID");
    // The entity reads every current column, so bring the table the rest of
    // the way to the current schema before reading through it.
    for later in [
        "m20261127_000002_external_plugin_isolation",
        "m20261127_000003_external_plugin_services",
    ] {
        Migrator::migrations()
            .into_iter()
            .find(|migration| migration.name() == later)
            .unwrap_or_else(|| panic!("{later} must be registered"))
            .up(&manager)
            .await
            .unwrap_or_else(|error| panic!("apply {later} after the consent schema: {error}"));
    }
    let persisted = external_plugin_actors::Entity::find_by_id(recreated_id)
        .one(db.as_ref())
        .await
        .expect("read recreated actor consent")
        .expect("recreated actor is persisted");
    assert!(!persisted.ai_tools_enabled);
    assert!(recreated.config.permissions.is_empty());
    eprintln!("Actor migration: populated up -> down (all three tables absent, users preserved) -> up passed");
}
