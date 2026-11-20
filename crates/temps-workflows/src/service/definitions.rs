// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The cross-project list of workflow definitions: every workflow an
//! environment has run or its executor declares, named by project and
//! environment slug.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::future::Future;

use chrono::{DateTime, Utc};
use temps_authz::AuthContext;
use temps_core::ProjectAccessChecker;

use super::{
    global::{listing_is_unrestricted, visible_projects},
    WorkflowService,
};
use crate::error::WorkflowResult;
use crate::store::{EnvironmentName, WorkflowStore};

/// At most this many (environment, workflow) pairs are listed; the run
/// history query is bounded by it and so is the response.
pub const MAX_WORKFLOW_DEFINITIONS: u64 = 1_000;

/// Projects read and access-checked per batch when scoping a caller limited
/// to some projects.
const PROJECT_BATCH: u64 = 500;

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
            let declared: Vec<i32> = declaring
                .iter()
                .map(|environment| environment.project_id)
                .collect();
            let mut visible = BTreeSet::new();
            for batch in declared.chunks(PROJECT_BATCH as usize) {
                visible.extend(visible_projects(auth, checker, batch).await?);
            }
            let scan = CallerProjects {
                store: self.store.as_ref(),
                auth,
                checker,
            };
            let with_runs =
                readable_in_batches(&scan, PROJECT_BATCH, MAX_WORKFLOW_DEFINITIONS as usize)
                    .await?;
            visible.extend(with_runs);
            Some(visible)
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

/// The projects with workflow runs, one page at a time, and which of them a
/// caller may read.
trait ProjectScan {
    fn page(&self, after: i32, limit: u64)
        -> impl Future<Output = WorkflowResult<Vec<i32>>> + Send;
    fn readable(
        &self,
        page: Vec<i32>,
    ) -> impl Future<Output = WorkflowResult<BTreeSet<i32>>> + Send;
}

struct CallerProjects<'a> {
    store: &'a dyn WorkflowStore,
    auth: &'a AuthContext,
    checker: Option<&'a dyn ProjectAccessChecker>,
}

impl ProjectScan for CallerProjects<'_> {
    fn page(
        &self,
        after: i32,
        limit: u64,
    ) -> impl Future<Output = WorkflowResult<Vec<i32>>> + Send {
        self.store.workflow_project_ids(after, limit)
    }
    async fn readable(&self, page: Vec<i32>) -> WorkflowResult<BTreeSet<i32>> {
        visible_projects(self.auth, self.checker, &page).await
    }
}

/// Walks project ids page by page (ascending) and keeps the readable ones,
/// so memory and each access check stay bounded by `batch`. Stops once
/// `enough` are readable: every project with runs adds at least one
/// workflow, so more could not change a list capped at `enough`.
async fn readable_in_batches(
    scan: &impl ProjectScan,
    batch: u64,
    enough: usize,
) -> WorkflowResult<BTreeSet<i32>> {
    let mut visible = BTreeSet::new();
    let mut after = 0;
    while visible.len() < enough {
        let page = scan.page(after, batch).await?;
        let Some(&last) = page.last() else {
            break;
        };
        after = last;
        let full = page.len() as u64 >= batch;
        visible.extend(scan.readable(page).await?);
        if !full {
            break;
        }
    }
    Ok(visible)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use super::{readable_in_batches, ProjectScan};
    use crate::error::{WorkflowError, WorkflowResult};

    /// Projects 1..=n; even ids are readable. Records each access check's size.
    struct Fake {
        n: i32,
        fail: bool,
        checked: Mutex<Vec<usize>>,
    }

    impl ProjectScan for Fake {
        async fn page(&self, after: i32, limit: u64) -> WorkflowResult<Vec<i32>> {
            Ok(((after + 1)..=self.n).take(limit as usize).collect())
        }
        async fn readable(&self, page: Vec<i32>) -> WorkflowResult<BTreeSet<i32>> {
            if self.fail {
                return Err(WorkflowError::ListingAccess { user_id: Some(7) });
            }
            self.checked.lock().unwrap().push(page.len());
            Ok(page.into_iter().filter(|id| id % 2 == 0).collect())
        }
    }

    fn fake(n: i32) -> Fake {
        Fake {
            n,
            fail: false,
            checked: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn every_project_is_checked_in_bounded_batches() {
        let scan = fake(23);
        let visible = readable_in_batches(&scan, 10, 1_000).await.unwrap();
        assert_eq!(visible, (2..=22).step_by(2).collect());
        // No access check is ever larger than one batch.
        assert_eq!(*scan.checked.lock().unwrap(), vec![10, 10, 3]);
    }

    #[tokio::test]
    async fn the_scan_stops_once_enough_projects_are_readable() {
        let scan = fake(10_000);
        let visible = readable_in_batches(&scan, 10, 7).await.unwrap();
        assert!(visible.len() >= 7, "{visible:?}");
        assert_eq!(scan.checked.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn no_projects_checks_nothing() {
        let scan = fake(0);
        assert!(readable_in_batches(&scan, 10, 1_000)
            .await
            .unwrap()
            .is_empty());
        assert!(scan.checked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_access_check_fails_the_scan() {
        let scan = Fake {
            fail: true,
            ..fake(5)
        };
        assert!(readable_in_batches(&scan, 10, 1_000).await.is_err());
    }
}
