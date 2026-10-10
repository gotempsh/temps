// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use temps_core::UtcDateTime;
use tracing::{debug, error, info};

/// Represents an externally pushed image (not built from git)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalImage {
    pub id: String,
    pub image_ref: String,
    pub digest: Option<String>,
    pub size: Option<u64>,
    pub pushed_at: UtcDateTime,
    pub metadata: Option<serde_json::Value>,
}

/// Deployment operation that can be executed independently
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DeploymentOperation {
    Deploy,
    MarkComplete,
    TakeScreenshot,
}

impl std::fmt::Display for DeploymentOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeploymentOperation::Deploy => write!(f, "deploy"),
            DeploymentOperation::MarkComplete => write!(f, "mark_complete"),
            DeploymentOperation::TakeScreenshot => write!(f, "take_screenshot"),
        }
    }
}

/// Where an operation is in its lifecycle.
///
/// An operation that does its work in the background (a screenshot capture)
/// is recorded as `Pending` when it starts and replaced by a `Completed` or
/// `Failed` record when it ends, so a caller can poll for the outcome instead
/// of being told it succeeded before anything ran.
///
/// Published as `DeploymentOperationStatus`: `temps-projects` already owns the
/// `OperationStatus` schema name, and two schemas with one name silently
/// replace each other in the merged OpenAPI document.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = DeploymentOperationStatus)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Pending,
    Completed,
    Failed,
}

/// What an operation record refers to, and what it produced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct DeploymentOperationDetails {
    pub project_id: i32,
    pub deployment_id: String,
    /// The stored image. Set only on a `completed` `take_screenshot`.
    pub screenshot: Option<DeploymentScreenshotCapture>,
}

impl DeploymentOperationDetails {
    pub fn new(project_id: i32, deployment_id: impl Into<String>) -> Self {
        Self {
            project_id,
            deployment_id: deployment_id.into(),
            screenshot: None,
        }
    }
}

/// A screenshot captured by a `take_screenshot` operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct DeploymentScreenshotCapture {
    /// Deployment URL that was captured.
    pub url: String,
    /// Image path relative to the static files directory, as recorded on the
    /// deployment's `screenshot_location`.
    pub screenshot_location: String,
    #[schema(value_type = String, format = DateTime, example = "2026-10-10T01:25:34.793514Z")]
    pub captured_at: UtcDateTime,
}

/// Result of an executed operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub operation: DeploymentOperation,
    pub status: OperationStatus,
    /// `true` only for a `Completed` operation.
    pub success: bool,
    pub message: String,
    pub data: DeploymentOperationDetails,
    pub executed_at: UtcDateTime,
}

/// Request to push an external image
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PushImageRequest {
    pub image_ref: String,
    pub metadata: Option<serde_json::Value>,
}

/// Request to deploy from external image
#[derive(Debug, Clone, Deserialize)]
pub struct DeployExternalImageRequest {
    pub image_ref: String,
    pub num_replicas: Option<i32>,
    pub environment_variables: Option<HashMap<String, String>>,
}

/// Response for in-memory external image operations (legacy push flow).
///
/// Renamed to avoid shadowing the richer database-backed `ExternalImageResponse`
/// in `handlers/remote_deployments.rs`. The two types serve different routes
/// (`/images` ephemeral push vs `/external-images` registered images).
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PushedExternalImageResponse {
    pub id: String,
    pub image_ref: String,
    pub digest: Option<String>,
    pub size: Option<u64>,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub pushed_at: UtcDateTime,
}

impl From<ExternalImage> for PushedExternalImageResponse {
    fn from(image: ExternalImage) -> Self {
        Self {
            id: image.id,
            image_ref: image.image_ref,
            digest: image.digest,
            size: image.size,
            pushed_at: image.pushed_at,
        }
    }
}

/// Operation records kept per deployment; older ones are dropped first.
const MAX_OPERATIONS_PER_DEPLOYMENT: usize = 50;

/// Operation history is keyed by project *and* deployment, so a caller
/// authorized for one project can never read or append to another project's
/// records by supplying a foreign deployment ID.
type OperationKey = (i32, String);

/// In-memory store for external images and operation results
/// This keeps external images and operations in memory without database changes
#[derive(Clone)]
pub struct ExternalDeploymentManager {
    images: Arc<RwLock<HashMap<String, ExternalImage>>>,
    operations: Arc<RwLock<HashMap<OperationKey, Vec<OperationResult>>>>,
}

