// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistence and validation for AI-first multi-project applications.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, TransactionTrait,
};
use serde_json::Value;
use temps_entities::{
    ai_application_projects, ai_applications, ai_conversations, ai_thread_artifacts, projects,
};

const MAX_PROJECTS: usize = 20;
const ALLOWED_ARTIFACT_KINDS: &[&str] = &[
    "topology",
    "execution_plan",
    "credential_request",
    "status",
    "form",
    "table",
];

#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    #[error("application '{0}' not found")]
    NotFound(String),
    #[error("application name must be between 1 and 200 characters")]
    InvalidName,
    #[error("an application must contain between 1 and {MAX_PROJECTS} unique projects")]
    InvalidProjects,
    #[error("project {0} does not exist")]
    ProjectNotFound(i32),
    #[error("conversation '{0}' does not belong to this application")]
    ConversationNotFound(String),
    #[error("artifact kind '{0}' is not supported")]
    InvalidArtifactKind(String),
    #[error("artifact payload contains a secret value at '{0}'; store it in the credential broker and include only a reference")]
    SecretValue(String),
    #[error("database operation failed: {0}")]
    Database(#[from] sea_orm::DbErr),
}

#[derive(Debug, Clone)]
pub struct ApplicationWithProjects {
    pub application: ai_applications::Model,
    pub projects: Vec<projects::Model>,
}

pub struct ApplicationService {
    db: Arc<DatabaseConnection>,
}

