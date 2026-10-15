// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional hook run as soon as a project is fenced for deletion.
//!
//! Project deletion first marks the project `is_deleted` and only removes its
//! rows after cancelling deployments and cleaning up containers and archives.
//! `Job::ProjectDeleted` is emitted only after that final delete, so a feature
//! that exposes project data publicly (e.g. published status pages) cannot
//! wait for it: if cleanup fails part-way, the job never fires. The projects
//! handler calls this hook right after the fence commits and awaits it before
//! continuing.

use async_trait::async_trait;

#[async_trait]
pub trait ProjectDeletionHook: Send + Sync {
    /// Called after `project_id` is marked deleted, before any cleanup. Also
    /// called again when a previously failed deletion is retried, so it must
    /// be idempotent. Must not fail the deletion: implementations log their
    /// own errors.
    async fn project_deletion_started(&self, project_id: i32);
}
