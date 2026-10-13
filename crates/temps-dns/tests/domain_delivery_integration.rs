// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    DnsProviderService, ManagedDnsRecordService, OwnershipScope, RecordOwnership,
};
use temps_dns::{
    errors::{DeliveryOperation, DeliveryStep},
    providers::{DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType},
    DnsError, OwnershipMarker,
};
use tokio::sync::{Mutex, Notify};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

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
        encryption.clone(),
    ));
    DomainDeliveryService::new(db, managed, encryption)
}

#[derive(Default)]
struct FakeDnsState {
    record: Option<DnsRecord>,
    /// Scope Temps wrote `record` under. `None` once someone else replaced
    /// it, so it reads back as unmanaged, like a record whose ownership
    /// marker no longer matches its content.
    record_scope: Option<OwnershipScope>,
    /// Records Temps did not write (e.g. created by someone else at the
    /// provider), keyed by their own record type.
    foreign: Vec<DnsRecord>,
    set_calls: usize,
    fail_next_set: bool,
    /// `(provider_id, zone)` of every cleanup ownership read and removal.
    cleanup_calls: Vec<(i32, String)>,
}

/// SQL run on the next ownership read, to simulate a change that lands while
/// apply is between its first reads and its writes.
type RaceHook = (Arc<DatabaseConnection>, String);

#[derive(Default)]
struct FakeDns {
    state: Mutex<FakeDnsState>,
    race_on_next_ownership_read: Mutex<Option<RaceHook>>,
    delay_set: AtomicBool,
    active_sets: AtomicUsize,
    max_active_sets: AtomicUsize,
    provider_calls_while_set_active: AtomicUsize,
    ownership_calls: AtomicUsize,
    set_entered: Notify,
}

impl FakeDns {
    async fn fail_next_set(&self) {
        self.state.lock().await.fail_next_set = true;
    }

    fn delay_set(&self) {
        self.delay_set.store(true, Ordering::SeqCst);
    }

    async fn race_on_next_ownership_read(&self, db: Arc<DatabaseConnection>, sql: String) {
        *self.race_on_next_ownership_read.lock().await = Some((db, sql));
    }

    /// Someone else replaces the record at the provider.
    async fn replace_record_externally(&self, content: DnsRecordContent) {
        let mut state = self.state.lock().await;
        let record = state.record.as_mut().expect("a record to replace");
        record.id = Some("external-1".into());
        record.content = content;
        state.record_scope = None;
    }

    fn marker_for(record: &DnsRecord, scope: OwnershipScope) -> OwnershipMarker {
        OwnershipMarker::new_signed(
            &[7; 32],
            "fake-instance",
            &record.zone,
            &record.name,
            record.content.record_type(),
            "fake-fingerprint",
            scope.project_id,
            scope.environment_id,
            scope.controller,
        )
        .expect("fake ownership marker")
    }
}

