// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Remote journal writes occur under the central run lock, before its transaction commits.
use super::{postgres::stmt, NewEvent, PgWorkflowStore, StepRow};
use crate::{
    error::{WorkflowError, WorkflowResult},
    payload::StoredPayload,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::workflow_journal::{
    ManagedWorkflowJournal, WorkflowJournalError, WorkflowJournalScope,
};
use uuid::Uuid;

#[derive(FromQueryResult)]
struct Placement {
    service_id: i32,
    project_id: i32,
}
#[derive(Serialize, Deserialize)]
struct Event {
    project_id: i32,
    environment_id: i32,
    name: String,
    key: String,
    encrypted: Option<String>,
    blob_key: Option<String>,
    bytes: i32,
    source: String,
    source_run_id: Option<Uuid>,
}
#[derive(Serialize, Deserialize)]
struct Revision {
    entry: StepRow,
    result_encrypted: Option<String>,
    error_encrypted: Option<String>,
    event: Option<Event>,
    #[serde(default)]
    child: Option<(super::NewRun, i64)>,
}
impl PgWorkflowStore {
    pub(super) async fn check_managed(&self, scope: WorkflowJournalScope) -> WorkflowResult<()> {
        self.backend(scope)?
            .check(scope.service_id, scope.project_id)
            .await?;
        Ok(())
    }
    pub fn with_managed_journal(mut self, managed: Arc<dyn ManagedWorkflowJournal>) -> Self {
        self.managed = Some(managed);
        self
    }
    pub(super) async fn placement<C: ConnectionTrait>(
        &self,
        conn: &C,
        run_id: Uuid,
    ) -> WorkflowResult<Option<WorkflowJournalScope>> {
        Ok(Placement::find_by_statement(stmt(
            "SELECT service_id,project_id FROM workflow_managed_runs WHERE run_id=$1",
            vec![run_id.into()],
        ))
        .one(conn)
        .await
        .map_err(WorkflowError::db("resolving managed journal"))?
        .map(|v| WorkflowJournalScope {
            service_id: v.service_id,
            project_id: v.project_id,
            run_id,
        }))
    }
    fn backend(
        &self,
        scope: WorkflowJournalScope,
    ) -> WorkflowResult<&Arc<dyn ManagedWorkflowJournal>> {
        self.managed.as_ref().ok_or_else(|| {
            WorkflowJournalError {
                service_id: scope.service_id,
                operation: "access journal",
                reason: "managed journal provider is unavailable",
            }
            .into()
        })
    }
    /// A remote revision remains after a rolled-back central transaction. Its
    /// first result and event win when that exact attempt is retried.
    pub(super) async fn persist_revision(
        &self,
        txn: &DatabaseTransaction,
        mut entry: StepRow,
        event: &mut Option<NewEvent>,
        child: &mut Option<(super::NewRun, i64)>,
    ) -> WorkflowResult<StepRow> {
        let Some(scope) = self.placement(txn, entry.run_id).await? else {
            return Ok(entry);
        };
        let key = format!(
            "{}:{}:{}:{}",
            entry.sequence, entry.lease_token, entry.attempts, entry.status
        );
        let previous = entry.managed_revision.clone();
        entry.managed_revision = Some(key.clone());
        let revision = Revision {
            child: child.clone(),
            result_encrypted: entry.result_encrypted.clone(),
            error_encrypted: entry.error_encrypted.clone(),
            entry,
            event: event.as_ref().map(|e| Event {
                project_id: e.project_id,
                environment_id: e.environment_id,
                name: e.name.clone(),
                key: e.key.clone(),
                encrypted: e.payload.encrypted.clone(),
                blob_key: e.payload.blob_key.clone(),
                bytes: e.payload.bytes,
                source: e.source.into(),
                source_run_id: e.source_run_id,
            }),
        };
        let encode_error = || {
            WorkflowError::from(WorkflowJournalError {
                service_id: scope.service_id,
                operation: "decode journal revision",
                reason: "stored revision is invalid or has the wrong scope",
            })
        };
        let encoded = serde_json::to_string(&revision).map_err(|_| encode_error())?;
        let stored = self
            .backend(scope)?
            .put_first(scope, &key, previous.as_deref(), &encoded)
            .await?;
        let mut canonical: Revision = serde_json::from_str(&stored).map_err(|_| encode_error())?;
        if canonical.entry.run_id != scope.run_id
            || canonical.entry.managed_revision.as_deref() != Some(&key)
        {
            return Err(encode_error());
        }
        if canonical.entry.name != revision.entry.name
            || canonical.entry.kind != revision.entry.kind
        {
            return Err(WorkflowError::Incompatible {
                run_id: scope.run_id,
                sequence: revision.entry.sequence,
                expected: format!("{}:{}", canonical.entry.kind, canonical.entry.name),
                found: format!("{}:{}", revision.entry.kind, revision.entry.name),
            });
        }
        canonical.entry.result_encrypted = canonical.result_encrypted;
        canonical.entry.error_encrypted = canonical.error_encrypted;
        let max_active = child.as_ref().map(|(_, limit)| *limit);
        *child = canonical
            .child
            .map(|(run, limit)| (run, max_active.unwrap_or(limit)));
        *event = canonical
            .event
            .map(|e| {
                Ok(NewEvent {
                    project_id: e.project_id,
                    environment_id: e.environment_id,
                    name: e.name,
                    key: e.key,
                    payload: StoredPayload {
                        encrypted: e.encrypted,
                        blob_key: e.blob_key,
                        bytes: e.bytes,
                    },
                    source: match e.source.as_str() {
                        "workflow" => "workflow",
                        "api" => "api",
                        "credential" => "credential",
                        _ => return Err(encode_error()),
                    },
                    source_run_id: e.source_run_id,
                })
            })
            .transpose()?;
        // Keep the central metadata index for fencing and quotas; payloads and
        // replay data are read from the pinned remote journal.
        let mut index = canonical.entry.clone();
        index.result_encrypted = None;
        index.error_encrypted = None;
        use sea_orm::{ActiveModelTrait, IntoActiveModel};
        let active = index.into_active_model().reset_all();
        active
            .update(txn)
            .await
            .map_err(WorkflowError::db("indexing managed journal revision"))?;
        Ok(canonical.entry)
    }
    pub(super) async fn hydrate(
        &self,
        run_id: Uuid,
        mut entries: Vec<StepRow>,
    ) -> WorkflowResult<Vec<StepRow>> {
        let keys: Vec<_> = entries
            .iter()
            .filter_map(|e| e.managed_revision.clone())
            .collect();
        if keys.is_empty() {
            return Ok(entries);
        }
        let scope = self
            .placement(self.db.as_ref(), run_id)
            .await?
            .ok_or_else(|| WorkflowError::Validation {
                field: "managed_journal",
                reason: format!("missing placement for run {run_id}"),
            })?;
        let mut decoded = Vec::with_capacity(keys.len());
        for batch in keys.chunks(100) {
            decoded.extend(self.backend(scope)?.read(scope, batch).await?);
        }
        let mut values = decoded.into_iter();
        for entry in &mut entries {
            if entry.managed_revision.is_some() {
                let unavailable = || {
                    WorkflowError::from(WorkflowJournalError {
                        service_id: scope.service_id,
                        operation: "read journal revision",
                        reason: "stored revision is missing, invalid or has the wrong scope",
                    })
                };
                let value = values.next().ok_or_else(unavailable)?;
                let mut revision: Revision =
                    serde_json::from_str(&value).map_err(|_| unavailable())?;
                if revision.entry.run_id != run_id
                    || revision.entry.sequence != entry.sequence
                    || revision.entry.managed_revision != entry.managed_revision
                {
                    return Err(unavailable());
                }
                revision.entry.result_encrypted = revision.result_encrypted;
                revision.entry.error_encrypted = revision.error_encrypted;
                // Resume changes scheduling metadata centrally without rewriting the payload revision.
                revision.entry.wake_at = entry.wake_at;
                *entry = revision.entry;
            }
        }
        Ok(entries)
    }
    pub(super) async fn clean_managed_journals(&self) -> WorkflowResult<()> {
        #[derive(FromQueryResult)]
        struct Pending {
            run_id: Uuid,
            service_id: i32,
            project_id: i32,
        }
        let pending=Pending::find_by_statement(stmt("SELECT run_id,service_id,project_id FROM workflow_journal_cleanup ORDER BY created_at LIMIT 20",vec![])).all(self.db.as_ref()).await.map_err(WorkflowError::db("listing managed journal cleanup"))?;
        for row in pending {
            let scope = WorkflowJournalScope {
                service_id: row.service_id,
                project_id: row.project_id,
                run_id: row.run_id,
            };
            match self.backend(scope)?.delete_run(scope).await {
                Ok(()) => {
                    self.db
                        .execute(stmt(
                            "DELETE FROM workflow_journal_cleanup WHERE run_id=$1",
                            vec![row.run_id.into()],
                        ))
                        .await
                        .map_err(WorkflowError::db("acknowledging managed journal cleanup"))?;
                }
                Err(error) => {
                    tracing::warn!(run_id=%row.run_id,service_id=row.service_id,error=%error,"managed journal cleanup will retry")
                }
            }
        }
        Ok(())
    }
}
