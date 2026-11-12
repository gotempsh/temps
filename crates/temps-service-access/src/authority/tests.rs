// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::{
    tokenless::{BindingTarget, PrepareSession, PreparedBinding},
    types::{AccessMethod, SandboxAccessPolicy},
};
use chrono::Utc;
use sea_orm::{MockDatabase, MockExecResult};
use std::collections::{BTreeMap, HashMap};
use temps_core::{secret_string::SecretString, NoopAuditLogger};
use temps_service_access_runtime::{HostAudit, SessionDelivery, WorkerControl};
use tokenless_core::{
    seal::{open, SealPublicKey, SealSecretKey},
    snapshot::rule_aad,
};

fn encryption() -> Arc<EncryptionService> {
    Arc::new(EncryptionService::new(&"ab".repeat(32)).unwrap())
}
fn store(db: DatabaseConnection) -> AuthorityStore {
    AuthorityStore::new(Arc::new(db), encryption(), Arc::new(NoopAuditLogger))
}
fn identity() -> (WorkerIdentity, SealSecretKey) {
    let secret = SealSecretKey::random_from_rng(rand08::rngs::OsRng);
    (
        WorkerIdentity {
            boot_id: Uuid::new_v4(),
            seal_public_key: SealPublicKey::from(&secret).to_bytes(),
        },
        secret,
    )
}
fn empty_store() -> AuthorityStore {
    store(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
}
fn input(node: Uuid, incarnation: Uuid, origins: &[&str]) -> AuthorizedBinding {
    let now = Utc::now();
    let requests: Vec<_> = origins
        .iter()
        .enumerate()
        .map(|(index, origin)| PrepareSession {
            target: BindingTarget {
                project_id: Some(1),
                sandbox_id: 1,
                incarnation_id: incarnation,
                node_id: node,
            },
            grant_id: format!("grant_{}", Uuid::new_v4().simple()),
            service_id: format!("svc_{}", Uuid::new_v4().simple()),
            service_version: 1,
            policy: if *origin == "https://github.com" {
                crate::github::tests::policy(crate::types::GithubAccessProtocol::Git)
            } else {
                SandboxAccessPolicy {
                    provider: None,
                    github_repository: None,
                    origin: (*origin).into(),
                    path_prefixes: vec!["/v1".into()],
                    methods: vec![AccessMethod::Get],
                    delivery: None,
                }
            },
            expires_at: now + chrono::Duration::seconds(30 + index as i64),
            max_ttl_seconds: 60,
        })
        .collect();
    let credentials = requests
        .iter()
        .map(|request| {
            (
                request.grant_id.clone(),
                SecretString::new("upstream-test-secret"),
            )
        })
        .collect();
    AuthorizedBinding {
        plan: PreparedBinding::prepare(requests, 1, now).unwrap(),
        credentials,
    }
}

#[tokio::test]
async fn rejects_invalid_identities_before_database_access() {
    let store = empty_store();
    let (mut identity, _) = identity();
    assert!(store.register(Some(0), &identity).await.is_err());
    identity.boot_id = Uuid::nil();
    assert!(store.register(None, &identity).await.is_err());
    identity.boot_id = Uuid::new_v4();
    identity.seal_public_key = [0; 32];
    assert!(store.register(None, &identity).await.is_err());
    assert!(store.close(None, &identity).await.is_err());
}

#[tokio::test]
async fn registration_errors_keep_operation_and_boot_context() {
    let (identity, _) = identity();
    let store = store(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("offline".into())])
            .into_connection(),
    );
    let error = store.register(None, &identity).await.unwrap_err();
    assert!(error.to_string().contains("read registration"));
    assert!(error.to_string().contains(&identity.boot_id.to_string()));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn generated_keys_are_encrypted_and_debug_and_audit_exclude_material() {
    let (identity, _) = identity();
    let store = empty_store();
    let row = store.generate(None, &identity).unwrap();
    let snapshot = store.decrypt(row.id, &row.snapshot_key_encrypted).unwrap();
    let session = store.decrypt(row.id, &row.session_key_encrypted).unwrap();
    let ca = store.decrypt(row.id, &row.ca_key_encrypted).unwrap();
    assert_eq!(snapshot.len(), 32);
    assert_eq!(session.len(), 32);
    assert_ne!(snapshot, session);
    assert!(ca.len() > 32);
    let audit = AuditOperation::serialize(&AuthorityAudit {
        boot_id: row.id,
        node_id: None,
        operation: "CREATE_SANDBOX_ACCESS_AUTHORITY",
    })
    .unwrap();
    for value in [
        &row.snapshot_key_encrypted,
        &row.session_key_encrypted,
        &row.ca_key_encrypted,
        &STANDARD.encode(snapshot.as_slice()),
        &STANDARD.encode(session.as_slice()),
        &STANDARD.encode(ca.as_slice()),
    ] {
        assert!(!format!("{row:?}").contains(value));
        assert!(!audit.contains(value));
    }
    assert_eq!(
        store
            .public_authority(&row, None, &identity)
            .unwrap()
            .boot_id,
        identity.boot_id
    );
}

