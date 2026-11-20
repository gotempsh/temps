// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The cross-project list of workflow definitions: every workflow an
//! environment has run or its executor declares, named by project and
//! environment slug.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use temps_authz::AuthContext;
use temps_core::ProjectAccessChecker;

use super::{
    global::{listing_is_unrestricted, visible_projects},
    WorkflowService,
};
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
    /// query naming the environments that have a manifest, and, for a caller
    /// limited to some projects, one query for the projects with runs and one
    /// batched access check. Nothing is read per workflow or per environment.
    ///
    /// A limited caller's readable projects are resolved before the history
    /// is bounded, so other projects' workflows never crowd theirs out of the
    /// list or its total. Most recently run first, then never-run ones by
    /// name.
    pub async fn workflow_definitions(
        &self,
        auth: &AuthContext,
        checker: Option<&dyn ProjectAccessChecker>,
    ) -> WorkflowResult<Vec<WorkflowDefinition>> {
        let manifests: BTreeMap<i32, _> = self.engine.cached_manifests().into_iter().collect();
        let manifest_environments: Vec<i32> = manifests.keys().copied().collect();
        let declaring = self.store.environment_names(&manifest_environments).await?;

        let scope: Option<BTreeSet<i32>> = if listing_is_unrestricted(auth, checker) {
            None
        } else {
            let mut candidates = self.store.workflow_project_ids().await?;
            candidates.extend(declaring.iter().map(|environment| environment.project_id));
            Some(visible_projects(auth, checker, &candidates).await?)
        };
        let scoped: Option<Vec<i32>> = scope.as_ref().map(|ids| ids.iter().copied().collect());
        let in_scope = |project_id: i32| scope.as_ref().is_none_or(|ids| ids.contains(&project_id));

        let history = self
            .store
            .global_workflow_summaries(MAX_WORKFLOW_DEFINITIONS, scoped.as_deref())
            .await?;

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
        let mut never_run = Vec::new();
        for environment in declaring {
            if !in_scope(environment.project_id) {
                continue;
            }
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
        definitions.retain(|definition| in_scope(definition.environment.project_id));
        definitions.truncate(MAX_WORKFLOW_DEFINITIONS as usize);
        Ok(definitions)
    }
}
