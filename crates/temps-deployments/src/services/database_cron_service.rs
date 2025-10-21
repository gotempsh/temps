//! Database Cron Configuration Service
//!
//! Implements cron job configuration using the database

use async_trait::async_trait;
use chrono::{DateTime, Timelike as _, Utc};
use cron::Schedule;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, Set,
};
use temps_core::UtcDateTime;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use temps_database::DbConnection;
use temps_entities::{cron_executions, crons, deployment_containers, deployments};
use tokio::time::{self, Duration};
use tracing::{debug, error, info, warn};
use thiserror::Error;

use crate::jobs::configure_crons::{CronConfig, CronConfigError, CronConfigService};

#[derive(Error, Debug)]
pub enum CronServiceError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Invalid cron schedule '{schedule}': {message}")]
    InvalidSchedule { schedule: String, message: String },

    #[error("Maximum number of crons ({max}) reached for environment {env_id}")]
    MaxCronsReached { max: i64, env_id: i32 },

    #[error("Couldn't execute cron {cron_id} at URL {url}: {message}")]
    ExecutionError {
        cron_id: i32,
        url: String,
        message: String,
    },

    #[error("Failed to execute cron {cron_id} at URL {url}: {message}")]
    ExecutionFailed {
        cron_id: i32,
        url: String,
        message: String,
    },

    #[error("No active deployment found for environment {env_id}")]
    NoActiveDeployment { env_id: i32 },

    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Cron {cron_id} not found in project {project_id}, environment {env_id}")]
    CronNotFound {
        cron_id: i32,
        project_id: i32,
        env_id: i32,
    },

    #[error("Failed to get database connection: {0}")]
    ConnectionError(String),

    #[error("Failed to notify about cron error: {0}")]
    NotificationError(String),
}

/// Database-backed cron configuration service
pub struct DatabaseCronConfigService {
    db: Arc<DatabaseConnection>,
    http_client: Arc<reqwest::Client>,
    queue: Arc<dyn temps_core::JobQueue>,
}

impl DatabaseCronConfigService {
    pub fn new(db: Arc<DbConnection>, queue: Arc<dyn temps_core::JobQueue>) -> Self {
        Self {
            db,
            http_client: Arc::new(reqwest::Client::new()),
            queue,
        }
    }