#[async_trait]
impl DomainDeliveryDns for FakeDns {
    async fn record_ownership(
        &self,
        _domain: &str,
        _name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError> {
        self.ownership_calls.fetch_add(1, Ordering::SeqCst);
        if self.active_sets.load(Ordering::SeqCst) > 0 {
            self.provider_calls_while_set_active
                .fetch_add(1, Ordering::SeqCst);
        }
        let race = self.race_on_next_ownership_read.lock().await.take();
        if let Some((db, sql)) = race {
            db.execute_unprepared(&sql)
                .await
                .expect("simulate a concurrent change");
        }
        let state = self.state.lock().await;
        let own = state
            .record
            .iter()
            .map(|record| (record, state.record_scope));
        let foreign = state.foreign.iter().map(|record| (record, None));
        let live = own
            .chain(foreign)
            .find(|(record, _)| record.content.record_type() == record_type);
        Ok(match live {
            Some((record, Some(scope))) => {
                RecordOwnership::Owned(record.clone(), Self::marker_for(record, scope))
            }
            Some((record, None)) => RecordOwnership::Unmanaged(record.clone()),
            None => RecordOwnership::NotFound,
        })
    }

    async fn record_ownership_for_provider(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError> {
        self.state
            .lock()
            .await
            .cleanup_calls
            .push((provider_id, zone.to_string()));
        self.record_ownership(zone, name, record_type).await
    }

    async fn import_record(
        &self,
        _domain: &str,
        _name: &str,
        _record_type: DnsRecordType,
        _scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        Ok(())
    }

    async fn set_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
        proxied: Option<bool>,
        scope: OwnershipScope,
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
        state.record_scope = Some(scope);
        self.active_sets.fetch_sub(1, Ordering::SeqCst);
        Ok(record)
    }

    async fn remove_record(
        &self,
        provider_id: i32,
        zone: &str,
        _name: &str,
        _record_type: DnsRecordType,
        _scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        if self.active_sets.load(Ordering::SeqCst) > 0 {
            self.provider_calls_while_set_active
                .fetch_add(1, Ordering::SeqCst);
        }
        let mut state = self.state.lock().await;
        state.cleanup_calls.push((provider_id, zone.to_string()));
        state.record = None;
        state.record_scope = None;
        Ok(())
    }
}

fn foreign_record(content: DnsRecordContent) -> DnsRecord {
    DnsRecord {
        id: Some("foreign-1".into()),
        zone: "example.test".into(),
        name: "app".into(),
        fqdn: "app.example.test".into(),
        content,
        ttl: 300,
        proxied: false,
        metadata: HashMap::new(),
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
    delivery_fixture_with_profile(db, slug, None).await
}

/// Like [`delivery_fixture`], but when `bunny` is set the project delivers
/// through a Bunny profile whose API calls go to that double.
async fn delivery_fixture_with_profile(
    db: Arc<DatabaseConnection>,
    slug: &str,
    bunny: Option<&MockServer>,
) -> (i32, i32, i32, i32, Arc<FakeDns>, DomainDeliveryService) {
    let project_id = insert_project(db.as_ref(), slug).await;
    let environment_id = insert_environment(db.as_ref(), project_id, "production").await;
    let actor_id = insert_user(db.as_ref(), &format!("{slug}@example.test")).await;
    let provider_id = insert_managed_provider(db.as_ref(), "example.test").await;
    let fake = Arc::new(FakeDns::default());
    let mut service = DomainDeliveryService::with_dns(
        db,
        fake.clone(),
        Arc::new(temps_core::EncryptionService::new_from_password(
            "delivery-test",
        )),
    );
    let profile = match bunny {
        Some(server) => {
            service = service.with_bunny_api_base_url(server.uri());
            service
                .create_profile_with_bunny(
                    "Bunny delivery".into(),
                    DeliveryProviderKind::Bunny,
                    Some(BUNNY_PULL_ZONE_ID),
                    Some("test-bunny-key".into()),
                )
                .await
                .expect("create Bunny delivery profile")
        }
        None => service
            .create_profile("Direct delivery".into(), DeliveryProviderKind::Direct)
            .await
            .expect("create delivery profile"),
    };
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

const BUNNY_PULL_ZONE_ID: i64 = 42;

/// A Bunny API double with a valid Pull Zone whose origin is the Temps edge
/// target used by [`bunny_preview_request`], accepting new hostnames.
async fn bunny_api_double() -> MockServer {
    let server = MockServer::start().await;
    mount_bunny_zone(&server, &[]).await;
    Mock::given(method("POST"))
        .and(path(format!("/pullzone/{BUNNY_PULL_ZONE_ID}/addHostname")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    server
}

/// Serve the Pull Zone with `attached` custom hostnames (none certified).
async fn mount_bunny_zone(server: &MockServer, attached: &[&str]) {
    let mut hostnames = vec![serde_json::json!({
        "Value": "temps-edge.b-cdn.net",
        "IsSystemHostname": true,
        "HasCertificate": false
    })];
    hostnames.extend(attached.iter().map(|hostname| {
        serde_json::json!({
            "Value": hostname,
            "IsSystemHostname": false,
            "HasCertificate": false
        })
    }));
    Mock::given(method("GET"))
        .and(path(format!("/pullzone/{BUNNY_PULL_ZONE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": BUNNY_PULL_ZONE_ID,
            "Name": "temps-edge",
            "OriginUrl": "https://edge.example.net",
            "Enabled": true,
            "Suspended": false,
            "AddHostHeader": true,
            "Hostnames": hostnames
        })))
        .mount(server)
        .await;
}

/// Make Bunny's certificate request fail `failures` times, then succeed.
async fn mount_certificate_requests(server: &MockServer, failures: u64) {
    if failures > 0 {
        Mock::given(method("GET"))
            .and(path("/pullzone/loadFreeCertificate"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(failures)
            .with_priority(1)
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/pullzone/loadFreeCertificate"))
        .respond_with(ResponseTemplate::new(200))
        .with_priority(2)
        .mount(server)
        .await;
}

fn bunny_preview_request(
    environment_id: i32,
    provider_id: i32,
) -> PreviewDomainDeliveryBindingRequest {
    PreviewDomainDeliveryBindingRequest {
        origin_target: "edge.example.net".into(),
        ..preview_request(environment_id, provider_id)
    }
}

async fn insert_custom_domain(
    db: &DatabaseConnection,
    project_id: i32,
    environment_id: i32,
    hostname: &str,
) -> i32 {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO project_custom_domains (project_id, environment_id, domain, status, created_at, updated_at) VALUES ($1, $2, $3, 'pending', now(), now()) RETURNING id",
        [project_id.into(), environment_id.into(), hostname.into()],
    ))
    .await
    .expect("insert custom domain")
    .expect("custom domain row")
    .try_get("", "id")
    .expect("custom domain id")
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

    for index in [
        "idx_domain_delivery_bindings_environment",
        "idx_domain_delivery_bindings_custom_domain",
        "idx_domain_delivery_bindings_dns_provider",
        "idx_domain_delivery_bindings_profile",
        "idx_domain_delivery_previews_expires_at",
    ] {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT to_regclass($1)::text AS index_name",
                [index.into()],
            ))
            .await
            .expect("inspect migrated index")
            .expect("inspection row");
        let actual: Option<String> = row.try_get("", "index_name").expect("index name");
        assert_eq!(
            actual.as_deref(),
            Some(index),
            "migration must create {index}"
        );
    }

    db.execute_unprepared(
        "INSERT INTO delivery_profiles (name, provider_kind) VALUES ('direct', 'direct')",
    )
    .await
    .expect("valid provider kind");
    db.execute_unprepared(
        "INSERT INTO delivery_profiles (name, provider_kind, bunny_pull_zone_id, bunny_hostname, bunny_api_key_encrypted) \
         VALUES ('bunny', 'bunny', 42, 'test.b-cdn.net', 'encrypted')",
    )
    .await
    .expect("complete Bunny profile is valid");
    assert!(
        db.execute_unprepared(
            "INSERT INTO delivery_profiles (name, provider_kind, bunny_pull_zone_id) \
             VALUES ('incomplete-bunny', 'bunny', 42)"
        )
        .await
        .is_err(),
        "Bunny profiles require a hostname and encrypted key"
    );
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
        service
            .list_profiles(temps_core::PaginationParams::default(), None)
            .await
            .expect("list profiles")
            .total,
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
    assert_eq!(
        service
            .list_profiles(temps_core::PaginationParams::default(), None)
            .await
            .expect("list after delete")
            .total,
        0
    );
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
    let missing_profile = service
        .update_settings(project_id, Some(i32::MAX), vec![])
        .await
        .expect_err("missing profile");
    assert!(
        matches!(
            missing_profile,
            DnsError::DeliveryProfileNotFound {
                profile_id: i32::MAX
            }
        ),
        "{missing_profile}"
    );
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

#[tokio::test]
async fn test_apply_refuses_other_record_type_created_after_preview() {
    let Some(test_db) = test_database("apply cross-type recheck").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-cross-type-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview with no records at the name");
    assert_eq!(preview.record.record_type, DnsRecordType::A);

    // Someone creates a CNAME at the same name between preview and apply.
    fake.state
        .lock()
        .await
        .foreign
        .push(foreign_record(DnsRecordContent::CNAME {
            target: "elsewhere.example.net".into(),
        }));

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("apply must re-check other routing record types");
    assert!(
        matches!(error, DnsError::RecordConflict { ref record_type, .. } if record_type == "CNAME"),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings"
        )
        .await,
        0,
        "a refused apply must not reserve a binding"
    );
}

#[tokio::test]
async fn test_apply_refuses_when_live_record_differs_from_preview() {
    let Some(test_db) = test_database("apply expected-record recheck").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-stale-record-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview with no record");
    assert!(preview.record.expected_existing_record.is_none());

    // An A record appears after preview; adopting or overwriting it would
    // act on something the user never reviewed.
    fake.state
        .lock()
        .await
        .foreign
        .push(foreign_record(DnsRecordContent::A {
            address: "203.0.113.9".into(),
        }));

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("apply must refuse a record that changed since preview");
    assert!(
        matches!(error, DnsError::RecordConflict { ref reason, .. } if reason.contains("changed after delivery preview")),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn test_apply_takes_provider_record_lock_before_rechecking_live_state() {
    let Some(test_db) = test_database("apply record lock ordering").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-record-lock-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");

    // Another DNS writer (managed records API, import, hostname sync) holds
    // the provider-record lock for this name on its own connection.
    let holder = sea_orm::TransactionTrait::begin(db.as_ref())
        .await
        .expect("begin lock holder");
    holder
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext('managed-dns:example.test:app'))".to_string(),
        ))
        .await
        .expect("hold record lock");
    let ownership_calls_before = fake.ownership_calls.load(Ordering::SeqCst);

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("apply must wait for the record lock before trusting live state");
    assert!(
        error
            .to_string()
            .to_ascii_lowercase()
            .contains("another dns operation"),
        "{error}"
    );
    assert_eq!(
        fake.ownership_calls.load(Ordering::SeqCst),
        ownership_calls_before,
        "live state must not be read before the record lock is held"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);

    holder.rollback().await.expect("release record lock");
    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("apply succeeds once the record lock is free");
    assert_eq!(binding.status, "dns_configured");
}

#[tokio::test]
async fn test_apply_binds_the_existing_route_it_locked() {
    let Some(test_db) = test_database("apply reuses existing route").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, _fake, service) =
        delivery_fixture(db.clone(), "delivery-route-reuse-project").await;
    let route_id =
        insert_custom_domain(db.as_ref(), project_id, environment_id, "app.example.test").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview reusing the existing route");
    assert!(!preview.routing.will_create_custom_domain);
    assert_eq!(preview.routing.custom_domain_id, Some(route_id));

    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("apply onto the unchanged route");
    assert_eq!(binding.custom_domain_id, route_id);
    assert_eq!(binding.status, "dns_configured");
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM project_custom_domains WHERE domain = 'app.example.test'",
        )
        .await,
        1,
        "the existing route is reused, not duplicated"
    );
}

