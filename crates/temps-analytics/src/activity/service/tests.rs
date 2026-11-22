// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{MockDatabase, MockExecResult};
use temps_ai::{AiError, AiResponse};
use tokio::sync::Notify;

struct FakeAi {
    available: bool,
    fail: bool,
}
#[async_trait::async_trait]
impl AiService for FakeAi {
    async fn chat_stream(
        &self,
        _: temps_ai::ChatTurnRequest,
    ) -> Result<temps_ai::TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }
    async fn is_available(&self) -> bool {
        self.available
    }
    async fn route_metadata(
        &self,
        _: Option<&str>,
        _: Option<i32>,
        _: Option<&str>,
    ) -> Option<temps_ai::AiRouteMetadata> {
        self.available.then(|| temps_ai::AiRouteMetadata {
            provider: "Test Provider".into(),
            model: "test-model".into(),
        })
    }
    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        if self.fail {
            return Err(model_not_found(&request.purpose));
        }
        assert_eq!(request.provider.as_deref(), Some("gateway"));
        assert_eq!(request.project_id, Some(1));
        if request.purpose == "analytics.activity_goals" {
            assert!(request.prompt.contains("Hosting"));
            let system = request.system.as_deref().unwrap_or_default();
            assert!(system.contains("Rank the best recommendation first"));
            assert!(system.contains("fewest missing tracking prerequisites"));
            assert!(system.contains("rationale explain why it is the best fit"));
            assert!(system.contains("not claim identity, purchase probability"));
            return Ok(AiResponse {
                text: String::new(),
                model: "test-model".into(),
                json: Some(
                    serde_json::json!({ "goals": (0..3).map(|i| serde_json::json!({
                "title": format!("Goal {i}"), "goal": "Understand readers of a hosting application", "rationale": "The site describes hosting", "missing_signals": "Confirm setup completion events"
            })).collect::<Vec<_>>() }),
                ),
            });
        }
        if request.purpose == "analytics.activity_setup" {
            return Ok(AiResponse {
                text: String::new(),
                model: "test-model".into(),
                json: Some(serde_json::json!({
                    "categories": [{"name": "Learning", "description": "Reading educational content without observed implementation."}]
                })),
            });
        }
        let data: serde_json::Value = serde_json::from_str(&request.prompt).unwrap();
        assert!(!request.prompt.contains("secret@example.test"));
        assert!(!request.prompt.contains("secret-token"));
        assert!(data["visitors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("visitor_id").is_none()));
        let visitors: Vec<serde_json::Value> = data["visitors"].as_array().unwrap().iter().map(|v| serde_json::json!({
            "visitor_ref": v["visitor_ref"], "categories": ["Learning"],
            "explanation": "Read the installation guide.", "evidence_references": [v["events"][0]["reference"]]
        })).collect();
        Ok(AiResponse {
            text: String::new(),
            model: "test-model".into(),
            json: Some(
                serde_json::json!({"summary":"Readers explored the documentation.", "visitors": visitors}),
            ),
        })
    }
}

fn model_not_found(purpose: &str) -> AiError {
    AiError::ProviderFailed {
        purpose: purpose.to_string(),
        model: "test-model".into(),
        kind: ProviderFailureKind::ModelNotFound,
        status: Some(404),
        retry_after_secs: None,
        reason: "The model `test-model` does not exist".into(),
    }
}

struct BlockingAi {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl AiService for BlockingAi {
    async fn chat_stream(
        &self,
        _: temps_ai::ChatTurnRequest,
    ) -> Result<temps_ai::TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _: AiRequest) -> Result<AiResponse, AiError> {
        self.entered.notify_one();
        self.release.notified().await;
        Err(AiError::NotAvailable)
    }
}
fn settings() -> ActivitySettings {
    ActivitySettings {
        application_context: "An application hosting service".into(),
        share_activity_with_ai: true,
        ..Default::default()
    }
}
fn event(visitor_id: i32) -> EventRow {
    EventRow {
        visitor_id,
        timestamp: Utc::now(),
        path: "/docs/install?token=secret-token".into(),
        title: Some("Install".into()),
        event: "pageview".into(),
        properties: EventProperties::default(),
        session_id: Some(format!("session-{visitor_id}")),
    }
}
fn stored() -> BTreeMap<String, sea_orm::Value> {
    let now = Utc::now();
    BTreeMap::from([
        (
            "settings".into(),
            serde_json::to_value(settings()).unwrap().into(),
        ),
        ("revision".into(), 1_i32.into()),
        ("daily_enabled".into(), false.into()),
        ("next_run_at".into(), now.into()),
        ("last_started_at".into(), Some(now).into()),
        ("locked_until".into(), Option::<DateTime<Utc>>::None.into()),
        (
            "cooldown_until".into(),
            Option::<DateTime<Utc>>::None.into(),
        ),
        ("last_error".into(), Option::<String>::None.into()),
        ("report".into(), Option::<serde_json::Value>::None.into()),
        ("run_history".into(), serde_json::json!([]).into()),
        ("visitor_checkpoints".into(), serde_json::json!({}).into()),
    ])
}
fn project(project_id: i32) -> BTreeMap<String, sea_orm::Value> {
    BTreeMap::from([("name".into(), format!("Sample site {project_id}").into())])
}
fn environment(environment_id: i32) -> BTreeMap<String, sea_orm::Value> {
    BTreeMap::from([("id".into(), environment_id.into())])
}
fn service(db: MockDatabase, available: bool, fail: bool) -> ActivityService {
    ActivityService::new(
        Arc::new(db.into_connection()),
        Arc::new(FakeAi { available, fail }),
    )
}

