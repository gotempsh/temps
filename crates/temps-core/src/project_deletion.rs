// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hooks run as soon as a project is fenced for deletion.
//!
//! Project deletion first marks the project `is_deleted` and only removes its
//! rows after cancelling deployments and cleaning up containers and archives.
//! `Job::ProjectDeleted` is emitted only after that final delete, so a feature
//! that exposes project data publicly (e.g. published status pages) or runs
//! something for the project outside the deployment pipeline (e.g. functions
//! on the workload plane) cannot wait for it: if cleanup fails part-way, the
//! job never fires, and once the rows are gone a cascading delete leaves
//! nothing to clean up from. The projects handler runs every registered hook
//! right after the fence commits and awaits them before continuing.
//!
//! Any number of plugins may register a hook: they share one
//! [`ProjectDeletionHooks`] list, created by whichever plugin asks for it
//! first through [`ProjectDeletionHooks::get_or_register`].

use std::fmt;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::plugin::ServiceRegistrationContext;

/// Why a hook could not finish its part of a project deletion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("project deletion hook '{hook}' failed for project {project_id}: {reason}")]
pub struct ProjectDeletionHookError {
    /// The hook's [`ProjectDeletionHook::name`].
    pub hook: String,
    pub project_id: i32,
    pub reason: String,
}

#[async_trait]
pub trait ProjectDeletionHook: Send + Sync {
    /// Names the hook in logs and errors.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Called after `project_id` is marked deleted, before any cleanup. Also
    /// called again when a previously failed deletion is retried, so it must
    /// be idempotent.
    ///
    /// Return an error only when the project's rows must not be deleted yet:
    /// it fails the deletion and leaves the project fenced, and the next
    /// `DELETE` runs every hook again. A hook whose leftovers are harmless
    /// once the rows cascade away should log its own errors and return
    /// `Ok(())`.
    async fn project_deletion_started(
        &self,
        project_id: i32,
    ) -> Result<(), ProjectDeletionHookError>;
}

/// Every failed hook of one project deletion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ProjectDeletionHooksFailed {
    pub project_id: i32,
    /// How many hooks ran.
    pub total: usize,
    pub failures: Vec<ProjectDeletionHookError>,
}

impl fmt::Display for ProjectDeletionHooksFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} of {} project deletion hook(s) failed for project {}: ",
            self.failures.len(),
            self.total,
            self.project_id
        )?;
        for (index, failure) in self.failures.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{failure}")?;
        }
        Ok(())
    }
}

/// The hooks registered by plugins, run in registration order.
#[derive(Default)]
pub struct ProjectDeletionHooks {
    hooks: RwLock<Vec<Arc<dyn ProjectDeletionHook>>>,
}

impl ProjectDeletionHooks {
    pub fn new() -> Self {
        Self::default()
    }

    /// The shared list in `context`, registering an empty one when no plugin
    /// has yet. Plugins register their services one at a time, so the first
    /// caller creates it and every later one gets the same list.
    pub fn get_or_register(context: &ServiceRegistrationContext) -> Arc<Self> {
        if let Some(hooks) = context.get_service::<Self>() {
            return hooks;
        }
        let hooks = Arc::new(Self::new());
        context.register_service(hooks.clone());
        hooks
    }

    pub fn add(&self, hook: Arc<dyn ProjectDeletionHook>) {
        self.hooks
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(hook);
    }

    pub fn len(&self) -> usize {
        self.hooks
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Run every hook for `project_id`, each one even when an earlier one
    /// failed (a failing hook must not keep another feature serving the
    /// project), and report every failure.
    pub async fn run(&self, project_id: i32) -> Result<(), ProjectDeletionHooksFailed> {
        // Cloned so no lock is held across an await.
        let hooks: Vec<Arc<dyn ProjectDeletionHook>> = self
            .hooks
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut failures = Vec::new();
        for hook in &hooks {
            if let Err(error) = hook.project_deletion_started(project_id).await {
                tracing::error!(
                    project_id,
                    hook = hook.name(),
                    %error,
                    "project deletion hook failed; the project stays fenced until a retried delete succeeds"
                );
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(ProjectDeletionHooksFailed {
                project_id,
                total: hooks.len(),
                failures,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Recording {
        name: &'static str,
        fail: bool,
        calls: Arc<Mutex<Vec<(&'static str, i32)>>>,
    }

    #[async_trait]
    impl ProjectDeletionHook for Recording {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn project_deletion_started(
            &self,
            project_id: i32,
        ) -> Result<(), ProjectDeletionHookError> {
            self.calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((self.name, project_id));
            if self.fail {
                return Err(ProjectDeletionHookError {
                    hook: self.name.to_string(),
                    project_id,
                    reason: "workload 9 could not be deleted".to_string(),
                });
            }
            Ok(())
        }
    }

    fn hook(
        name: &'static str,
        fail: bool,
        calls: &Arc<Mutex<Vec<(&'static str, i32)>>>,
    ) -> Arc<dyn ProjectDeletionHook> {
        Arc::new(Recording {
            name,
            fail,
            calls: calls.clone(),
        })
    }

    #[tokio::test]
    async fn every_hook_runs_in_order_and_success_is_ok() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let hooks = ProjectDeletionHooks::new();
        assert!(hooks.is_empty());
        hooks.add(hook("pages", false, &calls));
        hooks.add(hook("functions", false, &calls));
        assert_eq!(hooks.len(), 2);

        hooks.run(7).await.expect("no hook failed");
        assert_eq!(
            *calls.lock().expect("calls"),
            vec![("pages", 7), ("functions", 7)]
        );
    }

    #[tokio::test]
    async fn a_failing_hook_does_not_stop_the_others_and_is_reported_with_context() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let hooks = ProjectDeletionHooks::new();
        hooks.add(hook("functions", true, &calls));
        hooks.add(hook("pages", false, &calls));

        let error = hooks.run(42).await.expect_err("one hook failed");
        assert_eq!(
            *calls.lock().expect("calls"),
            vec![("functions", 42), ("pages", 42)],
            "the hook after the failing one still runs"
        );
        assert_eq!(error.project_id, 42);
        assert_eq!(error.total, 2);
        assert_eq!(error.failures.len(), 1);
        let message = error.to_string();
        assert!(message.contains("1 of 2"), "{message}");
        assert!(message.contains("'functions'"), "{message}");
        assert!(message.contains("project 42"), "{message}");
        assert!(message.contains("workload 9"), "{message}");
    }

    #[tokio::test]
    async fn without_hooks_run_is_ok() {
        ProjectDeletionHooks::new()
            .run(1)
            .await
            .expect("nothing to run");
    }

    #[test]
    fn plugins_share_one_list() {
        let context = ServiceRegistrationContext::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        ProjectDeletionHooks::get_or_register(&context).add(hook("pages", false, &calls));
        ProjectDeletionHooks::get_or_register(&context).add(hook("functions", false, &calls));
        let shared = context
            .get_service::<ProjectDeletionHooks>()
            .expect("registered");
        assert_eq!(shared.len(), 2);
    }
}