#[tokio::test]
async fn test_apply_refuses_route_renamed_after_apply_read_it() {
    let Some(test_db) = test_database("apply route rename race").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-route-rename-project").await;
    let route_id =
        insert_custom_domain(db.as_ref(), project_id, environment_id, "app.example.test").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview reusing the existing route");
    assert_eq!(preview.routing.custom_domain_id, Some(route_id));
    // The rename lands after apply read the route, before it reserves the
    // binding; no binding exists yet to make the rename refuse.
    fake.race_on_next_ownership_read(
        db.clone(),
        format!(
            "UPDATE project_custom_domains SET domain = 'renamed.example.test' WHERE id = {route_id}"
        ),
    )
    .await;

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("apply must not bind a route that was renamed");
    assert!(
        matches!(&error, DnsError::RecordConflict { record_type, reason, .. }
            if record_type == "ROUTE"
                && reason.contains(&format!("custom domain {route_id}"))
                && reason.contains("renamed.example.test")),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0,
        "no binding may describe the renamed route or its old hostname"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM project_custom_domains WHERE domain = 'app.example.test'",
        )
        .await,
        0,
        "the refused apply must not create a replacement route"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            &format!(
                "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'failed' AND last_error LIKE '%custom domain {route_id}%'"
            ),
        )
        .await,
        1
    );
}