#[test]
fn validates_context_categories_and_property_bounds() {
    assert!(validate_settings(1, &settings()).is_ok());
    assert!(validate_settings(1, &ActivitySettings::default()).is_err());
    let mut config = settings();
    config.categories.push(config.categories[0].clone());
    assert!(validate_settings(1, &config).is_err());
    config = settings();
    config.categories[0].name = UNKNOWN.into();
    assert!(validate_settings(1, &config).is_err());
    config = settings();
    config.property_keys = vec!["x".into(); 11];
    assert!(validate_settings(1, &config).is_err());
    config = settings();
    config.min_sessions = 0;
    assert!(validate_settings(1, &config).is_err());
    config.min_sessions = 20;
    config.min_page_paths = 20;
    assert!(validate_settings(1, &config).is_ok());
    config.min_page_paths = 21;
    assert!(validate_settings(1, &config).is_err());
    config = settings();
    config.goal_title = Some(" ".into());
    assert!(validate_settings(1, &config).is_err());
    config.goal_title = Some("x".repeat(100));
    assert!(validate_settings(1, &config).is_ok());
    config.goal_title = Some("x".repeat(101));
    assert!(validate_settings(1, &config).is_err());
}

#[test]
fn bounds_visitor_and_event_samples_and_strips_querystrings() {
    let (visitors, sampled) = prepare_visitors((1..=100).map(event).collect());
    assert!(sampled);
    assert_eq!(visitors.len(), MAX_VISITORS);
    assert_eq!(visitors[0].events[0].path, "/docs/install");
    let (visitors, sampled) = prepare_visitors((0..501).map(|_| event(1)).collect());
    assert!(sampled);
    assert_eq!(visitors[0].events.len(), MAX_VISITOR_EVENTS);
}

#[test]
fn consent_can_be_revoked_but_is_required_for_daily_runs() {
    let mut config = settings();
    config.share_activity_with_ai = false;
    assert!(validate_settings(1, &config).is_ok());
    config.daily_enabled = true;
    assert!(validate_settings(1, &config).is_err());
}

#[test]
fn old_settings_deserialize_without_environment_or_source_fields() {
    let settings: ActivitySettings = serde_json::from_value(serde_json::json!({
        "application_context": "A hosting service",
        "categories": [{"name": "Learning", "description": "Reading docs"}],
        "property_keys": [], "daily_enabled": false, "share_activity_with_ai": true
    }))
    .unwrap();
    assert_eq!(settings.environment_id, None);
    assert_eq!(settings.source_url, None);
    assert_eq!(settings.source_domain, None);
    assert_eq!(settings.goal_title, None);
}

#[test]
fn activity_input_is_bounded_even_with_large_selected_properties() {
    let rows = (0..500)
        .map(|index| {
            let mut row = event(index / 20 + 1);
            row.properties = EventProperties(
                (0..10)
                    .map(|key| (format!("field-{key}"), "x".repeat(200)))
                    .collect(),
            );
            row
        })
        .collect();
    let (visitors, sampled) = prepare_visitors(rows);
    assert!(sampled);
    assert!(!visitors.is_empty());
    assert!(visitors.iter().all(|visitor| !visitor.events.is_empty()));
    let bytes: usize = visitors
        .iter()
        .flat_map(|v| &v.events)
        .map(|e| serde_json::to_vec(e).unwrap().len())
        .sum();
    assert!(bytes <= 48 * 1024);
}

#[test]
fn eligibility_and_checkpoints_skip_low_activity_and_stable_unchanged_visitors() {
    let timestamp = Utc::now();
    let mut first = event(1);
    first.timestamp = timestamp;
    first.path = "/docs".into();
    first.session_id = Some("session-a".into());
    let mut second = event(1);
    second.timestamp = timestamp + chrono::Duration::seconds(1);
    second.path = "/docs?step=2".into();
    second.session_id = Some("session-b".into());
    let prepared = prepare_eligible_visitors(
        vec![second.clone(), first.clone()],
        2,
        2,
        7,
        3,
        &VisitorCheckpoints::default(),
    );
    assert_eq!(prepared.visitors.len(), 1);

    let mut path_first = event(3);
    path_first.timestamp = timestamp;
    path_first.path = "/docs".into();
    path_first.session_id = Some("one-session".into());
    let mut path_second = event(3);
    path_second.timestamp = timestamp + chrono::Duration::seconds(1);
    path_second.path = "/pricing".into();
    path_second.session_id = Some("one-session".into());
    assert_eq!(
        prepare_eligible_visitors(
            vec![path_second, path_first],
            2,
            2,
            7,
            3,
            &VisitorCheckpoints::default(),
        )
        .visitors
        .len(),
        1
    );

    let mut unrelated = event(2);
    unrelated.timestamp = timestamp + chrono::Duration::seconds(2);
    let unchanged = prepare_eligible_visitors(
        vec![unrelated, second, first],
        2,
        2,
        7,
        3,
        &prepared.checkpoints,
    );
    assert!(unchanged.visitors.is_empty());
    assert_eq!(unchanged.skipped_unchanged, 1);
    assert_eq!(unchanged.skipped_low_activity, 1);
}

#[tokio::test]
async fn scheduler_handles_no_due_projects_and_database_failures() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]);
    assert!(service(db, false, true).run_due().await.is_ok());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("offline".into())]);
    assert!(matches!(
        service(db, false, true).run_due().await,
        Err(ActivityError::Database {
            operation: "find due projects",
            ..
        })
    ));
}

/// Yields until `waiters` requests hold a capacity-waiter slot. Fails at once
/// if a waiter task already exited (it can no longer queue), and after a
/// bounded number of yields otherwise; a yield count, not a deadline, so it
/// also bounds paused-clock tests.
async fn until_capacity_waiters(
    service: &ActivityService,
    waiters: usize,
    exited: impl Fn() -> bool,
) {
    for _ in 0..10_000 {
        if MAX_CAPACITY_WAITERS - service.capacity_waiters.available_permits() >= waiters {
            return;
        }
        assert!(!exited(), "a waiter task exited before it queued");
        tokio::task::yield_now().await;
    }
    panic!("{waiters} capacity waiter(s) never queued");
}

