// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Mutex;

use async_trait::async_trait;
use temps_ai::{AiError, AiRequest, AiService, HarnessUpdateStatus};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<(String, Option<i32>, String)>>,
}

#[async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::AuditResult<()> {
        self.records.lock().unwrap().push((
            operation.operation_type(),
            operation.user_id(),
            operation.serialize()?,
        ));
        Ok(())
    }
}

/// Workspace harness behaviour for one provider: who used it recently, what
/// version each workspace has, and whether an update succeeds.
#[derive(Default)]
struct FakeWorkspaces {
    recent: HashMap<&'static str, Vec<i32>>,
    behind: Vec<i32>,
    busy: Vec<i32>,
    failing: Vec<i32>,
    /// The release lookup fails: an installed version, no latest one.
    lookup_failing: Vec<i32>,
    /// npm exits zero but the CLI on PATH still reports the old version.
    stale_after_update: Vec<i32>,
    updates: Mutex<Vec<(String, i32)>>,
    status_checks: Mutex<usize>,
}

fn status(provider: &str, installed: &str, latest: &str) -> HarnessUpdateStatus {
    HarnessUpdateStatus {
        provider_id: provider.to_string(),
        installed_version: Some(installed.to_string()),
        latest_version: Some(latest.to_string()),
        update_available: temps_ai_agent_cli::harness_update::is_newer(latest, installed),
        updating: false,
        checked_at: None,
        detail: None,
    }
}

#[async_trait]
impl AiService for FakeWorkspaces {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _request: AiRequest) -> Result<temps_ai::AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(
        &self,
        _request: temps_ai::ChatTurnRequest,
    ) -> Result<temps_ai::TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn recent_workspace_principals(&self, provider: &str, _within: Duration) -> Vec<i32> {
        self.recent.get(provider).cloned().unwrap_or_default()
    }

    async fn workspace_harness_update_status(
        &self,
        provider: &str,
        principal_id: i32,
        _refresh: bool,
    ) -> Result<HarnessUpdateStatus, AiError> {
        *self.status_checks.lock().unwrap() += 1;
        if self.lookup_failing.contains(&principal_id) {
            return Ok(HarnessUpdateStatus {
                latest_version: None,
                update_available: false,
                detail: Some("npm view timed out".into()),
                ..status(provider, "1.0.0", "1.0.0")
            });
        }
        Ok(if self.behind.contains(&principal_id) {
            status(provider, "1.0.0", "1.1.0")
        } else {
            status(provider, "1.1.0", "1.1.0")
        })
    }

    async fn update_workspace_harness(
        &self,
        provider: &str,
        principal_id: i32,
    ) -> Result<HarnessUpdateStatus, AiError> {
        if self.busy.contains(&principal_id) {
            return Err(AiError::Provider {
                purpose: "provider.harness.update.busy".into(),
                reason: "the persistent workspace is busy with a turn".into(),
            });
        }
        if self.failing.contains(&principal_id) {
            return Err(AiError::Provider {
                purpose: "provider.harness.update".into(),
                reason: "the update exited with 1".into(),
            });
        }
        self.updates
            .lock()
            .unwrap()
            .push((provider.to_string(), principal_id));
        if self.stale_after_update.contains(&principal_id) {
            return Ok(status(provider, "1.0.0", "1.1.0"));
        }
        Ok(status(provider, "1.1.0", "1.1.0"))
    }
}

fn service(
    db: Arc<DatabaseConnection>,
    ai: Arc<FakeWorkspaces>,
    audit: Arc<RecordingAudit>,
    registry: &str,
) -> CodingAgentService {
    CodingAgentService::with_registry(
        db,
        ai,
        audit,
        ProviderHttpClient::unrestricted_for_tests(),
        Url::parse(registry).unwrap(),
    )
}

async fn test_db() -> Option<temps_test_utils::TestDatabase> {
    match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&format!("{error:#}")) => {
            println!("Skipping: no container runtime for the PostgreSQL test database: {error:#}");
            None
        }
        Err(error) => panic!("test database setup failed: {error:#}"),
    }
}

async fn enable_auto_update(service: &CodingAgentService) {
    let (settings, changed) = service
        .update_settings(CodingAgentSettingsUpdate {
            auto_update: Some(true),
        })
        .await
        .unwrap();
    assert!(settings.auto_update);
    assert_eq!(changed, vec!["auto_update"]);
}

// ----------------------------------------------------------------------------
// Registry document
// ----------------------------------------------------------------------------

#[test]
fn the_registry_url_is_valid_and_scopes_are_encoded() {
    let registry = Url::parse(registry::NPM_REGISTRY).unwrap();
    assert_eq!(
        registry::latest_url(&registry, "@anthropic-ai/claude-code")
            .unwrap()
            .as_str(),
        "https://registry.npmjs.org/@anthropic-ai%2fclaude-code/latest"
    );
    assert_eq!(
        registry::latest_url(&registry, "opencode-ai")
            .unwrap()
            .as_str(),
        "https://registry.npmjs.org/opencode-ai/latest"
    );
}