#[test]
fn stored_authority_cannot_change_node_key_boot_or_reopen() {
    let (identity, _) = identity();
    let store = empty_store();
    let mut row = store.generate(None, &identity).unwrap();
    assert!(store.public_authority(&row, Some(1), &identity).is_err());
    let mut wrong = identity.clone();
    wrong.boot_id = Uuid::new_v4();
    assert!(store.public_authority(&row, None, &wrong).is_err());
    wrong = identity.clone();
    wrong.seal_public_key = [7; 32];
    assert!(store.public_authority(&row, None, &wrong).is_err());
    row.closed_at = Some(Utc::now());
    assert!(store.public_authority(&row, None, &identity).is_err());
    row.closed_at = None;
    row.snapshot_key_encrypted = store.encryption.encrypt(b"bad-length").unwrap();
    assert!(store
        .public_authority(&row, None, &identity)
        .unwrap_err()
        .to_string()
        .contains("32 bytes"));
    row.snapshot_key_encrypted = "corrupt-ciphertext".into();
    assert!(matches!(
        store.public_authority(&row, None, &identity),
        Err(AuthorityError::Encryption { .. })
    ));
}

#[tokio::test]
async fn missing_closed_or_changed_boot_cannot_issue_or_close_another_authority() {
    let (identity, _) = identity();
    let store = store(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<authorities::Model>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection(),
    );
    assert!(store
        .issue(None, &identity, 0, vec![])
        .await
        .unwrap_err()
        .to_string()
        .contains("closed"));
    assert!(store
        .close(None, &identity)
        .await
        .unwrap_err()
        .to_string()
        .contains("identity changed"));
}

#[tokio::test]
async fn signed_policy_seals_credentials_and_preserves_independent_scope_and_deadlines() {
    let (identity, secret) = identity();
    let mut row = empty_store().generate(None, &identity).unwrap();
    row.snapshot_version = 7;
    let snapshot_key = empty_store()
        .signing_key(row.id, &row.snapshot_key_encrypted)
        .unwrap();
    let incarnation = Uuid::new_v4();
    let binding = input(
        row.sdk_node_id,
        incarnation,
        &[
            "https://api.github.com",
            "https://api.example.com",
            "https://github.com",
        ],
    );
    let expected_deadlines: Vec<_> = binding
        .plan
        .grants()
        .iter()
        .map(|grant| grant.claims().exp)
        .collect();
    let store = store(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection(),
    );
    let issued = store
        .issue(None, &identity, 0, vec![binding])
        .await
        .unwrap();
    assert_eq!(issued.snapshot_version(), 7);
    assert!(issued.delivery(Uuid::new_v4()).is_err());
    let delivery = issued.delivery(incarnation).unwrap();
    assert!(delivery.github_git);
    delivery.validate().unwrap();
    let snapshot = delivery.policy.into_snapshot().unwrap();
    assert!(!snapshot.body.contains("upstream-test-secret"));
    assert!(!format!("{issued:?}").contains(&delivery.session));
    let body = snapshot.verify(&snapshot_key.verifying_key()).unwrap();
    assert_eq!(body.rules.len(), 3);
    assert_eq!(body.lease_secs, 30);
    assert_eq!(
        body.unmatched_host_policy,
        tokenless_core::snapshot::UnmatchedHostPolicy::Deny
    );
    assert!(
        body.egress.block_private
            && body.egress.allow.is_empty()
            && body.passthrough_hosts.is_empty()
    );
    for (rule, deadline) in body.rules.iter().zip(expected_deadlines) {
        assert_eq!(rule.not_after, Some(deadline));
        let aad = rule_aad(&body.node_id, &rule.rule_id, &rule.credential_id);
        let headers = open(&secret, &rule.headers, &aad).unwrap();
        let headers: tokenless_core::injection::InjectedHeaders =
            serde_json::from_slice(&headers).unwrap();
        let authorization = headers
            .headers
            .iter()
            .find(|header| header.name == "authorization")
            .unwrap();
        // The worker redacts the credential, in every form these list, from
        // whatever the upstream sends back to the sandbox.
        assert!(!authorization.fixed);
        let secrets: Vec<&str> = headers.secrets.iter().map(|s| s.expose()).collect();
        if rule.allow.git.is_some() {
            let value = authorization.value.strip_prefix("Basic ").unwrap();
            assert_eq!(
                STANDARD.decode(value).unwrap(),
                b"x-access-token:upstream-test-secret"
            );
            assert_eq!(
                secrets,
                [
                    "upstream-test-secret",
                    "x-access-token:upstream-test-secret"
                ]
            );
        } else {
            assert_eq!(authorization.value, "Bearer upstream-test-secret");
            assert_eq!(secrets, ["upstream-test-secret"]);
        }
        let (_, other) = self::identity();
        assert!(open(&other, &rule.headers, &aad).is_err());
    }
    let keys = HashMap::from([(
        body.session_keys[0].kid.clone(),
        body.session_keys[0].decode().unwrap(),
    )]);
    let claims =
        tokenless_core::session::verify_session(&delivery.session, &keys, Utc::now()).unwrap();
    assert_eq!(
        claims.sub.as_deref(),
        Some(format!("{incarnation}/1").as_str())
    );
    assert!(claims.strict);
}

