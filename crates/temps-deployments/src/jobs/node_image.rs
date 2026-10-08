// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Moving an image built on a worker node into another Docker daemon.
//!
//! A build moved off the control plane (build location `node`) leaves its
//! image on the worker that built it. The control plane's own jobs that read
//! the image -- source maps, static assets, vulnerability scans, and replicas
//! placed on the control plane -- need it in the control plane's daemon.

use temps_core::WorkflowError;

/// Outcome of [`copy_node_built_image`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NodeImageCopy {
    /// The destination already held the exact image the worker built.
    AlreadyPresent,
    /// The image was streamed from the worker and verified.
    Transferred,
}

/// Stream `image_tag` from the worker that built it into `destination`,
/// unless `destination` already holds the same image.
///
/// Identity is the image id, not the tag: a tag left over from an earlier
/// build would otherwise be deployed in place of the one just built.
pub(crate) async fn copy_node_built_image(
    owner: &dyn temps_deployer::ImageBuilder,
    owner_name: &str,
    destination: &dyn temps_deployer::ImageBuilder,
    image_tag: &str,
) -> Result<NodeImageCopy, WorkflowError> {
    let built = owner.inspect_image(image_tag).await.map_err(|error| {
        WorkflowError::JobExecutionFailed(format!(
            "Cannot inspect worker-built image '{image_tag}' on build node '{owner_name}': {error}"
        ))
    })?;
    if let Ok(cached) = destination.inspect_image(image_tag).await {
        if cached.id == built.id {
            return Ok(NodeImageCopy::AlreadyPresent);
        }
    }
    let stream = owner.export_image_stream(image_tag).await.map_err(|error| {
        WorkflowError::JobExecutionFailed(format!(
            "Failed to export image '{image_tag}' from build node '{owner_name}': {error}"
        ))
    })?;
    destination
        .import_image_stream(stream, image_tag)
        .await
        .map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to import image '{image_tag}' from build node '{owner_name}' into the \
                 control plane: {error}"
            ))
        })?;
    let imported = destination.inspect_image(image_tag).await.map_err(|error| {
        WorkflowError::JobExecutionFailed(format!(
            "Cannot verify image '{image_tag}' imported from build node '{owner_name}': {error}"
        ))
    })?;
    if imported.id != built.id {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Image '{image_tag}' imported into the control plane is {} but build node \
             '{owner_name}' built {}",
            imported.id, built.id
        )));
    }
    Ok(NodeImageCopy::Transferred)
}

#[cfg(test)]
pub(crate) mod fake {
    use async_trait::async_trait;
    use futures::StreamExt;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// An image store keyed by tag: `inspect_image` reports the id held for
    /// a tag, `export_image_stream` yields the id as the archive, and
    /// `import_image_stream` stores whatever id the archive carried.
    #[derive(Default)]
    pub(crate) struct FakeImageStore {
        pub(crate) images: Mutex<HashMap<String, String>>,
        pub(crate) imports: Mutex<Vec<String>>,
        /// Import this id instead of the archive's, to simulate a corrupt copy.
        pub(crate) import_as: Option<String>,
    }

    impl FakeImageStore {
        pub(crate) fn holding(tag: &str, id: &str) -> Self {
            let store = Self::default();
            store
                .images
                .lock()
                .unwrap()
                .insert(tag.to_string(), id.to_string());
            store
        }
    }

