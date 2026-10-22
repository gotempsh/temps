// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Upgrade test for DNS name ownership (ADR-055 §10, phase 0).
//!
//! Starts from the schema before the claims migration, seeds a zone with
//! the collisions a real install can have, runs the migration, and checks:
//! the winner of each name is deterministic, nothing is removed for a loser,
//! the loser's next publish is refused, the conflict is listed, and the down
//! migration gives the previous owners back.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Statement,
};
use sea_orm_migration::{MigrationTrait, SchemaManager};
use temps_database::test_utils::TestDatabase;
use temps_dns::services::{DnsRegistry, DnsRegistryError, EndpointDraft, OwnerKind, RecordType};
use temps_entities::{deployments, environments, projects, service_endpoints};
use temps_migrations::DnsNameClaimsMigration;

async fn database() -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error) if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some() => {
            panic!("Explicit test database failed: {error}");
        }
        Err(error) => {
            eprintln!("Postgres unavailable; skipping DNS claims upgrade test: {error}");
            None
        }
    }
}

async fn seed_environment(
    db: &DatabaseConnection,
    project_slug: &str,
    env_slug: &str,
) -> environments::Model {
    use temps_entities::{preset::Preset, upstream_config::UpstreamList};
    let project = projects::ActiveModel {
        name: Set(format!("Project {project_slug}")),
        slug: Set(project_slug.to_string()),
        repo_owner: Set("owner".to_string()),
        repo_name: Set("repo".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::NextJs),
        directory: Set("/".to_string()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        is_deleted: Set(false),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("seed project");
    environments::ActiveModel {
        project_id: Set(project.id),
        name: Set(env_slug.to_string()),
        slug: Set(env_slug.to_string()),
        host: Set(format!("{env_slug}.{project_slug}.example.test")),
        subdomain: Set(format!("{env_slug}.{project_slug}.example.test")),
        upstreams: Set(UpstreamList::default()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("seed environment")
}

async fn seed_deployment(
    db: &DatabaseConnection,
    env: &environments::Model,
    slug: &str,
) -> deployments::Model {
    deployments::ActiveModel {
        project_id: Set(env.project_id),
        environment_id: Set(env.id),
        slug: Set(slug.to_string()),
        state: Set("completed".to_string()),
        metadata: Set(Some(deployments::DeploymentMetadata::default())),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("seed deployment")
}

/// A row as an older binary writes it, with an explicit age.
async fn seed_row(
    db: &DatabaseConnection,
    fqdn: &str,
    ip: &str,
    owner_kind: &str,
    owner_id: i64,
    age_secs: i64,
) {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO service_endpoints (fqdn, record_type, target_ip, target_port, ttl, owner_kind, \
         owner_id, generation, created_at, updated_at) \
         VALUES ($1, 'A', $2, 80, 10, $3, $4, 1, now() - ($5::bigint || ' seconds')::interval, now())",
        [fqdn.into(), ip.into(), owner_kind.into(), owner_id.into(), age_secs.into()],
    ))
    .await
    .expect("seed endpoint row");
}

async fn owners_of(db: &DatabaseConnection, fqdn: &str) -> Vec<(String, i64, String)> {
    service_endpoints::Entity::find()
        .filter(service_endpoints::Column::Fqdn.eq(fqdn))
        .order_by_asc(service_endpoints::Column::OwnerKind)
        .order_by_asc(service_endpoints::Column::OwnerId)
        .order_by_asc(service_endpoints::Column::TargetIp)
        .all(db)
        .await
        .expect("read endpoints")
        .into_iter()
        .map(|r| (r.owner_kind, r.owner_id, r.target_ip.unwrap_or_default()))
        .collect()
}

#[tokio::test]
async fn claims_backfill_picks_a_deterministic_winner_and_reports_the_loser() {
    let Some(test_db) = database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let schema = SchemaManager::new(db.as_ref());

    // Back to the schema the previous release ran.
    DnsNameClaimsMigration
        .down(&schema)
        .await
        .expect("down migration");

    // Environment `primary` of project `foo`, redeployed once: the old
    // deployment's rows were taken over by the current one, as the old
    // upsert did, so only the current deployment owns rows.
    let env = seed_environment(db.as_ref(), "foo", "primary").await;
    let _old = seed_deployment(db.as_ref(), &env, "d-old").await;
    let current = seed_deployment(db.as_ref(), &env, "d-current").await;
    let mut active: environments::ActiveModel = env.clone().into();
    active.current_deployment_id = Set(Some(current.id));
    let env = active
        .update(db.as_ref())
        .await
        .expect("set current deployment");

    // Collision 1: the environment's name is also the primary name of a
    // clustered Postgres `foo` (service 4). The environment's row is OLDER,
    // so the service must win on kind, not on age.
    seed_row(
        db.as_ref(),
        "primary.foo.temps.local",
        "172.20.0.1",
        "deployment",
        current.id as i64,
        600,
    )
    .await;
    seed_row(
        db.as_ref(),
        "primary.foo.temps.local",
        "10.0.0.9",
        "service_role",
        4,
        60,
    )
    .await;
    // Collision 2: two owners of the same kind: the oldest row wins.
    seed_row(
        db.as_ref(),
        "m.cache.temps.local",
        "10.0.0.20",
        "service_member",
        8,
        30,
    )
    .await;
    seed_row(
        db.as_ref(),
        "m.cache.temps.local",
        "10.0.0.21",
        "service_member",
        7,
        300,
    )
    .await;
    // An orphan: a deployment that no longer exists.
    seed_row(
        db.as_ref(),
        "gone.app.temps.local",
        "172.20.0.1",
        "deployment",
        999_999,
        60,
    )
    .await;
    // An uncontested name, written with a different case.
    seed_row(
        db.as_ref(),
        "Solo.Temps.Local",
        "10.0.0.30",
        "service_role",
        11,
        60,
    )
    .await;

    DnsNameClaimsMigration
        .up(&schema)
        .await
        .expect("up migration");

    let registry = DnsRegistry::new(db.clone());

    // The deployment's row now belongs to its environment; nothing else
    // under the contested name was removed.
    assert_eq!(
        owners_of(db.as_ref(), "primary.foo.temps.local").await,
        vec![
            (
                "environment".to_string(),
                env.id as i64,
                "172.20.0.1".to_string()
            ),
            ("service_role".to_string(), 4, "10.0.0.9".to_string()),
        ]
    );
    assert!(
        owners_of(db.as_ref(), "gone.app.temps.local")
            .await
            .is_empty(),
        "orphans are deleted"
    );

    // Winners.
    let claim = |fqdn: &'static str| {
        let registry = registry.clone();
        async move {
            registry
                .claim_for(fqdn)
                .await
                .unwrap()
                .map(|c| (c.owner_kind, c.owner_id))
        }
    };
    assert_eq!(
        claim("primary.foo.temps.local").await,
        Some(("service_role".into(), 4)),
        "databases beat environments"
    );
    assert_eq!(
        claim("m.cache.temps.local").await,
        Some(("service_member".into(), 7)),
        "then the oldest row"
    );
    assert_eq!(
        claim("solo.temps.local").await,
        Some(("service_role".into(), 11)),
        "names are normalised"
    );

    // Losers are reported.
    let mut conflicts: Vec<_> = registry
        .list_name_conflicts()
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.fqdn, c.owner_kind, c.owner_id, c.holder_kind, c.holder_id))
        .collect();
    conflicts.sort();
    assert_eq!(
        conflicts,
        vec![
            (
                "m.cache.temps.local".into(),
                "service_member".into(),
                8,
                "service_member".into(),
                7
            ),
            (
                "primary.foo.temps.local".into(),
                "environment".into(),
                env.id as i64,
                "service_role".into(),
                4
            ),
        ]
    );

    // The loser's next publish is refused, and its records stay as they are.
    let env_draft = EndpointDraft {
        fqdn: "primary.foo.temps.local".into(),
        record_type: RecordType::A,
        target_ip: Some("172.20.0.1".into()),
        target_port: Some(80),
        ttl: 10,
        owner_kind: OwnerKind::Environment,
        owner_id: env.id as i64,
        node_id: None,
    };
    let refused = registry
        .replace_endpoints_for_owner(
            OwnerKind::Environment,
            env.id as i64,
            std::slice::from_ref(&env_draft),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, DnsRegistryError::NameClaimed { holder_id: 4, .. }),
        "{refused:?}"
    );

    // The winner's publish leaves the recorded loser's row alone.
    let role_draft = EndpointDraft {
        target_ip: Some("10.0.0.10".into()),
        owner_kind: OwnerKind::ServiceRole,
        owner_id: 4,
        ..env_draft.clone()
    };
    registry
        .replace_endpoints_for_owner(OwnerKind::ServiceRole, 4, &[role_draft])
        .await
        .expect("the holder publishes");
    assert_eq!(
        owners_of(db.as_ref(), "primary.foo.temps.local").await,
        vec![
            (
                "environment".to_string(),
                env.id as i64,
                "172.20.0.1".to_string()
            ),
            ("service_role".to_string(), 4, "10.0.0.10".to_string()),
        ],
        "reported, not removed"
    );

    // Running the backfill again changes nothing.
    for sql in temps_migrations::dns_name_claims_backfill_sql() {
        db.execute_unprepared(&sql).await.expect("re-run backfill");
    }
    assert_eq!(registry.list_name_conflicts().await.unwrap().len(), 2);

    // The down migration hands the environment's rows back to its current
    // deployment, which is what the previous release reads.
    DnsNameClaimsMigration
        .down(&schema)
        .await
        .expect("down migration again");
    assert_eq!(
        owners_of(db.as_ref(), "primary.foo.temps.local").await,
        vec![
            (
                "deployment".to_string(),
                current.id as i64,
                "172.20.0.1".to_string()
            ),
            ("service_role".to_string(), 4, "10.0.0.10".to_string()),
        ]
    );
    // And up again, as the next upgrade would.
    DnsNameClaimsMigration
        .up(&schema)
        .await
        .expect("up migration again");
    assert_eq!(
        claim("primary.foo.temps.local").await,
        Some(("service_role".into(), 4))
    );
}
