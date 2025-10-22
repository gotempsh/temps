use sea_orm::{
    ActiveModelTrait, ColumnTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::Serialize;
use serde_json::json;
use slug::slugify;
use std::sync::Arc;
use temps_core::problemdetails::Problem;
use temps_core::{EnvironmentCreatedJob, Job, JobQueue};
use temps_entities::{environment_domains, environments, projects};
use thiserror::Error;
use tracing::{info, warn};

#[derive(Error, Debug)]
pub enum EnvironmentError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Environment not found")]
    NotFound(String),

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Other error: {0}")]
    Other(String),
}

impl From<DbErr> for EnvironmentError {
    fn from(error: DbErr) -> Self {
        match error {
            DbErr::RecordNotFound(_) => EnvironmentError::NotFound(error.to_string()),
            _ => EnvironmentError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}

impl From<EnvironmentError> for Problem {
    fn from(error: EnvironmentError) -> Self {
        match error {
            EnvironmentError::NotFound(msg) => {
                temps_core::error_builder::not_found().detail(msg).build()
            }
            EnvironmentError::InvalidInput(msg) => {
                temps_core::error_builder::bad_request().detail(msg).build()
            }
            EnvironmentError::DatabaseConnectionError(msg) => {
                temps_core::error_builder::internal_server_error()
                    .detail(msg)
                    .build()
            }
            EnvironmentError::DatabaseError { reason } => {
                temps_core::error_builder::internal_server_error()
                    .detail(reason)
                    .build()
            }
            EnvironmentError::Other(msg) => temps_core::error_builder::internal_server_error()
                .detail(msg)
                .build(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DomainEnvironment {
    pub id: i32,
    pub name: String,
    pub slug: String,
}

#[derive(Clone)]
pub struct EnvironmentService {
    db: Arc<temps_database::DbConnection>,
    config_service: Arc<temps_config::ConfigService>,
    queue_service: Option<Arc<dyn JobQueue>>,
}

impl EnvironmentService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        config_service: Arc<temps_config::ConfigService>,
    ) -> Self {
        EnvironmentService {
            db,
            config_service,
            queue_service: None,
        }
    }

    pub fn with_queue_service(mut self, queue_service: Arc<dyn JobQueue>) -> Self {
        self.queue_service = Some(queue_service);
        self
    }

    pub async fn compute_environment_url(&self, environment_slug: &str) -> String {
        let settings = self.config_service.get_settings().await.unwrap_or_default();

        // Use external_url if configured, otherwise fall back to preview_domain
        let base_domain = settings.preview_domain.clone();

        // Determine protocol - use https if external_url is configured, otherwise http
        let protocol = if settings.external_url.is_some() {
            "https"
        } else {
            "http"
        };

        // Simple format: <scheme>://<slug>.<preview_domain>
        format!("{}://{}.{}", protocol, environment_slug, base_domain)
    }

    pub async fn create_environment(
        &self,
        project_id: i32,
        name: String,
        cpu_request: Option<i32>,
        cpu_limit: Option<i32>,
        memory_request: Option<i32>,
        memory_limit: Option<i32>,
        branch: String,
    ) -> anyhow::Result<environments::Model> {
        use serde_json::json;

        // Start a transaction
        let txn = self.db.begin().await?;

        // Get the project slug
        let project = projects::Entity::find_by_id(project_id)
            .one(&txn)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;

        let env_slug = slugify(&name);

        // Create main_url using project_slug-env_slug format
        let main_url = format!("{}-{}", project.slug, env_slug);

        // Create the new environment
        let new_environment = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(name),
            slug: Set(env_slug.clone()),
            subdomain: Set(main_url.clone()),
            host: Set("".to_string()),
            upstreams: Set(json!({})), // Fix: use serde_json::Value
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            current_deployment_id: Set(None),
            cpu_request: Set(cpu_request),
            cpu_limit: Set(cpu_limit),
            memory_request: Set(memory_request),
            memory_limit: Set(memory_limit),
            branch: Set(Some(branch)),
            replicas: Set(Some(1)),
            ..Default::default()
        };

        let environment = new_environment.insert(&txn).await?;

        // Create the environment domain with the stored identifier from main_url
        let new_domain = environment_domains::ActiveModel {
            environment_id: Set(environment.id),
            domain: Set(environment.subdomain.clone()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        new_domain.insert(&txn).await?;

        txn.commit().await?;

        // Emit EnvironmentCreated job
        if let Some(queue_service) = &self.queue_service {
            let env_created_job = Job::EnvironmentCreated(EnvironmentCreatedJob {
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                project_id: environment.project_id,
                subdomain: environment.subdomain.clone(),
            });

            if let Err(e) = queue_service.send(env_created_job).await {
                warn!(
                    "Failed to emit EnvironmentCreated job for environment {}: {}",
                    environment.id, e
                );
            } else {
                info!(
                    "Emitted EnvironmentCreated job for environment {}",
                    environment.id
                );
            }
        }

        Ok(environment)
    }

    pub async fn get_environments(
        &self,
        project_id_p: i32,
    ) -> Result<Vec<environments::Model>, EnvironmentError> {
        let envs = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .order_by_asc(environments::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(envs)
    }
    pub async fn get_project(
        &self,
        project_id_p: i32,
    ) -> Result<projects::Model, EnvironmentError> {
        let project = projects::Entity::find_by_id(project_id_p)
            .one(self.db.as_ref())
            .await?;

        project.ok_or(EnvironmentError::NotFound(format!(
            "Project {} not found",
            project_id_p
        )))
    }

    pub async fn get_environment(
        &self,
        project_id_p: i32,
        env_id: i32,
    ) -> Result<environments::Model, EnvironmentError> {
        // If not found by ID or if parsing failed, try by slug
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::Id.eq(env_id))
            .one(self.db.as_ref())
            .await?;

        environment.ok_or(EnvironmentError::NotFound(format!(
            "Environment {:?} not found",
            env_id
        )))
    }

    pub async fn get_default_environment(
        &self,
        project_id_p: i32,
    ) -> Result<environments::Model, EnvironmentError> {
        let default_environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .order_by_asc(environments::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?;

        match default_environment {
            Some(env) => Ok(env),
            None => Err(EnvironmentError::NotFound(format!(
                "Environment {} not found",
                project_id_p
            ))),
        }
    }

    pub async fn get_or_create_environment_for_branch(
        &self,
        project_id: i32,
        branch: &str,
    ) -> Result<environments::Model, EnvironmentError> {
        // First, get the project to check if this is the main branch
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?
            .ok_or_else(|| EnvironmentError::Other("Project not found".to_string()))?;

        if project.main_branch == branch {
            // If it's the main branch, return the default (first) environment
            info!("Using default environment for main branch: {}", branch);
            return self.get_default_environment(project_id).await.map_err(|e| {
                EnvironmentError::Other(format!("Failed to get default environment: {}", e))
            });
        }

        // For non-main branches, continue with preview environment logic
        let existing_env = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Branch.eq(branch))
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        if let Some(env) = existing_env {
            info!(
                "Found existing preview environment for branch {}: {}",
                branch, env.id
            );
            return Ok(env);
        }

        let env_name = "preview";
        info!("Creating new preview environment for branch: {}", branch);
        self.create_environment(
            project_id,
            env_name.to_string(),
            None,
            None,
            None,
            None,
            branch.to_string(),
        )
        .await
        .map_err(|e| EnvironmentError::Other(e.to_string()))
    }

    pub async fn create_new_environment(
        &self,
        project_id: i32,
        name: String,
        branch: String,
        replicas: Option<i32>,
    ) -> Result<environments::Model, EnvironmentError> {
        use sea_orm::TransactionTrait;

        // Verify project exists
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?
            .ok_or_else(|| {
                EnvironmentError::NotFound(format!("Project {} not found", project_id))
            })?;

        // Check if environment with same name already exists
        let existing_env = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Name.eq(&name))
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        if existing_env.is_some() {
            return Err(EnvironmentError::Other(
                "Environment with this name already exists".to_string(),
            ));
        }

        // Generate the environment identifier
        let env_slug = slugify(&name);

        // Create main_url using project_slug-env_slug format
        let main_url = format!("{}-{}", project.slug, env_slug);

        // Create the new environment
        let new_environment = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(name),
            slug: Set(env_slug.clone()),
            subdomain: Set(main_url.clone()),
            host: Set("".to_string()),
            upstreams: Set(json!({})),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            current_deployment_id: Set(None),
            cpu_request: Set(None),
            cpu_limit: Set(None),
            memory_request: Set(None),
            memory_limit: Set(None),
            branch: Set(Some(branch)),
            replicas: Set(replicas.or(Some(1))),
            ..Default::default()
        };

        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        // Insert the environment
        let environment = new_environment
            .insert(&txn)
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        // Create the environment domain with the stored identifier from main_url
        let new_domain = environment_domains::ActiveModel {
            environment_id: Set(environment.id),
            domain: Set(environment.subdomain.clone()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        new_domain
            .insert(&txn)
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        txn.commit()
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        Ok(environment)
    }

    pub async fn update_environment_settings(
        &self,
        project_id_param: i32,
        env_id: i32,
        settings: crate::handlers::UpdateEnvironmentSettingsRequest,
    ) -> Result<environments::Model, EnvironmentError> {
        // First get the environment to verify it exists and belongs to the project
        let environment = self.get_environment(project_id_param, env_id).await?;

        // Update the environment with new settings
        let mut active_model: environments::ActiveModel = environment.into();
        active_model.cpu_request = Set(settings.cpu_request);
        active_model.cpu_limit = Set(settings.cpu_limit);
        active_model.memory_request = Set(settings.memory_request);
        active_model.memory_limit = Set(settings.memory_limit);
        active_model.branch = Set(settings.branch);
        active_model.replicas = Set(settings.replicas);
        active_model.updated_at = Set(chrono::Utc::now());

        let updated_environment = active_model
            .update(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;

        Ok(updated_environment)
    }

    pub async fn get_environment_domains(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<environment_domains::Model>, EnvironmentError> {
        // First verify that the environment belongs to the project
        let environment_exists = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Id.eq(environment_id))
            .one(self.db.as_ref())
            .await?;

        if environment_exists.is_none() {
            return Err(EnvironmentError::NotFound(format!(
                ">>> Environment {} not found",
                environment_id
            )));
        }

        // Get all domains for this environment
        let domains = environment_domains::Entity::find()
            .filter(environment_domains::Column::EnvironmentId.eq(environment_id))
            .all(self.db.as_ref())
            .await?;

        Ok(domains)
    }

    pub async fn add_environment_domain(
        &self,
        project_id_p: i32,
        env_id: i32,
        domain: String,
    ) -> Result<environment_domains::Model, EnvironmentError> {
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::Id.eq(env_id))
            .one(self.db.as_ref())
            .await?;

        if let Some(env) = environment {
            let new_domain = environment_domains::ActiveModel {
                environment_id: Set(env.id),
                domain: Set(domain),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            };

            let inserted_domain = new_domain.insert(self.db.as_ref()).await?;
            return Ok(inserted_domain);
        }

        Err(EnvironmentError::NotFound(format!(
            "Environment {} not found",
            env_id
        )))
    }

    pub async fn delete_environment_domain(
        &self,
        project_id_p: i32,
        env_id: i32,
        domain_id: i32,
    ) -> Result<(), EnvironmentError> {
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::Id.eq(env_id))
            .one(self.db.as_ref())
            .await?;

        if let Some(env) = environment {
            let domain_to_delete = environment_domains::Entity::find()
                .filter(environment_domains::Column::EnvironmentId.eq(env.id))
                .filter(environment_domains::Column::Id.eq(domain_id))
                .one(self.db.as_ref())
                .await?;

            if let Some(_domain) = domain_to_delete {
                environment_domains::Entity::delete_by_id(domain_id)
                    .exec(self.db.as_ref())
                    .await?;

                return Ok(());
            }
        }

        Err(EnvironmentError::NotFound(format!(
            "Environment {} not found",
            env_id
        )))
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use std::sync::Arc;
//     use temps_config::{ConfigService, Settings};

//     // Mock database connection for testing
//     fn create_mock_db() -> Arc<temps_database::DbConnection> {
//         // Note: In real tests, you'd use a test database
//         // This is just for compilation testing
//         Arc::new(temps_database::DbConnection::default())
//     }

//     #[tokio::test]
//     async fn test_environment_service_creation() {
//         let db = create_mock_db();

//         // Create a minimal config service for testing
//         let config = Arc::new(temps_config::ConfigService::new(
//             db.clone(),
//             Arc::new(
//                 temps_core::EncryptionService::new(
//                     "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
//                 )
//                 .unwrap(),
//             ),
//         ));

//         let service = EnvironmentService::new(db, config);

//         // Test that service fields are accessible
//         assert!(std::ptr::addr_of!(service.db).is_null() == false);
//         assert!(std::ptr::addr_of!(service.config_service).is_null() == false);
//     }

//     #[test]
//     fn test_environment_error_display() {
//         let error = EnvironmentError::NotFound("test".to_string());
//         assert_eq!(error.to_string(), "Environment not found");

//         let error = EnvironmentError::InvalidInput("invalid input".to_string());
//         assert_eq!(error.to_string(), "Invalid input: invalid input");

//         let error = EnvironmentError::Other("some error".to_string());
//         assert_eq!(error.to_string(), "Other error: some error");
//     }

//     #[test]
//     fn test_domain_environment_struct() {
//         let domain_env = DomainEnvironment {
//             id: 1,
//             name: "production".to_string(),
//             slug: "prod".to_string(),
//         };

//         assert_eq!(domain_env.id, 1);
//         assert_eq!(domain_env.name, "production");
//         assert_eq!(domain_env.slug, "prod");
//     }

//     #[test]
//     fn test_environment_error_from_db_err() {
//         let db_error = DbErr::RecordNotFound("test".to_string());
//         let env_error = EnvironmentError::from(db_error);

//         match env_error {
//             EnvironmentError::NotFound(_) => assert!(true),
//             _ => panic!("Expected NotFound error"),
//         }
//     }
// }
