// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The HTTP log ingest path (ADR 108 D1): authenticate the key, parse the
//! batch, charge the project's ingestion budget, write the lines.
//!
//! Lines go through the same [`ChunkWriterService::write_line`] a container's
//! lines do, so they share the head buffers, WAL, sealing, line index,
//! retention and purge; nothing here batches separately or writes to the
//! database per line.
//!
//! ## Streams
//!
//! A chunk carries one set of labels, so each `(key, service)` pair is its
//! own stream, `ingest-<key id>-<service>`. A key may open at most
//! [`MAX_SERVICES_PER_KEY`] distinct services per process; lines naming a
//! further service are stored under the key's default service with the name
//! they sent kept as the `ingest_service` attribute. That bounds the head
//! buffers one key can hold open no matter what a client sends.
//!
//! ## Budget
//!
//! One limiter per key, against the same per-project budget and overrides a
//! container gets ([`IngestBudget`]). A batch is admitted whole or refused
//! whole with `429` (see [`IngestLimiter::admit_batch`]); a refused batch is
//! counted and lists the key on the pipeline status page.
//!
//! ## Memory
//!
//! A request holds at most one parsed batch ([`ndjson::MAX_BODY_BYTES`] of
//! input, [`ndjson::MAX_LINES_PER_BATCH`] lines). Per key the service keeps a
//! limiter and a set of at most [`MAX_SERVICES_PER_KEY`] names; keys are
//! capped per project. Both maps are touched once per request, never per
//! line.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use tracing::{debug, error};

use crate::error::LogAggregatorError;
use crate::ingest::keys::{AuthenticatedKey, LogIngestKeyService};
use crate::ingest::ndjson::{self, ParsedRecord};
use crate::services::{Admission, ChunkWriterService, IngestBudget, IngestLimiter, IngestSource};
use crate::types::ContainerContext;

/// Distinct services one key may open in this process.
pub const MAX_SERVICES_PER_KEY: usize = 16;

/// Environment label of a key that is not pinned to an environment.
pub const UNSCOPED_ENV: &str = "external";

/// What an accepted batch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    pub accepted: usize,
}

/// Writes the lines a collector sends. Only [`ChunkWriterService`] is
/// needed for storage, so the service can be tested with a fake writer.
#[async_trait::async_trait]
pub trait IngestLineWriter: Send + Sync {
    async fn write(&self, line: crate::types::LogLine) -> Result<(), LogAggregatorError>;
}

#[async_trait::async_trait]
impl IngestLineWriter for Arc<ChunkWriterService> {
    async fn write(&self, line: crate::types::LogLine) -> Result<(), LogAggregatorError> {
        self.write_line(line).await
    }
}

/// Authenticates the key a request presents. A trait so the ingest path can
/// be tested without a database.
#[async_trait::async_trait]
pub trait IngestKeyResolver: Send + Sync {
    async fn resolve(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError>;
    fn record_usage(&self, key_id: i32, lines: u64, at: DateTime<Utc>);
}

#[async_trait::async_trait]
impl IngestKeyResolver for LogIngestKeyService {
    async fn resolve(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
        self.authenticate(presented).await
    }
    fn record_usage(&self, key_id: i32, lines: u64, at: DateTime<Utc>) {
        LogIngestKeyService::record_usage(self, key_id, lines, at)
    }
}

pub struct LogIngestService {
    keys: Arc<dyn IngestKeyResolver>,
    writer: Arc<dyn IngestLineWriter>,
    budget: Arc<IngestBudget>,
    limiters: Mutex<HashMap<i32, IngestLimiter>>,
    services: Mutex<HashMap<i32, HashSet<String>>>,
}

impl LogIngestService {
    pub fn new(
        keys: Arc<dyn IngestKeyResolver>,
        writer: Arc<dyn IngestLineWriter>,
        budget: Arc<IngestBudget>,
    ) -> Self {
        Self {
            keys,
            writer,
            budget,
            limiters: Mutex::new(HashMap::new()),
            services: Mutex::new(HashMap::new()),
        }
    }

    /// Forget a key's limiter and services, after it is revoked.
    pub fn forget_key(&self, key_id: i32) {
        if let Ok(mut limiters) = self.limiters.lock() {
            limiters.remove(&key_id);
        }
        if let Ok(mut services) = self.services.lock() {
            services.remove(&key_id);
        }
    }