#[tokio::test]
async fn invalid_credential_and_duplicate_incarnation_fail_before_version_reservation() {
    let (identity, _) = identity();
    let store = empty_store();
    let mut maximum = input(Uuid::new_v4(), Uuid::new_v4(), &["https://api.example.com"]);
    *maximum.credentials.values_mut().next().unwrap() = SecretString::new("a".repeat(16384));
    super::issue::validate_bindings(identity.boot_id, &[maximum], Utc::now()).unwrap();
    for value in ["", "line\nbreak", "white space", &"a".repeat(16385)] {
        let mut binding = input(Uuid::new_v4(), Uuid::new_v4(), &["https://api.example.com"]);
        *binding.credentials.values_mut().next().unwrap() = SecretString::new(value);
        assert!(store
            .issue(None, &identity, 0, vec![binding])
            .await
            .is_err());
    }
    let incarnation = Uuid::new_v4();
    let node = Uuid::new_v4();
    assert!(store
        .issue(
            None,
            &identity,
            0,
            vec![
                input(node, incarnation, &["https://api.example.com"]),
                input(node, incarnation, &["https://api.example.com"])
            ]
        )
        .await
        .is_err());
    let mut binding = input(node, incarnation, &["https://api.example.com"]);
    binding.credentials = BTreeMap::new();
    assert!(store
        .issue(None, &identity, 0, vec![binding])
        .await
        .is_err());
}

