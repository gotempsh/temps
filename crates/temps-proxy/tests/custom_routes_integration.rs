use chrono::Utc;
use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
use std::sync::Arc;
use std::time::Duration;
use temps_database::test_utils::TestDatabase;
use temps_entities::custom_routes::{self, RouteType};
use temps_entities::deployments::DeploymentMetadata;
use temps_entities::preset::{ComposePublicPort, DockerComposeConfig, Preset, PresetConfig};
use temps_entities::upstream_config::UpstreamList;
use temps_entities::{deployment_containers, deployments, environments, projects};
use temps_migrations::{Migrator, MigratorTrait};
use temps_proxy::service::lb_service::{LbService, LbServiceError};
use temps_proxy::{CachedPeerTable, RouteTableListener};

// PostgreSQL extensions are database-global while TestDatabase isolates tests
// with schemas. Serializing these two migration-heavy tests prevents concurrent
// `CREATE EXTENSION` calls from racing in the shared test container.
static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct NoOpQueue;

#[temps_core::async_trait::async_trait]
impl temps_core::JobQueue for NoOpQueue {
    async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
        panic!("the route listener never subscribes to its output queue")
    }
}

async fn wait_for_generation(table: &CachedPeerTable, previous: u64) {
    for _ in 0..100 {
        if table.current_generation() > previous {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("route table did not reload after PostgreSQL NOTIFY");
}

fn route(domain: &str, port: i32) -> custom_routes::ActiveModel {
    custom_routes::ActiveModel {
        domain: Set(domain.to_string()),
        host: Set("93.184.216.34".to_string()),
        port: Set(port),
        domain_id: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        enabled: Set(true),
        route_type: Set(RouteType::Http),
        force_override: Set(false),
        ..Default::default()
    }
}

async fn seed_generated_managed_routes(
    db: &sea_orm::DatabaseConnection,
) -> (i32, String, String, String, String) {
    let project = projects::ActiveModel {
        name: Set("Managed project".to_string()),
        repo_name: Set("managed-repo".to_string()),
        repo_owner: Set("managed-owner".to_string()),
        directory: Set(".".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::DockerCompose),
        preset_config: Set(Some(PresetConfig::DockerCompose(DockerComposeConfig {
            public_ports: vec![ComposePublicPort {
                service: "web".to_string(),
                port: 8080,
            }],
            ..Default::default()
        }))),
        slug: Set("managed-project".to_string()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert managed project");
    let environment = environments::ActiveModel {
        name: Set("Production".to_string()),
        slug: Set("production".to_string()),
        subdomain: Set("production".to_string()),
        host: Set("production".to_string()),
        upstreams: Set(UpstreamList::default()),
        project_id: Set(project.id),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert managed environment");
    let deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("deploy-abc".to_string()),
        state: Set("completed".to_string()),
        metadata: Set(Some(DeploymentMetadata::default())),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert managed deployment");
    let environment_id = environment.id;
    let mut environment_update: environments::ActiveModel = environment.into();
    environment_update.current_deployment_id = Set(Some(deployment.id));
    environment_update
        .update(db)
        .await
        .expect("link current deployment");
    deployment_containers::ActiveModel {
        deployment_id: Set(deployment.id),
        container_id: Set("managed-container".to_string()),
        container_name: Set("managed-container".to_string()),
        container_port: Set(8080),
        host_port: Set(Some(32_001)),
        image_name: Set(Some("managed:test".to_string())),
        status: Set(Some("running".to_string())),
        service_name: Set(Some("web".to_string())),
        deployed_at: Set(Utc::now()),
        ready_at: Set(Some(Utc::now())),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert managed container");

    (
        environment_id,
        "production.localho.st".to_string(),
        "web-production.localho.st".to_string(),
        "deploy-abc.localho.st".to_string(),
        "production.managed-project.temps.local".to_string(),
    )
}

#[tokio::test]
async fn custom_route_constraints_and_service_crud_compose_with_postgres() {
    let _test_guard = POSTGRES_TEST_LOCK.lock().await;
    let mut test_db = match TestDatabase::new().await {
        Ok(database) => database,
        Err(error) => {
            eprintln!("Skipping PostgreSQL custom-route test: {error}");
            return;
        }
    };
    eprintln!("custom-route integration: database ready");
    Migrator::up(test_db.db.as_ref(), None)
        .await
        .expect("custom-route migrations must succeed once PostgreSQL is available");
    eprintln!("custom-route integration: migrations ready");

    route("Unique.Example.com", 8080)
        .insert(test_db.db.as_ref())
        .await
        .expect("first normalized domain must insert");
    let duplicate = route("unique.example.com.", 8081)
        .insert(test_db.db.as_ref())
        .await
        .expect_err("normalized duplicate must be rejected by PostgreSQL");
    assert!(duplicate.to_string().contains("23505") || duplicate.to_string().contains("duplicate"));

    let invalid_port = route("bad-port.example.com", 70_000)
        .insert(test_db.db.as_ref())
        .await
        .expect_err("out-of-range port must be rejected by PostgreSQL");
    assert!(invalid_port
        .to_string()
        .contains("chk_custom_routes_port_range"));
    eprintln!("custom-route integration: constraints verified");

    let service = LbService::new(test_db.db.clone());
    let (environment_id, env_preview, compose_service, deployment_fallback, internal_name) =
        seed_generated_managed_routes(test_db.db.as_ref()).await;
    for domain in [
        &env_preview,
        &compose_service,
        &deployment_fallback,
        &internal_name,
    ] {
        let error = service
            .create_route_with_options(
                domain.clone(),
                "93.184.216.34".to_string(),
                9090,
                Some(RouteType::Http),
                false,
                false,
            )
            .await
            .expect_err("generated managed names require force_override at creation");
        assert!(matches!(
            error,
            LbServiceError::ManagedDomainConflict { .. }
        ));
    }
    for (index, domain) in [&env_preview, &compose_service, &deployment_fallback]
        .into_iter()
        .enumerate()
    {
        route(domain, 9_100 + index as i32)
            .insert(test_db.db.as_ref())
            .await
            .expect("direct legacy insert simulates an API bypass");
    }
    let mut forced = route(&internal_name, 9_200);
    forced.force_override = Set(true);
    forced
        .insert(test_db.db.as_ref())
        .await
        .expect("explicit forced route inserts");

    let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
    let listener = Arc::new(RouteTableListener::new(
        route_table.clone(),
        test_db.database_url.clone(),
        Arc::new(NoOpQueue),
    ));
    listener
        .clone()
        .start_listening()
        .await
        .expect("route-table listener must connect");
    assert!(route_table.wait_until_loaded(Duration::from_secs(3)).await);
    eprintln!("custom-route integration: listener loaded");
    for domain in [&env_preview, &compose_service, &deployment_fallback] {
        let loaded = route_table
            .get_route_by_host(domain)
            .expect("generated managed route must remain loaded");
        assert!(
            loaded.project.is_some(),
            "non-forced custom route must not hijack {domain}"
        );
    }
    let forced = route_table
        .get_route_by_host(&internal_name)
        .expect("forced custom route must load");
    assert!(forced.project.is_none());
    assert_eq!(forced.get_backend_addr(), "93.184.216.34:9200");

    let sleeping_domains = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let observed_sleeping_domains = sleeping_domains.clone();
    route_table.set_on_sleeping_callback(Arc::new(move |entries, _| {
        *observed_sleeping_domains.lock() = entries.into_iter().map(|entry| entry.domain).collect();
    }));
    environments::ActiveModel {
        id: Set(environment_id),
        sleeping: Set(true),
        ..Default::default()
    }
    .update(test_db.db.as_ref())
    .await
    .expect("put managed environment to sleep");
    route_table
        .load_routes()
        .await
        .expect("reload routes after the environment sleeps");
    assert!(
        route_table.get_route_by_host(&env_preview).is_none(),
        "sleeping managed route must remain reserved instead of falling through to custom routing"
    );
    assert!(
        sleeping_domains.lock().contains(&env_preview),
        "sleeping callback must retain authority over the managed hostname"
    );

    let generation = route_table.current_generation();
    let created = service
        .create_route(
            "service.example.com".to_string(),
            "93.184.216.34".to_string(),
            8080,
            Some(RouteType::Http),
        )
        .await
        .expect("service route creation must succeed");
    assert_eq!(created.domain, "service.example.com");
    eprintln!("custom-route integration: HTTP route created");
    assert!(service.has_route_in_snapshot("service.example.com"));
    wait_for_generation(route_table.as_ref(), generation).await;
    assert_eq!(
        route_table
            .get_route_by_host("service.example.com")
            .expect("HTTP route must reach the live route table")
            .get_backend_addr(),
        "93.184.216.34:8080"
    );

    let generation = route_table.current_generation();
    service
        .create_route(
            "*.tls.example.com".to_string(),
            "93.184.216.34".to_string(),
            8443,
            Some(RouteType::Tls),
        )
        .await
        .expect("TLS wildcard route creation must succeed");
    eprintln!("custom-route integration: TLS route created");
    wait_for_generation(route_table.as_ref(), generation).await;
    assert_eq!(
        route_table
            .get_route_by_sni("api.tls.example.com")
            .expect("TLS wildcard must reach the live route table")
            .get_backend_addr(),
        "93.184.216.34:8443"
    );

    let generation = route_table.current_generation();
    service
        .delete_route("SERVICE.EXAMPLE.COM.")
        .await
        .expect("canonical exact deletion must succeed");
    wait_for_generation(route_table.as_ref(), generation).await;
    assert!(route_table
        .get_route_by_host("service.example.com")
        .is_none());
    let missing = service
        .get_route_exact("service.example.com")
        .await
        .expect_err("deleted route must be absent");
    assert!(matches!(missing, LbServiceError::NotFound(_)));
    eprintln!("custom-route integration: deletion verified");

    let service_a = LbService::new(test_db.db.clone());
    let service_b = LbService::new(test_db.db.clone());
    let (wildcard, exact) = tokio::join!(
        service_a.create_route(
            "*.race.example.com".to_string(),
            "93.184.216.34".to_string(),
            8080,
            Some(RouteType::Http),
        ),
        service_b.create_route(
            "api.race.example.com".to_string(),
            "93.184.216.34".to_string(),
            8081,
            Some(RouteType::Http),
        )
    );
    eprintln!("custom-route integration: concurrent writes completed");
    assert_eq!(
        usize::from(wildcard.is_ok()) + usize::from(exact.is_ok()),
        1,
        "the advisory lock must make concurrent wildcard/exact creation atomic"
    );
    let rejected = wildcard
        .err()
        .or_else(|| exact.err())
        .expect("one rejected");
    assert!(matches!(rejected, LbServiceError::RouteOverlap { .. }));
    listener.shutdown();
    drop(listener);
    drop(route_table);
    drop(service_a);
    drop(service_b);
    drop(service);
    test_db.cleanup().await;
}

#[tokio::test]
async fn custom_route_migration_rejects_legacy_collisions_without_data_loss() {
    let _test_guard = POSTGRES_TEST_LOCK.lock().await;
    let mut test_db = match TestDatabase::new().await {
        Ok(database) => database,
        Err(error) => {
            eprintln!("Skipping PostgreSQL custom-route migration test: {error}");
            return;
        }
    };
    Migrator::up(test_db.db.as_ref(), None)
        .await
        .expect("baseline migrations must succeed");
    Migrator::down(test_db.db.as_ref(), Some(1))
        .await
        .expect("the custom-route hardening migration must roll back");

    test_db
        .db
        .execute_unprepared(
            r#"
            INSERT INTO custom_routes
                (domain, host, port, domain_id, created_at, updated_at, enabled, route_type)
            VALUES
                ('Legacy.Example.com', '93.184.216.34', 8080, NULL, now(), now(), true, 'http'),
                ('legacy.example.com.', '93.184.216.34', 8081, NULL, now(), now(), true, 'http');
            "#,
        )
        .await
        .expect("legacy schema permits differently formatted duplicates");

    let error = Migrator::up(test_db.db.as_ref(), None)
        .await
        .expect_err("normalization collision must stop the upgrade");
    assert!(error.to_string().contains("duplicate domain"));

    let row = test_db
        .db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) AS count FROM custom_routes WHERE lower(rtrim(domain, '.')) = 'legacy.example.com'".to_string(),
        ))
        .await
        .expect("legacy row count query")
        .expect("count query returns one row");
    let count: i64 = row.try_get("", "count").expect("count is an integer");
    assert_eq!(count, 2, "failed migration must preserve both legacy rows");
    test_db.cleanup().await;
}

#[tokio::test]
async fn custom_route_migration_quarantines_unsafe_legacy_upstreams() {
    let _test_guard = POSTGRES_TEST_LOCK.lock().await;
    let mut test_db = match TestDatabase::new().await {
        Ok(database) => database,
        Err(error) => {
            eprintln!("Skipping PostgreSQL custom-route quarantine test: {error}");
            return;
        }
    };
    Migrator::up(test_db.db.as_ref(), None)
        .await
        .expect("baseline migrations must succeed");
    Migrator::down(test_db.db.as_ref(), Some(1))
        .await
        .expect("the custom-route hardening migration must roll back");
    test_db
        .db
        .execute_unprepared(
            r#"
            INSERT INTO custom_routes
                (domain, host, port, domain_id, created_at, updated_at, enabled, route_type)
            VALUES
                ('hostname.example.com', 'rebind.example.com', 8080, NULL, now(), now(), true, 'http'),
                ('metadata.example.com', '169.254.169.254', 8080, NULL, now(), now(), true, 'http'),
                ('benchmark.example.com', '198.18.0.1', 8080, NULL, now(), now(), true, 'http'),
                ('private.example.com', '127.0.0.1', 8080, NULL, now(), now(), true, 'http'),
                ('public-v6.example.com', '2001:4860:4860::8888', 8080, NULL, now(), now(), true, 'http');
            "#,
        )
        .await
        .expect("legacy schema accepts unvalidated upstreams");
    Migrator::up(test_db.db.as_ref(), None)
        .await
        .expect("hardening migration must quarantine unsafe legacy rows");

    let rows = test_db
        .db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT domain, host, enabled FROM custom_routes ORDER BY domain".to_string(),
        ))
        .await
        .expect("query migrated legacy routes");
    let migrated = rows
        .into_iter()
        .map(|row| {
            let domain: String = row.try_get("", "domain")?;
            let host: String = row.try_get("", "host")?;
            let enabled: bool = row.try_get("", "enabled")?;
            Ok::<_, sea_orm::DbErr>((domain, host, enabled))
        })
        .collect::<Result<Vec<_>, _>>()
        .expect("decode migrated legacy routes");
    assert_eq!(
        migrated,
        vec![
            (
                "benchmark.example.com".to_string(),
                "198.18.0.1".to_string(),
                false
            ),
            (
                "hostname.example.com".to_string(),
                "rebind.example.com".to_string(),
                false
            ),
            (
                "metadata.example.com".to_string(),
                "169.254.169.254".to_string(),
                false
            ),
            (
                "private.example.com".to_string(),
                "127.0.0.1".to_string(),
                true
            ),
            (
                "public-v6.example.com".to_string(),
                "[2001:4860:4860::8888]".to_string(),
                true
            ),
        ]
    );
    test_db.cleanup().await;
}
