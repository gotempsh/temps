// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed persistence for encrypted, immutable workflow journal revisions.
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
#[error("Managed workflow journal service {service_id}: {operation} failed ({reason})")]
pub struct WorkflowJournalError {
    pub service_id: i32,
    pub operation: &'static str,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct WorkflowJournalScope {
    pub service_id: i32,
    pub project_id: i32,
    pub run_id: Uuid,
}

#[async_trait]
pub trait ManagedWorkflowJournal: Send + Sync {
    /// Check the managed PostgreSQL service is linked to this project and reachable.
    async fn check(&self, service_id: i32, project_id: i32) -> Result<(), WorkflowJournalError>;
    /// First writer wins, including after the central transaction rolls back.
    async fn put_first(
        &self,
        scope: WorkflowJournalScope,
        key: &str,
        previous: Option<&str>,
        encrypted_record: &str,
    ) -> Result<String, WorkflowJournalError>;
    /// Bounded batch lookup, preserving the requested order. Missing entries are errors.
    async fn read(
        &self,
        scope: WorkflowJournalScope,
        keys: &[String],
    ) -> Result<Vec<String>, WorkflowJournalError>;
    /// Idempotent cleanup, also valid after the project or its service link is deleted.
    async fn delete_run(&self, scope: WorkflowJournalScope) -> Result<(), WorkflowJournalError>;
}
