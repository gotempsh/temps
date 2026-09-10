//! Real-Postgres coverage for project-scoped domain delivery configuration.
//!
//! Provider I/O is represented by a deterministic fake while migrations,
//! previews, route reservations, bindings, status changes, and retries use a
//! real database.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use temps_database::test_utils::TestDatabase;
use temps_dns::services::{
    domain_delivery::{
        DeliveryProviderKind, DomainDeliveryDns, DomainDeliveryService,
        EnvironmentDeliveryOverride, PreviewDomainDeliveryBindingRequest,
    },
    DnsProviderService, ManagedDnsRecordService,
};
use temps_dns::{
    providers::{DnsRecord, DnsRecordRequest, DnsRecordType},
    DnsError,
};
use tokio::sync::{Mutex, Notify};

async fn test_database(test_name: &str) -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error) => {
            eprintln!(
                "Docker/Postgres unavailable; skipping {test_name} domain-delivery integration test: {error}"
            );
            None
        }
    }
}

fn delivery_service(db: Arc<DatabaseConnection>) -> DomainDeliveryService {
    let encryption = Arc::new(
        temps_core::EncryptionService::new("0123456789abcdef0123456789abcdef")
            .expect("valid test encryption key"),
    );
    let providers = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
    let managed = Arc::new(ManagedDnsRecordService::new(
        db.clone(),
        providers,
        encryption,
    ));
    DomainDeliveryService::new(db, managed)
}

#[derive(Default)]
struct FakeDnsState {
    record: Option<DnsRecord>,
    set_calls: usize,
    fail_next_set: bool,
}

#[derive(Default)]
struct FakeDns {
    state: Mutex<FakeDnsState>,
    delay_set: AtomicBool,
    active_sets: AtomicUsize,
    max_active_sets: AtomicUsize,
    provider_calls_while_set_active: AtomicUsize,
    set_entered: Notify,
}

impl FakeDns {
    async fn fail_next_set(&self) {
        self.state.lock().await.fail_next_set = true;
    }

    fn delay_set(&self) {
        self.delay_set.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl DomainDeliveryDns for FakeDns {
    async fn record_ownership(
        &self,
        _domain: &str,
        _name: &str,
        _record_type: DnsRecordType,
    ) -> Result<temps_dns::services::RecordOwnership, DnsError> {
        if self.active_sets.load(Ordering::SeqCst) > 0 {
            self.provider_calls_while_set_active
                .fetch_add(1, Ordering::SeqCst);
        }
        Ok(match self.state.lock().await.record.clone() {
            Some(record) => temps_dns::services::RecordOwnership::Unmanaged(record),
            None => temps_dns::services::RecordOwnership::NotFound,
        })
    }

    async fn import_record(
        &self,
        _domain: &str,
        _name: &str,
        _record_type: DnsRecordType,
        _scope: temps_dns::services::OwnershipScope,
    ) -> Result<(), DnsError> {
        Ok(())
    }

    async fn set_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
        proxied: Option<bool>,
        _scope: temps_dns::services::OwnershipScope,
    ) -> Result<DnsRecord, DnsError> {
        let active = self.active_sets.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active_sets.fetch_max(active, Ordering::SeqCst);
        self.set_entered.notify_waiters();
        if self.delay_set.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let mut state = self.state.lock().await;
        state.set_calls += 1;
        if std::mem::take(&mut state.fail_next_set) {
            self.active_sets.fetch_sub(1, Ordering::SeqCst);
            return Err(DnsError::ConnectionFailed(
                "injected provider outage".into(),
            ));
        }
        let fqdn = if request.name == "@" {
            domain.to_string()
        } else {
            format!("{}.{}", request.name, domain)
        };
        let record = DnsRecord {
            id: Some(format!("fake-{}", state.set_calls)),
            zone: domain.into(),
            name: request.name,
            fqdn,
            content: request.content,
            ttl: request.ttl.unwrap_or(300),
            proxied: proxied.unwrap_or(request.proxied),
            metadata: HashMap::new(),
        };
        state.record = Some(record.clone());
        self.active_sets.fetch_sub(1, Ordering::SeqCst);
        Ok(record)
    }

    async fn remove_record(
        &self,
        _domain: &str,
        _name: &str,
        _record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        if self.active_sets.load(Ordering::SeqCst) > 0 {
            self.provider_calls_while_set_active
                .fetch_add(1, Ordering::SeqCst);
        }
        self.state.lock().await.record = None;
        Ok(())
    }
}

async fn insert_project(db: &DatabaseConnection, slug: &str) -> i32 {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, preset, created_at, updated_at, slug) VALUES ($1, 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), $1) RETURNING id",
        [slug.into()],
    ))
    .await
    .expect("insert project");
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id FROM projects WHERE slug = $1",
        [slug.into()],
    ))
    .await
    .expect("select project")
    .expect("project row")
    .try_get("", "id")
    .expect("project id")
}