    #[async_trait]
    impl temps_deployer::ImageBuilder for FakeImageStore {
        async fn build_image(
            &self,
            _request: temps_deployer::BuildRequest,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn build_image_with_callback(
            &self,
            _request: temps_deployer::BuildRequestWithCallback,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, temps_deployer::BuilderError> {
            unimplemented!("transfers must stream, never stage a file")
        }

        async fn import_image_stream(
            &self,
            mut stream: temps_deployer::ImageImportStream,
            tag: &str,
        ) -> Result<String, temps_deployer::BuilderError> {
            let mut archive = Vec::new();
            while let Some(chunk) = stream.next().await {
                archive.extend_from_slice(&chunk.map_err(temps_deployer::BuilderError::IoError)?);
            }
            let id = self
                .import_as
                .clone()
                .unwrap_or_else(|| String::from_utf8_lossy(&archive).to_string());
            self.images
                .lock()
                .unwrap()
                .insert(tag.to_string(), id.clone());
            self.imports.lock().unwrap().push(tag.to_string());
            Ok(id)
        }

        async fn export_image_stream(
            &self,
            image_name: &str,
        ) -> Result<temps_deployer::ImageImportStream, temps_deployer::BuilderError> {
            let id = self
                .images
                .lock()
                .unwrap()
                .get(image_name)
                .cloned()
                .ok_or_else(|| temps_deployer::BuilderError::ImageNotFound(image_name.into()))?;
            Ok(Box::pin(futures::stream::iter(vec![Ok(
                bytes::Bytes::from(id),
            )])))
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("transfers must stream, never stage a file")
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn remove_image(
            &self,
            _image_name: &str,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn inspect_image(
            &self,
            image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
            let id = self
                .images
                .lock()
                .unwrap()
                .get(image_name)
                .cloned()
                .ok_or_else(|| temps_deployer::BuilderError::ImageNotFound(image_name.into()))?;
            Ok(temps_deployer::ImageInfo {
                id,
                architecture: "amd64".to_string(),
                os: "linux".to_string(),
                platform: "linux/amd64".to_string(),
                size_bytes: 0,
                tags: vec![image_name.to_string()],
                created: None,
                working_dir: None,
            })
        }

        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeImageStore;
    use super::*;

    /// A build moved to a worker leaves its image there; a replica placed on
    /// the control plane needs it streamed into the control plane's Docker.
    #[tokio::test]
    async fn node_built_image_is_streamed_into_the_control_plane() {
        let worker = FakeImageStore::holding("app:latest", "sha256:built");
        let control_plane = FakeImageStore::default();

        let outcome = copy_node_built_image(&worker, "builder-1", &control_plane, "app:latest")
            .await
            .expect("copy succeeds");

        assert_eq!(outcome, NodeImageCopy::Transferred);
        assert_eq!(
            control_plane.images.lock().unwrap().get("app:latest"),
            Some(&"sha256:built".to_string())
        );
    }

    /// The same image already on the control plane is not copied again.
    #[tokio::test]
    async fn node_built_image_already_on_the_control_plane_is_reused() {
        let worker = FakeImageStore::holding("app:latest", "sha256:built");
        let control_plane = FakeImageStore::holding("app:latest", "sha256:built");

        let outcome = copy_node_built_image(&worker, "builder-1", &control_plane, "app:latest")
            .await
            .expect("copy succeeds");

        assert_eq!(outcome, NodeImageCopy::AlreadyPresent);
        assert!(control_plane.imports.lock().unwrap().is_empty());
    }

    /// A stale image under the same tag (an earlier local build) must be
    /// replaced, not deployed in place of the one the worker just built.
    #[tokio::test]
    async fn stale_control_plane_tag_is_replaced_by_the_node_built_image() {
        let worker = FakeImageStore::holding("app:latest", "sha256:built");
        let control_plane = FakeImageStore::holding("app:latest", "sha256:stale");

        let outcome = copy_node_built_image(&worker, "builder-1", &control_plane, "app:latest")
            .await
            .expect("copy succeeds");

        assert_eq!(outcome, NodeImageCopy::Transferred);
        assert_eq!(
            control_plane.images.lock().unwrap().get("app:latest"),
            Some(&"sha256:built".to_string())
        );
    }

    /// An import that lands a different image fails the deployment rather
    /// than running something other than what was built.
    #[tokio::test]
    async fn mismatched_import_fails_with_both_image_ids() {
        let worker = FakeImageStore::holding("app:latest", "sha256:built");
        let control_plane = FakeImageStore {
            import_as: Some("sha256:other".to_string()),
            ..Default::default()
        };

        let error = copy_node_built_image(&worker, "builder-1", &control_plane, "app:latest")
            .await
            .unwrap_err();

        match error {
            WorkflowError::JobValidationFailed(message) => {
                assert!(message.contains("sha256:other"), "{message}");
                assert!(message.contains("sha256:built"), "{message}");
                assert!(message.contains("builder-1"), "{message}");
            }
            other => panic!("expected JobValidationFailed, got {other:?}"),
        }
    }

    /// A worker that no longer holds the image is reported by name.
    #[tokio::test]
    async fn missing_node_built_image_names_the_build_node() {
        let worker = FakeImageStore::default();
        let control_plane = FakeImageStore::default();

        let error = copy_node_built_image(&worker, "builder-1", &control_plane, "app:latest")
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("builder-1"), "{message}");
        assert!(message.contains("app:latest"), "{message}");
        assert!(control_plane.imports.lock().unwrap().is_empty());
    }
}
