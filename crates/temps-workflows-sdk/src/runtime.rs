// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{Child, Entry, Error, Event, Invocation, Outcome, Transport, Write};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct StepOptions {
    pub retries: u32,
    pub timeout: Option<Duration>,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}
impl Default for StepOptions {
    fn default() -> Self {
        Self {
            retries: 3,
            timeout: None,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(300),
        }
    }
}
pub struct Context {
    pub input: Option<Value>,
    pub run_id: String,
    pub workflow: String,
    pub attempt: i32,
    pub(crate) request: Invocation,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) token: String,
    sequence: i32,
    journal: HashMap<i32, Entry>,
    cursor: Option<i32>,
    started: Instant,
    pub(crate) stopped: Arc<AtomicU16>,
    pub(crate) outcome: Option<Outcome>,
}
impl Context {
    pub(crate) fn new(
        mut request: Invocation,
        token: String,
        transport: Arc<dyn Transport>,
    ) -> Self {
        let journal = std::mem::take(&mut request.journal)
            .into_iter()
            .map(|entry| (entry.sequence, entry))
            .collect();
        Self {
            input: request.input.take(),
            run_id: request.run_id.clone(),
            workflow: request.workflow.clone(),
            attempt: request.attempt,
            cursor: request.journal_next_sequence,
            request,
            transport,
            token,
            sequence: 0,
            journal,
            started: Instant::now(),
            stopped: Arc::new(AtomicU16::new(0)),
            outcome: None,
        }
    }
    fn clock(&self) -> DateTime<Utc> {
        self.request.now + chrono::Duration::from_std(self.started.elapsed()).unwrap_or_default()
    }
    fn suspend(&mut self, outcome: Outcome) -> Error {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
        Error::Control
    }
    async fn call(
        &mut self,
        method: &str,
        suffix: &str,
        body: Option<Value>,
    ) -> Result<Value, Error> {
        if self.outcome.is_some() || self.stopped.load(Ordering::Acquire) != 0 {
            return Err(Error::Control);
        }
        let reply = self
            .transport
            .call(
                method,
                &format!("/workflow-runs/{}{suffix}", self.run_id),
                &self.token,
                body,
                true,
            )
            .await;
        if let Err(Error::Api { status, .. }) = &reply {
            if *status == 409 {
                self.stopped.store(202, Ordering::Release);
                return Err(Error::Control);
            }
            if *status == 0 || *status >= 500 {
                self.stopped.store(503, Ordering::Release);
                return Err(Error::Control);
            }
        }
        reply
    }
    async fn next(&mut self, name: &str, kind: &str) -> Result<(i32, Option<Entry>), Error> {
        valid_name(name, 200)?;
        if self.outcome.is_some() || self.stopped.load(Ordering::Acquire) != 0 {
            return Err(Error::Control);
        }
        let sequence = self.sequence;
        self.sequence += 1;
        while let Some(cursor) = self.cursor.filter(|cursor| sequence >= *cursor) {
            #[derive(serde::Deserialize)]
            struct Page {
                entries: Vec<Entry>,
                next_sequence: Option<i32>,
            }
            let reply = self
                .call("GET", &format!("/journal?from={cursor}"), None)
                .await?;
            let page: Page = serde_json::from_value(reply).map_err(|_| Error::Encoding {
                operation: "read journal".into(),
            })?;
            if page
                .next_sequence
                .is_some_and(|next| next <= cursor || next > 100_000)
            {
                self.stopped.store(503, Ordering::Release);
                return Err(Error::Control);
            }
            self.journal = page
                .entries
                .into_iter()
                .map(|entry| (entry.sequence, entry))
                .collect();
            self.cursor = page.next_sequence;
        }
        let entry = self.journal.remove(&sequence);
        if let Some(entry) = &entry {
            if entry.name != name || entry.kind != kind {
                return Err(self.suspend(Outcome::Incompatible {
                    sequence,
                    expected: format!("{} '{}'", entry.kind, entry.name),
                    found: format!("{kind} '{name}'"),
                }));
            }
        }
        Ok((sequence, entry))
    }
    fn fresh(sequence: i32, name: &str, kind: &str) -> Entry {
        Entry {
            sequence,
            name: name.into(),
            kind: kind.into(),
            status: "completed".into(),
            attempts: 1,
            result: None,
            error: None,
            wake_at: None,
        }
    }
    async fn record(
        &mut self,
        entry: Entry,
        event: Option<Event>,
        child: Option<Child>,
        duration_ms: Option<i64>,
    ) -> Result<Entry, Error> {
        let body = serde_json::to_value(Write {
            entry,
            event,
            child,
            duration_ms,
        })
        .map_err(|_| Error::Encoding {
            operation: "write step".into(),
        })?;
        let reply = self.call("POST", "/steps", Some(body)).await?;
        #[derive(serde::Deserialize)]
        struct Reply {
            entry: Entry,
        }
        serde_json::from_value::<Reply>(reply)
            .map(|reply| reply.entry)
            .map_err(|_| Error::Encoding {
                operation: "read step acknowledgment".into(),
            })
    }
    fn value(entry: Entry) -> Result<Value, Error> {
        if entry.status == "failed" {
            Err(Error::RecordedStep {
                sequence: entry.sequence,
                name: entry.name,
                message: entry.error.unwrap_or_else(|| "recorded failure".into()),
            })
        } else {
            Ok(entry.result.unwrap_or(Value::Null))
        }
    }
    pub async fn step<F, Fut>(
        &mut self,
        name: &str,
        options: StepOptions,
        body: F,
    ) -> Result<Value, Error>
    where
        F: FnOnce(u32) -> Fut,
        Fut: Future<Output = Result<Value, Error>>,
    {
        let (sequence, old) = self.next(name, "step").await?;
        if let Some(entry) = old.as_ref().filter(|entry| entry.status != "pending") {
            return Self::value(entry.clone());
        }
        let margin = (self.request.deadline_at - self.request.now)
            .num_seconds()
            .clamp(0, 120)
            / 4;
        if self.clock() >= self.request.deadline_at - chrono::Duration::seconds(margin) {
            return Err(self.suspend(Outcome::Retry {
                sequence,
                at: self.clock(),
            }));
        }
        let attempt = old.map_or(1, |entry| entry.attempts.saturating_add(1));
        let started = Instant::now();
        let result = match options.timeout {
            Some(timeout) => match tokio::time::timeout(timeout, body(attempt as u32)).await {
                Ok(result) => result,
                Err(_) => Err(Error::Step {
                    name: name.into(),
                    message: "attempt timed out".into(),
                }),
            },
            None => body(attempt as u32).await,
        };
        let mut entry = Self::fresh(sequence, name, "step");
        entry.attempts = attempt;
        match result {
            Ok(value) => entry.result = Some(value),
            Err(Error::Control) => return Err(Error::Control),
            Err(error) => {
                entry.error = Some(error.to_string().chars().take(16000).collect());
                entry.status = if attempt as u32 > options.retries {
                    "failed"
                } else {
                    "pending"
                }
                .into();
            }
        }
        let entry = self
            .record(
                entry,
                None,
                None,
                Some(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)),
            )
            .await?;
        if entry.status == "pending" {
            let delay = options
                .initial_backoff
                .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1).min(30) as u32))
                .min(options.max_backoff)
                .min(Duration::from_secs(300));
            return Err(self.suspend(Outcome::Retry {
                sequence,
                at: self.clock() + chrono::Duration::from_std(delay).unwrap_or_default(),
            }));
        }
        Self::value(entry)
    }
    fn deadline(&self, duration: Duration) -> Result<DateTime<Utc>, Error> {
        chrono::Duration::from_std(duration)
            .ok()
            .and_then(|duration| self.clock().checked_add_signed(duration))
            .ok_or_else(|| Error::Configuration {
                reason: "workflow duration exceeds the supported timestamp range".into(),
            })
    }
    pub async fn sleep(&mut self, name: &str, duration: Duration) -> Result<(), Error> {
        let until = self.deadline(duration)?;
        self.sleep_until(name, until).await
    }
    pub async fn sleep_until(&mut self, name: &str, until: DateTime<Utc>) -> Result<(), Error> {
        let (sequence, old) = self.next(name, "sleep").await?;
        if old
            .as_ref()
            .is_some_and(|entry| entry.status == "completed")
        {
            return Ok(());
        }
        let until = old
            .as_ref()
            .and_then(|entry| entry.wake_at)
            .unwrap_or(until);
        let mut entry = Self::fresh(sequence, name, "sleep");
        entry.wake_at = Some(until);
        if until > self.clock() {
            entry.status = "pending".into();
        }
        let entry = self.record(entry, None, None, None).await?;
        if entry.status == "completed" {
            Ok(())
        } else {
            Err(self.suspend(Outcome::Sleep { sequence, until }))
        }
    }
    pub async fn wait_for_event(
        &mut self,
        name: &str,
        event: &str,
        key: Option<String>,
        timeout: Option<Duration>,
    ) -> Result<Value, Error> {
        valid_name(event, 200)?;
        let (sequence, old) = self.next(name, "wait").await?;
        if let Some(entry) = old.as_ref().filter(|entry| entry.status == "completed") {
            return Self::value(entry.clone());
        }
        let deadline = if let Some(entry) = &old {
            entry.wake_at
        } else {
            timeout.map(|d| self.deadline(d)).transpose()?
        };
        let mut entry = Self::fresh(sequence, name, "wait");
        entry.wake_at = deadline;
        if deadline.is_some_and(|at| at <= self.clock()) {
            entry.result = Some(json!({"received":false,"payload":null}));
            return Self::value(self.record(entry, None, None, None).await?);
        }
        if old.is_none() {
            entry.status = "pending".into();
            let saved = self.record(entry, None, None, None).await?;
            if saved.status == "completed" {
                return Self::value(saved);
            }
        }
        Err(self.suspend(Outcome::Wait {
            sequence,
            event: event.into(),
            key,
            timeout_at: deadline,
        }))
    }
    pub async fn send_event(&mut self, name: &str, event: Event) -> Result<Value, Error> {
        let (sequence, old) = self.next(name, "send_event").await?;
        if let Some(entry) = old {
            return Self::value(entry);
        }
        let entry = Self::fresh(sequence, name, "send_event");
        Self::value(self.record(entry, Some(event), None, None).await?)
    }
    pub async fn child(
        &mut self,
        name: &str,
        workflow: &str,
        input: Option<Value>,
    ) -> Result<Value, Error> {
        valid_name(workflow, 128)?;
        let (sequence, old) = self.next(name, "child").await?;
        let entry = match old {
            Some(entry) => entry,
            None => {
                let mut entry = Self::fresh(sequence, name, "child");
                entry.status = "pending".into();
                self.record(
                    entry,
                    None,
                    Some(Child {
                        workflow: workflow.into(),
                        input,
                    }),
                    None,
                )
                .await?
            }
        };
        if entry.status == "completed" {
            return Self::value(entry);
        }
        let id = entry
            .result
            .as_ref()
            .and_then(|v| v.get("run_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Encoding {
                operation: "child result missing run ID".into(),
            })?
            .to_string();
        Err(self.suspend(Outcome::Wait {
            sequence,
            event: "temps.child".into(),
            key: Some(id),
            timeout_at: None,
        }))
    }
    pub async fn now(&mut self, name: &str) -> Result<Value, Error> {
        let (sequence, old) = self.next(name, "now").await?;
        if let Some(entry) = old {
            return Self::value(entry);
        }
        let mut entry = Self::fresh(sequence, name, "now");
        entry.result = Some(json!(self.clock().to_rfc3339()));
        Self::value(self.record(entry, None, None, None).await?)
    }
    pub async fn random(&mut self, name: &str) -> Result<Value, Error> {
        let (sequence, old) = self.next(name, "random").await?;
        if let Some(entry) = old {
            return Self::value(entry);
        }
        let mut entry = Self::fresh(sequence, name, "random");
        entry.result = Some(json!(rand::random::<f64>()));
        Self::value(self.record(entry, None, None, None).await?)
    }
}
pub(crate) fn valid_name(name: &str, max: usize) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > max
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b))
    {
        return Err(Error::Configuration {
            reason: format!("workflow name '{name}' must contain 1..{max} identifier characters"),
        });
    }
    Ok(())
}