#[tokio::test]
async fn test_apply_refuses_route_moved_to_another_environment_after_apply_read_it() {
    let Some(test_db) = test_database("apply route reassignment race").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-route-move-project").await;
    let staging_id = insert_environment(db.as_ref(), project_id, "staging").await;
    let route_id =
        insert_custom_domain(db.as_ref(), project_id, environment_id, "app.example.test").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview reusing the existing route");
    fake.race_on_next_ownership_read(
        db.clone(),
        format!(
            "UPDATE project_custom_domains SET environment_id = {staging_id} WHERE id = {route_id}"
        ),
    )
    .await;

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("apply must not bind a route moved to another environment");
    assert!(
        matches!(&error, DnsError::RecordConflict { record_type, reason, .. }
            if record_type == "ROUTE" && reason.contains(&format!("environment {staging_id}"))),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'failed'",
        )
        .await,
        1
    );
}

#[tokio::test]
async fn test_bunny_failure_after_dns_write_reports_progress_and_same_preview_resumes() {
    let Some(test_db) = test_database("partial Bunny apply resume").await else {
        return;
    };
    let db = test_db.connection_arc();
    let bunny = bunny_api_double().await;
    mount_certificate_requests(&bunny, 1).await;
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture_with_profile(db.clone(), "delivery-bunny-resume-project", Some(&bunny))
            .await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            bunny_preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview Bunny delivery");
    assert_eq!(preview.record.record_type, DnsRecordType::CNAME);

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("a failed certificate request must fail apply");
    let DnsError::DeliveryIncomplete(incomplete) = &error else {
        panic!("expected DeliveryIncomplete, got {error}");
    };
    assert_eq!(incomplete.operation, DeliveryOperation::Apply);
    assert_eq!(incomplete.project_id, project_id);
    assert_eq!(incomplete.environment_id, environment_id);
    assert_eq!(incomplete.hostname, "app.example.test");
    assert_eq!(incomplete.preview_id, Some(preview.preview_id));
    assert!(incomplete.binding_id.is_some());
    assert_eq!(incomplete.failed_step, DeliveryStep::CertificateRequested);
    assert_eq!(
        incomplete.completed_steps,
        vec![
            DeliveryStep::CustomDomainCreated,
            DeliveryStep::BindingReserved,
            DeliveryStep::BunnyHostnameAttached,
            DeliveryStep::DnsRecordWritten,
            DeliveryStep::DnsRecordVerified,
        ]
    );
    assert!(
        matches!(incomplete.source, DnsError::ApiError(_)),
        "{error}"
    );
    assert!(error.to_string().contains("dns_record_written"), "{error}");
    assert!(
        fake.state.lock().await.record.is_some(),
        "the DNS write is kept"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings WHERE status = 'failed' AND last_error LIKE '%certificate_requested%'",
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'failed' AND plan -> 'dns_write_receipt' IS NOT NULL",
        )
        .await,
        1,
        "the preview keeps a receipt of the DNS write"
    );

    // The same preview resumes: Temps' own record is not a change after
    // preview, so no new preview is needed.
    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("retrying the same preview resumes from its own DNS write");
    assert_eq!(binding.status, "dns_configured");
    assert!(binding.last_error.is_none());
    assert!(binding.applied_at.is_some());
    assert_eq!(
        fake.state.lock().await.set_calls,
        2,
        "the DNS write is repeated idempotently"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'applied'",
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM project_custom_domains WHERE domain = 'app.example.test'",
        )
        .await,
        1,
        "the resumed apply reuses the route the first attempt created"
    );
}

#[tokio::test]
async fn test_retry_refuses_record_changed_after_the_failed_attempt_wrote_it() {
    let Some(test_db) = test_database("partial apply retry after external change").await else {
        return;
    };
    let db = test_db.connection_arc();
    let bunny = bunny_api_double().await;
    mount_certificate_requests(&bunny, 1).await;
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture_with_profile(db.clone(), "delivery-bunny-tamper-project", Some(&bunny))
            .await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            bunny_preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview Bunny delivery");
    service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("the first attempt fails after writing DNS");

    // Someone else repoints the record at the provider.
    fake.replace_record_externally(DnsRecordContent::CNAME {
        target: "elsewhere.example.net".into(),
    })
    .await;
    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("a record changed by someone else must not be overwritten");
    assert!(
        matches!(&error, DnsError::RecordConflict { reason, .. }
            if reason.contains("changed after delivery preview")),
        "{error}"
    );

    // Even under this binding's own ownership scope, a value that differs
    // from the receipt is not resumed.
    {
        let mut state = fake.state.lock().await;
        state.record_scope = Some(OwnershipScope {
            project_id: Some(project_id),
            environment_id: Some(environment_id),
            controller: Some("domain-delivery"),
        });
    }
    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("a value other than the receipt must not be resumed");
    assert!(
        matches!(&error, DnsError::RecordConflict { reason, .. }
            if reason.contains("changed after delivery preview")),
        "{error}"
    );
    assert_eq!(
        fake.state.lock().await.set_calls,
        1,
        "refused retries never write"
    );
}