#[tokio::test]
async fn capacity_waiters_are_bounded_and_served_in_fifo_order() {
    let service = Arc::new(service(
        MockDatabase::new(DatabaseBackend::Postgres),
        true,
        false,
    ));
    let initial = service.capacity.acquire().await.unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);

    let first_service = service.clone();
    let first_sender = sender.clone();
    let first = tokio::spawn(async move {
        let _permit = first_service
            .acquire_capacity(
                1,
                "Sample site 1",
                ActivityOperation::WebsiteAnalysis,
                CAPACITY_WAIT_TIMEOUT,
                false,
            )
            .await
            .unwrap();
        first_sender.send(1).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    });
    until_capacity_waiters(&service, 1, || first.is_finished()).await;

    let second_service = service.clone();
    let second = tokio::spawn(async move {
        let _permit = second_service
            .acquire_capacity(
                2,
                "Sample site 2",
                ActivityOperation::WebsiteAnalysis,
                CAPACITY_WAIT_TIMEOUT,
                false,
            )
            .await
            .unwrap();
        sender.send(2).await.unwrap();
    });
    until_capacity_waiters(&service, 2, || first.is_finished() || second.is_finished()).await;

    drop(initial);
    assert_eq!(receiver.recv().await, Some(1));
    assert_eq!(receiver.recv().await, Some(2));
    first.await.unwrap();
    second.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn scheduled_waiter_survives_interactive_timeout_and_gets_capacity() {
    let service = Arc::new(service(
        MockDatabase::new(DatabaseBackend::Postgres),
        true,
        false,
    ));
    let initial = service.capacity.acquire().await.unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

    let scheduled_service = service.clone();
    let scheduled = tokio::spawn(async move {
        let _permit = scheduled_service
            .acquire_capacity(
                1,
                "Sample site 1",
                ActivityOperation::Report,
                SCHEDULED_CAPACITY_WAIT_TIMEOUT,
                true,
            )
            .await
            .unwrap();
        sender.send(()).await.unwrap();
    });
    tokio::task::yield_now().await;

    let interactive_service = service.clone();
    let interactive = tokio::spawn(async move {
        interactive_service
            .acquire_capacity(
                2,
                "Sample site 2",
                ActivityOperation::WebsiteAnalysis,
                CAPACITY_WAIT_TIMEOUT,
                false,
            )
            .await
            .map(|_| ())
    });
    // Scheduled runs skip the waiter budget: only the interactive one queues.
    until_capacity_waiters(&service, 1, || {
        scheduled.is_finished() || interactive.is_finished()
    })
    .await;
    tokio::time::advance(CAPACITY_WAIT_TIMEOUT + Duration::from_secs(1)).await;
    // Capacity is free of project 2's own work: it is another project's wait.
    assert!(matches!(
        interactive.await.unwrap(),
        Err(ActivityError::ServerBusy {
            project_id: 2,
            retry_after_secs: CAPACITY_RETRY_AFTER_SECS,
            ..
        })
    ));

    drop(initial);
    assert_eq!(receiver.recv().await, Some(()));
    scheduled.await.unwrap();
}

#[test]
fn interactive_admission_is_one_per_project_and_released_on_drop() {
    let service = service(MockDatabase::new(DatabaseBackend::Postgres), true, false);
    let first = service
        .acquire_interactive_admission(1, "Sample site", ActivityOperation::WebsiteAnalysis)
        .unwrap();
    let Err(ActivityError::AnalysisRunning {
        project_id: 1,
        project_name,
        operation,
        started_at,
        retry_after_secs,
    }) = service.acquire_interactive_admission(1, "Sample site", ActivityOperation::Preview)
    else {
        panic!("a second action for the same project must report the running one");
    };
    assert_eq!(project_name, "Sample site");
    assert_eq!(operation, ActivityOperation::WebsiteAnalysis);
    assert!(Utc::now() - started_at < chrono::Duration::seconds(5));
    assert!((1..=90).contains(&retry_after_secs));
    assert!(service
        .acquire_interactive_admission(2, "Other site", ActivityOperation::WebsiteAnalysis)
        .is_ok());
    drop(first);
    assert!(service
        .acquire_interactive_admission(1, "Sample site", ActivityOperation::WebsiteAnalysis)
        .is_ok());
}

#[test]
fn cooldown_is_scoped_per_project_and_operation() {
    let service = service(MockDatabase::new(DatabaseBackend::Postgres), true, false);
    service
        .cooldowns
        .start(1, ActivityOperation::WebsiteAnalysis, PROJECT_COOLDOWN);
    assert!(service
        .check_cooldown(2, "Other site", ActivityOperation::WebsiteAnalysis)
        .is_ok());
    assert!(service
        .check_cooldown(1, "Sample site", ActivityOperation::Preview)
        .is_ok());
    let Err(ActivityError::CoolingDown {
        project_name,
        operation,
        retry_after_secs,
        ..
    }) = service.check_cooldown(1, "Sample site", ActivityOperation::WebsiteAnalysis)
    else {
        panic!("project 1's website analysis is cooling down");
    };
    assert_eq!(project_name, "Sample site");
    assert_eq!(operation, ActivityOperation::WebsiteAnalysis);
    assert!((1..=30).contains(&retry_after_secs));
}

#[test]
fn cooldown_cap_evicts_the_oldest_entry_instead_of_blocking_other_projects() {
    let cooldowns = Cooldowns::default();
    for project_id in 0..MAX_COOLDOWN_ENTRIES as i32 {
        cooldowns.start(
            project_id + 1000,
            ActivityOperation::WebsiteAnalysis,
            PROJECT_COOLDOWN + Duration::from_millis(project_id as u64),
        );
    }
    // An unrelated project is never refused because the map is full.
    assert!(cooldowns
        .remaining(1, ActivityOperation::WebsiteAnalysis)
        .is_none());
    cooldowns.start(1, ActivityOperation::WebsiteAnalysis, PROJECT_COOLDOWN);
    assert!(cooldowns
        .remaining(1, ActivityOperation::WebsiteAnalysis)
        .is_some());
    let until = cooldowns.until.lock().unwrap();
    assert_eq!(until.len(), MAX_COOLDOWN_ENTRIES);
    // The entry that would have expired first made room.
    assert!(!until.contains_key(&(1000, ActivityOperation::WebsiteAnalysis)));
}

#[test]
fn a_lease_past_the_time_limit_still_reports_the_wait_until_it_ends() {
    let now = Utc::now();
    let Some(ActivityError::AnalysisRunning {
        retry_after_secs, ..
    }) = Some(ActivityService::lease_running(
        1,
        "Sample site 1",
        Some(now - chrono::Duration::minutes(10)),
        Some(now + chrono::Duration::seconds(60)),
    ))
    else {
        panic!("a held lease is a running report");
    };
    // The run's own time limit passed long ago; the lease has a minute left.
    assert!((59..=60).contains(&retry_after_secs), "{retry_after_secs}");
}

