// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use serde::{Deserialize, Serialize};
use temps_entities::workflow_grants::Model;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowGrantAction {
    Start,
    Event,
}
impl WorkflowGrantAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Event => "event",
        }
    }
}
impl WorkflowService {
    pub async fn grant_workflow_access(
        &self,
        project_id: i32,
        target: i32,
        source: i32,
        action: WorkflowGrantAction,
        resource: &str,
    ) -> WorkflowResult<Model> {
        self.require_environment(project_id, target).await?;
        if source == target {
            return Err(WorkflowError::Validation {
                field: "source_environment_id",
                reason: "must be a different environment".into(),
            });
        }
        if self.store.environment_project(source).await?.is_none() {
            return Err(WorkflowError::Validation {
                field: "source_environment_id",
                reason: format!("environment {source} does not exist"),
            });
        }
        validate_name(
            "resource",
            resource,
            match action {
                WorkflowGrantAction::Start => MAX_WORKFLOW_NAME,
                WorkflowGrantAction::Event => crate::types::MAX_EVENT_NAME,
            },
        )?;
        self.store
            .grant(source, target, action.as_str(), resource)
            .await
    }
    pub async fn workflow_grants(
        &self,
        project_id: i32,
        environment_id: i32,
        page: u64,
        page_size: u64,
    ) -> WorkflowResult<(Vec<Model>, u64)> {
        self.require_environment(project_id, environment_id).await?;
        self.store.grants(environment_id, page, page_size).await
    }
    pub async fn revoke_workflow_grant(
        &self,
        project_id: i32,
        environment_id: i32,
        grant_id: Uuid,
    ) -> WorkflowResult<bool> {
        self.require_environment(project_id, environment_id).await?;
        self.store.revoke_grant(environment_id, grant_id).await
    }
    /// Authorization is checked for each request. Revocation prevents subsequent
    /// admissions; an operation already authorized may finish in flight.
    pub async fn authorize_workflow_target(
        &self,
        source: &CredentialRow,
        target_project: i32,
        target_environment: i32,
        action: WorkflowGrantAction,
        resource: &str,
    ) -> WorkflowResult<()> {
        if self.store.environment_project(target_environment).await? != Some(target_project)
            || !self
                .store
                .has_grant(
                    source.environment_id,
                    target_environment,
                    action.as_str(),
                    resource,
                )
                .await?
        {
            return Err(WorkflowError::GrantDenied {
                source_environment: source.environment_id,
                target_environment,
                action: action.as_str(),
                resource: resource.into(),
            });
        }
        Ok(())
    }
}