#[tokio::test]
async fn postgres_registration_races_restart_versions_and_real_worker_binding() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping durable authority test: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    let store = AuthorityStore::new(Arc::clone(&db), encryption(), Arc::new(NoopAuditLogger));
    let worker = WorkerControl::new(Arc::new(HostAudit));
    let identity = worker.identity();
    let (a, b) = tokio::join!(
        store.register(None, &identity),
        store.register(None, &identity)
    );
    let authority = a.unwrap();
    assert_eq!(authority, b.unwrap());
    worker.configure(authority.clone()).await.unwrap();
    let restarted = AuthorityStore::new(Arc::clone(&db), encryption(), Arc::new(NoopAuditLogger));
    assert_eq!(
        authority,
        restarted.register(None, &identity).await.unwrap()
    );
    db.execute_unprepared(
        "UPDATE sandbox_access_authorities SET desired_revision=desired_revision+1",
    )
    .await
    .unwrap();
    assert!(restarted
        .issue(None, &identity, 0, vec![])
        .await
        .unwrap_err()
        .to_string()
        .contains("authorization revision changed"));
    let incarnation = Uuid::new_v4();
    let second_incarnation = Uuid::new_v4();
    let policy = restarted
        .issue(
            None,
            &identity,
            1,
            vec![
                input(
                    authority.node_id,
                    incarnation,
                    &["https://api.github.com", "https://api.example.com"],
                ),
                input(
                    authority.node_id,
                    second_incarnation,
                    &["https://api.example.com"],
                ),
            ],
        )
        .await
        .unwrap();
    let delivery = policy.delivery(incarnation).unwrap();
    assert!(!delivery.github_git);
    let version = worker
        .install(identity.boot_id, &delivery.policy.into_snapshot().unwrap())
        .await
        .unwrap();
    let binding = worker
        .bind(
            identity.boot_id,
            SessionDelivery::binding_spec(incarnation.to_string(), delivery.session, 1),
            version,
        )
        .await
        .unwrap();
    binding.ready(version).await.unwrap();
    let second_delivery = policy.delivery(second_incarnation).unwrap();
    // Both sandboxes receive the same worker snapshot, without renewing it.
    assert_eq!(
        worker
            .install(
                identity.boot_id,
                &second_delivery.policy.into_snapshot().unwrap()
            )
            .await
            .unwrap(),
        version
    );
    let second_binding = worker
        .bind(
            identity.boot_id,
            SessionDelivery::binding_spec(
                second_incarnation.to_string(),
                second_delivery.session,
                1,
            ),
            version,
        )
        .await
        .unwrap();
    second_binding.ready(version).await.unwrap();
    // Concurrent versions never collide, including across control-plane instances.
    let (a, b) = tokio::join!(
        store.reserve(None, &identity, 1),
        restarted.reserve(None, &identity, 1)
    );
    let a = a.unwrap().snapshot_version;
    let b = b.unwrap().snapshot_version;
    assert_ne!(a, b);
    assert!(a > version as i64 && b > version as i64);
    binding
        .revoke_and_wait(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert!(binding.ready(version).await.is_err());
    second_binding.ready(version).await.unwrap();
    second_binding
        .revoke_and_wait(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    drop(second_binding);
    store.close(None, &identity).await.unwrap();
    assert!(restarted.register(None, &identity).await.is_err());
    assert!(restarted.issue(None, &identity, 0, vec![]).await.is_err());
    // New worker boot has independent authority; old consent is not inherited.
    let next = WorkerControl::new(Arc::new(HostAudit));
    let fresh = restarted.register(None, &next.identity()).await.unwrap();
    assert_ne!(fresh.node_id, authority.node_id);
    assert_ne!(fresh.snapshot_verify_key, authority.snapshot_verify_key);
    assert!(next.configure(authority).await.is_err());
    drop(binding);
    drop(worker);
    drop(next);
    drop(store);
    drop(restarted);
    drop(db);
    test.cleanup().await;
}

#[tokio::test]
async fn issuance_rejects_wrong_node_expiry_duplicates_and_limits() {
    let (identity, _) = identity();
    let mut row = empty_store().generate(None, &identity).unwrap();
    row.snapshot_version = 1;
    let node = row.sdk_node_id;
    let store = store(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection(),
    );
    let wrong = input(Uuid::new_v4(), Uuid::new_v4(), &["https://api.example.com"]);
    assert!(store
        .issue(None, &identity, 0, vec![wrong])
        .await
        .unwrap_err()
        .to_string()
        .contains("another Tokenless node"));
    let binding = input(node, Uuid::new_v4(), &["https://api.example.com"]);
    let deadline = binding.plan.claims().exp;
    assert!(issue::validate_bindings(identity.boot_id, &[binding], deadline).is_err());
    let bindings: Vec<_> = (0..129)
        .map(|_| input(node, Uuid::new_v4(), &["https://api.example.com"]))
        .collect();
    assert!(
        issue::validate_bindings(identity.boot_id, &bindings, Utc::now())
            .unwrap_err()
            .to_string()
            .contains("128 bindings")
    );
    let mut binding = input(node, Uuid::new_v4(), &["https://api.example.com"]);
    let credential = binding.credentials.pop_first().unwrap().1;
    binding.credentials.insert("wrong-grant".into(), credential);
    assert!(
        issue::validate_bindings(identity.boot_id, &[binding], Utc::now())
            .unwrap_err()
            .to_string()
            .contains("missing")
    );
}

#[tokio::test]
async fn version_reservation_and_close_database_failures_are_contextual() {
    let (identity, _) = identity();
    let store = store(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("offline".into())])
            .append_exec_errors([sea_orm::DbErr::Custom("offline".into())])
            .into_connection(),
    );
    for error in [
        store.issue(None, &identity, 0, vec![]).await.unwrap_err(),
        store.close(None, &identity).await.unwrap_err(),
    ] {
        assert!(matches!(error, AuthorityError::Database { .. }));
        assert!(error.to_string().contains(&identity.boot_id.to_string()));
        assert!(std::error::Error::source(&error).is_some());
    }
}