async fn insert_environment(db: &DatabaseConnection, project_id: i32, slug: &str) -> i32 {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, updated_at, project_id) VALUES ($1, $1, $1, $2, '[]', now(), now(), $3)",
        [slug.into(), format!("{slug}.example.test").into(), project_id.into()],
    ))
    .await
    .expect("insert environment");
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id FROM environments WHERE project_id = $1 AND slug = $2",
        [project_id.into(), slug.into()],
    ))
    .await
    .expect("select environment")
    .expect("environment row")
    .try_get("", "id")
    .expect("environment id")
}

async fn insert_user(db: &DatabaseConnection, email: &str) -> i32 {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO users (name, email, created_at, updated_at) VALUES ('Delivery Tester', $1, now(), now())",
        [email.into()],
    ))
    .await
    .expect("insert user");
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id FROM users WHERE email = $1",
        [email.into()],
    ))
    .await
    .expect("select user")
    .expect("user row")
    .try_get("", "id")
    .expect("user id")
}

async fn insert_managed_provider(db: &DatabaseConnection, zone: &str) -> i32 {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO dns_providers (name, provider_type, credentials, is_active, created_at, updated_at) VALUES ($1, 'manual', '{}', true, now(), now())",
        [format!("provider-{zone}").into()],
    ))
    .await
    .expect("insert DNS provider");
    let provider_id: i32 = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM dns_providers WHERE name = $1",
            [format!("provider-{zone}").into()],
        ))
        .await
        .expect("select provider")
        .expect("provider row")
        .try_get("", "id")
        .expect("provider id");
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO dns_managed_domains (provider_id, domain, auto_manage, proxied_by_default, verified, generated_hostname_mode, sync_generated_records, created_at, updated_at) VALUES ($1, $2, true, false, true, 'standard', false, now(), now())",
        [provider_id.into(), zone.into()],
    ))
    .await
    .expect("insert managed domain");
    provider_id
}

async fn delivery_fixture(
    db: Arc<DatabaseConnection>,
    slug: &str,
) -> (i32, i32, i32, i32, Arc<FakeDns>, DomainDeliveryService) {
    let project_id = insert_project(db.as_ref(), slug).await;
    let environment_id = insert_environment(db.as_ref(), project_id, "production").await;
    let actor_id = insert_user(db.as_ref(), &format!("{slug}@example.test")).await;
    let provider_id = insert_managed_provider(db.as_ref(), "example.test").await;
    let fake = Arc::new(FakeDns::default());
    let service = DomainDeliveryService::with_dns(db, fake.clone());
    let profile = service
        .create_profile("Direct delivery".into(), DeliveryProviderKind::Direct)
        .await
        .expect("create delivery profile");
    service
        .update_settings(project_id, Some(profile.id), vec![])
        .await
        .expect("set project delivery default");
    (
        project_id,
        environment_id,
        actor_id,
        provider_id,
        fake,
        service,
    )
}