    /// Validate a cron schedule expression
    fn validate_cron_schedule(schedule: &str) -> Result<(), CronConfigError> {
        let schedule_str = schedule.to_string();

        // Parse the cron schedule
        let parsed_schedule = cron::Schedule::from_str(schedule).map_err(|e| {
            CronConfigError::InvalidSchedule(format!(
                "Invalid cron expression '{}': {}",
                schedule_str, e
            ))
        })?;

        // Validate minimum interval (1 minute)
        let upcoming = parsed_schedule.upcoming(Utc);
        let next_two: Vec<_> = upcoming.take(2).collect();

        if let [first, second] = next_two.as_slice() {
            let duration = *second - *first;
            if duration.num_seconds() < 60 {
                return Err(CronConfigError::InvalidSchedule(
                    "Cron schedule must be at least 1 minute apart".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Calculate the next run time for a cron schedule
    fn calculate_next_run(schedule: &str) -> Result<UtcDateTime, CronConfigError> {
        let parsed_schedule = cron::Schedule::from_str(schedule).map_err(|e| {
            CronConfigError::InvalidSchedule(format!("Invalid cron expression: {}", e))
        })?;

        let next_run = parsed_schedule.upcoming(Utc).next().ok_or_else(|| {
            CronConfigError::InvalidSchedule("No upcoming execution time".to_string())
        })?;

        Ok(next_run)
    }
}

#[async_trait]
impl CronConfigService for DatabaseCronConfigService {
    async fn configure_crons(
        &self,
        project_id: i32,
        environment_id: i32,
        cron_configs: Vec<CronConfig>,
    ) -> Result<(), CronConfigError> {
        info!(
            "Configuring {} cron job(s) for project {} in environment {}",
            cron_configs.len(),
            project_id,
            environment_id
        );

        // Validate all cron schedules first
        for cron in &cron_configs {
            Self::validate_cron_schedule(&cron.schedule)?;
        }

        // Fetch existing crons for this project/environment
        let existing_crons = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project_id))
            .filter(crons::Column::EnvironmentId.eq(environment_id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                CronConfigError::DatabaseError(format!("Failed to fetch existing crons: {}", e))
            })?;

        // Check max crons limit per environment
        const MAX_CRONS_PER_ENV: usize = 10;
        let existing_paths: HashSet<_> = existing_crons.iter().map(|c| c.path.as_str()).collect();
        let new_crons_count = cron_configs
            .iter()
            .filter(|c| !existing_paths.contains(c.path.as_str()))
            .count();

        if existing_crons.len() + new_crons_count > MAX_CRONS_PER_ENV {
            return Err(CronConfigError::ConfigError(format!(
                "Maximum number of crons ({}) would be exceeded. Current: {}, New: {}",
                MAX_CRONS_PER_ENV,
                existing_crons.len(),
                new_crons_count
            )));
        }

        // Create a set of paths from repo crons for easier comparison
        let repo_cron_paths: HashSet<_> = cron_configs.iter().map(|c| c.path.as_str()).collect();

        // Mark crons as deleted if they're not in the repo config
        for existing_cron in &existing_crons {
            if !repo_cron_paths.contains(existing_cron.path.as_str()) {
                info!(
                    "Marking cron job '{}' as deleted (no longer in .temps.yaml)",
                    existing_cron.path
                );

                let mut cron_update: crons::ActiveModel = existing_cron.clone().into();
                cron_update.deleted_at = Set(Some(Utc::now()));
                cron_update.update(self.db.as_ref()).await.map_err(|e| {
                    CronConfigError::DatabaseError(format!("Failed to delete cron: {}", e))
                })?;
            }
        }

        // Process each cron from the repo
        for cron_config in cron_configs {
            // Check if cron already exists
            let existing_cron = existing_crons.iter().find(|c| c.path == cron_config.path);

            match existing_cron {
                Some(cron) if cron.schedule != cron_config.schedule => {
                    // Update schedule if it changed
                    info!(
                        "Updating cron job '{}': schedule changed from '{}' to '{}'",
                        cron.path, cron.schedule, cron_config.schedule
                    );

                    let next_run = Self::calculate_next_run(&cron_config.schedule)?;

                    let mut cron_update: crons::ActiveModel = cron.clone().into();
                    cron_update.schedule = Set(cron_config.schedule.clone());
                    cron_update.updated_at = Set(Utc::now());
                    cron_update.next_run = Set(Some(next_run));
                    cron_update.update(self.db.as_ref()).await.map_err(|e| {
                        CronConfigError::DatabaseError(format!("Failed to update cron: {}", e))
                    })?;

                    info!(
                        "✅ Updated cron job '{}' with schedule '{}', next run at {:?}",
                        cron.path, cron_config.schedule, next_run
                    );
                }
                Some(cron) => {
                    info!(
                        "✓ Cron job '{}' already exists with same schedule",
                        cron.path
                    );
                }
                None => {
                    // Create new cron
                    info!("Creating new cron job '{}'", cron_config.path);

                    let next_run = Self::calculate_next_run(&cron_config.schedule)?;
                    let now = Utc::now();

                    let new_cron = crons::ActiveModel {
                        project_id: Set(project_id),
                        environment_id: Set(environment_id),
                        path: Set(cron_config.path.clone()),
                        schedule: Set(cron_config.schedule.clone()),
                        created_at: Set(now),
                        updated_at: Set(now),
                        next_run: Set(Some(next_run)),
                        deleted_at: Set(None),
                        ..Default::default()
                    };

                    match new_cron.insert(self.db.as_ref()).await {
                        Ok(cron) => {
                            info!(
                                "✅ Created cron job '{}' with schedule '{}', next run at {:?}",
                                cron.path, cron.schedule, cron.next_run
                            );
                        }
                        Err(e) => {
                            error!("Failed to create cron job '{}': {}", cron_config.path, e);
                            return Err(CronConfigError::DatabaseError(format!(
                                "Failed to create cron: {}",
                                e
                            )));
                        }
                    }
                }
            }
        }

        info!(
            "✅ Successfully configured cron jobs for project {} in environment {}",
            project_id, environment_id
        );

        Ok(())
    }
}

impl DatabaseCronConfigService {
    /// Get all cron jobs for a specific environment
    pub async fn get_environment_crons(
        &self,
        project_id: i32,
        env_id: i32,
    ) -> Result<Vec<crons::Model>, CronConfigError> {
        let crons_list = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project_id))
            .filter(crons::Column::EnvironmentId.eq(env_id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| CronConfigError::DatabaseError(format!("Failed to fetch crons: {}", e)))?;

        Ok(crons_list)
    }

    /// Get a specific cron job by ID
    pub async fn get_cron_by_id(
        &self,
        project_id: i32,
        env_id: i32,
        cron_id: i32,
    ) -> Result<crons::Model, CronConfigError> {
        crons::Entity::find_by_id(cron_id)
            .filter(crons::Column::ProjectId.eq(project_id))
            .filter(crons::Column::EnvironmentId.eq(env_id))
            .filter(crons::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| CronConfigError::DatabaseError(format!("Failed to fetch cron: {}", e)))?
            .ok_or_else(|| {
                CronConfigError::ConfigError(format!(
                    "Cron {} not found in project {}, environment {}",
                    cron_id, project_id, env_id
                ))
            })
    }

    /// Get execution history for a cron job
    pub async fn get_cron_executions(
        &self,
        project_id: i32,
        env_id: i32,
        cron_id: i32,
        page: i64,
        per_page: i64,
    ) -> Result<Vec<temps_entities::cron_executions::Model>, CronConfigError> {
        use temps_entities::cron_executions;

        // First verify the cron exists and belongs to the project/environment
        self.get_cron_by_id(project_id, env_id, cron_id).await?;

        let query = cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron_id))
            .order_by_desc(cron_executions::Column::ExecutedAt);

        let paginator = query.paginate(self.db.as_ref(), per_page as u64);
        let executions = paginator.fetch_page((page - 1) as u64).await.map_err(|e| {
            CronConfigError::DatabaseError(format!("Failed to fetch executions: {}", e))
        })?;

        Ok(executions)
    }