struct FailingAudit;
#[async_trait::async_trait]
impl AuditLogger for FailingAudit {
    async fn create_audit_log(&self, _: &dyn AuditOperation) -> temps_core::AuditResult<()> {
        Err(temps_core::AuditError::write(
            "authority",
            std::io::Error::other("audit unavailable"),
        ))
    }
}
#[tokio::test]
async fn registration_audit_failure_does_not_replace_or_discard_winning_keys() {
    let (identity, _) = identity();
    let row = empty_store().generate(None, &identity).unwrap();
    let expected = empty_store()
        .public_authority(&row, None, &identity)
        .unwrap();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<authorities::Model>::new(), vec![row]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let store = AuthorityStore::new(Arc::new(db), encryption(), Arc::new(FailingAudit));
    assert_eq!(store.register(None, &identity).await.unwrap(), expected);
}

#[tokio::test]
async fn oversized_worker_additions_do_not_block_healthy_binding_renewal() {
    let (identity, _) = identity();
    let mut row = empty_store().generate(None, &identity).unwrap();
    row.snapshot_version = 1;
    let key = empty_store()
        .signing_key(row.id, &row.snapshot_key_encrypted)
        .unwrap();
    let healthy = Uuid::new_v4();
    let tail = Uuid::new_v4();
    let mut bindings = vec![input(
        row.sdk_node_id,
        healthy,
        &["https://api.example.com"],
    )];
    let mut additions = Vec::new();
    for _ in 0..16 {
        let id = Uuid::new_v4();
        additions.push(id);
        let mut binding = input(row.sdk_node_id, id, &["https://api.example.com"; 16]);
        for credential in binding.credentials.values_mut() {
            *credential = SecretString::new("x".repeat(16384));
        }
        bindings.push(binding);
    }
    bindings.push(input(row.sdk_node_id, tail, &["https://api.example.com"]));
    let store = store(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection(),
    );
    let issued = store.issue(None, &identity, 0, bindings).await.unwrap();
    let delivery = issued.delivery(healthy).unwrap();
    assert!(issued.delivery(tail).is_ok());
    let admitted = additions
        .iter()
        .filter(|id| issued.delivery(**id).is_ok())
        .count();
    assert!(admitted > 0 && admitted < additions.len());
    let snapshot = delivery.policy.into_snapshot().unwrap();
    assert!(snapshot.body.len() <= temps_service_access_runtime::MAX_SNAPSHOT_BYTES);
    let body = snapshot.verify(&key.verifying_key()).unwrap();
    assert_eq!(
        body.rules.len(),
        2 + admitted * 16,
        "bindings are admitted atomically"
    );
}

