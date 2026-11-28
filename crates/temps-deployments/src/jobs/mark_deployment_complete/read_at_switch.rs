// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What is read from a deployment's row the moment it becomes current, and so
//! has to be written before the switch: the fields the route table is built
//! from, and the content a registry image resolved to, which a plugin's pin
//! names.

use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, Statement};
use temps_entities::deployments;

use super::MarkDeploymentCompleteJob;
use crate::workflow::WorkflowError;

impl MarkDeploymentCompleteJob {
    /// Write the fields the route table is built from before the switch.
    /// Written afterwards they would be missing under a reader that acted on
    /// the switch.
    pub(super) async fn persist_inputs_read_at_the_switch(
        &self,
        active_deployment: &deployments::ActiveModel,
    ) -> Result<(), WorkflowError> {
        let mut inputs = deployments::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(self.deployment_id),
            ..Default::default()
        };
        if let sea_orm::ActiveValue::Set(image) = &active_deployment.image_name {
            inputs.image_name = Set(image.clone());
        }
        if let sea_orm::ActiveValue::Set(static_dir) = &active_deployment.static_dir_location {
            inputs.static_dir_location = Set(static_dir.clone());
        }
        if inputs.is_changed() {
            inputs.update(self.db.as_ref()).await.map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to persist deployment routing inputs: {}",
                    e
                ))
            })?;
        }
        Ok(())
    }

    /// Record the Docker image ID a registry pull resolved to, before the
    /// switch. Nothing is written when the pull reported none: a worker node
    /// pulled the image itself.
    ///
    /// Only that one key is set, in place, so a change made to the metadata
    /// after this job read the row is kept, and a retried job writes the same
    /// value again.
    pub(super) async fn record_pulled_image(&self, image_id: &str) -> Result<(), WorkflowError> {
        if !image_id.starts_with("sha256:") {
            return Ok(());
        }
        let backend = self.db.as_ref().get_database_backend();
        self.db
            .as_ref()
            .execute(Statement::from_sql_and_values(
                backend,
                r#"
UPDATE deployments
SET metadata = (
    CASE WHEN jsonb_typeof(metadata::jsonb) = 'object' THEN metadata::jsonb ELSE '{}'::jsonb END
    || jsonb_build_object('pulledImageId', $1::text)
)::json
WHERE id = $2
"#,
                [image_id.into(), self.deployment_id.into()],
            ))
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to record the image {image_id} that deployment {} pulled: {error}",
                    self.deployment_id
                ))
            })?;
        Ok(())
    }
}
