//! Test utilities for workflow tests

use async_trait::async_trait;
use std::sync::Arc;
use temps_core::{LogWriter, WorkflowContext, WorkflowError};

/// Mock LogWriter for testing that doesn't actually write anything
pub struct MockLogWriter {
    stage_id: i32,
}

impl MockLogWriter {
    pub fn new(stage_id: i32) -> Self {
        Self { stage_id }
    }
}

#[async_trait]
impl LogWriter for MockLogWriter {
    async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
        // No-op for testing
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        self.stage_id
    }
}

/// Create a test WorkflowContext with a mock log writer
pub fn create_test_context(
    workflow_run_id: String,
    deployment_id: i32,
    project_id: i32,
    environment_id: i32,
) -> WorkflowContext {
    let log_writer = Arc::new(MockLogWriter::new(1));
    WorkflowContext::new(workflow_run_id, deployment_id, project_id, environment_id, log_writer)
}