// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

struct Resolver {
    stalled: i32,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl crate::github::GithubCredentialResolver for Resolver {
    async fn resolve(&self, id: i32) -> Result<SecretString, crate::github::GithubCredentialError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        if id <= self.stalled {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(SecretString::new("fixture-token"))
    }
}

#[tokio::test(start_paused = true)]
async fn slow_connections_cannot_starve_healthy_github_bindings() {
    // Also cover a healthy lookup queued behind an entire stalled batch.
    for stalled in [1, 4] {
        let resolver = Arc::new(Resolver {
            stalled,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        });
        let mut planner = planner(None);
        planner.github = resolver.clone();
        let boot = Uuid::new_v4();
        let now = Utc::now();
        let authority = temps_entities::sandbox_access_authorities::Model {
            id: boot,
            node_id: None,
            sdk_node_id: Uuid::new_v4(),
            seal_public_key: STANDARD.encode([1u8; 32]),
            snapshot_key_encrypted: String::new(),
            session_key_encrypted: String::new(),
            ca_certificate: String::new(),
            ca_key_encrypted: String::new(),
            snapshot_version: 0,
            desired_revision: 1,
            next_reconcile_at: None,
            closed_at: None,
            created_at: now,
            updated_at: now,
        };
        let mut runtimes = Vec::new();
        let mut candidates = Vec::new();
        for id in 1..=stalled + 1 {
            let runtime = sandbox_access_runtimes::Model {
                id: Uuid::new_v4(),
                project_id: Some(id),
                sandbox_id: id,
                node_id: None,
                provider_identity: format!("container-{id}"),
                authority_id: Some(boot),
                provider_backend: Some("docker".into()),
                provider_name: Some(format!("sandbox-{id}")),
                lease_expires_at: None,
                desired_version: 1,
                applied_version: 0,
                applied_revision: None,
                applied_grant_ids: vec![],
                closed_at: None,
                created_at: now,
                updated_at: now,
            };
            let mut grant = candidate(id, id);
            grant.admin = true;
            grant.runtime_id = runtime.id;
            grant.policy = serde_json::json!({
                "origin": "https://api.github.com", "path_prefixes": ["/repos/example/repo"], "methods": ["GET", "HEAD"],
                "provider": "github", "github_repository": {"connection_id": id, "owner": "example", "repository": "repo", "protocol": "api"}
            });
            // Two grants sharing a connection must perform one credential lookup.
            let mut duplicate = candidate(id, id);
            duplicate.admin = true;
            duplicate.runtime_id = runtime.id;
            duplicate.policy = grant.policy.clone();
            candidates.push(duplicate);
            candidates.push(grant);
            runtimes.push(runtime);
        }
        let healthy = runtimes.last().unwrap().id;
        let started = tokio::time::Instant::now();
        let plan = planner
            .prepare_rows(boot, authority, runtimes, candidates)
            .await
            .unwrap();
        assert_eq!(
            plan.bindings.len(),
            1,
            "healthy GitHub binding must renew despite stalled connections"
        );
        assert_eq!(plan.bindings[0].plan.target().incarnation_id, healthy);
        assert_eq!(plan.bindings[0].plan.grants().len(), 2);
        assert!(plan.retire.is_empty());
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            (stalled + 1) as usize
        );
        assert!(resolver.peak.load(Ordering::SeqCst) <= 4);
        assert_eq!(resolver.active.load(Ordering::SeqCst), 0);
        assert!(started.elapsed() <= Duration::from_millis(3100));
    }
}

/// A Basic password saved with spaces or non-ASCII letters must reach the
/// worker: planning covers every binding on the worker, so refusing it there
/// would also stop renewal for the worker's other sandboxes.
#[tokio::test]
async fn saved_basic_passwords_with_spaces_or_accents_are_planned() {
    let planner = planner(None);
    let boot = Uuid::new_v4();
    let now = Utc::now();
    let authority = temps_entities::sandbox_access_authorities::Model {
        id: boot,
        node_id: None,
        sdk_node_id: Uuid::new_v4(),
        seal_public_key: STANDARD.encode([1u8; 32]),
        snapshot_key_encrypted: String::new(),
        session_key_encrypted: String::new(),
        ca_certificate: String::new(),
        ca_key_encrypted: String::new(),
        snapshot_version: 0,
        desired_revision: 1,
        next_reconcile_at: None,
        closed_at: None,
        created_at: now,
        updated_at: now,
    };
    let runtime = sandbox_access_runtimes::Model {
        id: Uuid::new_v4(),
        project_id: Some(1),
        sandbox_id: 1,
        node_id: None,
        provider_identity: "container-1".into(),
        authority_id: Some(boot),
        provider_backend: Some("docker".into()),
        provider_name: Some("sandbox-1".into()),
        lease_expires_at: None,
        desired_version: 1,
        applied_version: 0,
        applied_revision: None,
        applied_grant_ids: vec![],
        closed_at: None,
        created_at: now,
        updated_at: now,
    };
    let saved = [
        "account:correct horse battery staple",
        "account:contraseña-ñandú",
    ];
    let candidates: Vec<_> = saved
        .iter()
        .map(|secret| {
            let mut grant = candidate(1, 1);
            grant.admin = true;
            grant.runtime_id = runtime.id;
            grant.policy = serde_json::json!({
                "origin": "https://registry.example.com", "path_prefixes": ["/v2"], "methods": ["GET"],
                "delivery": {"format": {"method": "basic"}}
            });
            grant.credential_encrypted = Some(encryption().encrypt(secret.as_bytes()).unwrap());
            grant
        })
        .collect();
    let plan = planner
        .prepare_rows(boot, authority, vec![runtime], candidates)
        .await
        .unwrap();
    assert_eq!(plan.bindings.len(), 1);
    let binding = &plan.bindings[0];
    assert_eq!(binding.plan.grants().len(), saved.len());
    let mut planned: Vec<_> = binding.credentials.values().map(|c| c.expose()).collect();
    planned.sort_unstable();
    let mut expected = saved.to_vec();
    expected.sort_unstable();
    assert_eq!(planned, expected);
}
