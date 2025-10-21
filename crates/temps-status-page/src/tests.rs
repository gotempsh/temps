use crate::services::*;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
use serde_json;
use std::sync::Arc;
use temps_config::{ConfigService, ServerConfig};
use temps_core::{Job, JobQueue, JobReceiver, QueueError};
use temps_database::test_utils::TestDatabase;
use temps_entities::{deployments, environments, projects, status_monitors, types::{PipelineStatus, ProjectType}};

fn create_test_config_service(db: &Arc<DatabaseConnection>) -> Arc<ConfigService> {
    let config = ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgres://test:test@localhost/test".to_string(),
        None,
        None,
    ).expect("Failed to create test config");
    Arc::new(ConfigService::new(Arc::new(config), db.clone()))
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::plugin::StatusPagePlugin;
    use temps_core::plugin::TempsPlugin;

    #[test]
    fn test_plugin_name() {
        let plugin = StatusPagePlugin::new();
        assert_eq!(plugin.name(), "status-page");
    }

    #[test]
    fn test_create_monitor_request() {
        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: 1,
            check_interval_seconds: Some(60),
        };

        assert_eq!(request.name, "Test Monitor");
        assert_eq!(request.monitor_type, "web");
        assert_eq!(request.environment_id, 1);
        assert_eq!(request.check_interval_seconds, Some(60));
    }

    #[test]
    fn test_create_incident_request() {
        let request = CreateIncidentRequest {
            title: "Service Outage".to_string(),
            description: Some("API is down".to_string()),
            severity: "critical".to_string(),
            environment_id: Some(1),
            monitor_id: Some(1),
        };

        assert_eq!(request.title, "Service Outage");
        assert_eq!(request.severity, "critical");
        assert!(request.description.is_some());
    }

    #[test]
    fn test_update_incident_status_request() {
        let request = UpdateIncidentStatusRequest {
            status: "resolved".to_string(),
            message: "Issue has been fixed".to_string(),
        };

        assert_eq!(request.status, "resolved");
        assert_eq!(request.message, "Issue has been fixed");
    }

    #[test]
    fn test_monitor_types() {
        let web_monitor = CreateMonitorRequest {
            name: "Web Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: 1,
            check_interval_seconds: None,
        };

        let api_monitor = CreateMonitorRequest {
            name: "API Monitor".to_string(),
            monitor_type: "api".to_string(),
            environment_id: 1,
            check_interval_seconds: None,
        };

        assert_eq!(web_monitor.monitor_type, "web");
        assert_eq!(api_monitor.monitor_type, "api");
    }

    #[test]
    fn test_incident_severities() {
        let minor = CreateIncidentRequest {
            title: "Minor Issue".to_string(),
            description: None,
            severity: "minor".to_string(),
            environment_id: Some(1),
            monitor_id: None,
        };

        let major = CreateIncidentRequest {
            title: "Major Issue".to_string(),
            description: None,
            severity: "major".to_string(),
            environment_id: Some(1),
            monitor_id: None,
        };

        let critical = CreateIncidentRequest {
            title: "Critical Issue".to_string(),
            description: None,
            severity: "critical".to_string(),
            environment_id: Some(1),
            monitor_id: None,
        };

        assert_eq!(minor.severity, "minor");
        assert_eq!(major.severity, "major");
        assert_eq!(critical.severity, "critical");
    }

    #[test]
    fn test_default_check_interval() {
        let monitor_without_interval = CreateMonitorRequest {
            name: "Test".to_string(),
            monitor_type: "web".to_string(),
            environment_id: 1,
            check_interval_seconds: None,
        };

        let monitor_with_interval = CreateMonitorRequest {
            name: "Test".to_string(),
            monitor_type: "web".to_string(),
            environment_id: 1,
            check_interval_seconds: Some(300),
        };

        assert!(monitor_without_interval.check_interval_seconds.is_none());
        assert_eq!(monitor_with_interval.check_interval_seconds, Some(300));
    }

    #[test]
    fn test_status_page_error() {
        let err = StatusPageError::NotFound;
        assert_eq!(format!("{}", err), "Not found");

        let err = StatusPageError::Validation("Invalid input".to_string());
        assert_eq!(format!("{}", err), "Validation error: Invalid input");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use tokio::sync::Mutex;

    // Mock Queue implementation for testing
    struct MockQueue {
        jobs: Arc<Mutex<VecDeque<Job>>>,
    }

    impl MockQueue {
        fn new() -> Self {
            Self {
                jobs: Arc::new(Mutex::new(VecDeque::new())),
            }
        }
    }

    #[async_trait]
    impl JobQueue for MockQueue {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            let mut jobs = self.jobs.lock().await;
            jobs.push_back(job);
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            Box::new(MockReceiver {
                jobs: self.jobs.clone(),
            })
        }
    }

    struct MockReceiver {
        jobs: Arc<Mutex<VecDeque<Job>>>,
    }

    #[async_trait]
    impl JobReceiver for MockReceiver {
        async fn recv(&mut self) -> Result<Job, QueueError> {
            let mut jobs = self.jobs.lock().await;
            jobs.pop_front()
                .ok_or(QueueError::ChannelClosed)
        }
    }

    #[tokio::test]
    async fn test_monitor_service_create_and_get() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project first
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.test.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        // Test monitor service
        let config_service = create_test_config_service(&db);
        let monitor_service = MonitorService::new(db.clone(), config_service);

        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };

        let monitor = monitor_service
            .create_monitor(project.id, request)
            .await
            .unwrap();

        assert_eq!(monitor.name, "Test Monitor");
        assert_eq!(monitor.monitor_type, "web");
        assert_eq!(monitor.project_id, project.id);
        assert_eq!(monitor.environment_id, Some(environment.id));

        // Test get monitor
        let fetched = monitor_service.get_monitor(monitor.id).await.unwrap();
        assert_eq!(fetched.id, monitor.id);
        assert_eq!(fetched.name, monitor.name);
    }

    #[tokio::test]
    async fn test_monitor_service_ensure_monitor_for_environment() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("staging".to_string()),
            slug: Set("staging".to_string()),
            subdomain: Set("staging".to_string()),
            host: Set("staging.test.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            branch: Set(Some("staging".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        let config_service = create_test_config_service(&db);
        let monitor_service = MonitorService::new(db.clone(), config_service);

        // First call should create a monitor
        let monitor1 = monitor_service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();

        assert_eq!(monitor1.name, format!("{} Monitor", environment.name));
        assert_eq!(monitor1.environment_id, Some(environment.id));

        // Second call should return the same monitor
        let monitor2 = monitor_service
            .ensure_monitor_for_environment(project.id, environment.id, &environment.name)
            .await
            .unwrap();

        assert_eq!(monitor1.id, monitor2.id);
    }

    #[tokio::test]
    async fn test_monitor_service_list_monitors() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("test-env".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set("test-env".to_string()),
            host: Set("test-env.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        let config_service = create_test_config_service(&db);
        let monitor_service = MonitorService::new(db.clone(), config_service);

        // Create multiple monitors
        for i in 1..=3 {
            let request = CreateMonitorRequest {
                name: format!("Monitor {}", i),
                monitor_type: "web".to_string(),
                environment_id: environment.id,
                check_interval_seconds: Some(60),
            };
            monitor_service.create_monitor(project.id, request).await.unwrap();
        }

        // List all monitors for the project
        let monitors = monitor_service
            .list_monitors(project.id, None)
            .await
            .unwrap();

        assert_eq!(monitors.len(), 3);
    }

    #[tokio::test]
    async fn test_monitor_service_record_check() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("test-env".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set("test-env".to_string()),
            host: Set("test-env.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        let config_service = create_test_config_service(&db);
        let monitor_service = MonitorService::new(db.clone(), config_service);

        // Create a monitor
        let request = CreateMonitorRequest {
            name: "Test Monitor".to_string(),
            monitor_type: "web".to_string(),
            environment_id: environment.id,
            check_interval_seconds: Some(60),
        };
        let monitor = monitor_service.create_monitor(project.id, request).await.unwrap();

        // Record a check
        let check = monitor_service
            .record_check(
                monitor.id,
                "operational".to_string(),
                Some(150),
                None,
            )
            .await
            .unwrap();

        assert_eq!(check.monitor_id, monitor.id);
        assert_eq!(check.status, "operational");
        assert_eq!(check.response_time_ms, Some(150));

        // Get latest check
        let latest = monitor_service
            .get_latest_check(monitor.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(latest.id, check.id);
    }

    #[tokio::test]
    async fn test_incident_service_create_and_get() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("test-env".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set("test-env".to_string()),
            host: Set("test-env.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        let incident_service = IncidentService::new(db.clone());

        let request = CreateIncidentRequest {
            title: "Database Connection Issue".to_string(),
            description: Some("Unable to connect to primary database".to_string()),
            severity: "major".to_string(),
            environment_id: Some(environment.id),
            monitor_id: None,
        };

        let incident = incident_service
            .create_incident(project.id, request)
            .await
            .unwrap();

        assert_eq!(incident.title, "Database Connection Issue");
        assert_eq!(incident.severity, "major");
        assert_eq!(incident.status, "investigating");
        assert!(incident.resolved_at.is_none());

        // Get the incident
        let fetched = incident_service.get_incident(incident.id).await.unwrap();
        assert_eq!(fetched.id, incident.id);
    }

    #[tokio::test]
    async fn test_incident_service_update_status() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("test-env".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set("test-env".to_string()),
            host: Set("test-env.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        let incident_service = IncidentService::new(db.clone());

        // Create an incident
        let request = CreateIncidentRequest {
            title: "API Latency".to_string(),
            description: None,
            severity: "minor".to_string(),
            environment_id: Some(environment.id),
            monitor_id: None,
        };

        let incident = incident_service
            .create_incident(project.id, request)
            .await
            .unwrap();

        // Update status to resolved
        let update = UpdateIncidentStatusRequest {
            status: "resolved".to_string(),
            message: "Issue has been fixed by scaling up the servers".to_string(),
        };

        let updated = incident_service
            .update_incident_status(incident.id, update)
            .await
            .unwrap();

        assert_eq!(updated.status, "resolved");
        assert!(updated.resolved_at.is_some());

        // Get incident updates
        let updates = incident_service
            .get_incident_updates(incident.id)
            .await
            .unwrap();

        assert!(updates.len() >= 2); // Initial + status update
    }

    #[tokio::test]
    async fn test_incident_service_validation() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        let incident_service = IncidentService::new(db.clone());

        // Try to create incident with invalid severity
        let request = CreateIncidentRequest {
            title: "Test".to_string(),
            description: None,
            severity: "invalid".to_string(),
            environment_id: Some(1),
            monitor_id: None,
        };

        let result = incident_service.create_incident(project.id, request).await;
        assert!(result.is_err());

        match result.err().unwrap() {
            StatusPageError::Validation(msg) => {
                assert!(msg.contains("Invalid severity"));
            }
            _ => panic!("Expected validation error"),
        }
    }

    #[tokio::test]
    async fn test_status_page_service_overview() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.test.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        let config_service = create_test_config_service(&db);
        let status_page_service = StatusPageService::new(db.clone(), config_service);

        // Create a monitor through the service
        status_page_service
            .on_environment_created(project.id, environment.id, &environment.name)
            .await
            .unwrap();

        // Get status overview
        let overview = status_page_service
            .get_status_overview(project.id, Some(environment.id))
            .await
            .unwrap();

        assert_eq!(overview.status, "operational"); // Should be operational by default
        assert!(!overview.monitors.is_empty()); // Should have the auto-created monitor
        assert!(overview.recent_incidents.is_empty()); // No incidents yet
    }

    #[tokio::test]
    async fn test_environment_created_job_creates_monitor() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("prod-subdomain".to_string()),
            host: Set("production.test.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        // Create a queue and receiver for testing
        let queue = Arc::new(MockQueue::new());
        let mut receiver = queue.subscribe();

        // Send an EnvironmentCreated job
        let job = temps_core::Job::EnvironmentCreated(temps_core::EnvironmentCreatedJob {
            environment_id: environment.id,
            environment_name: environment.name.clone(),
            project_id: project.id,
            subdomain: environment.subdomain.clone(),
        });
        queue.send(job.clone()).await.unwrap();

        // Create monitor service and process the job
        let config_service = create_test_config_service(&db);
        let monitor_service = Arc::new(MonitorService::new(db.clone(), config_service));

        // Process the job directly (simulating what the plugin does)
        match receiver.recv().await.unwrap() {
            temps_core::Job::EnvironmentCreated(env_job) => {
                let result = monitor_service
                    .ensure_monitor_for_environment(
                        env_job.project_id,
                        env_job.environment_id,
                        &env_job.environment_name,
                    )
                    .await;

                assert!(result.is_ok(), "Failed to create monitor: {:?}", result.err());
                let monitor = result.unwrap();
                assert_eq!(monitor.environment_id, Some(environment.id));
                assert_eq!(monitor.project_id, project.id);
                assert_eq!(monitor.name, format!("{} Monitor", environment.name));
            }
            _ => panic!("Unexpected job type"),
        }

        // Verify monitor was created in database
        let monitors = status_monitors::Entity::find()
            .all(db.as_ref())
            .await
            .unwrap();

        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].environment_id, Some(environment.id));
        assert_eq!(monitors[0].project_id, project.id);
    }

    #[tokio::test]
    async fn test_environment_deleted_job_deletes_monitors() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create an environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("staging".to_string()),
            slug: Set("staging".to_string()),
            subdomain: Set("staging-subdomain".to_string()),
            host: Set("staging.test.local".to_string()),
            upstreams: Set(serde_json::json!([])),
            branch: Set(Some("staging".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        let config_service = create_test_config_service(&db);
        let monitor_service = Arc::new(MonitorService::new(db.clone(), config_service));

        // First create some monitors for this environment
        for i in 1..=3 {
            let request = CreateMonitorRequest {
                name: format!("Monitor {}", i),
                monitor_type: "web".to_string(),
                environment_id: environment.id,
                check_interval_seconds: Some(60),
            };
            monitor_service.create_monitor(project.id, request).await.unwrap();
        }

        // Verify monitors exist
        let monitors_before = monitor_service
            .list_monitors(project.id, Some(environment.id))
            .await
            .unwrap();
        assert_eq!(monitors_before.len(), 3);

        // Create a queue and receiver for testing
        let queue = Arc::new(MockQueue::new());
        let mut receiver = queue.subscribe();

        // Send an EnvironmentDeleted job
        let job = temps_core::Job::EnvironmentDeleted(temps_core::EnvironmentDeletedJob {
            environment_id: environment.id,
            environment_name: environment.name.clone(),
            project_id: project.id,
        });
        queue.send(job).await.unwrap();

        // Process the job
        match receiver.recv().await.unwrap() {
            temps_core::Job::EnvironmentDeleted(env_job) => {
                // Get all monitors for this environment
                let monitors = monitor_service
                    .list_monitors(env_job.project_id, Some(env_job.environment_id))
                    .await
                    .unwrap();

                // Delete each monitor
                for monitor in monitors {
                    let result = monitor_service.delete_monitor(monitor.id).await;
                    assert!(result.is_ok(), "Failed to delete monitor {}: {:?}", monitor.id, result.err());
                }
            }
            _ => panic!("Unexpected job type"),
        }

        // Verify all monitors were deleted
        let monitors_after = monitor_service
            .list_monitors(project.id, Some(environment.id))
            .await
            .unwrap();
        assert_eq!(monitors_after.len(), 0);
    }

    #[tokio::test]
    async fn test_project_created_and_deleted_jobs() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create a queue for testing
        let queue = Arc::new(MockQueue::new());
        let mut receiver = queue.subscribe();

        // Send a ProjectCreated job
        let job = temps_core::Job::ProjectCreated(temps_core::ProjectCreatedJob {
            project_id: project.id,
            project_name: project.name.clone(),
        });
        queue.send(job).await.unwrap();

        // Process ProjectCreated job (should not create any monitors)
        match receiver.recv().await.unwrap() {
            temps_core::Job::ProjectCreated(job) => {
                // ProjectCreated doesn't create monitors, just log
                println!("Received ProjectCreated job: project_id={}, name={}", job.project_id, job.project_name);
            }
            _ => panic!("Unexpected job type"),
        }

        // Verify no monitors were created
        let monitors = status_monitors::Entity::find()
            .all(db.as_ref())
            .await
            .unwrap();
        assert_eq!(monitors.len(), 0);

        // Now test ProjectDeleted
        let job = temps_core::Job::ProjectDeleted(temps_core::ProjectDeletedJob {
            project_id: project.id,
            project_name: project.name.clone(),
        });
        queue.send(job).await.unwrap();

        // Process ProjectDeleted job (should not do anything since environments handle their own deletion)
        match receiver.recv().await.unwrap() {
            temps_core::Job::ProjectDeleted(job) => {
                // ProjectDeleted doesn't delete monitors directly, environments handle that
                println!("Received ProjectDeleted job: project_id={}, name={}", job.project_id, job.project_name);
            }
            _ => panic!("Unexpected job type"),
        }
    }

    #[tokio::test]
    async fn test_multiple_environments_lifecycle() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Multi-Env Project".to_string()),
            slug: Set("multi-env".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        let config_service = create_test_config_service(&db);
        let monitor_service = Arc::new(MonitorService::new(db.clone(), config_service));
        let queue = Arc::new(MockQueue::new());

        // Create multiple environments and send EnvironmentCreated jobs
        let env_names = vec!["production", "staging", "development"];
        let mut environment_ids = Vec::new();

        for name in &env_names {
            let environment = environments::ActiveModel {
                project_id: Set(project.id),
                name: Set(name.to_string()),
                slug: Set(name.to_string()),
                subdomain: Set(format!("{}-subdomain", name)),
                host: Set(format!("{}.test.local", name)),
                upstreams: Set(serde_json::json!([])),
                branch: Set(Some(name.to_string())),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            };
            let environment = environment.insert(db.as_ref()).await.unwrap();
            environment_ids.push(environment.id);

            // Send EnvironmentCreated job
            let job = temps_core::Job::EnvironmentCreated(temps_core::EnvironmentCreatedJob {
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                project_id: project.id,
                subdomain: environment.subdomain.clone(),
            });
            queue.send(job).await.unwrap();
        }

        // Process all EnvironmentCreated jobs
        let mut receiver = queue.subscribe();
        for _ in 0..env_names.len() {
            match receiver.recv().await.unwrap() {
                temps_core::Job::EnvironmentCreated(env_job) => {
                    monitor_service
                        .ensure_monitor_for_environment(
                            env_job.project_id,
                            env_job.environment_id,
                            &env_job.environment_name,
                        )
                        .await
                        .unwrap();
                }
                _ => panic!("Unexpected job type"),
            }
        }

        // Verify monitors were created for all environments
        let all_monitors = monitor_service
            .list_monitors(project.id, None)
            .await
            .unwrap();
        assert_eq!(all_monitors.len(), 3);

        // Delete one environment
        let job = temps_core::Job::EnvironmentDeleted(temps_core::EnvironmentDeletedJob {
            environment_id: environment_ids[1], // Delete staging
            environment_name: "staging".to_string(),
            project_id: project.id,
        });
        queue.send(job).await.unwrap();

        // Process the deletion
        match receiver.recv().await.unwrap() {
            temps_core::Job::EnvironmentDeleted(env_job) => {
                let monitors = monitor_service
                    .list_monitors(env_job.project_id, Some(env_job.environment_id))
                    .await
                    .unwrap();

                for monitor in monitors {
                    monitor_service.delete_monitor(monitor.id).await.unwrap();
                }
            }
            _ => panic!("Unexpected job type"),
        }

        // Verify only 2 monitors remain
        let remaining_monitors = monitor_service
            .list_monitors(project.id, None)
            .await
            .unwrap();
        assert_eq!(remaining_monitors.len(), 2);

        // Verify the staging monitor was deleted
        let staging_monitors = monitor_service
            .list_monitors(project.id, Some(environment_ids[1]))
            .await
            .unwrap();
        assert_eq!(staging_monitors.len(), 0);
    }

    #[tokio::test]
    async fn test_health_check_service_initialize_monitors() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            project_type: Set(ProjectType::Static),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create multiple environments and save the first one's ID
        let mut first_env_id = None;
        for i in 1..=3 {
            let environment = environments::ActiveModel {
                project_id: Set(project.id),
                name: Set(format!("env-{}", i)),
                slug: Set(format!("env-{}", i)),
                subdomain: Set(format!("env-{}", i)),
                host: Set(format!("env-{}.test.local", i)),
                upstreams: Set(serde_json::json!([])),
                branch: Set(Some("main".to_string())),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            };
            let env = environment.insert(db.as_ref()).await.unwrap();
            if i == 1 {
                first_env_id = Some(env.id);
            }
        }

        // Create a deployment for one environment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(first_env_id.unwrap()),
            commit_sha: Set(Some("abc123".to_string())),
            commit_message: Set(Some("Test deployment".to_string())),
            branch_ref: Set(Some("main".to_string())),
            slug: Set("test-deployment".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(serde_json::json!({})),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        deployment.insert(db.as_ref()).await.unwrap();

        // Create a mock ConfigService for testing
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: test_db.database_url.clone(),
            tls_address: None,
            console_address: "127.0.0.1:8080".to_string(),
            data_dir: std::path::PathBuf::from("/tmp/test"),
            auth_secret: "test_secret".to_string(),
            encryption_key: "test_encryption_key_32_bytes_long!!".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
        });
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
        let health_check_service = HealthCheckService::new(db.clone(), config_service);

        // Initialize monitors for all environments
        health_check_service.initialize_monitors().await.unwrap();

        // Verify monitors were created
        let monitors = status_monitors::Entity::find()
            .all(db.as_ref())
            .await
            .unwrap();

        assert_eq!(monitors.len(), 3); // One for each environment
    }
}