// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reaching the image a worker node built.
//!
//! A source build in the control-plane profile runs on a worker, and the image
//! stays on that worker. Jobs that read files out of the image afterwards —
//! a static site's output directory, a container's immutable assets — must
//! ask that node, not the control plane's (absent) daemon. `BuildImageJob`
//! records the node as its `builder_node_id` output; this module turns that
//! output back into an [`ImageBuilder`] for the node.

use async_trait::async_trait;
use std::sync::Arc;
use temps_core::{WorkflowContext, WorkflowError};
use temps_deployer::ImageBuilder;

/// Connects to the node that built an image.
#[async_trait]
pub trait BuilderNodeResolver: Send + Sync {
    async fn image_builder_for_node(
        &self,
        node_id: i32,
    ) -> Result<Arc<dyn ImageBuilder>, WorkflowError>;
}

/// Where the image produced by `build_job_id` can be read.
///
/// Returns `local` for a build that ran on this host, and the build node's
/// builder for a worker build. A worker build without a resolver is an error
/// naming the node rather than a silent fallback to `local`, which does not
/// have the image.
pub(crate) async fn image_builder_for_build(
    context: &WorkflowContext,
    build_job_id: &str,
    local: &Arc<dyn ImageBuilder>,
    resolver: Option<&Arc<dyn BuilderNodeResolver>>,
) -> Result<(Arc<dyn ImageBuilder>, Option<i32>), WorkflowError> {
    let builder_node_id: Option<i32> = context
        .get_output(build_job_id, "builder_node_id")?
        .flatten();
    let Some(node_id) = builder_node_id else {
        return Ok((local.clone(), None));
    };
    let resolver = resolver.ok_or_else(|| {
        WorkflowError::JobExecutionFailed(format!(
            "The image from '{build_job_id}' was built on node {node_id}, but this job \
             has no way to reach build nodes"
        ))
    })?;
    let builder = resolver.image_builder_for_node(node_id).await?;
    Ok((builder, Some(node_id)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Resolver returning a fixed builder per node id, recording each lookup.
    pub(crate) struct StaticResolver {
        pub builders: HashMap<i32, Arc<dyn ImageBuilder>>,
        pub lookups: Mutex<Vec<i32>>,
    }

    impl StaticResolver {
        pub(crate) fn new(node_id: i32, builder: Arc<dyn ImageBuilder>) -> Self {
            Self {
                builders: HashMap::from([(node_id, builder)]),
                lookups: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl BuilderNodeResolver for StaticResolver {
        async fn image_builder_for_node(
            &self,
            node_id: i32,
        ) -> Result<Arc<dyn ImageBuilder>, WorkflowError> {
            self.lookups.lock().unwrap().push(node_id);
            self.builders.get(&node_id).cloned().ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!("Build node {node_id} not found"))
            })
        }
    }

    fn context_with_builder(node_id: Option<i32>) -> WorkflowContext {
        let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
        context
            .set_output("build_image", "builder_node_id", node_id)
            .unwrap();
        context
    }

    /// A builder only compared by identity; any call is a test failure.
    struct PlaceholderBuilder;

    #[async_trait]
    impl ImageBuilder for PlaceholderBuilder {
        async fn build_image(
            &self,
            _request: temps_deployer::BuildRequest,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        async fn build_image_with_callback(
            &self,
            _request: temps_deployer::BuildRequestWithCallback,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        async fn import_image(
            &self,
            _image_path: std::path::PathBuf,
            _tag: &str,
        ) -> Result<String, temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        async fn remove_image(
            &self,
            _image_name: &str,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
            unimplemented!("placeholder builder")
        }
        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }

    fn mock_builder() -> Arc<dyn ImageBuilder> {
        Arc::new(PlaceholderBuilder)
    }

    #[tokio::test]
    async fn local_build_uses_the_local_builder_without_resolving() {
        let local = mock_builder();
        let remote = mock_builder();
        let resolver: Arc<dyn BuilderNodeResolver> = Arc::new(StaticResolver::new(7, remote));

        let (builder, node_id) = image_builder_for_build(
            &context_with_builder(None),
            "build_image",
            &local,
            Some(&resolver),
        )
        .await
        .unwrap();

        assert!(Arc::ptr_eq(&builder, &local));
        assert_eq!(node_id, None);
    }

    #[tokio::test]
    async fn worker_build_resolves_the_build_node() {
        let local = mock_builder();
        let remote = mock_builder();
        let resolver = Arc::new(StaticResolver::new(7, remote.clone()));
        let dyn_resolver: Arc<dyn BuilderNodeResolver> = resolver.clone();

        let (builder, node_id) = image_builder_for_build(
            &context_with_builder(Some(7)),
            "build_image",
            &local,
            Some(&dyn_resolver),
        )
        .await
        .unwrap();

        assert!(Arc::ptr_eq(&builder, &remote));
        assert_eq!(node_id, Some(7));
        assert_eq!(*resolver.lookups.lock().unwrap(), vec![7]);
    }

    #[tokio::test]
    async fn worker_build_without_resolver_names_the_node() {
        let local = mock_builder();

        let error = match image_builder_for_build(
            &context_with_builder(Some(7)),
            "build_image",
            &local,
            None,
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("worker build without a resolver must not fall back to local"),
        };

        let message = error.to_string();
        assert!(message.contains("node 7"), "{message}");
        assert!(message.contains("build_image"), "{message}");
    }
}
