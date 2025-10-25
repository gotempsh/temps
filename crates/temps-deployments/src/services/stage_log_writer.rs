//! Stage-specific log writer that writes to deployment stage log files

use async_trait::async_trait;
use std::sync::Arc;
use temps_core::{LogWriter, WorkflowError};
use temps_logs::{LogLevel, LogService};

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

    /// Detect log level from message content
    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌") || message.contains("Failed") || message.contains("Error") || message.contains("error") {
            LogLevel::Error
        } else if message.contains("⏳") || message.contains("Waiting") || message.contains("warning") {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }
}

#[async_trait]
impl LogWriter for DeploymentStageLogWriter {
    async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content
        let level = Self::detect_log_level(&message);

        self.log_service
            .append_structured_log(&self.log_id, level, message)
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

        let writer =
            DeploymentStageLogWriter::new(log_service.clone(), 123, "test-log".to_string());

        assert_eq!(writer.stage_id(), 123);

        // Test writing logs (file will be created automatically)
        writer
            .write_log("Test message".to_string())
            .await
            .unwrap();
        writer
            .write_logs(vec!["Line 1".to_string(), "Line 2".to_string()])
            .await
            .unwrap();

        // Verify logs were written as structured logs
        let logs = log_service.get_structured_logs("test-log").await.unwrap();
        assert!(logs.len() >= 3);
        assert!(logs.iter().any(|l| l.message.contains("Test message")));
        assert!(logs.iter().any(|l| l.message.contains("Line 1")));
        assert!(logs.iter().any(|l| l.message.contains("Line 2")));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap_or(());
    }
}
