//! Nixpacks implementation of ImageBuilder trait
//!
//! This module provides an implementation of the ImageBuilder trait using Nixpacks,
//! which automatically detects project languages and generates optimized build plans.

use crate::{BuildRequest, BuildResult, BuilderError, ImageBuilder};
use async_trait::async_trait;
use bollard::Docker;
use futures::StreamExt;
use nixpacks::nixpacks::{
    app::App,
    builder::{
        docker::{docker_image_builder::DockerImageBuilder, DockerBuilderOptions},
        ImageBuilder as NixpacksImageBuilder,
    },
    environment::Environment,
    logger::Logger,
    plan::{
        generator::{GeneratePlanOptions, NixpacksBuildPlanGenerator},
        PlanGenerator,
    },
};
use std::time::Instant;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::fs;
use tracing::{error, info};

pub struct NixpacksBuilder {
    out_dir: Option<String>,
    docker: Arc<Docker>,
}

impl NixpacksBuilder {
    pub fn new(out_dir: Option<String>, docker: Arc<Docker>) -> Self {
        Self { out_dir, docker }
    }

    pub fn with_default_out_dir(docker: Arc<Docker>) -> Self {
        Self::new(None, docker)
    }

    fn get_providers() -> &'static [&'static dyn nixpacks::providers::Provider] {
        &[
            &nixpacks::providers::node::NodeProvider {},
            &nixpacks::providers::python::PythonProvider {},
            &nixpacks::providers::rust::RustProvider {},
            &nixpacks::providers::go::GolangProvider {},
            &nixpacks::providers::java::JavaProvider {},
            &nixpacks::providers::php::PhpProvider {},
            &nixpacks::providers::ruby::RubyProvider {},
            &nixpacks::providers::deno::DenoProvider {},
            &nixpacks::providers::elixir::ElixirProvider {},
            &nixpacks::providers::csharp::CSharpProvider {},
            &nixpacks::providers::dart::DartProvider {},
            &nixpacks::providers::staticfile::StaticfileProvider {},
        ]
    }

    /// Generate Dockerfile using nixpacks DockerImageBuilder
    ///
    /// This uses nixpacks' async DockerImageBuilder.create_image() which:
    /// 1. Generates a build plan
    /// 2. Creates the actual Dockerfile at .nixpacks/Dockerfile
    /// 3. Returns the path to the generated Dockerfile
    async fn generate_dockerfile_with_nixpacks(
        &self,
        context_path: &Path,
        environment_vars: &[String],
    ) -> Result<PathBuf, BuilderError> {
        let path_str = context_path
            .to_str()
            .ok_or_else(|| BuilderError::InvalidContext("Invalid path encoding".to_string()))?;

        info!("Creating nixpacks app from path: {}", path_str);
        let app = App::new(path_str)
            .map_err(|e| BuilderError::InvalidContext(format!("Failed to create app: {}", e)))?;

        let environment =
            Environment::from_envs(environment_vars.iter().map(|s| s.as_str()).collect())
                .map_err(|e| BuilderError::Other(format!("Failed to create environment: {}", e)))?;

        let mut generator =
            NixpacksBuildPlanGenerator::new(Self::get_providers(), GeneratePlanOptions::default());

        info!("Generating nixpacks build plan");
        let (plan, app) = generator
            .generate_plan(&app, &environment)
            .map_err(|e| BuilderError::BuildFailed(format!("Failed to generate plan: {}", e)))?;

        // Check if we have a valid plan
        let phase_count = plan.phases.clone().map_or(0, |phases| phases.len());
        if phase_count > 0 {
            let start = plan.start_phase.clone().unwrap_or_default();
            if start.cmd.is_none() {
                return Err(BuilderError::BuildFailed(
                    "No start command could be found".to_string(),
                ));
            }
        } else {
            return Err(BuilderError::BuildFailed(
                "unable to generate a build plan for this app.\nPlease check the documentation for supported languages: https://nixpacks.com".to_string(),
            ));
        }

        info!("Creating Docker image with nixpacks");
        let builder = DockerImageBuilder::new(
            Logger::new(),
            DockerBuilderOptions {
                out_dir: self.out_dir.clone(),
                ..Default::default()
            },
        );

        builder
            .create_image(
                app.source.to_str().ok_or_else(|| {
                    BuilderError::InvalidContext("Invalid source path".to_string())
                })?,
                &plan,
                &environment,
            )
            .await
            .map_err(|e| BuilderError::BuildFailed(format!("Failed to create image: {}", e)))?;

        // Move the generated Dockerfile to the expected location
        let nixpacks_dockerfile = context_path.join(".nixpacks").join("Dockerfile");
        let target_dockerfile = context_path.join("Dockerfile");

        if nixpacks_dockerfile.exists() {
            fs::rename(&nixpacks_dockerfile, &target_dockerfile)
                .await
                .map_err(BuilderError::IoError)?;
            info!("Dockerfile generated at: {}", target_dockerfile.display());
            Ok(target_dockerfile)
        } else {
            Err(BuilderError::BuildFailed(
                "Nixpacks failed to generate Dockerfile".to_string(),
            ))
        }
    }
}

