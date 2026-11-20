// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The cross-project list of workflow definitions: every workflow an
//! environment has run or its executor declares, named by project and
//! environment slug.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use temps_authz::AuthContext;
use temps_core::ProjectAccessChecker;

use super::{global::visible_projects, WorkflowService};
use crate::error::WorkflowResult;
use crate::store::EnvironmentName;

/// At most this many (environment, workflow) pairs are listed; the run
/// history query is bounded by it and so is the response.
pub const MAX_WORKFLOW_DEFINITIONS: u64 = 1_000;

/// Whether the environment's current executor declares a workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionState {
    /// The cached manifest lists it.
    Available,
    /// The cached manifest does not list it: only its run history remains.
    Missing,
    /// No manifest has been read for the environment since this server
    /// started; opening the environment's workflows reads it.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDefinition {
    pub environment: EnvironmentName,
    pub name: String,
    pub version: Option<String>,
    pub concurrency: Option<u32>,
    pub state: DefinitionState,
    pub active_runs: i64,
    pub completed_runs: i64,
    pub failed_runs: i64,
    pub last_run_at: Option<DateTime<Utc>>,
}

impl WorkflowService {
    /// Workflows the caller may read across projects: one grouped query for
    /// run history, the in-memory manifests for what executors declare, one
    /// query naming environments that have a manifest but no runs yet, and
    /// one batched access check. Nothing is read per workflow or per
    /// environment. Most recently run first, then never-run ones by name.
    pub async fn workflow_definitions(
        &self,
        auth: &AuthContext,
        checker: Option<&dyn ProjectAccessChecker>,
    ) -> WorkflowResult<Vec<WorkflowDefinition>> {
        let history = self
            .store
            .global_workflow_summaries(MAX_WORKFLOW_DEFINITIONS)
            .await?;
        let manifests: BTreeMap<i32, _> = self.engine.cached_manifests().into_iter().collect();

        let mut definitions: Vec<WorkflowDefinition> = Vec::with_capacity(history.len());
        let mut listed: HashSet<(i32, String)> = HashSet::with_capacity(history.len());
        for row in history {
            let declared = manifests
                .get(&row.environment.environment_id)
                .map(|manifest| {
                    manifest
                        .workflows
                        .iter()
                        .find(|workflow| workflow.name == row.summary.workflow_name)
                });
            let (state, version, concurrency) = match declared {
                Some(Some(workflow)) => (
                    DefinitionState::Available,
                    workflow.version.clone(),
                    workflow.concurrency,
                ),
                Some(None) => (DefinitionState::Missing, None, None),
                None => (DefinitionState::Unknown, None, None),
            };
            listed.insert((
                row.environment.environment_id,
                row.summary.workflow_name.clone(),
            ));
            definitions.push(WorkflowDefinition {
                environment: row.environment,
                name: row.summary.workflow_name,
                version,
                concurrency,
                state,
                active_runs: row.summary.active,
                completed_runs: row.summary.completed,
                failed_runs: row.summary.failed,
                last_run_at: row.summary.last_run_at,
            });
        }

        // Declared workflows that have never run.
        let unnamed: Vec<i32> = manifests
            .iter()
            .filter(|(environment_id, manifest)| {
                manifest
                    .workflows
                    .iter()
                    .any(|workflow| !listed.contains(&(**environment_id, workflow.name.clone())))
            })
            .map(|(environment_id, _)| *environment_id)
            .collect();
        let mut never_run = Vec::new();
        for environment in self.store.environment_names(&unnamed).await? {
            let Some(manifest) = manifests.get(&environment.environment_id) else {
                continue;
            };
            for workflow in &manifest.workflows {
                if listed.contains(&(environment.environment_id, workflow.name.clone())) {
                    continue;
                }
                never_run.push(WorkflowDefinition {
                    environment: environment.clone(),
                    name: workflow.name.clone(),
                    version: workflow.version.clone(),
                    concurrency: workflow.concurrency,
                    state: DefinitionState::Available,
                    active_runs: 0,
                    completed_runs: 0,
                    failed_runs: 0,
                    last_run_at: None,
                });
            }
        }
        never_run.sort_by(|a, b| {
            (
                &a.environment.project_slug,
                &a.environment.environment_slug,
                &a.name,
            )
                .cmp(&(
                    &b.environment.project_slug,
                    &b.environment.environment_slug,
                    &b.name,
                ))
        });
        definitions.extend(never_run);
        definitions.truncate(MAX_WORKFLOW_DEFINITIONS as usize);

        let project_ids: Vec<i32> = definitions
            .iter()
            .map(|definition| definition.environment.project_id)
            .collect();
        let visible = visible_projects(auth, checker, &project_ids).await?;
        definitions.retain(|definition| visible.contains(&definition.environment.project_id));
        Ok(definitions)
    }
}
