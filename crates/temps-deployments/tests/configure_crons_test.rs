//! Standalone tests for cron configuration
//!
//! These tests can be run independently from the main test suite

use async_trait::async_trait;
use std::sync::Arc;
use temps_core::TempsConfig;
use temps_deployments::jobs::configure_crons::{
    CronConfig, CronConfigError, CronConfigService, NoOpCronConfigService,
};

// Mock CronConfigService for testing
struct MockCronConfigService {
    should_fail: bool,
    captured_configs: Arc<std::sync::Mutex<Vec<CronConfig>>>,
}

impl MockCronConfigService {
    fn new() -> Self {
        Self {
            should_fail: false,
            captured_configs: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn with_failure() -> Self {
        Self {
            should_fail: true,
            captured_configs: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn get_captured_configs(&self) -> Vec<CronConfig> {
        self.captured_configs.lock().unwrap().clone()
    }
}

#[async_trait]
impl CronConfigService for MockCronConfigService {
    async fn configure_crons(
        &self,
        _project_id: i32,
        _environment_id: i32,
        cron_configs: Vec<CronConfig>,
    ) -> Result<(), CronConfigError> {
        if self.should_fail {
            return Err(CronConfigError::ConfigError("Mock failure".to_string()));
        }
        self.captured_configs.lock().unwrap().extend(cron_configs);
        Ok(())
    }
}

#[test]
fn test_parse_temps_config() {
    let yaml = r#"
cron:
  - path: /api/cron/cleanup
    schedule: "0 0 * * *"
  - path: /api/cron/reports
    schedule: "0 9 * * 1"
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    assert!(config.has_crons());

    let crons = config.cron_jobs();
    assert_eq!(crons.len(), 2);
    assert_eq!(crons[0].path, "/api/cron/cleanup");
    assert_eq!(crons[0].schedule, "0 0 * * *");
    assert_eq!(crons[1].path, "/api/cron/reports");
    assert_eq!(crons[1].schedule, "0 9 * * 1");
}

#[test]
fn test_parse_temps_config_no_crons() {
    let yaml = r#"
# No cron configuration
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    assert!(!config.has_crons());
}

#[test]
fn test_parse_temps_config_empty_crons() {
    let yaml = r#"
cron: []
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    assert!(!config.has_crons());
    assert_eq!(config.cron_jobs().len(), 0);
}

#[test]
fn test_parse_temps_config_with_names() {
    let yaml = r#"
cron:
  - path: /api/cron/cleanup
    schedule: "0 0 * * *"
    name: "Daily Cleanup"
  - path: /api/cron/backup
    schedule: "0 2 * * *"
    name: "Nightly Backup"
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    assert!(config.has_crons());

    let crons = config.cron_jobs();
    assert_eq!(crons.len(), 2);
    assert_eq!(crons[0].name.as_deref(), Some("Daily Cleanup"));
    assert_eq!(crons[1].name.as_deref(), Some("Nightly Backup"));
}

#[test]
fn test_parse_temps_config_mixed_with_other_sections() {
    let yaml = r#"
cron:
  - path: /api/cron/task
    schedule: "*/5 * * * *"

build:
  dockerfile: Dockerfile
  context: .

env:
  NODE_ENV: production
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    assert!(config.has_crons());
    assert!(config.has_build_config());
    assert!(config.env.is_some());

    let crons = config.cron_jobs();
    assert_eq!(crons.len(), 1);
    assert_eq!(crons[0].path, "/api/cron/task");
}

#[test]
fn test_cron_config_conversion() {
    let yaml = r#"
cron:
  - path: /api/health
    schedule: "*/1 * * * *"
  - path: /api/cleanup
    schedule: "0 0 * * *"
  - path: /api/reports
    schedule: "0 9 * * 1"
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    let cron_jobs = config.cron_jobs();

    // Convert to CronConfig format
    let cron_configs: Vec<CronConfig> = cron_jobs
        .iter()
        .map(|job| CronConfig {
            path: job.path.clone(),
            schedule: job.schedule.clone(),
        })
        .collect();

    assert_eq!(cron_configs.len(), 3);
    assert_eq!(cron_configs[0].path, "/api/health");
    assert_eq!(cron_configs[0].schedule, "*/1 * * * *");
    assert_eq!(cron_configs[1].path, "/api/cleanup");
    assert_eq!(cron_configs[1].schedule, "0 0 * * *");
    assert_eq!(cron_configs[2].path, "/api/reports");
    assert_eq!(cron_configs[2].schedule, "0 9 * * 1");
}

#[test]
fn test_noop_cron_service() {
    let service = NoOpCronConfigService;
    let configs = vec![CronConfig {
        path: "/test".to_string(),
        schedule: "* * * * *".to_string(),
    }];

    // Should succeed without doing anything
    let result = tokio_test::block_on(service.configure_crons(1, 1, configs));
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_mock_cron_service_success() {
    let service = MockCronConfigService::new();
    let configs = vec![
        CronConfig {
            path: "/api/cron/task1".to_string(),
            schedule: "0 0 * * *".to_string(),
        },
        CronConfig {
            path: "/api/cron/task2".to_string(),
            schedule: "0 12 * * *".to_string(),
        },
    ];

    let result = service.configure_crons(1, 1, configs).await;
    assert!(result.is_ok());

    let captured = service.get_captured_configs();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].path, "/api/cron/task1");
    assert_eq!(captured[1].path, "/api/cron/task2");
}

#[tokio::test]
async fn test_mock_cron_service_failure() {
    let service = MockCronConfigService::with_failure();
    let configs = vec![CronConfig {
        path: "/api/cron/task".to_string(),
        schedule: "0 0 * * *".to_string(),
    }];

    let result = service.configure_crons(1, 1, configs).await;
    assert!(result.is_err());

    match result {
        Err(CronConfigError::ConfigError(msg)) => {
            assert_eq!(msg, "Mock failure");
        }
        _ => panic!("Expected ConfigError"),
    }
}

#[test]
fn test_cron_config_error_display() {
    let err = CronConfigError::DatabaseError("Connection failed".to_string());
    assert_eq!(err.to_string(), "Database error: Connection failed");

    let err = CronConfigError::InvalidSchedule("Bad format".to_string());
    assert_eq!(err.to_string(), "Invalid cron schedule: Bad format");

    let err = CronConfigError::ConfigError("Missing field".to_string());
    assert_eq!(err.to_string(), "Configuration error: Missing field");
}

#[test]
fn test_parse_invalid_yaml() {
    let yaml = r#"
cron:
  - path: /test
    schedule: "0 0 * * *"
    invalid_field_that_breaks_parsing: [[[
"#;

    let result = TempsConfig::from_yaml(yaml);
    assert!(result.is_err());
}

#[test]
fn test_parse_malformed_yaml() {
    let yaml = r#"
cron:
  - path
    schedule
"#;

    let result = TempsConfig::from_yaml(yaml);
    assert!(result.is_err());
}

#[test]
fn test_empty_cron_config_vec() {
    let yaml = r#"
cron: []
"#;
    let config = TempsConfig::from_yaml(yaml).unwrap();

    // Should handle empty cron array gracefully
    let cron_jobs = config.cron_jobs();
    assert_eq!(cron_jobs.len(), 0);

    let cron_configs: Vec<CronConfig> = cron_jobs
        .iter()
        .map(|job| CronConfig {
            path: job.path.clone(),
            schedule: job.schedule.clone(),
        })
        .collect();

    assert_eq!(cron_configs.len(), 0);
}

#[test]
fn test_single_cron_job() {
    let yaml = r#"
cron:
  - path: /api/single
    schedule: "0 0 * * *"
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    assert!(config.has_crons());

    let crons = config.cron_jobs();
    assert_eq!(crons.len(), 1);
    assert_eq!(crons[0].path, "/api/single");
    assert_eq!(crons[0].schedule, "0 0 * * *");
}

#[test]
fn test_complex_cron_schedules() {
    let yaml = r#"
cron:
  - path: /api/every-minute
    schedule: "* * * * *"
  - path: /api/every-hour
    schedule: "0 * * * *"
  - path: /api/daily-midnight
    schedule: "0 0 * * *"
  - path: /api/weekly-monday
    schedule: "0 0 * * 1"
  - path: /api/monthly-first
    schedule: "0 0 1 * *"
  - path: /api/complex
    schedule: "*/15 9-17 * * 1-5"
"#;

    let config = TempsConfig::from_yaml(yaml).unwrap();
    let crons = config.cron_jobs();

    assert_eq!(crons.len(), 6);
    assert_eq!(crons[0].schedule, "* * * * *");
    assert_eq!(crons[1].schedule, "0 * * * *");
    assert_eq!(crons[2].schedule, "0 0 * * *");
    assert_eq!(crons[3].schedule, "0 0 * * 1");
    assert_eq!(crons[4].schedule, "0 0 1 * *");
    assert_eq!(crons[5].schedule, "*/15 9-17 * * 1-5");
}
