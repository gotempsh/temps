use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use std::sync::Arc;
use temps_entities::{env_var_environments, env_vars, environments};
use thiserror::Error;

use super::types::{EnvVarEnvironment, EnvVarWithEnvironments};

#[derive(Error, Debug)]
pub enum EnvVarError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Environment variable not found")]
    NotFound(String),

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Other error: {0}")]
    Other(String),
}

impl From<sea_orm::DbErr> for EnvVarError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => EnvVarError::NotFound(error.to_string()),
            _ => EnvVarError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}

impl From<sea_orm::TransactionError<EnvVarError>> for EnvVarError {
    fn from(error: sea_orm::TransactionError<EnvVarError>) -> Self {
        match error {
            sea_orm::TransactionError::Transaction(e) => e,
            sea_orm::TransactionError::Connection(e) => {
                EnvVarError::DatabaseConnectionError(e.to_string())
            }
        }
    }
}

#[derive(Clone)]
pub struct EnvVarService {
    db: Arc<temps_database::DbConnection>,
}

impl EnvVarService {
    pub fn new(db: Arc<temps_database::DbConnection>) -> Self {
        EnvVarService { db }
    }

    pub async fn get_environment_variables(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<EnvVarWithEnvironments>, EnvVarError> {
        // Get all env vars for the project
        let vars = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .order_by_desc(env_vars::Column::UpdatedAt)
            .all(self.db.as_ref())
            .await?;

        // Get all env var IDs to query environments in bulk
        let var_ids: Vec<i32> = vars.iter().map(|v| v.id).collect();

        // Get all environment relationships for these vars with a JOIN to environments
        // This prevents N+1 queries by doing a single query with JOIN
        let mut env_relationships_query = env_var_environments::Entity::find()
            .filter(env_var_environments::Column::EnvVarId.is_in(var_ids));

        // If environment_id is provided, filter the relationships
        if let Some(env_id) = environment_id {
            env_relationships_query = env_relationships_query
                .filter(env_var_environments::Column::EnvironmentId.eq(env_id));
        }

        let env_relationships: Vec<(env_var_environments::Model, Option<environments::Model>)> =
            env_relationships_query
                .find_also_related(environments::Entity)
                .all(self.db.as_ref())
                .await?;

        // Build a map of env_var_id -> Vec<EnvVarEnvironment>
        let mut env_map: std::collections::HashMap<i32, Vec<EnvVarEnvironment>> =
            std::collections::HashMap::new();

        for (env_var_env, env_option) in env_relationships {
            if let Some(env) = env_option {
                env_map
                    .entry(env_var_env.env_var_id)
                    .or_default()
                    .push(EnvVarEnvironment {
                        id: env.id,
                        name: env.name,
                        main_url: env.subdomain,
                        current_deployment_id: env.current_deployment_id,
                    });
            }
        }

        // Build the final result, only including env vars that have environments (if filter applied)
        let result: Vec<EnvVarWithEnvironments> = vars
            .into_iter()
            .filter_map(|var| {
                let environments = env_map.get(&var.id).cloned().unwrap_or_default();

                // If environment_id filter is specified, only include vars that have environments
                if environment_id.is_some() && environments.is_empty() {
                    return None;
                }

                Some(EnvVarWithEnvironments {
                    id: var.id,
                    project_id: var.project_id,
                    key: var.key,
                    value: var.value,
                    created_at: var.created_at,
                    updated_at: var.updated_at,
                    environments,
                    include_in_preview: var.include_in_preview,
                })
            })
            .collect();

        Ok(result)
    }

    pub async fn create_environment_variable(
        &self,
        project_id: i32,
        environment_ids: Vec<i32>,
        key: String,
        value: String,
        include_in_preview: bool,
    ) -> Result<EnvVarWithEnvironments, EnvVarError> {
        // Check for conflicts before creating the new env var
        let existing_env_vars = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .filter(env_vars::Column::Key.eq(&key))
            .find_with_related(env_var_environments::Entity)
            .all(self.db.as_ref())
            .await?;

        let existing_env_ids: Vec<i32> = existing_env_vars
            .into_iter()
            .flat_map(|(_, env_var_envs)| {
                env_var_envs
                    .into_iter()
                    .map(|env_var_env| env_var_env.environment_id)
            })
            .collect();

        // Check if any of the requested environment_ids conflict with existing ones
        for env_id in &environment_ids {
            if existing_env_ids.contains(env_id) {
                return Err(EnvVarError::Other(format!(
                    "Environment variable '{}' already exists in one of the selected environments",
                    key
                )));
            }
        }

        let result = self
            .db
            .transaction::<_, EnvVarWithEnvironments, EnvVarError>(|txn| {
                Box::pin(async move {
                    // Create the env var
                    let new_var = env_vars::ActiveModel {
                        project_id: Set(project_id),
                        key: Set(key.clone()),
                        value: Set(value.clone()),
                        include_in_preview: Set(include_in_preview),
                        created_at: Set(chrono::Utc::now()),
                        updated_at: Set(chrono::Utc::now()),
                        environment_id: Set(None),
                        ..Default::default()
                    };

                    let var = new_var.insert(txn).await?;

                    // Create environment relationships and get environment names
                    let mut environments = Vec::new();
                    for env_id in &environment_ids {
                        let new_env_rel = env_var_environments::ActiveModel {
                            env_var_id: Set(var.id),
                            environment_id: Set(*env_id),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        };

                        new_env_rel.insert(txn).await?;

                        // Get environment name
                        let env = environments::Entity::find_by_id(*env_id)
                            .one(txn)
                            .await?
                            .ok_or(EnvVarError::Other("Environment not found".to_string()))?;

                        environments.push(EnvVarEnvironment {
                            id: env.id,
                            name: env.name,
                            main_url: env.subdomain,
                            current_deployment_id: env.current_deployment_id,
                        });
                    }

                    Ok(EnvVarWithEnvironments {
                        id: var.id,
                        project_id: var.project_id,
                        key: var.key,
                        value: var.value,
                        created_at: var.created_at,
                        updated_at: var.updated_at,
                        environments,
                        include_in_preview: var.include_in_preview,
                    })
                })
            })
            .await?;

        Ok(result)
    }

    pub async fn update_environment_variable(
        &self,
        project_id: i32,
        var_id: i32,
        key: String,
        value: String,
        environment_ids: Vec<i32>,
        include_in_preview: bool,
    ) -> Result<EnvVarWithEnvironments, EnvVarError> {
        let result = self
            .db
            .transaction::<_, EnvVarWithEnvironments, EnvVarError>(|txn| {
                Box::pin(async move {
                    // Update the env var
                    let env_var = env_vars::Entity::find_by_id(var_id)
                        .filter(env_vars::Column::ProjectId.eq(project_id))
                        .one(txn)
                        .await?
                        .ok_or(EnvVarError::Other(
                            "Environment variable not found".to_string(),
                        ))?;

                    let mut active_var: env_vars::ActiveModel = env_var.into();
                    active_var.key = Set(key.clone());
                    active_var.value = Set(value.clone());
                    active_var.include_in_preview = Set(include_in_preview);
                    active_var.updated_at = Set(chrono::Utc::now());
                    let var = active_var.update(txn).await?;

                    // Delete existing environment relationships
                    env_var_environments::Entity::delete_many()
                        .filter(env_var_environments::Column::EnvVarId.eq(var_id))
                        .exec(txn)
                        .await?;

                    // Create new environment relationships and collect environment info
                    let mut environments = Vec::new();
                    for env_id in &environment_ids {
                        let new_env_rel = env_var_environments::ActiveModel {
                            env_var_id: Set(var.id),
                            environment_id: Set(*env_id),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        };

                        new_env_rel.insert(txn).await?;

                        // Get environment name
                        let env = environments::Entity::find_by_id(*env_id)
                            .one(txn)
                            .await?
                            .ok_or(EnvVarError::Other("Environment not found".to_string()))?;

                        environments.push(EnvVarEnvironment {
                            id: env.id,
                            name: env.name,
                            main_url: env.subdomain,
                            current_deployment_id: env.current_deployment_id,
                        });
                    }

                    Ok(EnvVarWithEnvironments {
                        id: var.id,
                        project_id: var.project_id,
                        key: var.key,
                        value: var.value,
                        created_at: var.created_at,
                        updated_at: var.updated_at,
                        environments,
                        include_in_preview: var.include_in_preview,
                    })
                })
            })
            .await?;

        Ok(result)
    }

    pub async fn delete_environment_variable(
        &self,
        project_id: i32,
        var_id: i32,
    ) -> Result<(), EnvVarError> {
        self.db
            .transaction::<_, (), EnvVarError>(|txn| {
                Box::pin(async move {
                    // First delete the environment relationships
                    env_var_environments::Entity::delete_many()
                        .filter(env_var_environments::Column::EnvVarId.eq(var_id))
                        .exec(txn)
                        .await?;

                    // Then delete the environment variable itself
                    env_vars::Entity::delete_many()
                        .filter(env_vars::Column::Id.eq(var_id))
                        .filter(env_vars::Column::ProjectId.eq(project_id))
                        .exec(txn)
                        .await?;

                    Ok(())
                })
            })
            .await?;

        Ok(())
    }

    pub async fn get_environment_variable_value(
        &self,
        project_id: i32,
        key: &str,
        _environment_id: Option<i32>,
    ) -> Result<String, EnvVarError> {
        let var = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .filter(env_vars::Column::Key.eq(key))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| EnvVarError::Other("Environment variable not found".to_string()))?;

        Ok(var.value)
    }
}