#[async_trait]
impl ImageBuilder for NixpacksBuilder {
    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
        let start_time = Instant::now();

        info!(
            "Building image {} with nixpacks from context: {:?}",
            request.image_name, request.context_path
        );

        // Convert build args to environment variables format
        let env_vars: Vec<String> = request
            .build_args
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        // Generate Dockerfile using nixpacks
        let _dockerfile_path = self
            .generate_dockerfile_with_nixpacks(&request.context_path, &env_vars)
            .await?;

        info!("Dockerfile generated, now building with Docker");

        // Now use regular Docker to build the image
        let mut docker_build_cmd = tokio::process::Command::new("docker");
        docker_build_cmd
            .arg("build")
            .arg("-t")
            .arg(&request.image_name)
            .arg(".");

        // Add build args
        for (key, value) in &request.build_args {
            if !value.is_empty() {
                docker_build_cmd
                    .arg("--build-arg")
                    .arg(format!("{}={}", key, value));
            }
        }

        // Add platform if specified
        if let Some(platform) = &request.platform {
            docker_build_cmd.arg("--platform").arg(platform);
        }

        // Add dockerfile if not default
        if let Some(dockerfile) = &request.dockerfile_path {
            docker_build_cmd.arg("-f").arg(dockerfile);
        }

        docker_build_cmd.current_dir(&request.context_path);

        info!("Executing Docker build command");
        let output = docker_build_cmd.output().await.map_err(|e| {
            BuilderError::BuildFailed(format!("Failed to execute docker build: {}", e))
        })?;

