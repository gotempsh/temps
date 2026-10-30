// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`ContainerDeployer`] that also answers for replicas Docker cannot see.
//!
//! A `micro_vm` replica runs in a VM the workload plane started, so the
//! local Docker daemon answers "no such container" for it. Every consumer of
//! the shared deployer (the container health monitor, the Containers view,
//! the metrics and log endpoints) would otherwise need its own microVM
//! special case. Instead the shared deployer is wrapped once: reads go first
//! to a [`ReplicaReader`] that knows the replicas Docker cannot see, and
//! everything else, including every operation that acts on a container,
//! goes to the wrapped deployer unchanged.
//!
//! The reader is attached after construction ([`ReplicaAwareDeployer::set_reader`])
//! because the workload plane that backs it is built later than the deployer
//! is registered. Until then the wrapper is a plain pass-through.
//!
//! A reader that fails does not hide a container: the read falls through to
//! Docker, so a Docker container never disappears because the plane could
//! not classify it.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use crate::{
    ContainerDeployer, ContainerInfo, ContainerStats, DeployRequest, DeployResult, DeployerError,
    ExecResult,
};

/// Answers reads for replicas the local container runtime cannot see.
///
/// `Ok(None)` means "not mine": the caller asks its own runtime.
#[async_trait]
pub trait ReplicaReader: Send + Sync {
    async fn info(&self, container_id: &str) -> Result<Option<ContainerInfo>, DeployerError>;

    async fn stats(&self, container_id: &str) -> Result<Option<ContainerStats>, DeployerError>;

    /// The replica's current log tail, one entry per line.
    async fn logs(&self, container_id: &str) -> Result<Option<Vec<String>>, DeployerError>;
}

pub struct ReplicaAwareDeployer {
    local: Arc<dyn ContainerDeployer>,
    reader: OnceLock<Arc<dyn ReplicaReader>>,
}

impl ReplicaAwareDeployer {
    pub fn new(local: Arc<dyn ContainerDeployer>) -> Self {
        Self {
            local,
            reader: OnceLock::new(),
        }
    }

    /// Attach the reader. The first one wins; a second call is ignored.
    pub fn set_reader(&self, reader: Arc<dyn ReplicaReader>) {
        let _ = self.reader.set(reader);
    }

    fn reader(&self) -> Option<&Arc<dyn ReplicaReader>> {
        self.reader.get()
    }
}

/// Log a reader failure and fall through to the local runtime.
fn reader_failed(operation: &str, container_id: &str, error: &DeployerError) {
    tracing::warn!(
        container_id,
        operation,
        %error,
        "Replica reader failed; asking the local container runtime instead"
    );
}