struct FailingAi(ProviderFailureKind);

#[async_trait::async_trait]
impl AiService for FailingAi {
    async fn chat_stream(
        &self,
        _: temps_ai::ChatTurnRequest,
    ) -> Result<temps_ai::TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::ProviderFailed {
            purpose: request.purpose,
            model: "test-model".into(),
            kind: self.0,
            status: None,
            retry_after_secs: None,
            reason: "test failure".into(),
        })
    }
}

#[tokio::test]
async fn an_unreadable_reply_cools_down_but_a_rejected_request_does_not() {
    for (kind, cools_down) in [
        (ProviderFailureKind::MalformedResponse, true),
        (ProviderFailureKind::ModelNotFound, false),
        (ProviderFailureKind::Authentication, false),
        (ProviderFailureKind::Unreachable, false),
    ] {
        let svc = ActivityService::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            Arc::new(FailingAi(kind)),
        );
        let usage = ProviderUse::default();
        let result = svc.complete(1, &usage, AiRequest::default()).await;
        assert!(
            matches!(result, Err(ActivityError::Provider { kind: k, .. }) if k == kind),
            "{kind:?}"
        );
        assert_eq!(
            usage.cooldown(PROJECT_COOLDOWN).is_some(),
            cools_down,
            "{kind:?}"
        );
    }
}

#[test]
fn a_dropped_request_that_used_the_provider_still_cools_down() {
    let svc = ActivityService::new(
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        Arc::new(FakeAi {
            available: true,
            fail: false,
        }),
    );
    // Cancelled before it reached the provider: free to retry.
    drop(svc.cooldown_on_exit(1, ActivityOperation::Preview));
    assert!(svc
        .cooldowns
        .remaining(1, ActivityOperation::Preview)
        .is_none());
    // Cancelled after the provider spent tokens (a dropped request future
    // drops the guard): the cooldown starts anyway.
    let exit = svc.cooldown_on_exit(1, ActivityOperation::Preview);
    exit.usage.spent_tokens.store(true, Ordering::Relaxed);
    drop(exit);
    assert!(svc
        .cooldowns
        .remaining(1, ActivityOperation::Preview)
        .is_some());
    assert!(svc
        .cooldowns
        .remaining(1, ActivityOperation::WebsiteAnalysis)
        .is_none());
}

#[test]
fn only_spent_tokens_or_a_rate_limit_start_a_cooldown() {
    let usage = ProviderUse::default();
    assert_eq!(usage.cooldown(PROJECT_COOLDOWN), None);
    usage.spent_tokens.store(true, Ordering::Relaxed);
    assert_eq!(usage.cooldown(PROJECT_COOLDOWN), Some(PROJECT_COOLDOWN));
    let limited = ProviderUse::default();
    limited.rate_limited_secs.store(45, Ordering::Relaxed);
    assert_eq!(
        limited.cooldown(PROJECT_COOLDOWN),
        Some(Duration::from_secs(45))
    );
}

#[tokio::test]
async fn scheduler_continues_after_busy_project() {
    let due = vec![
        BTreeMap::from([("project_id".into(), 1_i32.into())]),
        BTreeMap::from([("project_id".into(), 2_i32.into())]),
    ];
    // Each project resolves its environment, then loses the claim to a live
    // lease held by another run: the re-read reports it running.
    let leased = || {
        let mut row = stored();
        row.insert(
            "locked_until".into(),
            Some(Utc::now() + chrono::Duration::minutes(4)).into(),
        );
        row
    };
    let no_claim = Vec::<BTreeMap<String, sea_orm::Value>>::new;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                due,
                vec![project(1)],
                vec![stored()],
                vec![environment(1)],
                no_claim(),
                vec![leased()],
                vec![project(2)],
                vec![stored()],
                vec![environment(1)],
                no_claim(),
                vec![leased()],
            ])
            .into_connection(),
    );
    let service = ActivityService::new(
        db.clone(),
        Arc::new(FakeAi {
            available: true,
            fail: false,
        }),
    );

    service.run_due().await.unwrap();
    drop(service);
    let db = Arc::try_unwrap(db).unwrap();
    let log = db
        .into_transaction_log()
        .iter()
        .map(|transaction| format!("{transaction:?}"))
        .collect::<Vec<_>>();
    assert_eq!(log.len(), 11, "{log:#?}");
    // Project 1's busy claim did not stop the scheduler: project 2 reached
    // its own claim.
    for (index, project_id) in [(4, 1), (9, 2)] {
        assert!(log[index].contains("SET locked_until"), "{}", log[index]);
        assert!(
            log[index].contains(&format!("Values([Int(Some({project_id}))")),
            "{}",
            log[index]
        );
    }
}

#[test]
fn rejects_invented_ids_categories_and_cross_visitor_evidence() {
    let (inputs, _) = prepare_visitors(vec![event(1), event(2)]);
    let mut output = ModelReport {
        summary: "Summary".into(),
        visitors: inputs
            .iter()
            .map(|v| ModelAssessment {
                visitor_ref: v.visitor_ref,
                categories: vec!["Learning".into()],
                explanation: "Reading".into(),
                evidence_references: vec![v.events[0].reference],
            })
            .collect(),
    };
    assert!(validate_output(1, &settings(), &inputs, &output).is_ok());
    output.visitors[0].evidence_references = vec![inputs[1].events[0].reference];
    assert!(validate_output(1, &settings(), &inputs, &output).is_err());
    output.visitors[0].evidence_references = vec![inputs[0].events[0].reference];
    output.visitors[0].categories = vec!["Invented".into()];
    assert!(validate_output(1, &settings(), &inputs, &output).is_err());
    output.visitors[0].categories = vec![UNKNOWN.into(), "Learning".into()];
    assert!(validate_output(1, &settings(), &inputs, &output).is_err());
    output.visitors[0].categories = vec![UNKNOWN.into()];
    output.visitors[0].visitor_ref = 999;
    assert!(validate_output(1, &settings(), &inputs, &output).is_err());
}