fn preview_request(environment_id: i32, provider_id: i32) -> PreviewDomainDeliveryBindingRequest {
    PreviewDomainDeliveryBindingRequest {
        hostname: "app.example.test".into(),
        environment_id,
        dns_provider_id: provider_id,
        zone: "example.test".into(),
        origin_target: "192.0.2.42".into(),
        delivery_profile_id: None,
    }
}

async fn scalar_i64(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await
        .expect("count query")
        .expect("count row")
        .try_get("", "count")
        .expect("count value")
}

#[tokio::test]
async fn test_domain_delivery_migration_fresh_schema_enforces_constraints() {
    let Some(test_db) = test_database("migration constraints").await else {
        return;
    };
    let db = test_db.connection_arc();

    for table in [
        "delivery_profiles",
        "project_delivery_settings",
        "environment_delivery_settings",
        "domain_delivery_bindings",
        "domain_delivery_previews",
    ] {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT to_regclass($1)::text AS table_name",
                [table.into()],
            ))
            .await
            .expect("inspect migrated schema")
            .expect("inspection row");
        let actual: Option<String> = row.try_get("", "table_name").expect("table name");
        assert_eq!(
            actual.as_deref(),
            Some(table),
            "migration must create {table}"
        );
    }

    db.execute_unprepared(
        "INSERT INTO delivery_profiles (name, provider_kind) VALUES ('direct', 'direct')",
    )
    .await
    .expect("valid provider kind");
    assert!(
        db.execute_unprepared(
            "INSERT INTO delivery_profiles (name, provider_kind) VALUES ('bad', 'route53')"
        )
        .await
        .is_err(),
        "provider_kind CHECK must reject unknown delivery adapters"
    );
    assert!(
        db.execute_unprepared(
            "INSERT INTO delivery_profiles (name, provider_kind) VALUES ('direct', 'cloudflare')"
        )
        .await
        .is_err(),
        "profile names must remain unique"
    );
}

#[tokio::test]
async fn test_profile_lifecycle_referenced_profile_conflicts_then_deletes() {
    let Some(test_db) = test_database("profile lifecycle").await else {
        return;
    };
    let db = test_db.connection_arc();
    let project_id = insert_project(db.as_ref(), "delivery-profile-project").await;
    let service = delivery_service(db);

    let direct = service
        .create_profile("  Shared direct  ".into(), DeliveryProviderKind::Direct)
        .await
        .expect("create profile");
    assert_eq!(direct.name, "Shared direct");
    assert_eq!(
        service.list_profiles().await.expect("list profiles").len(),
        1
    );

    service
        .update_settings(project_id, Some(direct.id), vec![])
        .await
        .expect("reference profile from project");
    let error = service
        .delete_profile(direct.id)
        .await
        .expect_err("referenced profile must not be deleted");
    assert!(error.to_string().contains("still referenced"), "{error}");

    service
        .update_settings(project_id, None, vec![])
        .await
        .expect("remove reference");
    service
        .delete_profile(direct.id)
        .await
        .expect("delete profile");
    assert!(service
        .list_profiles()
        .await
        .expect("list after delete")
        .is_empty());
}

