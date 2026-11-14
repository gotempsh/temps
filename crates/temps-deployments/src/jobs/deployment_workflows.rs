// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A persisted post-deployment job. Retrying it uses the same start keys, so
//! a crash between a workflow start and this job's completion is harmless.
use super::{
    temps_yaml::{read_temps_yaml, TempsYamlLookup},
    RepositoryOutput,
};
use crate::workflow::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use async_trait::async_trait;
use sea_orm::EntityTrait;
use std::{sync::Arc, time::Duration};
use temps_core::{WorkflowControl, WorkflowStart};
use temps_database::DbConnection;
use temps_deployment_logs::{LogLevel, LogService};
use temps_entities::{deployments, environments, projects};

pub struct DeploymentWorkflowsJob {
    pub job_id: String,
    pub download_job_id: String,
    pub dependency: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub db: Arc<DbConnection>,
    pub control: Arc<dyn WorkflowControl>,
    pub log_id: String,
    pub log_service: Arc<LogService>,
}
impl std::fmt::Debug for DeploymentWorkflowsJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeploymentWorkflowsJob")
            .field("job_id", &self.job_id)
            .finish_non_exhaustive()
    }
}
fn validate(names: &[String]) -> Result<(), WorkflowError> {
    if names.len() > 16 {
        return Err(WorkflowError::JobValidationFailed(
            ".temps.yaml deploymentWorkflows accepts at most 16 workflow names".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b))
            || !seen.insert(name)
        {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Deployment workflow '{name}' must be a unique 1–128 character workflow identifier"
            )));
        }
    }
    Ok(())
}
async fn start(
    control: &dyn WorkflowControl,
    project_id: i32,
    environment_id: i32,
    deployment_id: i32,
    name: &str,
) -> Result<temps_core::StartedWorkflowRun, WorkflowError> {
    tokio::time::timeout(Duration::from_secs(30),control.start(WorkflowStart {
        project_id,environment_id,workflow:name.into(),
        input:Some(serde_json::json!({"deployment_id":deployment_id,"project_id":project_id,"environment_id":environment_id})),
        idempotency_key:format!("deployment:{deployment_id}:{name}"),
    })).await.map_err(|_|WorkflowError::JobExecutionFailed(format!("Starting deployment {deployment_id} workflow '{name}' timed out after 30 seconds; retry this job with the same idempotency key")))?
    .map_err(|error|WorkflowError::JobExecutionFailed(error.to_string()))
}
impl DeploymentWorkflowsJob {
    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        self.log_service
            .append_structured_log(&self.log_id, LogLevel::Info, &message)
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Writing deployment workflow job {} log failed: {error}",
                    self.job_id
                ))
            })
    }
}
#[async_trait]
impl WorkflowTask for DeploymentWorkflowsJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }
    fn name(&self) -> &str {
        "Start Deployment Workflows"
    }
    fn description(&self) -> &str {
        "Start durable workflows declared in deploymentWorkflows after successful promotion"
    }
    fn depends_on(&self) -> Vec<String> {
        vec![self.dependency.clone()]
    }
    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let repo = RepositoryOutput::from_context(&context, &self.download_job_id)?;
        let project = projects::Entity::find_by_id(self.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Loading project {} for deployment workflows: {error}",
                    self.project_id
                ))
            })?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed(format!("Project {} not found", self.project_id))
            })?;
        let config = read_temps_yaml(&repo.repo_dir, &project.directory)
            .map_err(|error| WorkflowError::JobValidationFailed(error.to_string()))?;
        let TempsYamlLookup::Found { config, .. } = config else {
            return Ok(JobResult::success(context));
        };
        validate(&config.deployment_workflows)?;
        if config.deployment_workflows.is_empty() {
            return Ok(JobResult::success(context));
        }
        // Skip a job already superseded when it begins. Started runs follow
        // the normal replay-on-current-deployment contract.

        let environment = environments::Entity::find_by_id(self.environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Loading deployment workflow environment {}: {error}",
                    self.environment_id
                ))
            })?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed(format!(
                    "Environment {} not found",
                    self.environment_id
                ))
            })?;
        let deployment = deployments::Entity::find_by_id(context.deployment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Loading deployment {} for workflow start: {error}",
                    context.deployment_id
                ))
            })?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed(format!(
                    "Deployment {} not found",
                    context.deployment_id
                ))
            })?;
        if environment.project_id != self.project_id
            || deployment.project_id != self.project_id
            || deployment.environment_id != self.environment_id
        {
            return Err(WorkflowError::JobValidationFailed(
                "Deployment workflow project/environment ownership does not match its job".into(),
            ));
        }
        if environment.current_deployment_id != Some(context.deployment_id)
            || !matches!(deployment.state.as_str(), "completed" | "deployed")
        {
            self.log(format!("Skipped deployment workflows: deployment {} is no longer the live successful deployment",context.deployment_id)).await?;
            return Ok(JobResult::success(context));
        }
        for name in &config.deployment_workflows {
            let run = start(
                self.control.as_ref(),
                self.project_id,
                self.environment_id,
                context.deployment_id,
                name,
            )
            .await?;
            self.log(format!(
                "Deployment workflow '{name}': run {} ({})",
                run.run_id,
                if run.created {
                    "queued"
                } else {
                    "already started"
                }
            ))
            .await?;
        }
        Ok(JobResult::success(context))
    }
    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        RepositoryOutput::from_context(context, &self.download_job_id)?;
        Ok(())
    }
    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cron_workflows::fake::FakeWorkflowControl;
    #[tokio::test]
    async fn deployment_retries_reuse_runs_and_next_deploy_gets_new_runs() {
        let control = FakeWorkflowControl::default();
        let first = start(&control, 1, 2, 3, "after-deploy").await.unwrap();
        let repeated = start(&control, 1, 2, 3, "after-deploy").await.unwrap();
        assert_eq!(first.run_id, repeated.run_id);
        assert!(!repeated.created);
        assert_ne!(
            first.run_id,
            start(&control, 1, 2, 4, "after-deploy")
                .await
                .unwrap()
                .run_id
        );
        {
            let runs = control.runs.lock().unwrap();
            assert_eq!(runs.len(), 2);
            assert_eq!(runs[&first.run_id].2.as_ref().unwrap()["deployment_id"], 3);
        }
        *control.refuse.lock().unwrap() = Some("active run quota reached".into());
        assert!(start(&control, 1, 2, 5, "after-deploy")
            .await
            .unwrap_err()
            .to_string()
            .contains("quota"));
    }
    #[test]
    fn deployment_workflow_names_are_validated_before_any_start() {
        validate(&[]).unwrap();
        validate(&["after-deploy".into()]).unwrap();
        for names in [
            vec!["".into()],
            vec!["same".into(), "same".into()],
            vec!["bad name".into()],
            vec!["a".repeat(129)],
            (0..17).map(|i| format!("workflow-{i}")).collect(),
        ] {
            assert!(validate(&names).is_err());
        }
        let parsed =
            temps_core::TempsConfig::from_yaml("deploymentWorkflows: [after-deploy]").unwrap();
        assert_eq!(parsed.deployment_workflows, vec!["after-deploy"]);
    }
}