fn goal_pages() -> Vec<super::super::site_context::SitePage> {
    vec![super::super::site_context::SitePage {
        url: "https://example.com/".into(),
        text: "Hosting".into(),
    }]
}

#[tokio::test]
async fn goal_suggestions_validate_consent_url_and_project_before_network() {
    for request in [
        ActivityGoalsRequest {
            url: "https://example.com".into(),
            share_with_ai: false,
            environment_id: None,
        },
        ActivityGoalsRequest {
            url: "http://localhost".into(),
            share_with_ai: true,
            environment_id: None,
        },
    ] {
        assert!(matches!(
            service(MockDatabase::new(DatabaseBackend::Postgres), true, false)
                .suggest_goals(1, request)
                .await,
            Err(ActivityError::Validation { .. })
        ));
    }
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::projects::Model>::new()]);
    assert!(matches!(
        service(db, true, false)
            .suggest_goals(
                1,
                ActivityGoalsRequest {
                    url: "https://example.com".into(),
                    share_with_ai: true,
                    environment_id: None,
                }
            )
            .await,
        Err(ActivityError::NotFound { .. })
    ));
}

#[tokio::test]
async fn generated_goals_preserve_sources_and_handle_failures() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]);
    let result = service(db, true, false)
        .generate_goals(1, 1, goal_pages(), &ProviderUse::default())
        .await
        .unwrap();
    assert_eq!(result.goals.len(), 3);
    assert_eq!(result.pages_read, vec!["https://example.com/"]);
    assert_eq!(result.model, "test-model");
    let mut goals = result.goals;
    goals[1].title = goals[0].title.clone();
    assert!(validate_goals(1, &goals).is_err());
    assert!(validate_goals(1, &[]).is_err());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]);
    let usage = ProviderUse::default();
    assert!(matches!(
        service(db, true, true)
            .generate_goals(1, 1, goal_pages(), &usage)
            .await,
        Err(ActivityError::Provider {
            kind: ProviderFailureKind::ModelNotFound,
            ..
        })
    ));
    // A rejected request spent no tokens, so it must not start a cooldown.
    assert_eq!(usage.cooldown(PROJECT_COOLDOWN), None);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("offline".into())]);
    assert!(matches!(
        service(db, true, false)
            .generate_goals(1, 1, goal_pages(), &ProviderUse::default())
            .await,
        Err(ActivityError::Database { .. })
    ));
}

/// The persisted part of the status, without the cooldowns a preview starts.
async fn stored_view(svc: &ActivityService) -> serde_json::Value {
    let mut status = svc.status(1, None).await.unwrap();
    status.cooldowns = ActivityCooldowns::default();
    serde_json::to_value(status).unwrap()
}

fn preview_request() -> ActivityPreviewRequest {
    ActivityPreviewRequest {
        goal: "Understand readers of an application hosting service".into(),
        share_activity_with_ai: true,
        property_keys: vec![],
        environment_id: None,
        source_url: None,
        source_domain: None,
        min_sessions: 2,
        min_page_paths: 2,
    }
}

#[tokio::test]
async fn preview_rejects_invalid_input_before_accessing_data_or_ai() {
    for request in [
        ActivityPreviewRequest {
            goal: " ".into(),
            ..preview_request()
        },
        ActivityPreviewRequest {
            share_activity_with_ai: false,
            ..preview_request()
        },
        ActivityPreviewRequest {
            property_keys: vec!["x".into(); 11],
            ..preview_request()
        },
        ActivityPreviewRequest {
            goal: "x".repeat(4001),
            ..preview_request()
        },
    ] {
        assert!(matches!(
            service(MockDatabase::new(DatabaseBackend::Postgres), true, false)
                .preview(1, request)
                .await,
            Err(ActivityError::Validation { .. })
        ));
    }
}

#[tokio::test]
async fn preview_handles_missing_project_and_database_errors() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::projects::Model>::new()]);
    assert!(matches!(
        service(db, true, false).preview(1, preview_request()).await,
        Err(ActivityError::NotFound { project_id: 1 })
    ));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("offline".into())]);
    assert!(matches!(
        service(db, true, false).preview(1, preview_request()).await,
        Err(ActivityError::Database {
            operation: "find project",
            ..
        })
    ));
}

#[test]
fn generated_setup_cannot_define_permissions_or_schedules() {
    assert!(
        serde_json::from_value::<SuggestedCategories>(serde_json::json!({
            "categories": [], "daily_enabled": true, "property_keys": ["email"]
        }))
        .is_err()
    );
}

#[tokio::test]
async fn recent_activity_handles_presence_absence_and_database_failure() {
    let db =
        MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![BTreeMap::from(
            [("present".to_string(), sea_orm::Value::Int(Some(1)))],
        )]]);
    assert!(service(db, false, false)
        .has_recent_activity(1, 1)
        .await
        .unwrap());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]);
    assert!(!service(db, false, false)
        .has_recent_activity(1, 1)
        .await
        .unwrap());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("offline".into())]);
    assert!(matches!(
        service(db, false, false).has_recent_activity(1, 1).await,
        Err(ActivityError::Database {
            project_id: 1,
            operation: "check recent visitor activity",
            ..
        })
    ));
}

#[tokio::test]
async fn status_missing_project_is_not_found() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::projects::Model>::new()]);
    assert!(matches!(
        service(db, false, false).status(42, None).await,
        Err(ActivityError::NotFound { project_id: 42 })
    ));
}

#[tokio::test]
async fn malformed_persisted_json_fails_at_the_query_boundary() {
    let mut malformed = stored();
    malformed.insert("settings".into(), serde_json::json!("not settings").into());
    let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![malformed]]);
    assert!(matches!(
        service(db, false, false).stored(1).await,
        Err(ActivityError::Database {
            project_id: 1,
            operation: "read settings",
            ..
        })
    ));
}

#[tokio::test]
async fn save_validates_before_database_access() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    assert!(matches!(
        service(db, false, false)
            .save(1, ActivitySettings::default())
            .await,
        Err(ActivityError::Validation { .. })
    ));
}