    pub async fn start_cron_scheduler(&self) {
        debug!("Starting cron scheduler");

        loop {
            let now = Utc::now();

            // Only run at the start of each minute
            if now.second() != 0 {
                // Sleep until next minute
                let next_minute = now
                    .with_second(0)
                    .and_then(|dt| dt.with_nanosecond(0))
                    .unwrap()
                    + chrono::Duration::minutes(1);
                let sleep_duration = (next_minute - now).to_std().unwrap();
                time::sleep(sleep_duration).await;
                continue;
            }

            // Use a block to ensure db connection is dropped after use
            let crons_list = match crons::Entity::find().all(self.db.as_ref()).await {
                Ok(crons_list) => crons_list,
                Err(e) => {
                    error!("Failed to fetch crons: {}", e);
                    time::sleep(Duration::from_secs(60)).await;
                    continue;
                }
            };

            // Process crons in chunks to limit concurrency
            for chunk in crons_list.chunks(10) {
                let futures: Vec<_> = chunk
                    .iter()
                    .map(|cron| self.process_cron(cron, now))
                    .collect();

                let results = futures::future::join_all(futures).await;
                for result in results {
                    if let Err(e) = result {
                        error!("Error processing cron: {}", e);
                    }
                }
            }

            // Sleep until next minute
            let next_minute = now
                .with_second(0)
                .and_then(|dt| dt.with_nanosecond(0))
                .unwrap()
                + chrono::Duration::minutes(1);
            let sleep_duration = (next_minute - now).to_std().unwrap();
            time::sleep(sleep_duration).await;
        }
    }