#[tokio::test]
async fn test_cleanup_reports_dns_removed_when_bunny_detach_fails_and_retry_finishes() {
    let Some(test_db) = test_database("partial Bunny cleanup").await else {
        return;
    };
    let db = test_db.connection_arc();
    let bunny = bunny_api_double().await;
    mount_certificate_requests(&bunny, 0).await;
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture_with_profile(db.clone(), "delivery-bunny-cleanup-project", Some(&bunny))
            .await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            bunny_preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview Bunny delivery");
    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("apply Bunny delivery");

    // The Pull Zone now lists the hostname, and its first detach fails.
    bunny.reset().await;
    mount_bunny_zone(&bunny, &["app.example.test"]).await;
    let remove_path = format!("/pullzone/{BUNNY_PULL_ZONE_ID}/removeHostname");
    Mock::given(method("DELETE"))
        .and(path(remove_path.clone()))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&bunny)
        .await;
    Mock::given(method("DELETE"))
        .and(path(remove_path))
        .respond_with(ResponseTemplate::new(204))
        .with_priority(2)
        .mount(&bunny)
        .await;

    let error = service
        .delete_binding(project_id, binding.id)
        .await
        .expect_err("a failed Bunny detach must fail cleanup");
    let DnsError::DeliveryIncomplete(incomplete) = &error else {
        panic!("expected DeliveryIncomplete, got {error}");
    };
    assert_eq!(incomplete.operation, DeliveryOperation::Cleanup);
    assert_eq!(incomplete.binding_id, Some(binding.id));
    assert_eq!(incomplete.preview_id, None);
    assert_eq!(
        incomplete.completed_steps,
        vec![DeliveryStep::DnsRecordRemoved]
    );
    assert_eq!(incomplete.failed_step, DeliveryStep::BunnyHostnameRemoved);
    assert!(
        fake.state.lock().await.record.is_none(),
        "DNS was removed before the Bunny detach failed"
    );
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings WHERE status = 'cleanup_failed' AND last_error LIKE '%bunny_hostname_removed%'",
        )
        .await,
        1
    );

    let deleted = service
        .delete_binding(project_id, binding.id)
        .await
        .expect("retrying cleanup finishes it");
    assert_eq!(deleted.id, binding.id);
    assert_eq!(deleted.hostname, "app.example.test");
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0
    );
}

#[tokio::test]
async fn test_delete_binding_uses_its_own_provider_after_zone_stops_being_managed() {
    let Some(test_db) = test_database("cleanup after zone unverified").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-unverified-cleanup-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");
    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("apply");
    db.execute_unprepared(&format!(
        "UPDATE dns_managed_domains SET verified = false, auto_manage = false WHERE provider_id = {provider_id}"
    ))
    .await
    .expect("zone stops being verified and auto-managed");

    let deleted = service
        .delete_binding(project_id, binding.id)
        .await
        .expect("cleanup of a record Temps wrote must not be stranded");
    assert_eq!(deleted.id, binding.id);
    let state = fake.state.lock().await;
    assert!(state.record.is_none(), "the DNS record was removed");
    assert_eq!(
        state.cleanup_calls,
        vec![
            (provider_id, "example.test".to_string()),
            (provider_id, "example.test".to_string()),
        ],
        "ownership read and removal both go through the binding's own provider and zone"
    );
    drop(state);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0
    );
}

#[tokio::test]
async fn test_cleanup_zone_lookup_ignores_verification_and_auto_manage_but_needs_the_provider() {
    let Some(test_db) = test_database("cleanup zone lookup").await else {
        return;
    };
    let db = test_db.connection_arc();
    let encryption = Arc::new(
        temps_core::EncryptionService::new("0123456789abcdef0123456789abcdef")
            .expect("valid test encryption key"),
    );
    let providers = DnsProviderService::new(db.clone(), encryption);
    let provider_id = insert_managed_provider(db.as_ref(), "example.test").await;
    let other_provider_id = insert_managed_provider(db.as_ref(), "other.test").await;
    db.execute_unprepared(&format!(
        "UPDATE dns_managed_domains SET verified = false, auto_manage = false WHERE provider_id = {provider_id}"
    ))
    .await
    .expect("zone stops being verified and auto-managed");

    assert!(
        providers
            .find_provider_for_domain("example.test")
            .await
            .expect("apply-side lookup")
            .is_none(),
        "apply-side lookups no longer resolve the zone"
    );
    let (provider, managed) = providers
        .find_managed_zone_for_delivery_cleanup(provider_id, " Example.TEST. ")
        .await
        .expect("cleanup resolves the binding's own provider and zone");
    assert_eq!(provider.id, provider_id);
    assert_eq!(managed.provider_id, provider_id);
    assert_eq!(managed.domain, "example.test");
    assert!(!managed.verified && !managed.auto_manage);

    let error = providers
        .find_managed_zone_for_delivery_cleanup(other_provider_id, "example.test")
        .await
        .expect_err("the zone must belong to the binding's provider");
    assert!(
        matches!(&error, DnsError::DomainNotManaged(message)
            if message.contains(&format!("DNS provider {other_provider_id}"))),
        "{error}"
    );
    let error = providers
        .find_managed_zone_for_delivery_cleanup(i32::MAX, "example.test")
        .await
        .expect_err("the provider row must exist");
    assert!(
        matches!(error, DnsError::ProviderNotFound(id) if id == i32::MAX),
        "{error}"
    );
}