#[tokio::test]
async fn test_project_settings_persist_across_service_instances_without_touching_dns() {
    let Some(test_db) = test_database("settings persistence").await else {
        return;
    };
    let db = test_db.connection_arc();
    let project_id = insert_project(db.as_ref(), "delivery-settings-project").await;
    let environment_id = insert_environment(db.as_ref(), project_id, "production").await;
    db.execute_unprepared("INSERT INTO dns_providers (name, provider_type, credentials, is_active, created_at, updated_at) VALUES ('existing', 'manual', '{}', true, now(), now())")
        .await
        .expect("seed existing provider");
    db.execute_unprepared("INSERT INTO dns_managed_domains (provider_id, domain, auto_manage, proxied_by_default, verified, generated_hostname_mode, sync_generated_records, created_at, updated_at) SELECT id, 'existing.test', true, false, true, 'standard', false, now(), now() FROM dns_providers WHERE name = 'existing'")
        .await
        .expect("seed existing managed domain");
    let dns_providers_before =
        scalar_i64(db.as_ref(), "SELECT count(*) AS count FROM dns_providers").await;
    let managed_domains_before = scalar_i64(
        db.as_ref(),
        "SELECT count(*) AS count FROM dns_managed_domains",
    )
    .await;

    let first = delivery_service(db.clone());
    let project_profile = first
        .create_profile("Project default".into(), DeliveryProviderKind::Direct)
        .await
        .expect("project profile");
    let environment_profile = first
        .create_profile(
            "Environment override".into(),
            DeliveryProviderKind::Cloudflare,
        )
        .await
        .expect("environment profile");
    first
        .update_settings(
            project_id,
            Some(project_profile.id),
            vec![EnvironmentDeliveryOverride {
                environment_id,
                profile_id: Some(environment_profile.id),
            }],
        )
        .await
        .expect("persist settings");

    let restarted = delivery_service(db.clone());
    let settings = restarted
        .settings(project_id)
        .await
        .expect("reload settings");
    assert_eq!(settings.default_profile_id, Some(project_profile.id));
    assert_eq!(
        settings
            .effective_default_profile
            .expect("effective default")
            .name,
        "Project default"
    );
    assert_eq!(settings.environment_overrides.len(), 1);
    assert_eq!(
        settings.environment_overrides[0].environment_id,
        environment_id
    );
    assert_eq!(
        settings.environment_overrides[0].profile_id,
        Some(environment_profile.id)
    );
    assert_eq!(
        scalar_i64(db.as_ref(), "SELECT count(*) AS count FROM dns_providers").await,
        dns_providers_before
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM dns_managed_domains"
        )
        .await,
        managed_domains_before
    );
}

#[tokio::test]
async fn test_project_settings_reject_missing_and_cross_project_resources_atomically() {
    let Some(test_db) = test_database("settings validation").await else {
        return;
    };
    let db = test_db.connection_arc();
    let project_id = insert_project(db.as_ref(), "delivery-validation-project").await;
    let other_project_id = insert_project(db.as_ref(), "other-delivery-project").await;
    let other_environment_id =
        insert_environment(db.as_ref(), other_project_id, "other-production").await;
    let service = delivery_service(db);
    let profile = service
        .create_profile("Candidate".into(), DeliveryProviderKind::Direct)
        .await
        .expect("profile");

    assert!(service
        .settings(i32::MAX)
        .await
        .expect_err("missing project")
        .to_string()
        .contains("project"));
    assert!(service
        .update_settings(project_id, Some(i32::MAX), vec![])
        .await
        .expect_err("missing profile")
        .to_string()
        .contains("delivery profile"));
    assert!(service
        .update_settings(
            project_id,
            None,
            vec![EnvironmentDeliveryOverride {
                environment_id: i32::MAX,
                profile_id: Some(profile.id)
            }]
        )
        .await
        .expect_err("missing environment")
        .to_string()
        .contains("environment"));

    let error = service
        .update_settings(
            project_id,
            Some(profile.id),
            vec![EnvironmentDeliveryOverride {
                environment_id: other_environment_id,
                profile_id: Some(profile.id),
            }],
        )
        .await
        .expect_err("cross-project environment must be refused");
    assert!(error.to_string().contains("belongs to project"), "{error}");
    let settings = service
        .settings(project_id)
        .await
        .expect("settings after rejected update");
    assert_eq!(
        settings.default_profile_id, None,
        "a rejected override must not partially persist the new project default"
    );
    assert!(settings.environment_overrides.is_empty());
}