impl ApplicationService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn create(
        &self,
        user_id: i32,
        name: &str,
        description: Option<&str>,
        project_ids: &[i32],
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(ApplicationError::InvalidName);
        }
        let unique = project_ids.iter().copied().collect::<HashSet<_>>();
        if unique.is_empty() || unique.len() != project_ids.len() || unique.len() > MAX_PROJECTS {
            return Err(ApplicationError::InvalidProjects);
        }

        let mut found_projects = projects::Entity::find()
            .filter(projects::Column::Id.is_in(project_ids.iter().copied()))
            .filter(projects::Column::IsDeleted.eq(false))
            .all(self.db.as_ref())
            .await?;
        if found_projects.len() != project_ids.len() {
            let found = found_projects
                .iter()
                .map(|project| project.id)
                .collect::<HashSet<_>>();
            let missing = project_ids
                .iter()
                .find(|project_id| !found.contains(project_id))
                .copied()
                .unwrap_or_default();
            return Err(ApplicationError::ProjectNotFound(missing));
        }
        found_projects.sort_by_key(|project| {
            project_ids
                .iter()
                .position(|project_id| *project_id == project.id)
                .unwrap_or(usize::MAX)
        });

        let txn = self.db.begin().await?;
        let now = Utc::now();
        let application = ai_applications::ActiveModel {
            public_id: Set(format!("app_{}", uuid::Uuid::new_v4().simple())),
            name: Set(name.to_string()),
            description: Set(description
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)),
            status: Set("active".to_string()),
            created_by: Set(user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&txn)
        .await?;

        for project_id in project_ids {
            ai_application_projects::ActiveModel {
                application_id: Set(application.id),
                project_id: Set(*project_id),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
        }
        txn.commit().await?;

        Ok(ApplicationWithProjects {
            application,
            projects: found_projects,
        })
    }

    pub async fn list(
        &self,
        user_id: i32,
    ) -> Result<Vec<ApplicationWithProjects>, ApplicationError> {
        let applications = ai_applications::Entity::find()
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq("active"))
            .order_by_desc(ai_applications::Column::UpdatedAt)
            .all(self.db.as_ref())
            .await?;
        if applications.is_empty() {
            return Ok(Vec::new());
        }

        // Load the complete topology in two batched queries. Application lists
        // are used by the global chat switcher, so one query per application
        // would become increasingly expensive as a workspace grows.
        let application_ids = applications
            .iter()
            .map(|application| application.id)
            .collect::<Vec<_>>();
        let links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.is_in(application_ids))
            .order_by_asc(ai_application_projects::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let project_ids = links
            .iter()
            .map(|link| link.project_id)
            .collect::<HashSet<_>>();
        let project_by_id = projects::Entity::find()
            .filter(projects::Column::Id.is_in(project_ids))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|project| (project.id, project))
            .collect::<HashMap<_, _>>();
        let mut projects_by_application = HashMap::<i64, Vec<projects::Model>>::new();
        for link in links {
            if let Some(project) = project_by_id.get(&link.project_id) {
                projects_by_application
                    .entry(link.application_id)
                    .or_default()
                    .push(project.clone());
            }
        }

        Ok(applications
            .into_iter()
            .map(|application| ApplicationWithProjects {
                projects: projects_by_application
                    .remove(&application.id)
                    .unwrap_or_default(),
                application,
            })
            .collect())
    }

    pub async fn get(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let application = ai_applications::Entity::find()
            .filter(ai_applications::Column::PublicId.eq(public_id))
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq("active"))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApplicationError::NotFound(public_id.to_string()))?;
        let projects = self.projects(application.id).await?;
        Ok(ApplicationWithProjects {
            application,
            projects,
        })
    }

    pub async fn conversations(
        &self,
        application_id: i64,
        user_id: i32,
    ) -> Result<Vec<ai_conversations::Model>, ApplicationError> {
        Ok(ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ApplicationId.eq(application_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::Status.eq("active"))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn create_artifact(
        &self,
        application_id: i64,
        conversation_public_id: &str,
        user_id: i32,
        kind: &str,
        title: Option<&str>,
        payload: Value,
    ) -> Result<ai_thread_artifacts::Model, ApplicationError> {
        if !ALLOWED_ARTIFACT_KINDS.contains(&kind) {
            return Err(ApplicationError::InvalidArtifactKind(kind.to_string()));
        }
        validate_secret_free_payload(&payload, "$")?;
        let conversation = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::PublicId.eq(conversation_public_id))
            .filter(ai_conversations::Column::ApplicationId.eq(application_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApplicationError::ConversationNotFound(conversation_public_id.to_string())
            })?;
        let now = Utc::now();
        Ok(ai_thread_artifacts::ActiveModel {
            public_id: Set(format!("art_{}", uuid::Uuid::new_v4().simple())),
            conversation_id: Set(conversation.id),
            application_id: Set(application_id),
            kind: Set(kind.to_string()),
            schema_version: Set(1),
            title: Set(title
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)),
            payload: Set(payload),
            status: Set("active".to_string()),
            created_by: Set(user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?)
    }

    pub async fn artifacts(
        &self,
        application_id: i64,
        conversation_public_id: &str,
        user_id: i32,
    ) -> Result<Vec<ai_thread_artifacts::Model>, ApplicationError> {
        let conversation = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::PublicId.eq(conversation_public_id))
            .filter(ai_conversations::Column::ApplicationId.eq(application_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApplicationError::ConversationNotFound(conversation_public_id.to_string())
            })?;
        Ok(ai_thread_artifacts::Entity::find()
            .filter(ai_thread_artifacts::Column::ConversationId.eq(conversation.id))
            .filter(ai_thread_artifacts::Column::Status.eq("active"))
            .order_by_asc(ai_thread_artifacts::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?)
    }

    async fn projects(
        &self,
        application_id: i64,
    ) -> Result<Vec<projects::Model>, ApplicationError> {
        let links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.eq(application_id))
            .order_by_asc(ai_application_projects::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let ids = links.iter().map(|link| link.project_id).collect::<Vec<_>>();
        let mut projects = projects::Entity::find()
            .filter(projects::Column::Id.is_in(ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        projects.sort_by_key(|project| {
            ids.iter()
                .position(|id| *id == project.id)
                .unwrap_or(usize::MAX)
        });
        Ok(projects)
    }
}

fn validate_secret_free_payload(value: &Value, path: &str) -> Result<(), ApplicationError> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                let normalized = key.to_ascii_lowercase();
                let is_reference =
                    normalized.ends_with("_ref") || normalized.ends_with("_reference");
                let is_secret_field = [
                    "secret",
                    "password",
                    "token",
                    "api_key",
                    "private_key",
                    "credential",
                ]
                .iter()
                .any(|marker| normalized == *marker || normalized.ends_with(&format!("_{marker}")));
                if is_secret_field && !is_reference && !child.is_null() {
                    return Err(ApplicationError::SecretValue(child_path));
                }
                validate_secret_free_payload(child, &child_path)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_secret_free_payload(child, &format!("{path}[{index}]"))?;
            }
        }
        Value::String(text) if crate::sensitive::contains_likely_credential(text) => {
            return Err(ApplicationError::SecretValue(path.to_string()));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_payload_accepts_credential_references() {
        let payload = serde_json::json!({
            "capability": "payments",
            "credential_ref": "vault://connections/conn_123",
            "targets": ["payments-api"]
        });
        assert!(validate_secret_free_payload(&payload, "$").is_ok());
    }

    #[test]
    fn artifact_payload_rejects_nested_secret_values() {
        let payload = serde_json::json!({
            "form": { "fields": [{ "api_key": "sk_test_value" }] }
        });
        assert!(matches!(
            validate_secret_free_payload(&payload, "$"),
            Err(ApplicationError::SecretValue(path)) if path == "$.form.fields[0].api_key"
        ));
    }

    #[test]
    fn artifact_payload_rejects_secret_disguised_under_neutral_key() {
        let payload = serde_json::json!({
            "content": "STRIPE_KEY=sk_test_1234567890123456"
        });
        assert!(matches!(
            validate_secret_free_payload(&payload, "$"),
            Err(ApplicationError::SecretValue(path)) if path == "$.content"
        ));
    }
}
