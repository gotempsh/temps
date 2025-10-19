//! Stage-specific log writer that writes to deployment stage log files

use async_trait::async_trait;
use std::sync::Arc;
use temps_core::{LogWriter, WorkflowError};
use temps_logs::LogService;

/// Log writer implementation for deployment stages
/// Each stage writes to its own log file via the LogService
pub struct DeploymentStageLogWriter {
    log_service: Arc<LogService>,
    stage_id: i32,
    log_id: String,
}

impl DeploymentStageLogWriter {
    pub fn new(log_service: Arc<LogService>, stage_id: i32, log_id: String) -> Self {
        Self {
            log_service,
            stage_id,
            log_id,
        }
    }
}

#[async_trait]
impl LogWriter for DeploymentStageLogWriter {
    async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
        self.log_service
            .append_to_log(&self.log_id, &message)
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        self.stage_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stage_log_writer() {
        // Create a real LogService with file backend for testing
        let temp_dir = std::env::temp_dir().join("temps-test-logs");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let log_service = Arc::new(temps_logs::LogService::new(temp_dir.clone()));

        let writer = DeploymentStageLogWriter::new(
            log_service.clone(),
            123,
            "test-log".to_string(),
        );

        assert_eq!(writer.stage_id(), 123);

        // Test writing logs (file will be created automatically)
        writer.write_log("Test message\n".to_string()).await.unwrap();
        writer.write_logs(vec![
            "Line 1\n".to_string(),
            "Line 2\n".to_string(),
        ]).await.unwrap();

        // Verify logs were written
        let content = log_service.get_log_content("test-log").await.unwrap();
        assert!(content.contains("Test message"));
        assert!(content.contains("Line 1"));
        assert!(content.contains("Line 2"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap_or(());
    }
}