#[test]
fn only_a_release_version_of_the_expected_package_is_accepted() {
    let package = "@openai/codex";
    assert_eq!(
        registry::parse_latest(
            package,
            br#"{"name":"@openai/codex","version":"0.171.0","dist":{}}"#
        )
        .unwrap(),
        "0.171.0"
    );
    for body in [
        br#"{"name":"@openai/other","version":"0.171.0"}"#.as_slice(),
        br#"{"name":"@openai/codex","version":"latest"}"#,
        br#"{"name":"@openai/codex","version":"0.171.0; rm -rf /"}"#,
        br#"{"name":"@openai/codex"}"#,
        b"not json",
    ] {
        let error = registry::parse_latest(package, body).unwrap_err();
        assert!(
            matches!(error, CodingAgentError::RegistryParse { ref package, .. } if package == "@openai/codex"),
            "{error}"
        );
    }
}

#[test]
fn a_cached_release_expires_by_outcome() {
    let now = Utc::now();
    let ok = CachedRelease {
        latest_version: Some("1.0.0".into()),
        error: None,
        checked_at: now - chrono::Duration::hours(5),
    };
    assert!(ok.fresh(now, false));
    assert!(!ok.fresh(now, true), "refresh re-asks after a minute");
    let failed = CachedRelease {
        latest_version: None,
        error: Some("down".into()),
        checked_at: now - chrono::Duration::minutes(20),
    };
    assert!(!failed.fresh(now, false), "failures retry after 15 minutes");
}

#[tokio::test]
async fn the_latest_release_is_read_once_and_a_failure_keeps_the_last_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/opencode-ai/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"name":"opencode-ai","version":"1.19.6"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let service = service(
        Arc::new(DatabaseConnection::Disconnected),
        Arc::new(FakeWorkspaces::default()),
        Arc::new(RecordingAudit::default()),
        &format!("{}/", server.uri()),
    );
    let package = temps_ai_agent_cli::harness_update::harness_package("opencode").unwrap();
    let first = service.release(package, false).await;
    assert_eq!(first.latest_version.as_deref(), Some("1.19.6"));
    assert_eq!(first.error, None);
    // Served from the cache: the mock expects exactly one request.
    assert_eq!(service.release(package, false).await, first);
    server.verify().await;

    // The registry starts failing; an expired entry keeps its last version.
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    service
        .releases
        .lock()
        .await
        .get_mut("opencode")
        .unwrap()
        .checked_at -= chrono::Duration::hours(7);
    let failed = service.release(package, false).await;
    assert_eq!(failed.latest_version.as_deref(), Some("1.19.6"));
    assert!(
        failed
            .error
            .as_deref()
            .unwrap()
            .contains("HTTP 503 for opencode-ai"),
        "{failed:?}"
    );
}

#[tokio::test]
async fn an_oversized_registry_answer_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
            b' ';
            registry::MAX_RELEASE_BYTES
                + 1
        ]))
        .mount(&server)
        .await;
    let error = registry::fetch_latest(
        &ProviderHttpClient::unrestricted_for_tests(),
        &Url::parse(&format!("{}/", server.uri())).unwrap(),
        "opencode-ai",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, CodingAgentError::RegistryTooLarge { .. }),
        "{error}"
    );
}

// ----------------------------------------------------------------------------
// Settings and automatic updates (real database)
// ----------------------------------------------------------------------------

#[tokio::test]
async fn settings_default_off_and_only_real_changes_are_reported() {
    let Some(db) = test_db().await else { return };
    let service = service(
        db.connection_arc(),
        Arc::new(FakeWorkspaces::default()),
        Arc::new(RecordingAudit::default()),
        "https://registry.invalid/",
    );
    let initial = service.settings().await.unwrap();
    assert!(!initial.auto_update);
    assert_eq!(initial.updated_at, None);

    enable_auto_update(&service).await;
    let (again, changed) = service
        .update_settings(CodingAgentSettingsUpdate {
            auto_update: Some(true),
        })
        .await
        .unwrap();
    assert!(again.auto_update);
    assert!(changed.is_empty());
    assert!(service.settings().await.unwrap().updated_at.is_some());
}

#[tokio::test]
async fn nothing_is_updated_while_auto_update_is_off() {
    let Some(db) = test_db().await else { return };
    let ai = Arc::new(FakeWorkspaces {
        recent: HashMap::from([("claude_cli", vec![1])]),
        behind: vec![1],
        ..Default::default()
    });
    let service = service(
        db.connection_arc(),
        ai.clone(),
        Arc::new(RecordingAudit::default()),
        "https://registry.invalid/",
    );
    assert_eq!(
        service.run_auto_update_pass().await.unwrap(),
        AutoUpdatePass::default()
    );
    assert!(ai.updates.lock().unwrap().is_empty());
    assert_eq!(*ai.status_checks.lock().unwrap(), 0);
}