impl ExternalDeploymentManager {
    pub fn new() -> Self {
        Self {
            images: Arc::new(RwLock::new(HashMap::new())),
            operations: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an externally pushed image
    pub fn push_image(&self, image: ExternalImage) -> Result<ExternalImage, String> {
        debug!("Pushing external image: {}", image.image_ref);

        let mut images = self
            .images
            .write()
            .map_err(|e| format!("Failed to acquire write lock: {}", e))?;

        // Check if image already exists
        if images
            .iter()
            .any(|(_, img)| img.image_ref == image.image_ref)
        {
            let msg = format!("Image {} already registered", image.image_ref);
            error!("{}", msg);
            return Err(msg);
        }

        images.insert(image.id.clone(), image.clone());
        info!("External image registered: {}", image.image_ref);

        Ok(image)
    }

    /// Get a registered external image by ID
    pub fn get_image(&self, image_id: &str) -> Option<ExternalImage> {
        self.images
            .read()
            .ok()
            .and_then(|images| images.get(image_id).cloned())
    }

    /// Get image by image reference string
    pub fn get_image_by_ref(&self, image_ref: &str) -> Option<ExternalImage> {
        self.images.read().ok().and_then(|images| {
            images
                .iter()
                .find(|(_, img)| img.image_ref == image_ref)
                .map(|(_, img)| img.clone())
        })
    }

    /// List all registered external images
    pub fn list_images(&self) -> Vec<ExternalImage> {
        self.images
            .read()
            .ok()
            .map(|images| images.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Record an operation result for a deployment of `project_id`
    pub fn record_operation(
        &self,
        project_id: i32,
        deployment_id: &str,
        result: OperationResult,
    ) -> Result<(), String> {
        debug!(
            "Recording operation {} for deployment {} in project {}",
            result.operation, deployment_id, project_id
        );

        let mut operations = self.operations.write().map_err(|e| {
            format!(
                "Failed to acquire write lock to record {} for deployment {} in project {}: {}",
                result.operation, deployment_id, project_id, e
            )
        })?;
        let deployment_ops = operations
            .entry((project_id, deployment_id.to_string()))
            .or_insert_with(Vec::new);
        // Keep per-deployment history bounded: repeated operations must not
        // grow this in-memory map without limit.
        if deployment_ops.len() >= MAX_OPERATIONS_PER_DEPLOYMENT {
            let excess = deployment_ops.len() + 1 - MAX_OPERATIONS_PER_DEPLOYMENT;
            deployment_ops.drain(0..excess);
        }
        deployment_ops.push(result);

        Ok(())
    }

    /// Get all operations for a deployment of `project_id`
    pub fn get_operations(&self, project_id: i32, deployment_id: &str) -> Vec<OperationResult> {
        self.operations
            .read()
            .ok()
            .and_then(|ops| ops.get(&(project_id, deployment_id.to_string())).cloned())
            .unwrap_or_default()
    }

    /// Get the latest result for a specific operation type
    pub fn get_latest_operation(
        &self,
        project_id: i32,
        deployment_id: &str,
        operation: &DeploymentOperation,
    ) -> Option<OperationResult> {
        self.operations.read().ok().and_then(|ops| {
            ops.get(&(project_id, deployment_id.to_string()))
                .and_then(|ops| {
                    ops.iter()
                        .rev()
                        .find(|op| &op.operation == operation)
                        .cloned()
                })
        })
    }

    /// Check if an operation has been completed for a deployment
    pub fn has_completed_operation(
        &self,
        project_id: i32,
        deployment_id: &str,
        operation: &DeploymentOperation,
    ) -> bool {
        self.get_latest_operation(project_id, deployment_id, operation)
            .map(|op| op.success)
            .unwrap_or(false)
    }
}

impl Default for ExternalDeploymentManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn operation_status_schema_does_not_collide_with_project_operations() {
        assert_eq!(
            <OperationStatus as utoipa::ToSchema>::name(),
            "DeploymentOperationStatus"
        );
    }

    #[test]
    fn test_push_and_retrieve_image() {
        let manager = ExternalDeploymentManager::new();
        let image = ExternalImage {
            id: "img_1".to_string(),
            image_ref: "myapp:v1.0".to_string(),
            digest: Some("sha256:abc123".to_string()),
            size: Some(1024 * 1024),
            pushed_at: Utc::now(),
            metadata: None,
        };

        let result = manager.push_image(image.clone());
        assert!(result.is_ok());

        let retrieved = manager.get_image("img_1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().image_ref, "myapp:v1.0");
    }

    #[test]
    fn test_duplicate_image_rejection() {
        let manager = ExternalDeploymentManager::new();
        let image = ExternalImage {
            id: "img_1".to_string(),
            image_ref: "myapp:v1.0".to_string(),
            digest: None,
            size: None,
            pushed_at: Utc::now(),
            metadata: None,
        };

        manager.push_image(image.clone()).unwrap();

        let duplicate = ExternalImage {
            id: "img_2".to_string(),
            image_ref: "myapp:v1.0".to_string(),
            digest: None,
            size: None,
            pushed_at: Utc::now(),
            metadata: None,
        };

        let result = manager.push_image(duplicate);
        assert!(result.is_err());
    }

    fn record(operation: DeploymentOperation, status: OperationStatus) -> OperationResult {
        OperationResult {
            operation,
            status,
            success: status == OperationStatus::Completed,
            message: format!("{:?}", status),
            data: DeploymentOperationDetails::new(3, "7"),
            executed_at: Utc::now(),
        }
    }

    #[test]
    fn test_record_and_retrieve_operations() {
        let manager = ExternalDeploymentManager::new();

        manager
            .record_operation(
                3,
                "deploy_123",
                record(DeploymentOperation::Deploy, OperationStatus::Completed),
            )
            .unwrap();

        let operations = manager.get_operations(3, "deploy_123");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operation, DeploymentOperation::Deploy);
        assert!(operations[0].success);
    }

    #[test]
    fn test_check_completed_operation() {
        let manager = ExternalDeploymentManager::new();

        manager
            .record_operation(
                3,
                "deploy_123",
                record(DeploymentOperation::Deploy, OperationStatus::Failed),
            )
            .unwrap();
        assert!(!manager.has_completed_operation(3, "deploy_123", &DeploymentOperation::Deploy));

        manager
            .record_operation(
                3,
                "deploy_123",
                record(DeploymentOperation::Deploy, OperationStatus::Completed),
            )
            .unwrap();
        assert!(manager.has_completed_operation(3, "deploy_123", &DeploymentOperation::Deploy));
    }

    #[test]
    fn test_operations_of_another_project_are_invisible() {
        let manager = ExternalDeploymentManager::new();
        manager
            .record_operation(
                3,
                "7",
                record(
                    DeploymentOperation::TakeScreenshot,
                    OperationStatus::Completed,
                ),
            )
            .unwrap();

        // Same deployment ID, different project: nothing to read.
        assert!(manager.get_operations(99, "7").is_empty());
        assert!(manager
            .get_latest_operation(99, "7", &DeploymentOperation::TakeScreenshot)
            .is_none());
        assert!(!manager.has_completed_operation(99, "7", &DeploymentOperation::TakeScreenshot));

        // ...and writing there leaves the owner's history untouched.
        manager
            .record_operation(
                99,
                "7",
                record(DeploymentOperation::TakeScreenshot, OperationStatus::Failed),
            )
            .unwrap();
        assert_eq!(
            manager
                .get_latest_operation(3, "7", &DeploymentOperation::TakeScreenshot)
                .unwrap()
                .status,
            OperationStatus::Completed
        );
    }

    #[test]
    fn test_operation_history_is_bounded_per_deployment() {
        let manager = ExternalDeploymentManager::new();
        for _ in 0..(MAX_OPERATIONS_PER_DEPLOYMENT + 10) {
            manager
                .record_operation(
                    3,
                    "7",
                    record(DeploymentOperation::TakeScreenshot, OperationStatus::Failed),
                )
                .unwrap();
        }
        assert_eq!(
            manager.get_operations(3, "7").len(),
            MAX_OPERATIONS_PER_DEPLOYMENT
        );
    }

    #[test]
    fn test_operation_details_schema_names_are_distinct() {
        assert_eq!(
            <DeploymentOperationDetails as utoipa::ToSchema>::name(),
            "DeploymentOperationDetails"
        );
        assert_eq!(
            <DeploymentScreenshotCapture as utoipa::ToSchema>::name(),
            "DeploymentScreenshotCapture"
        );
    }
}
