//! Deploy Command
//!
//! Deploy pre-built Docker images or static files to Temps environments
//! without Git integration.

use clap::{Args, Subcommand};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

#[derive(Args)]
pub struct DeployCommand {
    #[command(subcommand)]
    command: DeployCommands,
}

#[derive(Subcommand)]
enum DeployCommands {
    /// Deploy a pre-built Docker image
    Image(DeployImageArgs),
    /// Deploy static files (tar.gz or zip archive)
    Static(DeployStaticArgs),
}

#[derive(Args)]
struct DeployImageArgs {
    /// Docker image reference (e.g., "ghcr.io/org/app:v1.0")
    #[arg(short, long)]
    image: String,

    /// Project slug or ID
    #[arg(short, long)]
    project: String,

    /// Environment name (default: production)
    #[arg(short, long, default_value = "production")]
    environment: String,

    /// Temps API URL
    #[arg(long, env = "TEMPS_API_URL")]
    api_url: String,

    /// Temps API token
    #[arg(long, env = "TEMPS_API_TOKEN")]
    api_token: String,

    /// Wait for deployment to complete
    #[arg(long, default_value = "false")]
    wait: bool,

    /// Timeout in seconds for --wait (default: 300)
    #[arg(long, default_value = "300")]
    timeout: u64,

    /// Additional metadata (JSON format)
    #[arg(long)]
    metadata: Option<String>,
}

#[derive(Args)]
struct DeployStaticArgs {
    /// Path to static files archive (tar.gz or zip) or directory
    #[arg(short, long)]
    path: PathBuf,

    /// Project slug or ID
    #[arg(short, long)]
    project: String,

    /// Environment name (default: production)
    #[arg(short, long, default_value = "production")]
    environment: String,

    /// Temps API URL
    #[arg(long, env = "TEMPS_API_URL")]
    api_url: String,

    /// Temps API token
    #[arg(long, env = "TEMPS_API_TOKEN")]
    api_token: String,

    /// Wait for deployment to complete
    #[arg(long, default_value = "false")]
    wait: bool,

    /// Timeout in seconds for --wait (default: 300)
    #[arg(long, default_value = "300")]
    timeout: u64,

    /// Additional metadata (JSON format)
    #[arg(long)]
    metadata: Option<String>,
}

// API Response types
#[derive(Debug, Deserialize)]
struct ProjectResponse {
    id: i32,
    slug: String,
}

#[derive(Debug, Deserialize)]
struct EnvironmentResponse {
    id: i32,
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct DeploymentResponse {
    id: i32,
    slug: String,
    state: String,
}

#[derive(Debug, Serialize)]
struct DeployImageRequest {
    image_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

impl DeployCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        match self.command {
            DeployCommands::Image(args) => Self::execute_image_deploy(args),
            DeployCommands::Static(args) => Self::execute_static_deploy(args),
        }
    }