#[tokio::test]
async fn test_project_settings_database_failure_rolls_back_all_changes() {
    let Some(test_db) = test_database("settings transaction rollback").await else {
        return;
    };
    let db = test_db.connection_arc();
    let project_id = insert_project(db.as_ref(), "delivery-rollback-project").await;
    let environment_id = insert_environment(db.as_ref(), project_id, "production").await;
    let service = delivery_service(db.clone());
    let original = service
        .create_profile("Original settings".into(), DeliveryProviderKind::Direct)
        .await
        .expect("original profile");
    let replacement = service
        .create_profile(
            "Replacement settings".into(),
            DeliveryProviderKind::Cloudflare,
        )
        .await
        .expect("replacement profile");
    service
        .update_settings(
            project_id,
            Some(original.id),
            vec![EnvironmentDeliveryOverride {
                environment_id,
                profile_id: Some(original.id),
            }],
        )
        .await
        .expect("seed original settings");
    db.execute_unprepared(
        r#"
CREATE FUNCTION fail_delivery_environment_update() RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION 'injected environment settings write failure';
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER fail_delivery_environment_update
BEFORE UPDATE ON environment_delivery_settings
FOR EACH ROW EXECUTE FUNCTION fail_delivery_environment_update();
"#,
    )
    .await
    .expect("install failure trigger");

    let error = service
        .update_settings(
            project_id,
            Some(replacement.id),
            vec![EnvironmentDeliveryOverride {
                environment_id,
                profile_id: Some(replacement.id),
            }],
        )
        .await
        .expect_err("environment write failure must reject the settings update");
    assert!(
        error
            .to_string()
            .contains("injected environment settings write failure"),
        "{error}"
    );

    let persisted = service.settings(project_id).await.expect("reload settings");
    assert_eq!(persisted.default_profile_id, Some(original.id));
    assert_eq!(persisted.environment_overrides.len(), 1);
    assert_eq!(
        persisted.environment_overrides[0].profile_id,
        Some(original.id),
        "the project write and environment write must commit or roll back together"
    );
}

#[tokio::test]
async fn test_apply_direct_delivery_persists_route_binding_and_provider_write() {
    let Some(test_db) = test_database("delivery apply").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-apply-project").await;

    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview direct delivery");
    assert_eq!(preview.profile_source, "project");
    assert_eq!(preview.record.name, "app");
    assert_eq!(preview.record.record_type, DnsRecordType::A);
    assert!(!preview.record.requires_adoption);

    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("apply direct delivery");
    assert_eq!(binding.hostname, "app.example.test");
    assert_eq!(binding.status, "dns_configured");
    assert_eq!(binding.origin_target, "192.0.2.42");
    assert!(!binding.proxied);
    assert!(binding.applied_at.is_some());

    let delete_error = temps_projects::CustomDomainService::new(db.clone())
        .delete_custom_domain(binding.custom_domain_id)
        .await
        .expect_err("a supported route must not be deleted while its delivery binding exists");
    assert!(
        matches!(
            delete_error,
            temps_projects::CustomDomainError::DeliveryBindingExists {
                domain_id,
                binding_id,
            } if domain_id == binding.custom_domain_id && binding_id == binding.id
        ),
        "deletion must identify the blocking delivery binding"
    );

    let state = fake.state.lock().await;
    assert_eq!(state.set_calls, 1);
    let record = state.record.as_ref().expect("provider record written");
    assert_eq!(record.fqdn, "app.example.test");
    assert_eq!(record.ttl, 300);
    drop(state);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM project_custom_domains WHERE domain = 'app.example.test' AND status = 'pending'",
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings WHERE hostname = 'app.example.test' AND status = 'dns_configured' AND last_error IS NULL",
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'applied' AND applied_at IS NOT NULL",
        )
        .await,
        1
    );

    assert!(
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM environments WHERE id = $1",
            [environment_id.into()],
        ))
        .await
        .is_err(),
        "the binding FK must block deleting its environment before DNS cleanup"
    );
    assert!(
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM projects WHERE id = $1",
            [project_id.into()],
        ))
        .await
        .is_err(),
        "the binding FK must block deleting its project before DNS cleanup"
    );

    service
        .delete_binding(project_id, binding.id)
        .await
        .expect("remove DNS and binding before deleting parents");
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings"
        )
        .await,
        0
    );
    assert!(
        fake.state.lock().await.record.is_none(),
        "delivery cleanup must remove the provider record"
    );
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "DELETE FROM environments WHERE id = $1",
        [environment_id.into()],
    ))
    .await
    .expect("environment deletion is allowed after delivery cleanup");
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "DELETE FROM projects WHERE id = $1",
        [project_id.into()],
    ))
    .await
    .expect("project deletion is allowed after delivery cleanup");
}