#[tokio::test]
async fn test_managed_cleanup_reaches_the_bindings_provider_after_zone_is_unverified() {
    let Some(test_db) = test_database("managed cleanup provider resolution").await else {
        return;
    };
    let db = test_db.connection_arc();
    let encryption = Arc::new(
        temps_core::EncryptionService::new("0123456789abcdef0123456789abcdef")
            .expect("valid test encryption key"),
    );
    let credentials = encryption
        .encrypt_string("{}")
        .expect("encrypt provider credentials");
    let provider_id: i32 = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO dns_providers (name, provider_type, credentials, is_active, created_at, updated_at) VALUES ('cleanup-provider', 'manual', $1, true, now(), now()) RETURNING id",
            [credentials.into()],
        ))
        .await
        .expect("insert DNS provider")
        .expect("provider row")
        .try_get("", "id")
        .expect("provider id");
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO dns_managed_domains (provider_id, domain, auto_manage, proxied_by_default, verified, generated_hostname_mode, sync_generated_records, created_at, updated_at) VALUES ($1, 'example.test', false, false, false, 'standard', false, now(), now())",
        [provider_id.into()],
    ))
    .await
    .expect("insert unverified, non-automated managed domain");
    let providers = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
    let managed = ManagedDnsRecordService::new(db.clone(), providers, encryption);

    // The zone-based lookup no longer resolves the zone at all...
    let error = managed
        .record_ownership("example.test", "app", DnsRecordType::A)
        .await
        .expect_err("unverified zone");
    assert!(matches!(error, DnsError::DomainNotManaged(_)), "{error}");

    // ...while cleanup resolves the binding's own provider and zone and gets
    // as far as the provider. The manual provider cannot read records, so
    // its refusal is what proves the lookup no longer depends on the flags.
    let error = managed
        .record_ownership_for_provider(provider_id, "example.test", "app", DnsRecordType::A)
        .await
        .expect_err("the manual provider cannot read records");
    assert!(matches!(error, DnsError::NotSupported(_)), "{error}");
    let error = managed
        .remove_managed_record_for_provider(
            provider_id,
            "example.test",
            "app",
            DnsRecordType::A,
            OwnershipScope::for_controller("domain-delivery"),
        )
        .await
        .expect_err("the manual provider cannot remove records");
    assert!(matches!(error, DnsError::NotSupported(_)), "{error}");
}

#[tokio::test]
async fn test_apply_refuses_zone_removed_after_apply_checked_it() {
    let Some(test_db) = test_database("apply zone removal race").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-zone-removal-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");
    // The zone is removed after apply validated it, before it reserves the
    // binding; no binding exists yet to make the removal refuse.
    fake.race_on_next_ownership_read(
        db.clone(),
        format!("DELETE FROM dns_managed_domains WHERE provider_id = {provider_id}"),
    )
    .await;

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("apply must not reserve a binding for a removed zone");
    assert!(
        matches!(&error, DnsError::DeliveryZoneUnavailable { provider_id: id, zone, reason, .. }
            if *id == provider_id && zone == "example.test" && reason.contains("removed")),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);
    for table in ["domain_delivery_bindings", "project_custom_domains"] {
        assert_eq!(
            scalar_i64(
                db.as_ref(),
                &format!("SELECT count(*) AS count FROM {table}"),
            )
            .await,
            0,
            "the refused reservation must roll back entirely ({table})"
        );
    }
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_previews WHERE status = 'failed'",
        )
        .await,
        1
    );
}

#[tokio::test]
async fn test_apply_refuses_provider_deactivated_after_apply_checked_it() {
    let Some(test_db) = test_database("apply provider deactivation race").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-provider-off-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");
    fake.race_on_next_ownership_read(
        db.clone(),
        format!("UPDATE dns_providers SET is_active = false WHERE id = {provider_id}"),
    )
    .await;

    let error = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("apply must not reserve a binding on a deactivated provider");
    assert!(
        matches!(&error, DnsError::DeliveryZoneUnavailable { reason, .. }
            if reason.contains("deactivated")),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0
    );
}

#[tokio::test]
async fn test_binding_reservation_waits_for_a_zone_change_holding_the_zone_row() {
    let Some(test_db) = test_database("reservation waits for zone lock").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-zone-lock-project").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");

    // A zone change (as the managed-domain guards make it) holds the zone row
    // FOR UPDATE while it checks for bindings.
    let holder = sea_orm::TransactionTrait::begin(db.as_ref())
        .await
        .expect("begin zone change");
    let holder_pid: i32 = holder
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT pg_backend_pid() AS pid".to_string(),
        ))
        .await
        .expect("holder backend pid")
        .expect("pid row")
        .try_get("", "pid")
        .expect("pid value");
    holder
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT id FROM dns_managed_domains WHERE provider_id = {provider_id} FOR UPDATE"
            ),
        ))
        .await
        .expect("lock the zone row");

    let service = Arc::new(service);
    let applying = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .apply(project_id, actor_id, preview.preview_id, vec![])
                .await
        })
    };
    // Wait until the reservation is blocked behind the zone change.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let blocked: i64 = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS count FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))",
                [holder_pid.into()],
            ))
            .await
            .expect("inspect lock waits")
            .expect("count row")
            .try_get("", "count")
            .expect("count value");
        if blocked > 0 {
            break;
        }
        assert!(
            !applying.is_finished(),
            "apply finished without waiting for the zone row lock"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "apply never waited for the zone row lock"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    holder
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            format!("UPDATE dns_managed_domains SET auto_manage = false WHERE provider_id = {provider_id}"),
        ))
        .await
        .expect("turn off auto-management");
    holder.commit().await.expect("commit the zone change");

    let error = applying
        .await
        .expect("apply task joins")
        .expect_err("apply must see the zone change it waited for");
    assert!(
        matches!(&error, DnsError::DeliveryZoneUnavailable { reason, .. }
            if reason.contains("no longer auto-managed")),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0);
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0
    );
}