        // Write logs
        let logs = format!(
            "STDOUT:\n{}\nSTDERR:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        fs::write(&request.log_path, &logs)
            .await
            .map_err(BuilderError::IoError)?;

        if !output.status.success() {
            error!("Docker build failed: {}", logs);
            return Err(BuilderError::BuildFailed(format!(
                "Docker build failed with status: {}",
                output.status
            )));
        }

        let build_duration = start_time.elapsed().as_millis() as u64;

        // Get image info using Bollard
        let image_info = self
            .docker
            .inspect_image(&request.image_name)
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to inspect image: {}", e)))?;

        let size_bytes = image_info.size.unwrap_or(0) as u64;
        let image_id = image_info.id.unwrap_or_else(|| request.image_name.clone());

        info!(
            "Successfully built image {} in {}ms",
            request.image_name, build_duration
        );

        Ok(BuildResult {
            image_id,
            image_name: request.image_name,
            size_bytes,
            build_duration_ms: build_duration,
        })
    }

    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError> {
        info!("Importing image from {:?} with tag: {}", image_path, tag);

        use bollard::query_parameters::ImportImageOptions;

        // Read the tar file into memory
        let tar_data = tokio::fs::read(&image_path)
            .await
            .map_err(BuilderError::IoError)?;

        // Import the image
        let import_options = ImportImageOptions {
            quiet: false,
            ..Default::default()
        };

        use bytes::Bytes;
        use http_body_util::Either;
        use http_body_util::Full;

        let body = Either::Left(Full::new(Bytes::from(tar_data)));

        let mut import_stream = self.docker.import_image(import_options, body, None);

        // Consume the stream
        let mut image_id = String::new();
        while let Some(result) = import_stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(id) = info.id {
                        image_id = id;
                    }
                }
                Err(e) => {
                    return Err(BuilderError::Other(format!(
                        "Failed to import image: {}",
                        e
                    )));
                }
            }
        }

        // Tag the image if we got an ID and it's different from the requested tag
        if !image_id.is_empty() && image_id != tag {
            use bollard::query_parameters::TagImageOptions;

            let repo_tag: Vec<&str> = tag.split(':').collect();
            let (repo, tag_name) = if repo_tag.len() == 2 {
                (repo_tag[0], repo_tag[1])
            } else {
                (tag, "latest")
            };

            let tag_options = TagImageOptions {
                repo: Some(repo.to_string()),
                tag: Some(tag_name.to_string()),
            };

            self.docker
                .tag_image(&image_id, Some(tag_options))
                .await
                .map_err(|e| BuilderError::Other(format!("Failed to tag image: {}", e)))?;
        }

        Ok(if image_id.is_empty() {
            tag.to_string()
        } else {
            image_id
        })
    }

    async fn extract_from_image(
        &self,
        image_name: &str,
        source_path: &str,
        destination_path: &Path,
    ) -> Result<(), BuilderError> {
        info!(
            "Extracting {} from image {} to {:?}",
            source_path, image_name, destination_path
        );

        use bollard::query_parameters::{DownloadFromContainerOptions, RemoveContainerOptions};

        // Create a temporary container
        let config = bollard::models::ContainerCreateBody {
            image: Some(image_name.to_string()),
            cmd: Some(vec!["/bin/sh".to_string()]),
            tty: Some(true),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(
                Some(bollard::query_parameters::CreateContainerOptionsBuilder::new().build()),
                config,
            )
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to create container: {}", e)))?;

        let container_id = container.id;

        // Extract files
        let download_options = Some(DownloadFromContainerOptions {
            path: source_path.to_string(),
        });

        let mut stream = self
            .docker
            .download_from_container(&container_id, download_options);
        let mut tar_data = Vec::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(data) => tar_data.extend_from_slice(&data),
                Err(e) => {
                    // Cleanup container
                    let _ = self
                        .docker
                        .remove_container(
                            &container_id,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                    return Err(BuilderError::Other(format!(
                        "Failed to download from container: {}",
                        e
                    )));
                }
            }
        }

        // Extract tar data to destination
        let cursor = std::io::Cursor::new(tar_data);
        let mut archive = tar::Archive::new(cursor);
        archive
            .unpack(destination_path)
            .map_err(BuilderError::IoError)?;

        // Cleanup container
        let _ = self
            .docker
            .remove_container(
                &container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        Ok(())
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        use bollard::query_parameters::ListImagesOptions;

        let images = self
            .docker
            .list_images(Some(ListImagesOptions {
                all: false,
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to list images: {}", e)))?;

        let image_names: Vec<String> = images
            .iter()
            .flat_map(|image| {
                image.repo_tags.iter().filter_map(|tag| {
                    if tag != "<none>:<none>" {
                        Some(tag.clone())
                    } else {
                        None
                    }
                })
            })
            .collect();

        Ok(image_names)
    }

    async fn build_image_with_callback(
        &self,
        request: crate::BuildRequestWithCallback,
    ) -> Result<BuildResult, BuilderError> {
        // Nixpacks doesn't support log callbacks yet, just delegate to regular build_image
        self.build_image(request.request).await
    }

    async fn remove_image(&self, image_name: &str) -> Result<(), BuilderError> {
        info!("Removing image: {}", image_name);

        use bollard::query_parameters::RemoveImageOptions;

        self.docker
            .remove_image(
                image_name,
                Some(RemoveImageOptions {
                    force: true,
                    ..Default::default()
                }),
                None,
            )
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to remove image: {}", e)))?;

        Ok(())
    }
}
