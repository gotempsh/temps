// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project build credentials authorize artifact management, never public DSNs.
use super::SourceMapService;
use chrono::Utc;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use sha2::{Digest, Sha256};
use temps_entities::{deployment_tokens, projects};

#[derive(Debug, thiserror::Error)]
pub enum BuildCredentialError {
    #[error("Invalid, inactive, or expired project deployment credential")]
    Unauthorized,
    #[error("Project {project_id} is inactive for this build credential")]
    InactiveProject { project_id: i32 },
    #[error("Source artifact writes require full access to project {project_id}")]
    Forbidden { project_id: i32 },
    #[error("Project '{slug}' was not found")]
    ProjectNotFound { slug: String },
    #[error(
        "Build credential operation '{operation}' failed for project {project_id:?}: {source}"
    )]
    Storage {
        operation: &'static str,
        project_id: Option<i32>,
        #[source]
        source: sea_orm::DbErr,
    },
}

pub struct ProjectBuildCredential {
    pub project_id: i32,
    pub token_id: i32,
}

impl SourceMapService {
    pub async fn authenticate_build_token(
        &self,
        token: &str,
    ) -> Result<ProjectBuildCredential, BuildCredentialError> {
        if !token.starts_with("dt_") || token.len() <= 3 {
            return Err(BuildCredentialError::Unauthorized);
        }
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let prefix: String = token.chars().take(8).collect();
        let credential = deployment_tokens::Entity::find()
            .filter(deployment_tokens::Column::TokenHash.eq(hash))
            .filter(deployment_tokens::Column::TokenPrefix.eq(prefix))
            .filter(deployment_tokens::Column::IsActive.eq(true))
            .filter(
                Condition::any()
                    .add(deployment_tokens::Column::ExpiresAt.is_null())
                    .add(deployment_tokens::Column::ExpiresAt.gt(Utc::now())),
            )
            .one(self.db.as_ref())
            .await
            .map_err(|source| BuildCredentialError::Storage {
                operation: "authenticate deployment token",
                project_id: None,
                source,
            })?
            .ok_or(BuildCredentialError::Unauthorized)?;
        let project = projects::Entity::find_by_id(credential.project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(projects::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|source| BuildCredentialError::Storage {
                operation: "validate active build project",
                project_id: Some(credential.project_id),
                source,
            })?
            .ok_or(BuildCredentialError::InactiveProject {
                project_id: credential.project_id,
            })?;
        let full_access = credential
            .permissions
            .as_ref()
            .and_then(|value| value.as_array())
            .is_some_and(|permissions| {
                permissions
                    .iter()
                    .any(|value| matches!(value.as_str(), Some("*") | Some("full_access")))
            });
        if !full_access {
            return Err(BuildCredentialError::Forbidden {
                project_id: project.id,
            });
        }
        Ok(ProjectBuildCredential {
            project_id: project.id,
            token_id: credential.id,
        })
    }

    pub async fn resolve_build_project(
        &self,
        slug: &str,
        expected_project_id: i32,
    ) -> Result<i32, BuildCredentialError> {
        if slug.parse::<i32>().ok() == Some(expected_project_id) {
            return Ok(expected_project_id);
        }
        let project = projects::Entity::find()
            .filter(projects::Column::Slug.eq(slug))
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(projects::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|source| BuildCredentialError::Storage {
                operation: "resolve build project slug",
                project_id: Some(expected_project_id),
                source,
            })?
            .ok_or_else(|| BuildCredentialError::ProjectNotFound {
                slug: slug.chars().take(128).collect(),
            })?;
        if project.id != expected_project_id {
            return Err(BuildCredentialError::Forbidden {
                project_id: expected_project_id,
            });
        }
        Ok(project.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ConnectionTrait, DbBackend, Set, Statement};
    use std::sync::Arc;

    #[tokio::test]
    async fn build_credentials_enforce_secret_expiry_permissions_and_active_scope() {
        let test_db = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test_db.connection_arc();
        let project = projects::ActiveModel {
            name: Set("Synthetic build".into()),
            slug: Set(format!("synthetic-build-{}", uuid::Uuid::new_v4())),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let token = "dt_synthetic_build_credential_for_native_tests";
        let credential = deployment_tokens::ActiveModel {
            project_id: Set(project.id),
            name: Set("Synthetic build".into()),
            token_hash: Set(hex::encode(Sha256::digest(token.as_bytes()))),
            token_prefix: Set(token.chars().take(8).collect()),
            permissions: Set(Some(serde_json::json!(["full_access"]))),
            is_active: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let service = SourceMapService::new(Arc::clone(&db));
        assert_eq!(
            service
                .authenticate_build_token(token)
                .await
                .unwrap()
                .token_id,
            credential.id
        );
        assert!(matches!(
            service.authenticate_build_token(&"a".repeat(32)).await,
            Err(BuildCredentialError::Unauthorized)
        ));
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE deployment_tokens SET permissions=$1 WHERE id=$2",
            [
                serde_json::json!(["read_only"]).into(),
                credential.id.into(),
            ],
        ))
        .await
        .unwrap();
        assert!(matches!(
            service.authenticate_build_token(token).await,
            Err(BuildCredentialError::Forbidden { .. })
        ));
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE deployment_tokens SET permissions=$1,expires_at=NOW()-INTERVAL '1 second' WHERE id=$2", [serde_json::json!(["*"]).into(), credential.id.into()])).await.unwrap();
        assert!(matches!(
            service.authenticate_build_token(token).await,
            Err(BuildCredentialError::Unauthorized)
        ));
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE deployment_tokens SET expires_at=NULL,is_active=false WHERE id=$1",
            [credential.id.into()],
        ))
        .await
        .unwrap();
        assert!(matches!(
            service.authenticate_build_token(token).await,
            Err(BuildCredentialError::Unauthorized)
        ));
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE deployment_tokens SET is_active=true WHERE id=$1",
            [credential.id.into()],
        ))
        .await
        .unwrap();
        assert_eq!(
            service
                .authenticate_build_token(token)
                .await
                .unwrap()
                .project_id,
            project.id
        );
        assert_eq!(
            service
                .resolve_build_project(&project.slug, project.id)
                .await
                .unwrap(),
            project.id
        );
        assert_eq!(
            service
                .resolve_build_project(&project.id.to_string(), project.id)
                .await
                .unwrap(),
            project.id
        );
        assert!(matches!(
            service
                .resolve_build_project(&project.slug, project.id + 1)
                .await,
            Err(BuildCredentialError::Forbidden { .. })
        ));
        for statement in [
            "UPDATE projects SET is_deleted=true WHERE id=$1",
            "UPDATE projects SET is_deleted=false,deleted_at=NOW() WHERE id=$1",
        ] {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                statement,
                [project.id.into()],
            ))
            .await
            .unwrap();
            assert!(matches!(
                service.authenticate_build_token(token).await,
                Err(BuildCredentialError::InactiveProject { .. })
            ));
        }
    }
}