#[tokio::test]
async fn test_profile_search_matches_names_ignoring_case_and_wildcards_literally() {
    let Some(test_db) = test_database("profile search").await else {
        return;
    };
    let db = test_db.connection_arc();
    let service = DomainDeliveryService::with_dns(
        db.clone(),
        Arc::new(FakeDns::default()),
        Arc::new(temps_core::EncryptionService::new_from_password(
            "delivery-test",
        )),
    );
    for name in [
        "Edge EU",
        "edge-us",
        "Origin 100%",
        "under_score",
        "underXscore",
    ] {
        service
            .create_profile(name.into(), DeliveryProviderKind::Direct)
            .await
            .expect("create profile");
    }
    let by_name = temps_core::PaginationParams {
        sort_by: Some("name".into()),
        sort_order: Some("asc".into()),
        ..temps_core::PaginationParams::default()
    };
    let search = |params: temps_core::PaginationParams, term: &'static str| {
        let service = &service;
        async move {
            let page = service
                .list_profiles(params, Some(term))
                .await
                .unwrap_or_else(|error| panic!("search {term:?}: {error}"));
            let names: Vec<String> = page.items.iter().map(|item| item.name.clone()).collect();
            (names, page.total)
        }
    };

    assert_eq!(
        search(by_name.clone(), " EDGE ").await,
        (vec!["Edge EU".to_string(), "edge-us".to_string()], 2),
        "case-insensitive substring, trimmed"
    );
    assert_eq!(
        search(by_name.clone(), "100%").await,
        (vec!["Origin 100%".to_string()], 1),
        "`%` matches only itself"
    );
    assert_eq!(
        search(by_name.clone(), "under_").await,
        (vec!["under_score".to_string()], 1),
        "`_` matches only itself"
    );
    assert_eq!(
        search(by_name.clone(), "  ").await.1,
        5,
        "blank is no filter"
    );
    assert_eq!(
        search(
            temps_core::PaginationParams {
                page: Some(2),
                page_size: Some(1),
                ..by_name.clone()
            },
            "edge",
        )
        .await,
        (vec!["edge-us".to_string()], 2),
        "search combines with paging; total counts matches"
    );
}

/// Start applying `preview_id` while `holder` keeps the row `lock_sql`
/// selects locked `FOR UPDATE`, wait until the apply is blocked behind it,
/// then run `change_sql` on the holder and commit, as project and
/// environment deletion do.
async fn apply_behind_concurrent_change(
    db: &Arc<DatabaseConnection>,
    service: DomainDeliveryService,
    project_id: i32,
    actor_id: i32,
    preview_id: uuid::Uuid,
    lock_sql: String,
    change_sql: String,
) -> Result<temps_dns::services::domain_delivery::DomainDeliveryBindingResponse, DnsError> {
    let holder = sea_orm::TransactionTrait::begin(db.as_ref())
        .await
        .expect("begin the concurrent change");
    let holder_pid: i32 = holder
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT pg_backend_pid() AS pid".to_string(),
        ))
        .await
        .expect("holder backend pid")
        .expect("pid row")
        .try_get("", "pid")
        .expect("pid value");
    holder
        .execute(Statement::from_string(DatabaseBackend::Postgres, lock_sql))
        .await
        .expect("lock the row");

    let service = Arc::new(service);
    let applying = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .apply(project_id, actor_id, preview_id, vec![])
                .await
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let blocked: i64 = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS count FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))",
                [holder_pid.into()],
            ))
            .await
            .expect("inspect lock waits")
            .expect("count row")
            .try_get("", "count")
            .expect("count value");
        if blocked > 0 {
            break;
        }
        assert!(
            !applying.is_finished(),
            "apply finished without waiting for the locked row"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "apply never waited for the locked row"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    holder
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            change_sql,
        ))
        .await
        .expect("apply the concurrent change");
    holder.commit().await.expect("commit the concurrent change");
    applying.await.expect("apply task joins")
}

#[tokio::test]
async fn test_binding_reservation_waits_for_project_deletion_and_refuses_its_fence() {
    let Some(test_db) = test_database("reservation waits for project deletion").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-project-deletion-race").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");

    // Project deletion locks the project row, finds no binding yet, and
    // writes its deletion fence.
    let error = apply_behind_concurrent_change(
        &db,
        service,
        project_id,
        actor_id,
        preview.preview_id,
        format!("SELECT id FROM projects WHERE id = {project_id} FOR UPDATE"),
        format!(
            "UPDATE projects SET is_deleted = true, deleted_at = now() WHERE id = {project_id}"
        ),
    )
    .await
    .expect_err("apply must see the deletion fence it waited for");
    assert!(
        matches!(
            &error,
            DnsError::DeliveryProjectBeingDeleted { project_id: id, hostname }
                if *id == project_id && hostname == "app.example.test"
        ),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0, "no DNS was written");
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0,
        "no binding appears behind the fence"
    );
}

#[tokio::test]
async fn test_binding_reservation_waits_for_environment_deletion_and_refuses_it() {
    let Some(test_db) = test_database("reservation waits for environment deletion").await else {
        return;
    };
    let db = test_db.connection_arc();
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture(db.clone(), "delivery-environment-deletion-race").await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview");

    let error = apply_behind_concurrent_change(
        &db,
        service,
        project_id,
        actor_id,
        preview.preview_id,
        format!("SELECT id FROM environments WHERE id = {environment_id} FOR UPDATE"),
        format!("UPDATE environments SET deleted_at = now() WHERE id = {environment_id}"),
    )
    .await
    .expect_err("apply must see the soft delete it waited for");
    assert!(
        matches!(
            &error,
            DnsError::DeliveryEnvironmentDeleted { project_id: project, environment_id: environment, .. }
                if *project == project_id && *environment == environment_id
        ),
        "{error}"
    );
    assert_eq!(fake.state.lock().await.set_calls, 0, "no DNS was written");
    assert_eq!(
        scalar_i64(
            db.as_ref(),
            "SELECT count(*) AS count FROM domain_delivery_bindings",
        )
        .await,
        0,
        "no binding appears for the deleted environment"
    );
}