#[tokio::test]
async fn only_workspaces_behind_are_updated_audited_and_not_retried_soon() {
    let Some(db) = test_db().await else { return };
    let ai = Arc::new(FakeWorkspaces {
        recent: HashMap::from([("claude_cli", vec![1, 2, 3, 4]), ("codex_cli", vec![1])]),
        behind: vec![1, 3, 4],
        busy: vec![3],
        failing: vec![4],
        ..Default::default()
    });
    let audit = Arc::new(RecordingAudit::default());
    let service = service(
        db.connection_arc(),
        ai.clone(),
        audit.clone(),
        "https://registry.invalid/",
    );
    enable_auto_update(&service).await;

    let pass = service.run_auto_update_pass().await.unwrap();
    assert_eq!(
        pass,
        AutoUpdatePass {
            checked: 5,
            updated: 2,
            failed: 1,
            busy: 1
        }
    );
    assert_eq!(
        *ai.updates.lock().unwrap(),
        vec![("claude_cli".to_string(), 1), ("codex_cli".to_string(), 1)]
    );
    let records = audit.records.lock().unwrap().clone();
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|(operation, user, _)| { operation == CODING_AGENT_AUTO_UPDATED && user.is_none() }));
    assert!(
        records[0].2.contains(r#""from_version":"1.0.0""#),
        "{}",
        records[0].2
    );
    assert!(
        records[0].2.contains(r#""to_version":"1.1.0""#),
        "{}",
        records[0].2
    );

    // Within the cooldown only the busy workspace is looked at again.
    let second = service.run_auto_update_pass().await.unwrap();
    assert_eq!(second.checked, 1);
    assert_eq!(second.busy, 1);
    assert_eq!(ai.updates.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_failed_lookup_or_an_update_that_did_not_take_is_a_failure_not_a_success() {
    let Some(db) = test_db().await else { return };
    let ai = Arc::new(FakeWorkspaces {
        recent: HashMap::from([("claude_cli", vec![1, 2])]),
        behind: vec![2],
        lookup_failing: vec![1],
        stale_after_update: vec![2],
        ..Default::default()
    });
    let audit = Arc::new(RecordingAudit::default());
    let service = service(
        db.connection_arc(),
        ai.clone(),
        audit.clone(),
        "https://registry.invalid/",
    );
    enable_auto_update(&service).await;

    let pass = service.run_auto_update_pass().await.unwrap();
    assert_eq!(
        pass,
        AutoUpdatePass {
            checked: 2,
            updated: 0,
            failed: 2,
            busy: 0
        }
    );
    // The lookup failure installs nothing; the stale update was attempted
    // but is not audited as a success.
    assert_eq!(
        *ai.updates.lock().unwrap(),
        vec![("claude_cli".to_string(), 2)]
    );
    assert!(audit.records.lock().unwrap().is_empty());
}

#[test]
fn an_update_counts_only_when_the_cli_reports_the_new_version() {
    let before = status("claude_cli", "1.0.0", "1.1.0");
    assert!(update_took_effect(
        &before,
        &status("claude_cli", "1.1.0", "1.1.0")
    ));
    // A release published between the check and the install is still new.
    assert!(update_took_effect(
        &before,
        &status("claude_cli", "1.2.0", "1.2.0")
    ));
    assert!(!update_took_effect(
        &before,
        &status("claude_cli", "1.0.0", "1.1.0")
    ));
    let unreadable = HarnessUpdateStatus {
        installed_version: None,
        ..status("claude_cli", "1.1.0", "1.1.0")
    };
    assert!(!update_took_effect(&before, &unreadable));
}

#[tokio::test]
async fn one_pass_touches_a_bounded_number_of_workspaces() {
    let Some(db) = test_db().await else { return };
    let many: Vec<i32> = (1..=40).collect();
    let ai = Arc::new(FakeWorkspaces {
        recent: HashMap::from([("claude_cli", many.clone())]),
        behind: many,
        ..Default::default()
    });
    let service = service(
        db.connection_arc(),
        ai.clone(),
        Arc::new(RecordingAudit::default()),
        "https://registry.invalid/",
    );
    enable_auto_update(&service).await;
    let pass = service.run_auto_update_pass().await.unwrap();
    assert_eq!(pass.checked, MAX_UPDATES_PER_PASS);
    assert_eq!(ai.updates.lock().unwrap().len(), MAX_UPDATES_PER_PASS);
    // The next pass continues with workspaces not yet attempted.
    let next = service.run_auto_update_pass().await.unwrap();
    assert_eq!(next.updated, MAX_UPDATES_PER_PASS);
    assert_eq!(ai.updates.lock().unwrap()[MAX_UPDATES_PER_PASS].1, 11);
}