    /// Ingest one request body sent with `presented` as its key.
    pub async fn ingest(
        &self,
        presented: &str,
        body: &[u8],
    ) -> Result<IngestOutcome, LogAggregatorError> {
        let key = self.keys.resolve(presented).await?;
        let now = Utc::now();
        let records = ndjson::parse_batch(body, now)?;
        if records.is_empty() {
            return Ok(IngestOutcome { accepted: 0 });
        }

        let (admission, notice) = self.charge(&key, records.len() as u64, body.len() as u64, now);
        if let Some(notice) = notice {
            // Explains an earlier refused batch where its gap is seen.
            if let Err(e) = self.writer.write(notice).await {
                error!(key_id = key.key_id, error = %e, "Failed to write a log ingest drop notice");
            }
        }
        if let Admission::Drop(reason) = admission {
            return Err(LogAggregatorError::IngestBudgetExceeded {
                project_id: key.project_id,
                accepted: 0,
                total: records.len(),
                reason: reason.to_string(),
            });
        }

        let total = records.len();
        let lines = self.label(&key, records);
        for line in lines {
            self.writer.write(line).await?;
        }
        self.keys.record_usage(key.key_id, total as u64, now);
        debug!(
            key_id = key.key_id,
            project_id = key.project_id,
            lines = total,
            "Ingested a log batch"
        );
        Ok(IngestOutcome { accepted: total })
    }

    /// Charge the key's limiter for a batch, and collect a pending drop
    /// notice. The limiter map is locked once for the whole decision.
    fn charge(
        &self,
        key: &AuthenticatedKey,
        lines: u64,
        bytes: u64,
        now: DateTime<Utc>,
    ) -> (Admission, Option<crate::types::LogLine>) {
        let Ok(mut limiters) = self.limiters.lock() else {
            error!(
                key_id = key.key_id,
                "log ingest limiter registry is poisoned; admitting the batch"
            );
            return (Admission::Admit, None);
        };
        let limiter = limiters
            .entry(key.key_id)
            .or_insert_with(|| self.budget.limiter(source_for(key)));
        let admission = limiter.admit_batch(lines, bytes, now);
        (admission, limiter.take_drop_notice())
    }

    /// Give each record its stream's labels.
    fn label(
        &self,
        key: &AuthenticatedKey,
        records: Vec<ParsedRecord>,
    ) -> Vec<crate::types::LogLine> {
        let mut services = self.services.lock().ok();
        let mut contexts: HashMap<String, ContainerContext> = HashMap::new();
        records
            .into_iter()
            .map(|mut record| {
                let requested = record.service.take();
                let service = match (&requested, services.as_mut()) {
                    (Some(name), Some(services)) => {
                        let seen = services.entry(key.key_id).or_default();
                        if seen.contains(name) || seen.len() < MAX_SERVICES_PER_KEY {
                            seen.insert(name.clone());
                            name.clone()
                        } else {
                            record.object.insert(
                                "ingest_service".to_string(),
                                serde_json::Value::String(name.clone()),
                            );
                            key.default_service.clone()
                        }
                    }
                    (Some(name), None) => name.clone(),
                    (None, _) => key.default_service.clone(),
                };
                let ctx = contexts
                    .entry(service.clone())
                    .or_insert_with(|| context_for(key, &service));
                record.into_line(ctx)
            })
            .collect()
    }
}

/// Stream id of a key's service. Safe as a WAL file name: the service is
/// already reduced to `[a-z0-9._-]`.
pub fn stream_id(key_id: i32, service: &str) -> String {
    format!("ingest-{key_id}-{service}")
}

fn context_for(key: &AuthenticatedKey, service: &str) -> ContainerContext {
    ContainerContext {
        project_id: key.project_id,
        external_service_id: None,
        env: key
            .environment_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| UNSCOPED_ENV.to_string()),
        service: service.to_string(),
        container_id: stream_id(key.key_id, service),
        deploy_id: None,
    }
}

