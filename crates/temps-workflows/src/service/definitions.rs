// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The cross-project list of workflow definitions: every workflow an
//! environment has run or its executor declares, named by project and
//! environment slug.

use std::cmp::Reverse;
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
use crate::store::{EnvironmentName, EnvironmentWorkflowSummary, WorkflowStore};

/// At most this many (environment, workflow) pairs are listed; the run
/// history query is bounded by it and so is the response.
pub const MAX_WORKFLOW_DEFINITIONS: u64 = 1_000;

/// Projects read, access-checked and summarized per batch when scoping a
/// caller limited to some projects.
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
    /// Workflows the caller may read across projects: grouped run history,
    /// the in-memory manifests for what executors declare, and one query
    /// naming the environments that have a manifest. Nothing is read per
    /// workflow or per environment.
    ///
    /// An unrestricted caller's history is one query. A caller limited to
    /// some projects has every project with runs walked in batches: each
    /// batch is access-checked, its readable projects' history read, and the
    /// newest kept, so other projects' workflows never crowd theirs out and
    /// a recent workflow in a high-numbered project is never cut. Most
    /// recently run first, then never-run ones by name.
    pub async fn workflow_definitions(
        &self,
        auth: &AuthContext,
        checker: Option<&dyn ProjectAccessChecker>,
    ) -> WorkflowResult<Vec<WorkflowDefinition>> {
        let manifests: BTreeMap<i32, _> = self.engine.cached_manifests().into_iter().collect();
        let manifest_environments: Vec<i32> = manifests.keys().copied().collect();
        let declaring = self.store.environment_names(&manifest_environments).await?;

        // Which declaring projects the caller may read; `None` is all.
        let (scope, history): (Option<BTreeSet<i32>>, _) = if listing_is_unrestricted(auth, checker)
        {
            let history = self
                .store
                .global_workflow_summaries(MAX_WORKFLOW_DEFINITIONS, None)
                .await?;
            (None, history)
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
            let history =
                newest_readable_history(&scan, PROJECT_BATCH, MAX_WORKFLOW_DEFINITIONS).await?;
            (Some(visible), history)
        };
        let in_scope = |project_id: i32| scope.as_ref().is_none_or(|ids| ids.contains(&project_id));

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
        definitions.truncate(MAX_WORKFLOW_DEFINITIONS as usize);
        Ok(definitions)
    }
}

/// The projects with workflow runs, one page at a time, which of them a
/// caller may read, and their newest workflow history.
trait ProjectScan {
    fn page(&self, after: i32, limit: u64)
        -> impl Future<Output = WorkflowResult<Vec<i32>>> + Send;
    fn readable(
        &self,
        page: Vec<i32>,
    ) -> impl Future<Output = WorkflowResult<BTreeSet<i32>>> + Send;
    fn history(
        &self,
        projects: Vec<i32>,
        limit: u64,
    ) -> impl Future<Output = WorkflowResult<Vec<EnvironmentWorkflowSummary>>> + Send;
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
    async fn history(
        &self,
        projects: Vec<i32>,
        limit: u64,
    ) -> WorkflowResult<Vec<EnvironmentWorkflowSummary>> {
        self.store
            .global_workflow_summaries(limit, Some(&projects))
            .await
    }
}

/// The history query's order: most recently run first, then by name.
fn recency(row: &EnvironmentWorkflowSummary) -> impl Ord + '_ {
    (
        Reverse(row.summary.last_run_at),
        &row.summary.workflow_name,
        row.environment.project_id,
        row.environment.environment_id,
    )
}

