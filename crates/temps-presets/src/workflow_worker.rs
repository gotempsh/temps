// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{
    docker::DockerfilePreset, DockerfileConfig, DockerfileWithArgs, Preset, PresetResolutionError,
    ProjectType, StoredPreset,
};
use async_trait::async_trait;
use std::{fmt, path::Path};
use temps_entities::preset::{DockerfileVariant, Preset as StoredPresetType, PresetConfig};

/// A worker remains an ordinary HTTP service: invocations need a reachable replica.
pub struct WorkflowWorker;
#[async_trait]
impl Preset for WorkflowWorker {
    fn slug(&self) -> String {
        "workflow-worker".into()
    }
    fn label(&self) -> String {
        "Durable workflow worker".into()
    }
    fn description(&self) -> String {
        "Serve durable workflows with the SDK. Run temps workflows init, configure the environment credential, and deploy the generated Dockerfile on port 3000.".into()
    }
    fn icon_url(&self) -> String {
        "/presets/docker.svg".into()
    }
    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }
    fn stored_preset(&self) -> Option<StoredPresetType> {
        Some(StoredPresetType::Dockerfile)
    }
    fn resolve_storage(
        &self,
        config: Option<PresetConfig>,
    ) -> Result<StoredPreset, PresetResolutionError> {
        let mut stored = DockerfilePreset.resolve_storage(config)?;
        if let Some(PresetConfig::Dockerfile(config)) = &mut stored.config {
            config.variant = DockerfileVariant::WorkflowWorker;
        }
        Ok(stored)
    }
    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        DockerfilePreset.dockerfile(config).await
    }
    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        DockerfilePreset.dockerfile_with_build_dir(local_path).await
    }
    fn dirs_to_upload(&self) -> Vec<String> {
        DockerfilePreset.dirs_to_upload()
    }
}
impl fmt::Display for WorkflowWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_is_discoverable_and_persists_as_an_ordinary_docker_service() {
        let worker = crate::get_preset_by_slug("workflow-worker").expect("selectable worker");
        let stored = worker.resolve_storage(None).expect("persistable");
        assert_eq!(stored.preset, StoredPresetType::Dockerfile);
        assert_eq!(worker.default_port(), 3000);
        assert_eq!(worker.project_type(), ProjectType::Server);
        assert!(worker.description().contains("credential"));
        assert_eq!(
            crate::runtime_slug(stored.preset, stored.config.as_ref()),
            "workflow-worker"
        );
    }
}