#[tokio::test]
async fn test_apply_provider_failure_persists_failure_and_same_preview_retries() {
    let Some(test_db) = test_database("delivery retry").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-retry-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview direct delivery");
    fake.fail_next_set().await;

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("injected provider outage must fail apply");
    assert!(
        error.to_string().contains("injected provider outage"),
        "{error}"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings WHERE status = 'failed' AND last_error LIKE '%injected provider outage%'",
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'failed' AND last_error LIKE '%injected provider outage%'",
        )
        .await,
        1
    );

    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("retry the same failed preview");
    assert_eq!(binding.status, "dns_configured");
    assert!(binding.last_error.is_none());
    assert!(binding.applied_at.is_some());
    assert_eq!(fake.state.lock().await.set_calls, 2);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings WHERE hostname = 'app.example.test'",
        )
        .await,
        1,
        "retry must update the reserved binding rather than duplicate it"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'applied' AND last_error IS NULL",
        )
        .await,
        1
    );
}

#[tokio::test]
async fn test_apply_same_hostname_serializes_provider_mutation() {
    let Some(test_db) = test_database("same-host apply serialization").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db, "delivery-concurrency-project").await;
    let first = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("first preview");
    let second = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("overlapping preview");
    fake.delay_set();

    let (first_result, second_result) = tokio::join!(
        service.apply(project_id, actor_id, first.preview_id, vec![]),
        service.apply(project_id, actor_id, second.preview_id, vec![]),
    );

    assert_eq!(
        usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
        1,
        "one preview applies and the serialized stale preview is refused"
    );
    let rejected = first_result.err().or_else(|| second_result.err()).unwrap();
    assert!(
        rejected.to_string().contains("already running"),
        "{rejected}"
    );
    assert_eq!(fake.max_active_sets.load(Ordering::SeqCst), 1);
    assert_eq!(
        fake.provider_calls_while_set_active.load(Ordering::SeqCst),
        0,
        "the second operation must not enter any provider call during the first mutation"
    );
    assert_eq!(fake.state.lock().await.set_calls, 1);
}

#[tokio::test]
async fn test_delete_binding_refuses_same_hostname_apply_provider_mutation() {
    let Some(test_db) = test_database("apply-cleanup serialization").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-cleanup-race-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");
    fake.delay_set();
    let entered = fake.set_entered.notified();
    tokio::pin!(entered);
    let service = Arc::new(service);
    let applying = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .apply(project_id, actor_id, preview.preview_id, vec![])
                .await
        })
    };
    entered.await;
    let binding_id: i32 = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT id FROM domain_delivery_bindings WHERE hostname = 'app.example.test'"
                .to_string(),
        ))
        .await
        .expect("query in-flight binding")
        .expect("apply reserves binding before provider write")
        .try_get("", "id")
        .expect("binding id");

    let cleanup_error = service
        .delete_binding(project_id, binding_id)
        .await
        .expect_err("cleanup must be refused while apply owns the hostname lock");
    assert!(
        cleanup_error.to_string().contains("already running"),
        "{cleanup_error}"
    );
    applying
        .await
        .expect("apply task joins")
        .expect("apply succeeds while overlapping cleanup is refused");
    assert_eq!(
        fake.provider_calls_while_set_active.load(Ordering::SeqCst),
        0,
        "cleanup must not inspect or remove the provider record during apply's provider write"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings"
        )
        .await,
        1,
        "refused cleanup must leave the completed binding intact"
    );
}