    fn execute_image_deploy(args: DeployImageArgs) -> anyhow::Result<()> {
        info!("Deploying image {} to {}", args.image, args.project);

        // Create tokio runtime
        let rt = tokio::runtime::Runtime::new()?;

        rt.block_on(async {
            // Create HTTP client
            let client = reqwest::Client::new();

            println!();
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
            );
            println!("{}", "   🚀 Deploying Docker Image".bright_white().bold());
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
            );
            println!();
            println!("  {} {}", "Image:".bright_white(), args.image.bright_cyan());
            println!(
                "  {} {}",
                "Project:".bright_white(),
                args.project.bright_white()
            );
            println!(
                "  {} {}",
                "Environment:".bright_white(),
                args.environment.bright_white()
            );
            println!();

            // Look up project
            println!("{}", "Looking up project...".bright_white());
            let project =
                Self::get_project(&client, &args.api_url, &args.api_token, &args.project).await?;
            println!(
                "  {} {} (id: {})",
                "✓".bright_green(),
                project.slug.bright_cyan(),
                project.id
            );

            // Look up environment
            println!("{}", "Looking up environment...".bright_white());
            let environment = Self::get_environment(
                &client,
                &args.api_url,
                &args.api_token,
                project.id,
                &args.environment,
            )
            .await?;
            println!(
                "  {} {} (id: {})",
                "✓".bright_green(),
                environment.name.bright_cyan(),
                environment.id
            );

            // Parse metadata if provided
            let metadata: Option<serde_json::Value> = if let Some(meta_str) = &args.metadata {
                Some(
                    serde_json::from_str(meta_str)
                        .map_err(|e| anyhow::anyhow!("Invalid metadata JSON: {}", e))?,
                )
            } else {
                None
            };

            // Trigger deployment
            println!("{}", "Starting deployment...".bright_white());
            let deploy_url = format!(
                "{}/projects/{}/environments/{}/deploy/image",
                args.api_url.trim_end_matches('/'),
                project.id,
                environment.id
            );

            let request = DeployImageRequest {
                image_ref: args.image.clone(),
                metadata,
            };

            let response = client
                .post(&deploy_url)
                .header("Authorization", format!("Bearer {}", args.api_token))
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to send deploy request: {}", e))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "Deploy request failed with status {}: {}",
                    status,
                    body
                ));
            }

            let deployment: DeploymentResponse = response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse deploy response: {}", e))?;

            println!(
                "  {} Deployment started: {} (state: {})",
                "✓".bright_green(),
                deployment.slug.bright_cyan(),
                deployment.state.bright_white()
            );

            // Wait for completion if requested
            if args.wait {
                println!();
                println!(
                    "{}",
                    format!(
                        "Waiting for deployment to complete (timeout: {}s)...",
                        args.timeout
                    )
                    .bright_white()
                );

                let deployment_url = format!(
                    "{}/deployments/{}",
                    args.api_url.trim_end_matches('/'),
                    deployment.id
                );

                let start = std::time::Instant::now();
                let timeout = std::time::Duration::from_secs(args.timeout);

                loop {
                    if start.elapsed() > timeout {
                        return Err(anyhow::anyhow!(
                            "Deployment timed out after {}s",
                            args.timeout
                        ));
                    }

                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                    let status_response = client
                        .get(&deployment_url)
                        .header("Authorization", format!("Bearer {}", args.api_token))
                        .send()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to get deployment status: {}", e))?;

                    if !status_response.status().is_success() {
                        continue; // Retry on error
                    }

                    let status: DeploymentResponse = status_response
                        .json()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to parse status response: {}", e))?;

                    print!(
                        "\r  {} Current state: {}     ",
                        "⏳".bright_yellow(),
                        status.state
                    );

                    match status.state.as_str() {
                        "running" => {
                            println!();
                            println!(
                                "  {} Deployment completed successfully!",
                                "✅".bright_green()
                            );
                            break;
                        }
                        "failed" | "cancelled" => {
                            println!();
                            return Err(anyhow::anyhow!(
                                "Deployment failed with state: {}",
                                status.state
                            ));
                        }
                        _ => continue,
                    }
                }
            }

            println!();
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
            );
            println!(
                "{}",
                "   ✅ Deployment initiated successfully!"
                    .bright_green()
                    .bold()
            );
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
            );
            println!();

            Ok(())
        })
    }

    fn execute_static_deploy(args: DeployStaticArgs) -> anyhow::Result<()> {
        info!(
            "Deploying static files from {} to {}",
            args.path.display(),
            args.project
        );

        // Create tokio runtime
        let rt = tokio::runtime::Runtime::new()?;

        rt.block_on(async {
            // Create HTTP client
            let client = reqwest::Client::new();

            println!();
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
            );
            println!("{}", "   📦 Deploying Static Files".bright_white().bold());
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
            );
            println!();
            println!(
                "  {} {}",
                "Path:".bright_white(),
                args.path.display().to_string().bright_cyan()
            );
            println!(
                "  {} {}",
                "Project:".bright_white(),
                args.project.bright_white()
            );
            println!(
                "  {} {}",
                "Environment:".bright_white(),
                args.environment.bright_white()
            );
            println!();

            // Validate path exists
            if !args.path.exists() {
                return Err(anyhow::anyhow!(
                    "Path does not exist: {}",
                    args.path.display()
                ));
            }

            // Look up project
            println!("{}", "Looking up project...".bright_white());
            let project =
                Self::get_project(&client, &args.api_url, &args.api_token, &args.project).await?;
            println!(
                "  {} {} (id: {})",
                "✓".bright_green(),
                project.slug.bright_cyan(),
                project.id
            );

            // Look up environment
            println!("{}", "Looking up environment...".bright_white());
            let environment = Self::get_environment(
                &client,
                &args.api_url,
                &args.api_token,
                project.id,
                &args.environment,
            )
            .await?;
            println!(
                "  {} {} (id: {})",
                "✓".bright_green(),
                environment.name.bright_cyan(),
                environment.id
            );

            // Prepare the file for upload
            let (file_data, filename, content_type) = Self::prepare_static_bundle(&args.path)?;

            println!(
                "  {} Prepared bundle: {} ({:.2} MB)",
                "✓".bright_green(),
                filename.bright_cyan(),
                file_data.len() as f64 / (1024.0 * 1024.0)
            );

            // Upload static bundle
            println!("{}", "Uploading static bundle...".bright_white());
            let upload_url = format!(
                "{}/projects/{}/upload/static",
                args.api_url.trim_end_matches('/'),
                project.id
            );

            let form = reqwest::multipart::Form::new().part(
                "file",
                reqwest::multipart::Part::bytes(file_data)
                    .file_name(filename.clone())
                    .mime_str(&content_type)?,
            );

            let response = client
                .post(&upload_url)
                .header("Authorization", format!("Bearer {}", args.api_token))
                .multipart(form)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to upload static bundle: {}", e))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "Upload failed with status {}: {}",
                    status,
                    body
                ));
            }

            #[derive(Debug, Deserialize)]
            struct StaticBundleResponse {
                id: i32,
            }

            let bundle: StaticBundleResponse = response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse upload response: {}", e))?;

            println!("  {} Bundle uploaded: id={}", "✓".bright_green(), bundle.id);

            // Trigger deployment
            println!("{}", "Starting deployment...".bright_white());
            let deploy_url = format!(
                "{}/projects/{}/environments/{}/deploy/static",
                args.api_url.trim_end_matches('/'),
                project.id,
                environment.id
            );

            #[derive(Debug, Serialize)]
            struct DeployStaticRequest {
                static_bundle_id: i32,
                #[serde(skip_serializing_if = "Option::is_none")]
                metadata: Option<serde_json::Value>,
            }

            let metadata: Option<serde_json::Value> = if let Some(meta_str) = &args.metadata {
                Some(
                    serde_json::from_str(meta_str)
                        .map_err(|e| anyhow::anyhow!("Invalid metadata JSON: {}", e))?,
                )
            } else {
                None
            };

            let request = DeployStaticRequest {
                static_bundle_id: bundle.id,
                metadata,
            };

            let response = client
                .post(&deploy_url)
                .header("Authorization", format!("Bearer {}", args.api_token))
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to send deploy request: {}", e))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "Deploy request failed with status {}: {}",
                    status,
                    body
                ));
            }

            let deployment: DeploymentResponse = response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse deploy response: {}", e))?;

            println!(
                "  {} Deployment started: {} (state: {})",
                "✓".bright_green(),
                deployment.slug.bright_cyan(),
                deployment.state.bright_white()
            );

            // Wait for completion if requested (same as image deploy)
            if args.wait {
                println!();
                println!(
                    "{}",
                    format!(
                        "Waiting for deployment to complete (timeout: {}s)...",
                        args.timeout
                    )
                    .bright_white()
                );

                let deployment_url = format!(
                    "{}/deployments/{}",
                    args.api_url.trim_end_matches('/'),
                    deployment.id
                );

                let start = std::time::Instant::now();
                let timeout = std::time::Duration::from_secs(args.timeout);

                loop {
                    if start.elapsed() > timeout {
                        return Err(anyhow::anyhow!(
                            "Deployment timed out after {}s",
                            args.timeout
                        ));
                    }

                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                    let status_response = client
                        .get(&deployment_url)
                        .header("Authorization", format!("Bearer {}", args.api_token))
                        .send()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to get deployment status: {}", e))?;

                    if !status_response.status().is_success() {
                        continue;
                    }

                    let status: DeploymentResponse = status_response
                        .json()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to parse status response: {}", e))?;

                    print!(
                        "\r  {} Current state: {}     ",
                        "⏳".bright_yellow(),
                        status.state
                    );

                    match status.state.as_str() {
                        "running" => {
                            println!();
                            println!(
                                "  {} Deployment completed successfully!",
                                "✅".bright_green()
                            );
                            break;
                        }
                        "failed" | "cancelled" => {
                            println!();
                            return Err(anyhow::anyhow!(
                                "Deployment failed with state: {}",
                                status.state
                            ));
                        }
                        _ => continue,
                    }
                }
            }

            println!();
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
            );
            println!(
                "{}",
                "   ✅ Deployment initiated successfully!"
                    .bright_green()
                    .bold()
            );
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
            );
            println!();

            Ok(())
        })
    }

    async fn get_project(
        client: &reqwest::Client,
        api_url: &str,
        api_token: &str,
        project_slug: &str,
    ) -> anyhow::Result<ProjectResponse> {
        // Try to get project by slug first
        let url = format!(
            "{}/projects/by-slug/{}",
            api_url.trim_end_matches('/'),
            project_slug
        );

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_token))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch project: {}", e))?;

        if response.status().is_success() {
            return response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse project response: {}", e));
        }

        // Try as project ID if slug lookup failed
        if let Ok(project_id) = project_slug.parse::<i32>() {
            let url = format!("{}/projects/{}", api_url.trim_end_matches('/'), project_id);

            let response = client
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_token))
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to fetch project: {}", e))?;

            if response.status().is_success() {
                return response
                    .json()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to parse project response: {}", e));
            }
        }

        Err(anyhow::anyhow!("Project '{}' not found", project_slug))
    }

    async fn get_environment(
        client: &reqwest::Client,
        api_url: &str,
        api_token: &str,
        project_id: i32,
        environment_name: &str,
    ) -> anyhow::Result<EnvironmentResponse> {
        let url = format!(
            "{}/projects/{}/environments",
            api_url.trim_end_matches('/'),
            project_id
        );

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_token))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch environments: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch environments for project {}",
                project_id
            ));
        }

        let environments: Vec<EnvironmentResponse> = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse environments response: {}", e))?;

        environments
            .into_iter()
            .find(|e| e.name.eq_ignore_ascii_case(environment_name))
            .ok_or_else(|| {
                anyhow::anyhow!("Environment '{}' not found in project", environment_name)
            })
    }

    fn prepare_static_bundle(path: &PathBuf) -> anyhow::Result<(Vec<u8>, String, String)> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::Builder;

        if path.is_file() {
            // It's already an archive file
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("bundle")
                .to_string();

            let content_type = if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
                "application/gzip"
            } else if filename.ends_with(".zip") {
                "application/zip"
            } else {
                return Err(anyhow::anyhow!(
                    "Unsupported archive format. Use .tar.gz, .tgz, or .zip"
                ));
            };

            let data = std::fs::read(path)
                .map_err(|e| anyhow::anyhow!("Failed to read archive file: {}", e))?;

            Ok((data, filename, content_type.to_string()))
        } else if path.is_dir() {
            // Create a tar.gz from the directory
            println!(
                "  {} Creating tar.gz archive from directory...",
                "⏳".bright_yellow()
            );

            let mut tar_data = Vec::new();
            {
                let enc = GzEncoder::new(&mut tar_data, Compression::default());
                let mut builder = Builder::new(enc);

                builder
                    .append_dir_all(".", path)
                    .map_err(|e| anyhow::anyhow!("Failed to create tar archive: {}", e))?;

                let enc = builder
                    .into_inner()
                    .map_err(|e| anyhow::anyhow!("Failed to finalize tar archive: {}", e))?;
                enc.finish()
                    .map_err(|e| anyhow::anyhow!("Failed to compress archive: {}", e))?;
            }

            let filename = format!(
                "{}.tar.gz",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("bundle")
            );

            Ok((tar_data, filename, "application/gzip".to_string()))
        } else {
            Err(anyhow::anyhow!(
                "Path is neither a file nor a directory: {}",
                path.display()
            ))
        }
    }
}