    async fn process_cron(
        &self,
        cron: &crons::Model,
        now: DateTime<Utc>,
    ) -> Result<(), CronServiceError> {
        // Skip if cron is deleted
        if cron.deleted_at.is_some() {
            return Ok(());
        }

        let schedule =
            Schedule::from_str(&cron.schedule).map_err(|e| CronServiceError::InvalidSchedule {
                schedule: cron.schedule.clone(),
                message: e.to_string(),
            })?;
        let next_run = cron.next_run;

        let should_run = match next_run {
            Some(next) => next <= now,
            None => {
                // If next_run is not set, calculate it from the schedule
                if let Some(next) = schedule.upcoming(Utc).next() {
                    next <= now
                } else {
                    false
                }
            }
        };

        if should_run {
            // Calculate the next run time
            let next_run = schedule.upcoming(Utc).next();

            // Update the next_run time in the database
            if let Some(next_run) = next_run {
                let mut cron_update: crons::ActiveModel = cron.clone().into();
                cron_update.next_run = Set(Some(next_run));
                cron_update.update(self.db.as_ref()).await?;
            }

            // Execute the cron
            let start_time = std::time::Instant::now();
            let result = self.execute_cron(cron).await;
            let execution_time = start_time.elapsed().as_millis() as i32;

            // Get the deployment URL for recording the execution
            let url = format!("{}{}", self.get_deployment_url(cron).await?, cron.path);

            // Record the execution with appropriate status and error message
            let (status_code, error_message, headers) = match &result {
                Ok(response) => {
                    let header_map: std::collections::HashMap<String, String> = response
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
                        .collect();
                    (
                        response.status().as_u16() as i32,
                        None,
                        serde_json::to_string(&header_map).unwrap_or_default(),
                    )
                }
                Err(e) => (
                    500,
                    Some(e.to_string()),
                    serde_json::to_string(&std::collections::HashMap::<String, String>::new())
                        .unwrap_or_default(),
                ),
            };

            let new_execution = cron_executions::ActiveModel {
                cron_id: Set(cron.id),
                executed_at: Set(now),
                url: Set(url.clone()),
                status_code: Set(status_code),
                headers: Set(headers),
                response_time_ms: Set(execution_time),
                error_message: Set(error_message.clone()),
                ..Default::default()
            };

            new_execution.insert(self.db.as_ref()).await?;

            // Handle any execution errors - send notification via queue
            if let Err(e) = result {
                let error_data = temps_core::CronInvocationErrorData {
                    project_id: cron.project_id,
                    environment_id: cron.environment_id,
                    cron_job_id: cron.id,
                    cron_job_name: cron.path.clone(),
                    error_message: e.to_string(),
                    schedule: cron.schedule.clone(),
                    timestamp: Utc::now(),
                    last_successful_run: None,
                };

                if let Err(queue_err) = self.queue.send(temps_core::Job::CronInvocationError(error_data)).await {
                    warn!(
                        "Failed to send cron error notification for cron {}: {}",
                        cron.id, queue_err
                    );
                }
                return Err(e);
            }
        }

        Ok(())
    }
    async fn get_deployment_url(&self, cron: &crons::Model) -> Result<String, CronServiceError> {
        // Get the first deployment for this environment
        let deployment = deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(cron.environment_id))
            .order_by_desc(deployments::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| CronServiceError::NoActiveDeployment {
                env_id: cron.environment_id,
            })?;

        // Get deployment containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        // Pick a random container if there's more than one, otherwise use the first
        // Use deployment ID as seed for consistent selection per deployment
        let container = if containers.len() > 1 {
            use rand::seq::SliceRandom;
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(deployment.id as u64);
            containers.choose(&mut rng)
        } else {
            containers.first()
        }
        .ok_or_else(|| CronServiceError::NoActiveDeployment {
            env_id: cron.environment_id,
        })?;