#[tokio::test]
async fn run_reports_missing_project_and_requires_provider() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::projects::Model>::new()]);
    assert!(matches!(
        service(db, true, false).run(1, false).await,
        Err(ActivityError::NotFound { project_id: 1 })
    ));
    let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
        vec![project(1)],
        vec![stored()],
        vec![environment(1)],
    ]);
    assert!(matches!(
        service(db, false, false).run(1, false).await,
        Err(ActivityError::Unavailable { .. })
    ));
}

#[tokio::test]
async fn run_does_not_call_provider_if_lease_unavailable() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
        vec![project(1)],
        vec![stored()],
        vec![environment(1)],
        vec![],
        vec![stored()],
    ]);
    // The re-read finds no live lease and no cooldown: the claim lost a race
    // with a settings change.
    assert!(matches!(
        service(db, true, false).run(1, false).await,
        Err(ActivityError::Superseded { project_id: 1, .. })
    ));
}

#[tokio::test]
async fn run_reports_a_live_lease_as_running_and_a_cooldown_with_its_wait() {
    let started = Utc::now() - chrono::Duration::seconds(20);
    let mut locked = stored();
    locked.insert(
        "locked_until".into(),
        Some(Utc::now() + chrono::Duration::minutes(4)).into(),
    );
    locked.insert("last_started_at".into(), Some(started).into());
    let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
        vec![project(1)],
        vec![stored()],
        vec![environment(1)],
        vec![],
        vec![locked],
    ]);
    let Err(ActivityError::AnalysisRunning {
        operation,
        started_at,
        retry_after_secs,
        project_name,
        ..
    }) = service(db, true, false).run(1, false).await
    else {
        panic!("a live lease is a running report");
    };
    assert_eq!(operation, ActivityOperation::Report);
    assert_eq!(project_name, "Sample site 1");
    assert_eq!(started_at.timestamp(), started.timestamp());
    // `Retry-After` follows the lease, which outlasts the run's time limit.
    assert!(
        (239..=240).contains(&retry_after_secs),
        "{retry_after_secs}"
    );

    let mut cooling = stored();
    cooling.insert(
        "cooldown_until".into(),
        Some(Utc::now() + chrono::Duration::seconds(42)).into(),
    );
    let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
        vec![project(1)],
        vec![stored()],
        vec![environment(1)],
        vec![],
        vec![cooling],
    ]);
    let Err(ActivityError::CoolingDown {
        operation,
        retry_after_secs,
        ..
    }) = service(db, true, false).run(1, false).await
    else {
        panic!("a cooldown after spent tokens reports its wait");
    };
    assert_eq!(operation, ActivityOperation::Report);
    assert!((41..=42).contains(&retry_after_secs), "{retry_after_secs}");
}

#[tokio::test]
async fn run_empty_activity_saves_report_without_calling_provider() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([
            vec![project(1)],
            vec![stored()],
            vec![environment(1)],
            vec![stored()],
            vec![],
        ])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let report = service(db, true, true).run(1, false).await.unwrap();
    assert!(report.visitors.is_empty());
    assert_eq!(report.model, None);
}

#[tokio::test]
async fn run_rejects_finalization_after_lease_ownership_is_lost() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([
            vec![project(1)],
            vec![stored()],
            vec![environment(1)],
            vec![stored()],
            vec![],
        ])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }]);
    assert!(matches!(
        service(db, true, true).run(1, false).await,
        Err(ActivityError::Superseded { project_id: 1, .. })
    ));
}

#[tokio::test]
async fn database_errors_are_contextual() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project(9)]])
        .append_query_errors([sea_orm::DbErr::Custom("offline".into())]);
    assert!(matches!(
        service(db, true, false).run(9, false).await,
        Err(ActivityError::Database {
            project_id: 9,
            operation: "read settings",
            ..
        })
    ));
}