/// The budget source for a key: its default stream, so the pipeline page
/// names the key's stream and a drop notice lands in it.
fn source_for(key: &AuthenticatedKey) -> IngestSource {
    IngestSource {
        ctx: context_for(key, &key.default_service),
        node_id: None,
        node_name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::IngestLimits;
    use crate::types::LogLine;

    struct FixedKey(AuthenticatedKey);

    #[async_trait::async_trait]
    impl IngestKeyResolver for FixedKey {
        async fn resolve(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
            if presented == "good" {
                Ok(self.0.clone())
            } else {
                Err(LogAggregatorError::IngestKeyRejected)
            }
        }
        fn record_usage(&self, _key_id: i32, _lines: u64, _at: DateTime<Utc>) {}
    }

    #[derive(Default)]
    struct Recorded(Mutex<Vec<LogLine>>);

    #[async_trait::async_trait]
    impl IngestLineWriter for Recorded {
        async fn write(&self, line: LogLine) -> Result<(), LogAggregatorError> {
            self.0.lock().expect("lines lock").push(line);
            Ok(())
        }
    }

    fn key() -> AuthenticatedKey {
        AuthenticatedKey {
            key_id: 5,
            project_id: 7,
            environment_id: Some(3),
            default_service: "external".to_string(),
        }
    }

    fn service(limits: IngestLimits) -> (LogIngestService, Arc<Recorded>) {
        let writer = Arc::new(Recorded::default());
        let service = LogIngestService::new(
            Arc::new(FixedKey(key())),
            writer.clone(),
            Arc::new(IngestBudget::new(limits)),
        );
        (service, writer)
    }

    #[tokio::test]
    async fn a_batch_is_stored_under_the_keys_project_environment_and_service_streams() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let body = b"{\"msg\":\"a\",\"service\":\"web\"}\n{\"msg\":\"b\"}\n";
        let outcome = service.ingest("good", body).await.expect("accepted");
        assert_eq!(outcome.accepted, 2);
        let lines = writer.0.lock().expect("lines lock");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].project_id, 7);
        assert_eq!(lines[0].env, "3");
        assert_eq!(lines[0].service, "web");
        assert_eq!(lines[0].container_id, "ingest-5-web");
        assert_eq!(lines[1].service, "external");
        assert_eq!(lines[1].container_id, "ingest-5-external");
        assert!(lines
            .iter()
            .all(|l| l.deploy_id.is_none() && l.external_service_id.is_none()));
    }

    #[tokio::test]
    async fn a_line_cannot_choose_another_project() {
        // The project, environment and stream come from the key; a record
        // carrying owner-shaped keys keeps them as attributes only.
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let body = br#"{"msg":"x","project_id":99,"env":"1","container_id":"abc"}"#;
        service.ingest("good", body).await.expect("accepted");
        let lines = writer.0.lock().expect("lines lock");
        assert_eq!(lines[0].project_id, 7);
        assert_eq!(lines[0].env, "3");
        assert_eq!(lines[0].container_id, "ingest-5-external");
    }

    #[tokio::test]
    async fn an_unknown_key_stores_nothing() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        assert!(matches!(
            service.ingest("bad", b"{\"msg\":\"a\"}").await,
            Err(LogAggregatorError::IngestKeyRejected)
        ));
        assert!(writer.0.lock().expect("lines lock").is_empty());
    }

    #[tokio::test]
    async fn an_invalid_line_stores_nothing_of_its_batch() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let result = service.ingest("good", b"{\"msg\":\"a\"}\nnope\n").await;
        assert!(matches!(
            result,
            Err(LogAggregatorError::IngestLineInvalid { line: 2, .. })
        ));
        assert!(writer.0.lock().expect("lines lock").is_empty());
    }

    #[tokio::test]
    async fn an_over_budget_batch_is_refused_whole_and_the_gap_is_explained_later() {
        // 2 lines/s with a 10 s burst: 20 lines fit.
        let (service, writer) = service(IngestLimits {
            lines_per_sec: 2,
            bytes_per_sec: 0,
        });
        let batch = "{\"msg\":\"a\"}\n".repeat(20);
        service
            .ingest("good", batch.as_bytes())
            .await
            .expect("first batch fits");
        let refused = service.ingest("good", batch.as_bytes()).await;
        match refused {
            Err(LogAggregatorError::IngestBudgetExceeded {
                project_id,
                accepted,
                total,
                ..
            }) => assert_eq!((project_id, accepted, total), (7, 0, 20)),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            writer.0.lock().expect("lines lock").len(),
            20,
            "nothing of the refused batch was stored"
        );
    }

    #[tokio::test]
    async fn a_key_cannot_open_unbounded_streams() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let body: String = (0..MAX_SERVICES_PER_KEY + 5)
            .map(|i| format!("{{\"msg\":\"m\",\"service\":\"svc{i}\"}}\n"))
            .collect();
        service
            .ingest("good", body.as_bytes())
            .await
            .expect("accepted");
        let lines = writer.0.lock().expect("lines lock");
        let streams: HashSet<&str> = lines.iter().map(|l| l.container_id.as_str()).collect();
        assert_eq!(
            streams.len(),
            MAX_SERVICES_PER_KEY + 1,
            "the cap plus the default stream"
        );
        let folded = lines.iter().filter(|l| l.service == "external").count();
        assert_eq!(folded, 5);
        let kept = lines
            .iter()
            .find(|l| l.service == "external")
            .and_then(|l| l.fields.as_ref())
            .and_then(|f| f.get("ingest_service"))
            .cloned();
        assert!(kept.is_some(), "the requested name is kept as an attribute");
    }

    #[test]
    fn stream_ids_are_file_name_safe() {
        let id = stream_id(12, "nginx.access");
        assert_eq!(id, "ingest-12-nginx.access");
        assert!(!id.contains('/'));
    }
}