        // Use container port to construct URL
        Ok(format!("http://localhost:{}", container.container_port))
    }

    async fn execute_cron(
        &self,
        cron: &crons::Model,
    ) -> Result<reqwest::Response, CronServiceError> {
        let url = format!("{}{}", self.get_deployment_url(cron).await?, cron.path);
        debug!("Executing cron {} at {}", cron.id, url);

        let response = self
            .http_client
            .get(&url)
            .header("X-Cron-Job", "true")
            .send()
            .await
            .map_err(|e| CronServiceError::ExecutionError {
                cron_id: cron.id,
                url: url.clone(),
                message: format!("Failed with status: {}", e.to_string()),
            })?;

        if !response.status().is_success() {
            return Err(CronServiceError::ExecutionFailed {
                cron_id: cron.id,
                url,
                message: format!("Failed with status: {}", response.status()),
            });
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;

    // Mock queue for tests
    struct MockQueue;

    #[async_trait]
    impl temps_core::JobQueue for MockQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("Not needed for tests")
        }
    }

    // Helper function to create test project and environment
    async fn create_test_project_and_environment(
        db: &DatabaseConnection,
    ) -> Result<(temps_entities::projects::Model, temps_entities::environments::Model), Box<dyn std::error::Error>> {
        use temps_entities::{projects, environments, types::ProjectType};

        // Create project
        let project = projects::ActiveModel {
            name: Set("test-project".to_string()),
            slug: Set("test-project".to_string()),
            directory: Set("test-directory".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(ProjectType::Server),
            ..Default::default()
        };
        let project = project.insert(db).await?;

        // Create environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("test-env".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set("test-env".to_string()),
            host: Set("test-env.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            ..Default::default()
        };
        let environment = environment.insert(db).await?;

        Ok((project, environment))
    }

    #[test]
    fn test_validate_cron_schedule_valid() {
        // Valid schedules
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 */5 * * * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 0 * * * *").is_ok());
        assert!(DatabaseCronConfigService::validate_cron_schedule("0 0 0 * * *").is_ok());
    }

    #[test]
    fn test_validate_cron_schedule_invalid() {
        // Invalid: less than 1 minute apart
        assert!(DatabaseCronConfigService::validate_cron_schedule("* * * * * *").is_err());

        // Invalid syntax
        assert!(DatabaseCronConfigService::validate_cron_schedule("invalid").is_err());
    }

    #[test]
    fn test_calculate_next_run() {
        // Should return a future timestamp
        let next_run = DatabaseCronConfigService::calculate_next_run("0 0 * * * *");
        assert!(next_run.is_ok());

        let next_run_time = next_run.unwrap();
        let now = Utc::now();
        assert!(next_run_time > now);
    }

    #[tokio::test]
    async fn test_configure_crons_create_new() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let queue = Arc::new(MockQueue);

        // Create test project and environment
        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        let service = DatabaseCronConfigService::new(db.clone(), queue);

        let configs = vec![CronConfig {
            path: "/api/cron/cleanup".to_string(),
            schedule: "0 0 * * * *".to_string(),
        }];

        service
            .configure_crons(project.id, environment.id, configs)
            .await?;

        // Verify cron was created
        let crons_list = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .all(db.as_ref())
            .await?;

        assert_eq!(crons_list.len(), 1);
        assert_eq!(crons_list[0].path, "/api/cron/cleanup");
        assert_eq!(crons_list[0].schedule, "0 0 * * * *");
        assert!(crons_list[0].next_run.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_configure_crons_update_existing() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let queue = Arc::new(MockQueue);

        // Create test project and environment
        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        let service = DatabaseCronConfigService::new(db.clone(), queue);

        // Create initial cron
        let configs = vec![CronConfig {
            path: "/api/cron/task".to_string(),
            schedule: "0 0 * * * *".to_string(),
        }];
        service
            .configure_crons(project.id, environment.id, configs)
            .await?;

        // Update with different schedule
        let updated_configs = vec![CronConfig {
            path: "/api/cron/task".to_string(),
            schedule: "0 */5 * * * *".to_string(),
        }];
        service
            .configure_crons(project.id, environment.id, updated_configs)
            .await?;

        // Verify cron was updated
        let crons_list = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(db.as_ref())
            .await?;

        assert_eq!(crons_list.len(), 1);
        assert_eq!(crons_list[0].schedule, "0 */5 * * * *");

        Ok(())
    }

    #[tokio::test]
    async fn test_configure_crons_delete_removed() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let queue = Arc::new(MockQueue);

        // Create test project and environment
        let (project, environment) = create_test_project_and_environment(db.as_ref()).await?;

        let service = DatabaseCronConfigService::new(db.clone(), queue);

        // Create two crons
        let configs = vec![
            CronConfig {
                path: "/api/cron/task1".to_string(),
                schedule: "0 0 * * * *".to_string(),
            },
            CronConfig {
                path: "/api/cron/task2".to_string(),
                schedule: "0 0 * * * *".to_string(),
            },
        ];
        service
            .configure_crons(project.id, environment.id, configs)
            .await?;

        // Update with only one cron (task2 removed)
        let updated_configs = vec![CronConfig {
            path: "/api/cron/task1".to_string(),
            schedule: "0 0 * * * *".to_string(),
        }];
        service
            .configure_crons(project.id, environment.id, updated_configs)
            .await?;

        // Verify only one active cron remains
        let active_crons = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .filter(crons::Column::DeletedAt.is_null())
            .all(db.as_ref())
            .await?;

        assert_eq!(active_crons.len(), 1);
        assert_eq!(active_crons[0].path, "/api/cron/task1");

        // Verify task2 is marked as deleted
        let all_crons = crons::Entity::find()
            .filter(crons::Column::ProjectId.eq(project.id))
            .filter(crons::Column::EnvironmentId.eq(environment.id))
            .all(db.as_ref())
            .await?;

        assert_eq!(all_crons.len(), 2);
        let deleted_cron = all_crons
            .iter()
            .find(|c| c.path == "/api/cron/task2")
            .unwrap();
        assert!(deleted_cron.deleted_at.is_some());

        Ok(())
    }
}
