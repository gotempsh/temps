// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Event delivery (ADR-103 §8).
//!
//! An event is stored first, then delivered to every run of the environment
//! waiting on its name and key: the payload is re-encrypted under each run's
//! wait entry and the entry completed with a compare-and-set, so a delivery
//! repeated by a crash, a second process or the reconciliation pass is a
//! no-op. A run that suspends on an event already received picks it up when
//! it suspends ([`WorkflowEngine::deliver_buffered`]); deliveries a crash
//! interrupted are found by [`WorkflowEngine::reconcile_waits`].

use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use super::WorkflowEngine;
use crate::error::{WorkflowError, WorkflowResult};
use crate::payload::{PayloadField, StoredPayload};
use crate::protocol::wait_result;
use crate::store::{EventRow, NewEvent, WaitingRun};
use crate::types::{validate_key, validate_name, WorkflowSettings};

/// Waiting runs handled per page of a delivery.
const DELIVERY_PAGE: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventDelivery {
    pub delivered: u64,
}

/// An event to store.
#[derive(Debug, Clone, Copy)]
pub struct EventSpec<'a> {
    pub project_id: i32,
    pub environment_id: i32,
    pub name: &'a str,
    /// Empty for an event without a key.
    pub key: &'a str,
    pub payload: Option<&'a Value>,
    /// `api`, `credential` or `workflow`.
    pub source: &'static str,
    pub source_run_id: Option<Uuid>,
}

/// Event payloads have no blob column: they are always inline.
pub(crate) fn inline_only(settings: &WorkflowSettings) -> WorkflowSettings {
    WorkflowSettings {
        max_payload_bytes: settings.inline_payload_limit_bytes,
        ..settings.clone()
    }
}

impl WorkflowEngine {
    /// Validate, encrypt and store an event; returns it for delivery.
    pub(crate) async fn store_event(&self, spec: EventSpec<'_>) -> WorkflowResult<EventRow> {
        let event = self.seal_event(spec).await?;
        let id = self.store.insert_event(event).await?;
        self.store
            .event(id)
            .await?
            .ok_or(WorkflowError::Validation {
                field: "event",
                reason: format!("event {id} disappeared right after it was stored"),
            })
    }

    pub(crate) async fn seal_event(&self, spec: EventSpec<'_>) -> WorkflowResult<NewEvent> {
        validate_name("event name", spec.name, 200)?;
        if !spec.key.is_empty() {
            validate_key("event key", spec.key)?;
        }
        let settings = inline_only(&self.store.settings(spec.environment_id).await?);
        let sealed = match spec.payload {
            Some(payload) => {
                self.codec
                    .seal(
                        &Self::event_scope(spec.project_id, spec.environment_id),
                        &PayloadField::Event {
                            name: spec.name.to_string(),
                            key: spec.key.to_string(),
                        },
                        payload,
                        0,
                        &settings,
                    )
                    .await?
            }
            None => StoredPayload::default(),
        };
        Ok(NewEvent {
            project_id: spec.project_id,
            environment_id: spec.environment_id,
            name: spec.name.to_string(),
            key: spec.key.to_string(),
            payload: sealed,
            source: spec.source,
            source_run_id: spec.source_run_id,
        })
    }

    async fn open_event(&self, event: &EventRow) -> WorkflowResult<Option<Value>> {
        let settings = inline_only(&self.store.settings(event.environment_id).await?);
        self.codec
            .open(
                &Self::event_scope(event.project_id, event.environment_id),
                &PayloadField::Event {
                    name: event.name.clone(),
                    key: event.event_key.clone(),
                },
                &StoredPayload {
                    encrypted: event.payload_encrypted.clone(),
                    blob_key: None,
                    bytes: event.payload_bytes,
                },
                &settings,
            )
            .await
    }