#[async_trait]
impl ContainerDeployer for ReplicaAwareDeployer {
    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        self.local.deploy_container(request).await
    }

    async fn exec_in_container(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout: std::time::Duration,
    ) -> Result<ExecResult, DeployerError> {
        self.local
            .exec_in_container(container_id, command, timeout)
            .await
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.start_container(container_id).await
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.stop_container(container_id).await
    }

    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.pause_container(container_id).await
    }

    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.resume_container(container_id).await
    }

    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.local.remove_container(container_id).await
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        if let Some(reader) = self.reader() {
            match reader.info(container_id).await {
                Ok(Some(info)) => return Ok(info),
                Ok(None) => {}
                Err(error) => reader_failed("inspect", container_id, &error),
            }
        }
        self.local.get_container_info(container_id).await
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<ContainerStats, DeployerError> {
        if let Some(reader) = self.reader() {
            match reader.stats(container_id).await {
                Ok(Some(stats)) => return Ok(stats),
                Ok(None) => {}
                Err(error) => reader_failed("stats", container_id, &error),
            }
        }
        self.local.get_container_stats(container_id).await
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        self.local.list_containers().await
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        if let Some(reader) = self.reader() {
            match reader.logs(container_id).await {
                Ok(Some(lines)) => return Ok(lines.concat()),
                Ok(None) => {}
                Err(error) => reader_failed("logs", container_id, &error),
            }
        }
        self.local.get_container_logs(container_id).await
    }

    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        if let Some(reader) = self.reader() {
            match reader.logs(container_id).await {
                Ok(Some(lines)) => return Ok(Box::new(futures::stream::iter(lines))),
                Ok(None) => {}
                Err(error) => reader_failed("log stream", container_id, &error),
            }
        }
        self.local.stream_container_logs(container_id).await
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        self.local.image_exists(image_name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// The local runtime: every read says which runtime answered.
    struct Local;

    fn info(container_id: &str, image: &str) -> ContainerInfo {
        ContainerInfo {
            container_id: container_id.to_string(),
            container_name: format!("name-{container_id}"),
            image_name: image.to_string(),
            status: crate::ContainerStatus::Running,
            ..Default::default()
        }
    }

    #[async_trait]
    impl ContainerDeployer for Local {
        async fn deploy_container(&self, _: DeployRequest) -> Result<DeployResult, DeployerError> {
            Err(DeployerError::DeploymentFailed("local deploy".into()))
        }
        async fn exec_in_container(
            &self,
            _: &str,
            _: Vec<String>,
            _: std::time::Duration,
        ) -> Result<ExecResult, DeployerError> {
            Err(DeployerError::DeploymentFailed("local exec".into()))
        }
        async fn start_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }
        async fn stop_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }
        async fn pause_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }
        async fn resume_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }
        async fn remove_container(&self, _: &str) -> Result<(), DeployerError> {
            Ok(())
        }
        async fn get_container_info(&self, id: &str) -> Result<ContainerInfo, DeployerError> {
            Ok(info(id, "docker-image"))
        }
        async fn get_container_stats(&self, id: &str) -> Result<ContainerStats, DeployerError> {
            Ok(ContainerStats {
                container_id: id.to_string(),
                memory_bytes: 1,
                ..Default::default()
            })
        }
        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            Ok(Vec::new())
        }
        async fn get_container_logs(&self, _: &str) -> Result<String, DeployerError> {
            Ok("docker logs".to_string())
        }
        async fn stream_container_logs(
            &self,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            Ok(Box::new(futures::stream::iter(vec!["docker line".into()])))
        }
    }

    /// Owns the ids starting with `vm-`; fails for ids starting with `bad-`.
    struct Reader;

    #[async_trait]
    impl ReplicaReader for Reader {
        async fn info(&self, id: &str) -> Result<Option<ContainerInfo>, DeployerError> {
            if id.starts_with("bad-") {
                return Err(DeployerError::DeploymentFailed("plane down".into()));
            }
            Ok(id.starts_with("vm-").then(|| info(id, "microvm-image")))
        }
        async fn stats(&self, id: &str) -> Result<Option<ContainerStats>, DeployerError> {
            if id.starts_with("bad-") {
                return Err(DeployerError::DeploymentFailed("plane down".into()));
            }
            Ok(id.starts_with("vm-").then(|| ContainerStats {
                container_id: id.to_string(),
                memory_bytes: 99,
                ..Default::default()
            }))
        }
        async fn logs(&self, id: &str) -> Result<Option<Vec<String>>, DeployerError> {
            if id.starts_with("bad-") {
                return Err(DeployerError::DeploymentFailed("plane down".into()));
            }
            Ok(id
                .starts_with("vm-")
                .then(|| vec!["vm one\n".to_string(), "vm two\n".to_string()]))
        }
    }

    fn deployer(with_reader: bool) -> ReplicaAwareDeployer {
        let deployer = ReplicaAwareDeployer::new(Arc::new(Local));
        if with_reader {
            deployer.set_reader(Arc::new(Reader));
        }
        deployer
    }

    #[tokio::test]
    async fn a_replica_the_reader_owns_is_answered_by_the_reader() {
        let d = deployer(true);
        assert_eq!(
            d.get_container_info("vm-1").await.expect("info").image_name,
            "microvm-image"
        );
        assert_eq!(
            d.get_container_stats("vm-1")
                .await
                .expect("stats")
                .memory_bytes,
            99
        );
        assert_eq!(
            d.get_container_logs("vm-1").await.expect("logs"),
            "vm one\nvm two\n"
        );
        let lines: Vec<String> = d
            .stream_container_logs("vm-1")
            .await
            .expect("stream")
            .collect()
            .await;
        assert_eq!(lines, vec!["vm one\n", "vm two\n"]);
    }

    #[tokio::test]
    async fn every_other_container_is_answered_by_the_local_runtime() {
        let d = deployer(true);
        assert_eq!(
            d.get_container_info("c-1").await.expect("info").image_name,
            "docker-image"
        );
        assert_eq!(
            d.get_container_stats("c-1")
                .await
                .expect("stats")
                .memory_bytes,
            1
        );
        assert_eq!(
            d.get_container_logs("c-1").await.expect("logs"),
            "docker logs"
        );
    }

    #[tokio::test]
    async fn a_failing_reader_never_hides_a_container_from_the_local_runtime() {
        let d = deployer(true);
        assert_eq!(
            d.get_container_info("bad-1")
                .await
                .expect("info")
                .image_name,
            "docker-image"
        );
        assert_eq!(
            d.get_container_stats("bad-1")
                .await
                .expect("stats")
                .memory_bytes,
            1
        );
        assert_eq!(
            d.get_container_logs("bad-1").await.expect("logs"),
            "docker logs"
        );
        let lines: Vec<String> = d
            .stream_container_logs("bad-1")
            .await
            .expect("stream")
            .collect()
            .await;
        assert_eq!(lines, vec!["docker line"]);
    }

    #[tokio::test]
    async fn before_a_reader_is_attached_it_is_a_pass_through() {
        let d = deployer(false);
        assert_eq!(
            d.get_container_info("vm-1").await.expect("info").image_name,
            "docker-image"
        );
    }

    #[tokio::test]
    async fn the_first_reader_wins() {
        struct Other;
        #[async_trait]
        impl ReplicaReader for Other {
            async fn info(&self, id: &str) -> Result<Option<ContainerInfo>, DeployerError> {
                Ok(Some(info(id, "other")))
            }
            async fn stats(&self, _: &str) -> Result<Option<ContainerStats>, DeployerError> {
                Ok(None)
            }
            async fn logs(&self, _: &str) -> Result<Option<Vec<String>>, DeployerError> {
                Ok(None)
            }
        }
        let d = deployer(true);
        d.set_reader(Arc::new(Other));
        assert_eq!(
            d.get_container_info("x").await.expect("info").image_name,
            "docker-image",
            "the second reader must not replace the first"
        );
    }

    #[tokio::test]
    async fn actions_always_go_to_the_local_runtime() {
        let d = deployer(true);
        assert!(d.stop_container("vm-1").await.is_ok());
        let error = d
            .exec_in_container("vm-1", vec![], std::time::Duration::from_secs(1))
            .await
            .expect_err("local exec");
        assert!(error.to_string().contains("local exec"), "{error}");
    }
}