#[tokio::test]
async fn credential_formats_and_fixed_headers_are_sealed_and_stripped_from_the_sandbox() {
    use crate::types::{CredentialDelivery, CredentialFormat, RequestHeader};
    let (identity, secret) = identity();
    let row = empty_store().generate(None, &identity).unwrap();
    let incarnation = Uuid::new_v4();
    let now = Utc::now();
    let header = |name: &str, value: &str| RequestHeader {
        name: name.into(),
        value: value.into(),
    };
    let cases = [
        (
            "https://api-header.example.com",
            CredentialDelivery {
                format: CredentialFormat::Header {
                    name: "x-api-key".into(),
                    prefix: None,
                },
                headers: vec![header("anthropic-version", "2023-06-01")],
            },
            "upstream-test-secret",
        ),
        (
            "https://api-prefix.example.com",
            CredentialDelivery {
                format: CredentialFormat::Header {
                    name: "x-token".into(),
                    prefix: Some("Token ".into()),
                },
                headers: Vec::new(),
            },
            "upstream-test-secret",
        ),
        (
            "https://api-basic.example.com",
            CredentialDelivery {
                format: CredentialFormat::Basic,
                headers: Vec::new(),
            },
            "account:upstream-test-secret",
        ),
        (
            "https://api-bearer.example.com",
            CredentialDelivery {
                format: CredentialFormat::Bearer,
                headers: vec![header("notion-version", "2022-06-28")],
            },
            "upstream-test-secret",
        ),
    ];
    let requests: Vec<_> = cases
        .iter()
        .enumerate()
        .map(|(index, (origin, delivery, _))| PrepareSession {
            target: BindingTarget {
                project_id: Some(1),
                sandbox_id: 1,
                incarnation_id: incarnation,
                node_id: row.sdk_node_id,
            },
            grant_id: format!("grant_{}", Uuid::new_v4().simple()),
            service_id: format!("svc_{}", Uuid::new_v4().simple()),
            service_version: 1,
            policy: SandboxAccessPolicy {
                provider: None,
                github_repository: None,
                origin: (*origin).into(),
                path_prefixes: vec!["/v1".into()],
                methods: vec![AccessMethod::Get],
                delivery: Some(delivery.clone()),
            },
            expires_at: now + chrono::Duration::seconds(30 + index as i64),
            max_ttl_seconds: 60,
        })
        .collect();
    let credentials = requests
        .iter()
        .zip(&cases)
        .map(|(request, (_, _, secret))| (request.grant_id.clone(), SecretString::new(*secret)))
        .collect();
    let binding = AuthorizedBinding {
        plan: PreparedBinding::prepare(requests, 1, now).unwrap(),
        credentials,
    };
    let store = store(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row.clone()]])
            .into_connection(),
    );
    let issued = store
        .issue(None, &identity, 0, vec![binding])
        .await
        .unwrap();
    let key = empty_store()
        .signing_key(row.id, &row.snapshot_key_encrypted)
        .unwrap();
    let body = issued
        .delivery(incarnation)
        .unwrap()
        .policy
        .into_snapshot()
        .unwrap()
        .verify(&key.verifying_key())
        .unwrap();
    assert_eq!(body.rules.len(), 4);
    let mut by_host = BTreeMap::new();
    let mut fixed_by_host = BTreeMap::new();
    for rule in &body.rules {
        let aad = rule_aad(&body.node_id, &rule.rule_id, &rule.credential_id);
        let headers: tokenless_core::injection::InjectedHeaders =
            serde_json::from_slice(&open(&secret, &rule.headers, &aad).unwrap()).unwrap();
        let injected: BTreeMap<_, _> = headers
            .headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect();
        // The sandbox may never send a header the broker injects.
        for name in injected.keys() {
            assert!(
                rule.strip_headers.contains(name),
                "{name} must be stripped from sandbox requests"
            );
        }
        assert!(rule.strip_headers.contains(&"authorization".to_string()));
        // Fixed headers are configuration and stay unredacted in responses;
        // the credential header and its raw secret are redacted.
        let fixed: BTreeMap<_, _> = headers
            .headers
            .iter()
            .map(|header| (header.name.clone(), header.fixed))
            .collect();
        let secrets: Vec<String> = headers
            .secrets
            .iter()
            .map(|s| s.expose().to_string())
            .collect();
        assert!(
            secrets.iter().any(|s| s == "upstream-test-secret"),
            "{secrets:?}"
        );
        fixed_by_host.insert(rule.allow.hosts[0].to_string(), (fixed, secrets));
        by_host.insert(rule.allow.hosts[0].to_string(), injected);
    }
    let fixed = |host: &str, name: &str| fixed_by_host[host].0[name];
    assert!(fixed("api-header.example.com", "anthropic-version"));
    assert!(!fixed("api-header.example.com", "x-api-key"));
    assert!(!fixed("api-prefix.example.com", "x-token"));
    assert!(!fixed("api-basic.example.com", "authorization"));
    assert!(fixed("api-bearer.example.com", "notion-version"));
    assert!(!fixed("api-bearer.example.com", "authorization"));
    assert_eq!(
        fixed_by_host["api-basic.example.com"].1,
        ["upstream-test-secret", "account:upstream-test-secret"]
    );
    let header_rule = &by_host["api-header.example.com"];
    assert_eq!(header_rule["x-api-key"], "upstream-test-secret");
    assert_eq!(header_rule["anthropic-version"], "2023-06-01");
    assert!(!header_rule.contains_key("authorization"));
    assert_eq!(
        by_host["api-prefix.example.com"]["x-token"],
        "Token upstream-test-secret"
    );
    let basic = by_host["api-basic.example.com"]["authorization"]
        .strip_prefix("Basic ")
        .unwrap();
    assert_eq!(
        STANDARD.decode(basic).unwrap(),
        b"account:upstream-test-secret"
    );
    let bearer_rule = &by_host["api-bearer.example.com"];
    assert_eq!(bearer_rule["authorization"], "Bearer upstream-test-secret");
    assert_eq!(bearer_rule["notion-version"], "2022-06-28");
}