    /// Deliver a stored event to every run waiting on it.
    pub async fn deliver_event(&self, event: &EventRow) -> WorkflowResult<EventDelivery> {
        let payload = self.open_event(event).await?;
        let mut delivery = EventDelivery::default();
        let mut after = None;
        loop {
            let page = self
                .store
                .waiting_runs(
                    event.environment_id,
                    &event.name,
                    &event.event_key,
                    after,
                    DELIVERY_PAGE,
                )
                .await?;
            let Some(last) = page.last() else {
                break;
            };
            after = Some(last.run_id);
            let full = page.len() as u64 == DELIVERY_PAGE;
            for waiting in &page {
                match self.deliver_to(waiting, event.id, payload.clone()).await {
                    Ok(true) => delivery.delivered += 1,
                    Ok(false) => {}
                    Err(error) => tracing::warn!(
                        run_id = %waiting.run_id,
                        event_id = event.id,
                        %error,
                        "could not deliver a workflow event; reconciliation retries it"
                    ),
                }
            }
            if !full {
                break;
            }
        }
        tracing::info!(
            project_id = event.project_id,
            environment_id = event.environment_id,
            event_id = event.id,
            event_name = %event.name,
            delivered = delivery.delivered,
            "workflow event delivered"
        );
        Ok(delivery)
    }

    /// Store an event then deliver it in the background, bounded by the
    /// caller's own rate (one task per accepted event).
    pub(crate) fn deliver_in_background(self: &Arc<Self>, event: EventRow) {
        let engine = self.clone();
        tokio::spawn(async move {
            if let Err(error) = engine.deliver_event(&event).await {
                tracing::warn!(event_id = event.id, %error, "workflow event delivery failed; reconciliation retries it");
            }
        });
    }

    /// Complete one run's wait with `payload`. False when the wait was no
    /// longer pending (delivered already, timed out, run cancelled).
    async fn deliver_to(
        &self,
        waiting: &WaitingRun,
        event_id: i64,
        payload: Option<Value>,
    ) -> WorkflowResult<bool> {
        let settings = self.store.settings(waiting.environment_id).await?;
        let scope = crate::payload::PayloadScope {
            project_id: waiting.project_id,
            environment_id: waiting.environment_id,
            run_id: waiting.run_id,
        };
        let sealed = self
            .codec
            .seal(
                &scope,
                &PayloadField::StepResult(waiting.sequence),
                &wait_result(payload),
                // Blob keys must differ per write; the event id does.
                event_id,
                &settings,
            )
            .await?;
        let completed = self
            .store
            .complete_wait(
                waiting.run_id,
                waiting.sequence,
                Some(event_id),
                sealed.clone(),
            )
            .await;
        match completed {
            Ok(true) => Ok(true),
            Ok(false) => {
                self.codec.discard(waiting.project_id, &sealed).await;
                Ok(false)
            }
            Err(error) => {
                self.codec.discard(waiting.project_id, &sealed).await;
                Err(error)
            }
        }
    }

    /// A run just suspended on `name`/`key`: deliver an event it already
    /// received, if any.
    pub(crate) async fn deliver_buffered(&self, waiting: &WaitingRun) -> WorkflowResult<bool> {
        let Some(event) = self
            .store
            .buffered_event(waiting.run_id, &waiting.event_name, &waiting.event_key)
            .await?
        else {
            return Ok(false);
        };
        let payload = self.open_event(&event).await?;
        self.deliver_to(waiting, event.id, payload).await
    }

    /// Deliver events whose delivery a crash interrupted. Returns how many
    /// runs were woken.
    pub async fn reconcile_waits(&self, limit: u64) -> WorkflowResult<u64> {
        let mut woken = 0;
        for waiting in self.store.undelivered_waits(limit).await? {
            match self.deliver_buffered(&waiting).await {
                Ok(true) => woken += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(run_id = %waiting.run_id, %error, "could not reconcile a workflow wait")
                }
            }
        }
        if woken > 0 {
            tracing::info!(woken, "reconciled interrupted workflow event deliveries");
        }
        Ok(woken)
    }
}