/// The newest `limit` workflows among the projects the caller may read.
/// Walks every project with runs page by page (ascending id), checks each
/// page's access and reads only its readable projects' history, keeping the
/// newest `limit` rows across pages. Project ids say nothing about recency,
/// so the walk never stops early; each access check and history query covers
/// at most `batch` projects and at most `2 * limit` rows are held.
async fn newest_readable_history(
    scan: &impl ProjectScan,
    batch: u64,
    limit: u64,
) -> WorkflowResult<Vec<EnvironmentWorkflowSummary>> {
    let keep = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut newest: Vec<EnvironmentWorkflowSummary> = Vec::new();
    let mut after = 0;
    loop {
        let page = scan.page(after, batch).await?;
        let Some(&last) = page.last() else {
            break;
        };
        after = last;
        let full = page.len() as u64 >= batch;
        let readable: Vec<i32> = scan.readable(page).await?.into_iter().collect();
        if !readable.is_empty() {
            newest.extend(scan.history(readable, limit).await?);
            newest.sort_by(|a, b| recency(a).cmp(&recency(b)));
            newest.truncate(keep);
        }
        if !full {
            break;
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use chrono::{DateTime, TimeZone, Utc};

    use super::{newest_readable_history, ProjectScan};
    use crate::error::{WorkflowError, WorkflowResult};
    use crate::store::{EnvironmentName, EnvironmentWorkflowSummary, WorkflowSummary};

    /// Projects 1..=n with one workflow each, last run at `ran(id)` minutes.
    /// Records the size of each access check and each history read.
    struct Fake {
        n: i32,
        readable: fn(i32) -> bool,
        ran: fn(i32) -> i64,
        fail: bool,
        checked: Mutex<Vec<usize>>,
        summarized: Mutex<Vec<usize>>,
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_767_225_600 + minutes * 60, 0).unwrap()
    }

    fn row(project_id: i32, last_run_at: DateTime<Utc>) -> EnvironmentWorkflowSummary {
        EnvironmentWorkflowSummary {
            environment: EnvironmentName {
                project_id,
                environment_id: project_id * 10,
                project_slug: format!("project-{project_id}"),
                environment_slug: "production".into(),
            },
            summary: WorkflowSummary {
                workflow_name: format!("workflow-{project_id}"),
                active: 0,
                completed: 1,
                failed: 0,
                last_run_at: Some(last_run_at),
            },
        }
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
            Ok(page.into_iter().filter(|id| (self.readable)(*id)).collect())
        }
        async fn history(
            &self,
            projects: Vec<i32>,
            limit: u64,
        ) -> WorkflowResult<Vec<EnvironmentWorkflowSummary>> {
            self.summarized.lock().unwrap().push(projects.len());
            // As the query does: newest first, at most `limit` rows.
            let mut rows: Vec<_> = projects
                .into_iter()
                .map(|id| row(id, at((self.ran)(id))))
                .collect();
            rows.sort_by_key(|row| std::cmp::Reverse(row.summary.last_run_at));
            rows.truncate(limit as usize);
            Ok(rows)
        }
    }

    fn fake(n: i32) -> Fake {
        Fake {
            n,
            readable: |id| id % 2 == 0,
            ran: i64::from,
            fail: false,
            checked: Mutex::new(Vec::new()),
            summarized: Mutex::new(Vec::new()),
        }
    }

    fn projects(rows: &[EnvironmentWorkflowSummary]) -> Vec<i32> {
        rows.iter().map(|row| row.environment.project_id).collect()
    }

    #[tokio::test]
    async fn every_project_is_checked_and_summarized_in_bounded_batches() {
        let scan = fake(23);
        let newest = newest_readable_history(&scan, 10, 1_000).await.unwrap();
        // Only readable projects, newest first.
        assert_eq!(
            projects(&newest),
            (2..=22).rev().step_by(2).collect::<Vec<_>>()
        );
        // No access check or history read is ever larger than one batch.
        assert_eq!(*scan.checked.lock().unwrap(), vec![10, 10, 3]);
        assert_eq!(*scan.summarized.lock().unwrap(), vec![5, 5, 1]);
    }

    #[tokio::test]
    async fn the_newest_workflow_is_kept_whichever_project_it_is_in() {
        // 1,001 readable projects, the newest run in the last one: the
        // first 1,000 projects alone would fill the list.
        let scan = Fake {
            readable: |_| true,
            ..fake(1_001)
        };
        let newest = newest_readable_history(&scan, 500, 1_000).await.unwrap();
        assert_eq!(newest.len(), 1_000);
        assert_eq!(newest[0].environment.project_id, 1_001);
        // The oldest one is the one left out.
        assert!(!projects(&newest).contains(&1));
        assert_eq!(*scan.checked.lock().unwrap(), vec![500, 500, 1]);
    }

    #[tokio::test]
    async fn rows_from_every_batch_are_merged_newest_first() {
        let scan = Fake {
            // Recency unrelated to project order.
            ran: |id| i64::from((id * 7) % 31),
            ..fake(30)
        };
        let newest = newest_readable_history(&scan, 4, 5).await.unwrap();
        let mut expected: Vec<i32> = (2..=30).step_by(2).collect();
        expected.sort_by_key(|id| std::cmp::Reverse((id * 7) % 31));
        expected.truncate(5);
        assert_eq!(projects(&newest), expected);
    }

    #[tokio::test]
    async fn the_same_result_when_every_project_fits_in_one_batch() {
        let scan = fake(8);
        let newest = newest_readable_history(&scan, 500, 1_000).await.unwrap();
        assert_eq!(projects(&newest), vec![8, 6, 4, 2]);
        assert_eq!(*scan.summarized.lock().unwrap(), vec![4]);
    }

    #[tokio::test]
    async fn a_batch_with_nothing_readable_reads_no_history() {
        let scan = Fake {
            readable: |_| false,
            ..fake(25)
        };
        assert!(newest_readable_history(&scan, 10, 1_000)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(scan.checked.lock().unwrap().len(), 3);
        assert!(scan.summarized.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_projects_checks_nothing() {
        let scan = fake(0);
        assert!(newest_readable_history(&scan, 10, 1_000)
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
        assert!(newest_readable_history(&scan, 10, 1_000).await.is_err());
    }
}