#[tokio::test]
async fn real_database_run_isolated_auditable_and_recovers_from_failure() {
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_err()
        && !std::process::Command::new("docker")
            .arg("info")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        eprintln!("Skipping real database activity test: Docker unavailable");
        return;
    }
    let test_db = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test_db.db.clone();
    use sea_orm::{ActiveModelTrait, Set};
    for id in [1, 2] {
        temps_entities::projects::ActiveModel {
            id: Set(id),
            name: Set(format!("activity-test-{id}")),
            slug: Set(format!("activity-test-{id}")),
            repo_name: Set("test".into()),
            repo_owner: Set("test".into()),
            directory: Set("/".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
    }
    db.execute_unprepared("INSERT INTO environments (id, name, slug, subdomain, host, project_id, upstreams, created_at, updated_at)
        VALUES (1, 'production', 'production', 'activity-one', 'activity-one.example.test', 1, '[]', NOW(), NOW()),
               (2, 'production', 'production', 'activity-two', 'activity-two.example.test', 2, '[]', NOW(), NOW()),
               (3, 'staging', 'staging', 'activity-stage', 'activity-stage.example.test', 1, '[]', NOW(), NOW());
        INSERT INTO visitor (id, visitor_id, project_id, environment_id, first_seen, last_seen, is_crawler)
        VALUES (1, 'anonymous-1', 1, 1, NOW(), NOW(), FALSE), (2, 'other-project', 2, 2, NOW(), NOW(), FALSE),
            (3, 'bot', 1, 1, NOW(), NOW(), TRUE), (4, 'ghost', 1, 1, NOW(), NOW(), FALSE),
            (5, 'staging-visitor', 1, 3, NOW(), NOW(), FALSE);
        INSERT INTO request_sessions (session_id, visitor_id, started_at, last_accessed_at, data)
        VALUES ('activity-1', 1, NOW(), NOW(), '{}'), ('activity-1b', 1, NOW(), NOW(), '{}'), ('activity-2', 2, NOW(), NOW(), '{}'),
               ('activity-3', 3, NOW(), NOW(), '{}'), ('activity-5', 5, NOW(), NOW(), '{}');
        INSERT INTO events (timestamp, project_id, environment_id, visitor_id, session_id, hostname, pathname, page_path, href, event_type, is_crawler, props)
        VALUES (NOW() - INTERVAL '1 minute', 1, 1, 1, 'activity-1', 'example.test', '/docs/install', '/docs/install', 'https://example.test/?token=secret-token', 'pageview', FALSE, '{\"email\":\"secret@example.test\",\"plan\":\"trial\"}'),
               (NOW() - INTERVAL '2 minutes', 1, 1, 1, 'activity-1b', 'example.test', '/pricing', '/pricing', 'https://example.test/pricing', 'pageview', FALSE, '{\"plan\":\"trial\"}'),
               (NOW() - INTERVAL '1 minute', 2, 2, 2, 'activity-2', 'example.test', '/private', '/private', 'https://example.test/', 'pageview', FALSE, '{}'),
               (NOW() - INTERVAL '1 minute', 1, 1, 3, 'activity-3', 'example.test', '/bot', '/bot', 'https://example.test/', 'pageview', FALSE, '{}'),
               (NOW() - INTERVAL '1 minute', 1, 3, 5, 'activity-5', 'stage.example.test', '/staging', '/staging', 'https://stage.example.test/', 'pageview', FALSE, '{}');").await.unwrap();
    let svc = ActivityService::new(
        db.clone(),
        Arc::new(FakeAi {
            available: true,
            fail: false,
        }),
    );
    let mut config = settings();
    config.goal_title = Some("  Improve activation  ".into());
    config.property_keys = vec!["plan".into()];
    config.daily_enabled = true;
    config.source_url = Some("https://example.com/docs".into());
    config.source_domain = Some("activity-one".into());
    svc.save(1, config.clone()).await.unwrap();
    let initial = svc.status(1, None).await.unwrap();
    assert!(initial.configured);
    assert_eq!(initial.ai_provider.as_deref(), Some("Test Provider"));
    assert_eq!(initial.ai_model.as_deref(), Some("test-model"));
    assert!(initial.has_recent_activity);
    assert_eq!(initial.settings.environment_id, Some(1));
    assert_eq!(initial.settings.source_url, config.source_url);
    assert_eq!(initial.settings.source_domain, config.source_domain);
    assert_eq!(
        initial.settings.goal_title.as_deref(),
        Some("Improve activation")
    );
    assert_eq!(
        svc.status(1, Some(3))
            .await
            .unwrap()
            .selected_environment_id,
        Some(3)
    );
    let mut foreign = config.clone();
    foreign.environment_id = Some(2);
    assert!(matches!(
        svc.save(1, foreign).await,
        Err(ActivityError::Validation { .. })
    ));
    // Old, crawler, ghost and other-project activity cannot unlock preview.
    db.execute_unprepared("UPDATE events SET is_crawler = TRUE WHERE project_id = 1 AND visitor_id = 1;
        INSERT INTO events (timestamp, project_id, environment_id, visitor_id, session_id, hostname, pathname, page_path, href, event_type, is_crawler)
        VALUES (NOW() - INTERVAL '25 hours', 1, 1, 1, 'activity-1', 'example.test', '/old', '/old', 'https://example.test/old', 'pageview', FALSE)").await.unwrap();
    assert!(!svc.status(1, None).await.unwrap().has_recent_activity);
    assert!(matches!(
        svc.preview(1, preview_request()).await,
        Err(ActivityError::Validation { .. })
    ));
    db.execute_unprepared(
        "DELETE FROM events WHERE project_id = 1 AND pathname = '/old';
        UPDATE events SET is_crawler = FALSE WHERE project_id = 1 AND visitor_id = 1",
    )
    .await
    .unwrap();
    assert!(initial.report.is_none());

    // A real run interrupted after its production claim must not consume
    // the schedule. Once the lease expires, run_due can retry it.
    let due_before_claim = svc.stored(1).await.unwrap().unwrap().next_run_at;
    let entered = Arc::new(Notify::new());
    let interrupted_service = Arc::new(ActivityService::new(
        db.clone(),
        Arc::new(BlockingAi {
            entered: entered.clone(),
            release: Arc::new(Notify::new()),
        }),
    ));
    let interrupted_run = {
        let service = interrupted_service.clone();
        tokio::spawn(async move { service.run(1, true).await })
    };
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let interrupted = svc.stored(1).await.unwrap().unwrap();
    assert_eq!(interrupted.next_run_at, due_before_claim);
    assert!(interrupted
        .locked_until
        .is_some_and(|until| until > Utc::now()));
    interrupted_run.abort();
    assert!(interrupted_run.await.unwrap_err().is_cancelled());
    db.execute_unprepared(
        "UPDATE visitor_activity_reports
         SET locked_until = NOW() - INTERVAL '1 second',
             last_started_at = NOW() - INTERVAL '6 minutes'
         WHERE project_id = 1",
    )
    .await
    .unwrap();

    let retry_started_at = Utc::now();
    svc.run_due().await.unwrap();
    let report = svc.status(1, None).await.unwrap().report.unwrap();
    let success_status = svc.status(1, None).await.unwrap();
    assert_eq!(success_status.recent_runs.len(), 1);
    assert_eq!(success_status.recent_runs[0].trigger, "scheduled");
    assert_eq!(success_status.recent_runs[0].status, "success");
    assert_eq!(success_status.recent_runs[0].environment_id, Some(1));
    let other_environment_status = svc.status(1, Some(3)).await.unwrap();
    assert!(other_environment_status.report.is_none());
    assert!(other_environment_status.recent_runs.is_empty());
    assert_eq!(report.visitors.len(), 1);
    assert_eq!(report.visitors[0].visitor_id, 1);
    assert!(report.visitors[0]
        .evidence
        .iter()
        .flat_map(|event| &event.properties)
        .any(|property| property.key == "plan"));
    assert_eq!(report.model.as_deref(), Some("test-model"));
    let after_success = svc.stored(1).await.unwrap().unwrap();
    let successful_checkpoints = after_success.visitor_checkpoints.clone();
    assert!(!successful_checkpoints.0.is_empty());
    assert!(after_success.next_run_at > retry_started_at + chrono::Duration::hours(23));
    assert!(after_success.next_run_at < Utc::now() + chrono::Duration::hours(25));
    // That run spent provider tokens: a manual run now waits for the cooldown.
    let Err(ActivityError::CoolingDown {
        operation: ActivityOperation::Report,
        retry_after_secs,
        ..
    }) = svc.run(1, false).await
    else {
        panic!("a run that spent tokens starts the report cooldown");
    };
    assert!(
        (290..=300).contains(&retry_after_secs),
        "{retry_after_secs}"
    );
    assert!(svc
        .status(1, None)
        .await
        .unwrap()
        .cooldowns
        .report_secs
        .is_some());
    assert_eq!(
        svc.status(1, None)
            .await
            .unwrap()
            .report
            .unwrap()
            .visitors
            .len(),
        1
    );
    // A preview uses unsaved categories without replacing a report, revision,
    // schedule or the persisted report cooldown (the snapshot below leaves out
    // countdowns, which change with time, so the timestamp is compared too).
    let persisted_cooldown = || async {
        svc.stored(1)
            .await
            .unwrap()
            .expect("the report row exists")
            .cooldown_until
    };
    let cooldown_before = persisted_cooldown().await;
    assert!(cooldown_before.is_some());
    let before = stored_view(&svc).await;
    let preview = svc.preview(1, preview_request()).await.unwrap();
    assert_eq!(preview.settings.categories.len(), 1);
    assert!(!preview.settings.daily_enabled);
    assert!(preview.settings.property_keys.is_empty());
    assert_eq!(preview.report.visitors.len(), 1);
    assert!(preview.report.visitors[0].evidence[0].properties.is_empty());
    assert_eq!(preview.report.settings_revision, 0);
    assert_eq!(stored_view(&svc).await, before);
    assert_eq!(persisted_cooldown().await, cooldown_before);
    assert!(matches!(
        svc.preview(1, preview_request()).await,
        Err(ActivityError::CoolingDown {
            operation: ActivityOperation::Preview,
            ..
        })
    ));
    assert!(svc
        .status(1, None)
        .await
        .unwrap()
        .cooldowns
        .preview_secs
        .is_some());
    let unavailable = ActivityService::new(
        db.clone(),
        Arc::new(FakeAi {
            available: false,
            fail: false,
        }),
    );
    assert!(matches!(
        unavailable.preview(1, preview_request()).await,
        Err(ActivityError::Unavailable { .. })
    ));
    let failed_preview = ActivityService::new(
        db.clone(),
        Arc::new(FakeAi {
            available: true,
            fail: true,
        }),
    );
    assert!(matches!(
        failed_preview.preview(1, preview_request()).await,
        Err(ActivityError::Provider {
            kind: ProviderFailureKind::ModelNotFound,
            ..
        })
    ));
    // The rejected preview spent nothing: it can be retried at once and
    // reaches the provider again rather than reporting a cooldown.
    assert!(failed_preview
        .cooldowns
        .remaining(1, ActivityOperation::Preview)
        .is_none());
    assert!(matches!(
        failed_preview.preview(1, preview_request()).await,
        Err(ActivityError::Provider { .. })
    ));
    assert_eq!(stored_view(&svc).await, before);
    assert_eq!(persisted_cooldown().await, cooldown_before);
    db.execute_unprepared("UPDATE visitor_activity_reports SET cooldown_until = NULL")
        .await
        .unwrap();
    db.execute_unprepared(
        "UPDATE events SET page_title = 'Changed after checkpoint' \
         WHERE project_id = 1 AND visitor_id = 1 AND pathname = '/docs/install'",
    )
    .await
    .unwrap();
    let failing = ActivityService::new(
        db.clone(),
        Arc::new(FakeAi {
            available: true,
            fail: true,
        }),
    );
    assert!(matches!(
        failing.run(1, false).await,
        Err(ActivityError::Provider {
            kind: ProviderFailureKind::ModelNotFound,
            ..
        })
    ));
    let status = svc.status(1, None).await.unwrap();
    // The stored error tells the operator what to fix, not "check the logs".
    assert!(status
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("model 'test-model' not found")));
    // The provider rejected the run before spending tokens: no cooldown.
    assert_eq!(status.cooldowns.report_secs, None);
    assert!(!status.running);
    assert!(status.report.is_some());
    assert_eq!(status.recent_runs[0].trigger, "manual");
    assert_eq!(status.recent_runs[0].status, "failed");
    assert_eq!(
        svc.stored(1).await.unwrap().unwrap().visitor_checkpoints,
        successful_checkpoints
    );

    // Returning the event to its checkpointed form proves unchanged runs
    // skip the provider even when that provider would fail if called. The
    // retry needs no wait: the failed run started no cooldown.
    db.execute_unprepared(
        "UPDATE events SET page_title = NULL
         WHERE project_id = 1 AND visitor_id = 1 AND pathname = '/docs/install'",
    )
    .await
    .unwrap();
    let unchanged = ActivityService::new(
        db.clone(),
        Arc::new(FakeAi {
            available: true,
            fail: true,
        }),
    );
    unchanged.run(1, false).await.unwrap();
    let skipped = svc.status(1, None).await.unwrap();
    assert_eq!(skipped.recent_runs[0].status, "skipped");
    assert_eq!(skipped.recent_runs[0].skipped_unchanged, 1);
    assert_eq!(skipped.recent_runs[1].status, "failed");
    assert!(skipped.report.is_some());
    assert_eq!(
        svc.stored(1).await.unwrap().unwrap().visitor_checkpoints,
        successful_checkpoints
    );

    // Legacy history had no environment provenance and must fail closed.
    db.execute_unprepared(
        "UPDATE visitor_activity_reports SET run_history =
         (SELECT COALESCE(jsonb_agg(item - 'environment_id'), '[]'::jsonb)
          FROM jsonb_array_elements(run_history) item) WHERE project_id = 1",
    )
    .await
    .unwrap();
    assert!(svc.status(1, None).await.unwrap().recent_runs.is_empty());
    svc.save(1, config).await.unwrap();
    assert_eq!(svc.status(1, None).await.unwrap().settings_revision, 2);
    db.execute_unprepared("UPDATE environments SET deleted_at = NOW() WHERE id = 1")
        .await
        .unwrap();
    let after_delete = svc.status(1, None).await.unwrap();
    assert_eq!(after_delete.selected_environment_id, Some(3));
    assert!(after_delete.report.is_none());
    assert!(after_delete.recent_runs.is_empty());
}
