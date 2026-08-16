use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::audit::PermissionDeniedAudit;

const DEFAULT_QUEUE_CAPACITY: usize = 1_024;
const DEFAULT_MAX_AGGREGATIONS: usize = 1_024;
/// A one-minute window limits normal persistence to at most 16 detailed rows
/// plus one reserved suppression-summary row (17 writes/minute globally).
const DEFAULT_WINDOW: Duration = Duration::from_secs(60);
const DEFAULT_MAX_DETAIL_ROWS: usize = 16;
const DEFAULT_MAX_DETAIL_ROWS_PER_ACTOR: usize = 4;
const MIXED_VALUE: &str = "multiple";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AuthSourceKind {
    Anonymous,
    Session,
    CliToken,
    ApiKey,
    DeploymentToken,
}

impl AuthSourceKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Session => "session",
            Self::CliToken => "cli_token",
            Self::ApiKey => "api_key",
            Self::DeploymentToken => "deployment_token",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SafePrincipal {
    pub(crate) user_id: Option<i32>,
    pub(crate) source: AuthSourceKind,
    pub(crate) credential_id: Option<i32>,
}

impl SafePrincipal {
    pub(crate) const fn anonymous() -> Self {
        Self {
            user_id: None,
            source: AuthSourceKind::Anonymous,
            credential_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PermissionDenialEvent {
    pub(crate) principal: SafePrincipal,
    pub(crate) method: String,
    pub(crate) route: String,
    pub(crate) denial_kind: String,
    pub(crate) required_permission: Option<String>,
    pub(crate) ip_address: Option<String>,
    pub(crate) user_agent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AggregationKey {
    user_id: Option<i32>,
    source: AuthSourceKind,
    method: String,
    route: String,
    denial_kind: String,
    required_permission: Option<String>,
}

impl From<&PermissionDenialEvent> for AggregationKey {
    fn from(event: &PermissionDenialEvent) -> Self {
        Self {
            user_id: event.principal.user_id,
            source: event.principal.source,
            method: event.method.clone(),
            route: event.route.clone(),
            denial_kind: event.denial_kind.clone(),
            required_permission: event.required_permission.clone(),
        }
    }
}

impl PermissionDenialEvent {
    fn into_audit(self) -> PermissionDeniedAudit {
        PermissionDeniedAudit {
            user_id: self.principal.user_id,
            auth_source: self.principal.source.as_str().to_string(),
            credential_id: self.principal.credential_id,
            multiple_credentials: false,
            method: self.method,
            route: self.route,
            denial_kind: self.denial_kind,
            required_permission: self.required_permission,
            attempt_count: 1,
            multiple_origins: false,
            suppressed_by_budget: false,
            ip_address: self.ip_address,
            user_agent: self.user_agent,
        }
    }
}

struct RecorderCounters {
    queue_drops: AtomicU64,
    aggregation_overflows: AtomicU64,
    write_failures: AtomicU64,
    budget_suppressed_attempts: AtomicU64,
}

impl RecorderCounters {
    fn new() -> Self {
        Self {
            queue_drops: AtomicU64::new(0),
            aggregation_overflows: AtomicU64::new(0),
            write_failures: AtomicU64::new(0),
            budget_suppressed_attempts: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RecorderConfig {
    queue_capacity: usize,
    max_aggregations: usize,
    window: Duration,
    max_detail_rows: usize,
    max_detail_rows_per_actor: usize,
}

/// Non-blocking, explicitly bounded entry point for permission-denial audit
/// events. Request handling performs only `try_send`; aggregation and audit
/// logger I/O happen in the spawned worker.
pub struct PermissionDenialRecorder {
    sender: mpsc::Sender<PermissionDenialEvent>,
    counters: Arc<RecorderCounters>,
}

impl PermissionDenialRecorder {
    pub fn new(logger: Arc<dyn temps_core::AuditLogger>) -> Arc<Self> {
        Self::with_config(
            logger,
            RecorderConfig {
                queue_capacity: DEFAULT_QUEUE_CAPACITY,
                max_aggregations: DEFAULT_MAX_AGGREGATIONS,
                window: DEFAULT_WINDOW,
                max_detail_rows: DEFAULT_MAX_DETAIL_ROWS,
                max_detail_rows_per_actor: DEFAULT_MAX_DETAIL_ROWS_PER_ACTOR,
            },
        )
    }

    fn with_config(logger: Arc<dyn temps_core::AuditLogger>, config: RecorderConfig) -> Arc<Self> {
        let (sender, receiver) = mpsc::channel(config.queue_capacity);
        let counters = Arc::new(RecorderCounters::new());
        let worker = PermissionDenialWorker {
            receiver,
            logger,
            counters: counters.clone(),
            max_aggregations: config.max_aggregations,
            window: config.window,
            max_detail_rows: config.max_detail_rows,
            max_detail_rows_per_actor: config.max_detail_rows_per_actor,
            pending: HashMap::with_capacity(config.max_aggregations),
        };
        tokio::spawn(worker.run());

        Arc::new(Self { sender, counters })
    }

    pub(crate) fn record(&self, event: PermissionDenialEvent) {
        if let Err(error) = self.sender.try_send(event) {
            let event = error.into_inner();
            let total = self.counters.queue_drops.fetch_add(1, Ordering::Relaxed) + 1;
            if should_log_counter(total) {
                tracing::warn!(
                    total_queue_drops = total,
                    user_id = event.principal.user_id,
                    auth_source = event.principal.source.as_str(),
                    credential_id = event.principal.credential_id,
                    method = event.method,
                    route = event.route,
                    denial_kind = event.denial_kind,
                    required_permission = event.required_permission,
                    "permission-denial audit queue saturated or closed; event dropped"
                );
            }
        }
    }
}

fn should_log_counter(total: u64) -> bool {
    total.is_power_of_two()
}

struct PermissionDenialWorker {
    receiver: mpsc::Receiver<PermissionDenialEvent>,
    logger: Arc<dyn temps_core::AuditLogger>,
    counters: Arc<RecorderCounters>,
    max_aggregations: usize,
    window: Duration,
    max_detail_rows: usize,
    max_detail_rows_per_actor: usize,
    pending: HashMap<AggregationKey, PermissionDeniedAudit>,
}

impl PermissionDenialWorker {
    async fn run(mut self) {
        let start = tokio::time::Instant::now() + self.window;
        let mut ticker = tokio::time::interval_at(start, self.window);

        loop {
            tokio::select! {
                event = self.receiver.recv() => {
                    match event {
                        Some(event) => self.aggregate(event),
                        None => {
                            self.flush().await;
                            break;
                        }
                    }
                }
                _ = ticker.tick() => self.flush().await,
            }
        }
    }

    fn aggregate(&mut self, event: PermissionDenialEvent) {
        let key = AggregationKey::from(&event);
        if let Some(audit) = self.pending.get_mut(&key) {
            audit.attempt_count = audit.attempt_count.saturating_add(1);
            merge_attribution(audit, &event);
            return;
        }

        if self.pending.len() >= self.max_aggregations {
            let total = self
                .counters
                .aggregation_overflows
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if should_log_counter(total) {
                tracing::warn!(
                    total_aggregation_overflows = total,
                    max_aggregations = self.max_aggregations,
                    user_id = event.principal.user_id,
                    auth_source = event.principal.source.as_str(),
                    credential_id = event.principal.credential_id,
                    method = event.method,
                    route = event.route,
                    denial_kind = event.denial_kind,
                    required_permission = event.required_permission,
                    "permission-denial aggregation map full; new key dropped"
                );
            }
            return;
        }

        self.pending.insert(key, event.into_audit());
    }

    async fn flush(&mut self) {
        // At most `max_aggregations` entries are moved into this temporary
        // vector. The queue and map remain bounded while logger I/O is in flight.
        let mut audits: Vec<_> = self.pending.drain().map(|(_, audit)| audit).collect();
        audits.sort_by(|left, right| {
            right
                .attempt_count
                .cmp(&left.attempt_count)
                .then_with(|| left.user_id.cmp(&right.user_id))
                .then_with(|| left.auth_source.cmp(&right.auth_source))
                .then_with(|| left.method.cmp(&right.method))
                .then_with(|| left.route.cmp(&right.route))
                .then_with(|| left.denial_kind.cmp(&right.denial_kind))
                .then_with(|| left.required_permission.cmp(&right.required_permission))
        });

        let mut actor_rows: HashMap<(Option<i32>, String), usize> = HashMap::new();
        let mut selected = Vec::with_capacity(self.max_detail_rows);
        let mut suppressed_attempts = 0_u64;
        for audit in audits {
            let actor = (audit.user_id, audit.auth_source.clone());
            let actor_count = actor_rows.entry(actor).or_default();
            if selected.len() < self.max_detail_rows
                && *actor_count < self.max_detail_rows_per_actor
            {
                *actor_count += 1;
                selected.push(audit);
            } else {
                suppressed_attempts = suppressed_attempts.saturating_add(audit.attempt_count);
            }
        }

        if suppressed_attempts > 0 {
            let total = self
                .counters
                .budget_suppressed_attempts
                .fetch_add(suppressed_attempts, Ordering::Relaxed)
                .saturating_add(suppressed_attempts);
            if should_log_counter(total) || total == suppressed_attempts {
                tracing::warn!(
                    suppressed_attempts,
                    total_budget_suppressed_attempts = total,
                    max_detail_rows = self.max_detail_rows,
                    max_detail_rows_per_actor = self.max_detail_rows_per_actor,
                    "permission-denial detail persistence budget reached"
                );
            }
            selected.push(suppression_summary(suppressed_attempts));
        }

        let audits = selected;
        for audit in audits {
            if let Err(error) = self.logger.create_audit_log(&audit).await {
                let total = self.counters.write_failures.fetch_add(1, Ordering::Relaxed) + 1;
                if should_log_counter(total) {
                    tracing::warn!(
                        total_write_failures = total,
                        user_id = audit.user_id,
                        auth_source = audit.auth_source,
                        credential_id = audit.credential_id,
                        method = audit.method,
                        route = audit.route,
                        denial_kind = audit.denial_kind,
                        required_permission = audit.required_permission,
                        error = %error,
                        "failed to persist permission-denial audit; denial response was unaffected"
                    );
                }
            }
        }
    }
}

fn merge_attribution(audit: &mut PermissionDeniedAudit, event: &PermissionDenialEvent) {
    if !audit.multiple_credentials && audit.credential_id != event.principal.credential_id {
        audit.credential_id = None;
        audit.multiple_credentials = true;
    }

    let mixed_ip = audit.ip_address != event.ip_address;
    let mixed_user_agent = audit.user_agent != event.user_agent;
    if mixed_ip || mixed_user_agent {
        audit.ip_address = None;
        audit.user_agent = MIXED_VALUE.to_string();
        audit.multiple_origins = true;
    }
}

fn suppression_summary(attempt_count: u64) -> PermissionDeniedAudit {
    PermissionDeniedAudit {
        user_id: None,
        auth_source: MIXED_VALUE.to_string(),
        credential_id: None,
        multiple_credentials: true,
        method: MIXED_VALUE.to_string(),
        route: MIXED_VALUE.to_string(),
        denial_kind: "persistence_budget_suppressed".to_string(),
        required_permission: None,
        attempt_count,
        multiple_origins: true,
        suppressed_by_budget: true,
        ip_address: None,
        user_agent: MIXED_VALUE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::Result;
    use temps_core::{AuditLogger, AuditOperation};

    use super::*;

    #[derive(Default)]
    struct RecordingLogger {
        records: Mutex<Vec<serde_json::Value>>,
        user_agents: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingLogger {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> Result<()> {
            let serialized = operation.serialize()?;
            let value = serde_json::from_str(&serialized)?;
            self.records
                .lock()
                .map_err(|_| anyhow::anyhow!("test logger lock poisoned"))?
                .push(value);
            self.user_agents
                .lock()
                .map_err(|_| anyhow::anyhow!("test user-agent lock poisoned"))?
                .push(operation.user_agent().to_string());
            Ok(())
        }
    }

    struct FailingLogger;

    #[async_trait::async_trait]
    impl AuditLogger for FailingLogger {
        async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> Result<()> {
            Err(anyhow::anyhow!("intentional logger failure"))
        }
    }

    fn event(route: &str) -> PermissionDenialEvent {
        PermissionDenialEvent {
            principal: SafePrincipal {
                user_id: Some(42),
                source: AuthSourceKind::ApiKey,
                credential_id: Some(9),
            },
            method: "PATCH".to_string(),
            route: route.to_string(),
            denial_kind: "insufficient_permission".to_string(),
            required_permission: Some("projects:write".to_string()),
            ip_address: Some("203.0.113.7".to_string()),
            user_agent: "test-agent".to_string(),
        }
    }

    fn test_config(queue_capacity: usize, max_aggregations: usize) -> RecorderConfig {
        RecorderConfig {
            queue_capacity,
            max_aggregations,
            window: Duration::from_millis(20),
            max_detail_rows: 8,
            max_detail_rows_per_actor: 8,
        }
    }

    async fn advance_past_flush_window(window: Duration) {
        // Give the detached worker a chance to receive queued events and arm
        // its interval before advancing deterministic Tokio test time.
        tokio::task::yield_now().await;
        tokio::time::advance(window.saturating_add(Duration::from_millis(1))).await;
        tokio::task::yield_now().await;
    }

    #[tokio::test(start_paused = true)]
    async fn mixed_ip_clears_singular_origin_attribution() {
        let logger = Arc::new(RecordingLogger::default());
        let recorder = PermissionDenialRecorder::with_config(logger.clone(), test_config(8, 8));
        recorder.record(event("/projects/{project_id}"));
        let mut second = event("/projects/{project_id}");
        second.ip_address = Some("198.51.100.99".to_string());
        recorder.record(second);

        advance_past_flush_window(Duration::from_millis(20)).await;
        let records = logger.records.lock().expect("test logger lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["attempt_count"], 2);
        assert_eq!(records[0]["route"], "/projects/{project_id}");
        assert_eq!(records[0]["auth_source"], "api_key");
        assert_eq!(records[0]["credential_id"], 9);
        assert_eq!(records[0]["ip_address"], serde_json::Value::Null);
        assert_eq!(
            logger.user_agents.lock().expect("test user-agent lock")[0],
            MIXED_VALUE
        );
        assert_eq!(records[0]["multiple_origins"], true);
        assert!(records[0].get("key_name").is_none());
        assert!(records[0].get("token_name").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn mixed_user_agent_clears_singular_origin_attribution() {
        let logger = Arc::new(RecordingLogger::default());
        let recorder = PermissionDenialRecorder::with_config(logger.clone(), test_config(8, 8));
        recorder.record(event("/projects/{project_id}"));
        let mut second = event("/projects/{project_id}");
        second.user_agent = "different-agent".to_string();
        recorder.record(second);

        advance_past_flush_window(Duration::from_millis(20)).await;
        let records = logger.records.lock().expect("test logger lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["ip_address"], serde_json::Value::Null);
        assert_eq!(
            logger.user_agents.lock().expect("test user-agent lock")[0],
            MIXED_VALUE
        );
        assert_eq!(records[0]["multiple_origins"], true);
    }

    #[tokio::test(start_paused = true)]
    async fn mixed_credential_ids_cannot_multiply_aggregation_keys() {
        let logger = Arc::new(RecordingLogger::default());
        let recorder = PermissionDenialRecorder::with_config(logger.clone(), test_config(8, 8));
        recorder.record(event("/projects/{project_id}"));
        let mut second = event("/projects/{project_id}");
        second.principal.credential_id = Some(10);
        recorder.record(second);

        advance_past_flush_window(Duration::from_millis(20)).await;
        let records = logger.records.lock().expect("test logger lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["attempt_count"], 2);
        assert_eq!(records[0]["credential_id"], serde_json::Value::Null);
        assert_eq!(records[0]["multiple_credentials"], true);
    }

    #[tokio::test(start_paused = true)]
    async fn adversarial_cardinality_obeys_actor_and_global_write_budgets() {
        let logger = Arc::new(RecordingLogger::default());
        let recorder = PermissionDenialRecorder::with_config(
            logger.clone(),
            RecorderConfig {
                queue_capacity: 256,
                max_aggregations: 256,
                window: Duration::from_millis(30),
                max_detail_rows: 16,
                max_detail_rows_per_actor: 4,
            },
        );

        for actor_offset in 0..5 {
            for route_offset in 0..10 {
                let mut attempt = event(&format!("/route/{actor_offset}/{route_offset}"));
                attempt.principal.user_id = Some(40 + actor_offset);
                attempt.principal.credential_id = Some(1_000 + actor_offset * 10 + route_offset);
                recorder.record(attempt);
            }
        }

        advance_past_flush_window(Duration::from_millis(30)).await;
        let records = logger.records.lock().expect("test logger lock");
        let details: Vec<_> = records
            .iter()
            .filter(|record| record["suppressed_by_budget"] == false)
            .collect();
        let summaries: Vec<_> = records
            .iter()
            .filter(|record| record["suppressed_by_budget"] == true)
            .collect();

        assert_eq!(details.len(), 16, "global detail ceiling must hold");
        let mut rows_by_actor = HashMap::<i64, usize>::new();
        for detail in details {
            let actor = detail["user_id"].as_i64().expect("detail has actor");
            *rows_by_actor.entry(actor).or_default() += 1;
        }
        assert!(rows_by_actor.values().all(|count| *count <= 4));
        assert_eq!(summaries.len(), 1, "summary uses one reserved row");
        assert_eq!(summaries[0]["attempt_count"], 34);
        assert_eq!(
            recorder
                .counters
                .budget_suppressed_attempts
                .load(Ordering::Relaxed),
            34
        );
    }

    #[tokio::test(start_paused = true)]
    async fn aggregation_cardinality_is_bounded() {
        let logger = Arc::new(RecordingLogger::default());
        let recorder = PermissionDenialRecorder::with_config(logger.clone(), test_config(8, 1));
        recorder.record(event("/first"));
        recorder.record(event("/second"));

        advance_past_flush_window(Duration::from_millis(20)).await;
        assert_eq!(
            recorder
                .counters
                .aggregation_overflows
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(logger.records.lock().expect("test logger lock").len(), 1);
    }

    #[tokio::test]
    async fn queue_overflow_is_counted_without_blocking() {
        let (sender, _receiver) = mpsc::channel(1);
        let counters = Arc::new(RecorderCounters::new());
        let recorder = PermissionDenialRecorder {
            sender,
            counters: counters.clone(),
        };

        recorder.record(event("/first"));
        recorder.record(event("/second"));
        assert_eq!(counters.queue_drops.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn logger_failure_is_counted_and_worker_continues() {
        let recorder =
            PermissionDenialRecorder::with_config(Arc::new(FailingLogger), test_config(8, 8));
        recorder.record(event("/first"));
        advance_past_flush_window(Duration::from_millis(20)).await;
        assert_eq!(recorder.counters.write_failures.load(Ordering::Relaxed), 1);

        recorder.record(event("/second"));
        advance_past_flush_window(Duration::from_millis(20)).await;
        assert_eq!(recorder.counters.write_failures.load(Ordering::Relaxed), 2);
    }
}