/// Accept hostname additions and removals on the double, expecting exactly
/// `adds` and `removes` of them.
async fn mount_hostname_changes(server: &MockServer, adds: u64, removes: u64) {
    Mock::given(method("POST"))
        .and(path(format!("/pullzone/{BUNNY_PULL_ZONE_ID}/addHostname")))
        .respond_with(ResponseTemplate::new(204))
        .expect(adds)
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/pullzone/{BUNNY_PULL_ZONE_ID}/removeHostname"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(removes)
        .mount(server)
        .await;
}

async fn owned_bindings(db: &DatabaseConnection) -> i64 {
    scalar_i64(
        db,
        "SELECT count(*) AS count FROM domain_delivery_bindings WHERE bunny_hostname_owned",
    )
    .await
}

#[tokio::test]
async fn test_bunny_hostname_attached_before_setup_is_reused_and_kept_on_cleanup() {
    let Some(test_db) = test_database("preexisting Bunny hostname").await else {
        return;
    };
    let db = test_db.connection_arc();
    // The user attached the hostname to the Pull Zone before Temps set up
    // delivery for it.
    let bunny = MockServer::start().await;
    mount_bunny_zone(&bunny, &["app.example.test"]).await;
    mount_certificate_requests(&bunny, 0).await;
    mount_hostname_changes(&bunny, 0, 0).await;
    let (project_id, environment_id, actor_id, provider_id, fake, service) =
        delivery_fixture_with_profile(db.clone(), "delivery-bunny-preexisting", Some(&bunny)).await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            bunny_preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview Bunny delivery");

    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("apply reuses the attached hostname");
    assert!(!binding.bunny_hostname_owned, "Temps did not add it");
    assert_eq!(owned_bindings(db.as_ref()).await, 0);

    let deleted = service
        .delete_binding(project_id, binding.id)
        .await
        .expect("cleanup succeeds");
    assert!(!deleted.bunny_hostname_owned);
    assert!(
        fake.state.lock().await.record.is_none(),
        "the DNS record Temps wrote is removed"
    );
    // Dropping the double verifies no hostname was added or removed.
    bunny.verify().await;
}

#[tokio::test]
async fn test_bunny_hostname_added_by_temps_is_owned_and_detached_on_cleanup() {
    let Some(test_db) = test_database("Temps-added Bunny hostname").await else {
        return;
    };
    let db = test_db.connection_arc();
    let bunny = MockServer::start().await;
    mount_bunny_zone(&bunny, &[]).await;
    mount_certificate_requests(&bunny, 0).await;
    mount_hostname_changes(&bunny, 1, 0).await;
    let (project_id, environment_id, actor_id, provider_id, _fake, service) =
        delivery_fixture_with_profile(db.clone(), "delivery-bunny-owned", Some(&bunny)).await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            bunny_preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview Bunny delivery");
    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("apply adds the hostname");
    assert!(binding.bunny_hostname_owned);
    assert_eq!(owned_bindings(db.as_ref()).await, 1);
    bunny.verify().await;

    // The Pull Zone now lists the hostname Temps added; cleanup detaches it.
    bunny.reset().await;
    mount_bunny_zone(&bunny, &["app.example.test"]).await;
    mount_hostname_changes(&bunny, 0, 1).await;
    let deleted = service
        .delete_binding(project_id, binding.id)
        .await
        .expect("cleanup detaches the hostname");
    assert!(deleted.bunny_hostname_owned);
    bunny.verify().await;
}

#[tokio::test]
async fn test_resumed_apply_keeps_ownership_of_the_hostname_its_first_attempt_added() {
    let Some(test_db) = test_database("resumed apply keeps Bunny ownership").await else {
        return;
    };
    let db = test_db.connection_arc();
    let bunny = MockServer::start().await;
    mount_bunny_zone(&bunny, &[]).await;
    mount_certificate_requests(&bunny, 1).await;
    mount_hostname_changes(&bunny, 1, 0).await;
    let (project_id, environment_id, actor_id, provider_id, _fake, service) =
        delivery_fixture_with_profile(db.clone(), "delivery-bunny-owned-resume", Some(&bunny))
            .await;
    let preview = service
        .preview(
            project_id,
            actor_id,
            bunny_preview_request(environment_id, provider_id),
        )
        .await
        .expect("preview Bunny delivery");
    service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect_err("the certificate request fails after the hostname was added");
    assert_eq!(
        owned_bindings(db.as_ref()).await,
        1,
        "the failed attempt already recorded that Temps added the hostname"
    );
    bunny.verify().await;

    // The retry finds the hostname on the Pull Zone because the first
    // attempt put it there, and must still own it.
    bunny.reset().await;
    mount_bunny_zone(&bunny, &["app.example.test"]).await;
    mount_certificate_requests(&bunny, 0).await;
    mount_hostname_changes(&bunny, 0, 1).await;
    let binding = service
        .apply(project_id, actor_id, preview.preview_id, vec![])
        .await
        .expect("the same preview resumes");
    assert!(binding.bunny_hostname_owned);
    service
        .delete_binding(project_id, binding.id)
        .await
        .expect("cleanup detaches the hostname Temps added");
    bunny.verify().await;
}
