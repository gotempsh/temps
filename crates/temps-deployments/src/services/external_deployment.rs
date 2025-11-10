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

/// Result of an executed operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub operation: DeploymentOperation,
    pub success: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
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

/// Response for external image operations
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExternalImageResponse {
    pub id: String,
    pub image_ref: String,
    pub digest: Option<String>,
    pub size: Option<u64>,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub pushed_at: UtcDateTime,
}

impl From<ExternalImage> for ExternalImageResponse {
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

/// In-memory store for external images and operation results
/// This keeps external images and operations in memory without database changes
#[derive(Clone)]
pub struct ExternalDeploymentManager {
    images: Arc<RwLock<HashMap<String, ExternalImage>>>,
    operations: Arc<RwLock<HashMap<String, Vec<OperationResult>>>>,
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

    /// Record an operation result for a deployment
    pub fn record_operation(
        &self,
        deployment_id: &str,
        result: OperationResult,
    ) -> Result<(), String> {
        debug!(
            "Recording operation {} for deployment {}",
            result.operation, deployment_id
        );

        let mut operations = self
            .operations
            .write()
            .map_err(|e| format!("Failed to acquire write lock: {}", e))?;
        let deployment_ops = operations
            .entry(deployment_id.to_string())
            .or_insert_with(Vec::new);
        deployment_ops.push(result);

        Ok(())
    }

    /// Get all operations for a deployment
    pub fn get_operations(&self, deployment_id: &str) -> Vec<OperationResult> {
        self.operations
            .read()
            .ok()
            .and_then(|ops| ops.get(deployment_id).cloned())
            .unwrap_or_default()
    }

    /// Get the latest result for a specific operation type
    pub fn get_latest_operation(
        &self,
        deployment_id: &str,
        operation: &DeploymentOperation,
    ) -> Option<OperationResult> {
        self.operations.read().ok().and_then(|ops| {
            ops.get(deployment_id).and_then(|ops| {
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
        deployment_id: &str,
        operation: &DeploymentOperation,
    ) -> bool {
        self.get_latest_operation(deployment_id, operation)
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

    #[test]
    fn test_record_and_retrieve_operations() {
        let manager = ExternalDeploymentManager::new();
        let deployment_id = "deploy_123";

        let result = OperationResult {
            operation: DeploymentOperation::Deploy,
            success: true,
            message: "Deployment successful".to_string(),
            data: Some(serde_json::json!({"containers": ["app-0", "app-1"]})),
            executed_at: Utc::now(),
        };

        manager
            .record_operation(deployment_id, result.clone())
            .unwrap();

        let operations = manager.get_operations(deployment_id);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operation, DeploymentOperation::Deploy);
        assert!(operations[0].success);
    }

    #[test]
    fn test_check_completed_operation() {
        let manager = ExternalDeploymentManager::new();
        let deployment_id = "deploy_123";

        let failed_result = OperationResult {
            operation: DeploymentOperation::Deploy,
            success: false,
            message: "Failed to deploy".to_string(),
            data: None,
            executed_at: Utc::now(),
        };

        manager
            .record_operation(deployment_id, failed_result)
            .unwrap();

        assert!(!manager.has_completed_operation(deployment_id, &DeploymentOperation::Deploy));

        let success_result = OperationResult {
            operation: DeploymentOperation::Deploy,
            success: true,
            message: "Deployment successful".to_string(),
            data: None,
            executed_at: Utc::now(),
        };

        manager
            .record_operation(deployment_id, success_result)
            .unwrap();

        assert!(manager.has_completed_operation(deployment_id, &DeploymentOperation::Deploy));
    }
}
