# Temps Deployer

A comprehensive container building and deployment library with support for both traditional Docker and automated Nixpacks workflows.

## Features

### 🐳 **Docker Runtime**
- Full Docker API integration using bollard
- BuildKit support with resource limits
- Container lifecycle management (start, stop, pause, resume, remove)
- Network management and port mapping
- Log streaming and container inspection
- Image operations (build, import, extract, list, remove)

### 📦 **Nixpacks Integration**
- Automatic language detection and Dockerfile generation
- Support for 15+ languages and frameworks
- Zero-config builds for standard project structures
- Optimized build plans with caching
- Seamless integration with Docker backend

### 🏗️ **Trait-Based Architecture**
- `ImageBuilder` trait for building and managing container images
- `ContainerDeployer` trait for container lifecycle management
- `ContainerRuntime` trait for runtime information and resource management
- Extensible design ready for future runtimes (Firecracker, etc.)

## Quick Start

### Using Nixpacks for Automatic Builds

```rust
use temps_deployer::{nixpacks::NixpacksBuilder, BuildRequest, ImageBuilder};
use std::collections::HashMap;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a nixpacks builder
    let builder = NixpacksBuilder::with_default_out_dir();

    // Build a Node.js, Python, Rust, or any supported project
    let request = BuildRequest {
        image_name: "my-app:latest".to_string(),
        context_path: PathBuf::from("./my-nodejs-app"),
        dockerfile_path: None, // Nixpacks will generate this
        build_args: HashMap::new(),
        platform: None,
        log_path: PathBuf::from("./build.log"),
    };

    let result = builder.build_image(request).await?;
    println!("Built image: {} in {}ms", result.image_name, result.build_duration_ms);

    Ok(())
}
```

### Using Docker Runtime for Traditional Builds

```rust
use temps_deployer::{docker::DockerRuntime, BuildRequest, ImageBuilder};
use bollard::Docker;
use std::collections::HashMap;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create Docker runtime
    let docker = Docker::connect_with_local_defaults()?;
    let runtime = DockerRuntime::new(docker, true, "my-network".to_string());

    // Build with existing Dockerfile
    let request = BuildRequest {
        image_name: "my-app:latest".to_string(),
        context_path: PathBuf::from("./my-app"),
        dockerfile_path: Some(PathBuf::from("./Dockerfile")),
        build_args: {
            let mut args = HashMap::new();
            args.insert("NODE_ENV".to_string(), "production".to_string());
            args
        },
        platform: Some("linux/amd64".to_string()),
        log_path: PathBuf::from("./build.log"),
    };

    let result = runtime.build_image(request).await?;
    println!("Built image: {} ({}bytes)", result.image_name, result.size_bytes);

    Ok(())
}
```

### Container Deployment

```rust
use temps_deployer::{
    docker::DockerRuntime,
    DeployRequest, ContainerDeployer, PortMapping, Protocol,
    ResourceLimits, RestartPolicy
};
use std::collections::HashMap;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let docker = bollard::Docker::connect_with_local_defaults()?;
    let runtime = DockerRuntime::new(docker, false, "my-network".to_string());

    // Deploy the container
    let deploy_request = DeployRequest {
        image_name: "my-app:latest".to_string(),
        container_name: "my-running-app".to_string(),
        environment_vars: {
            let mut env = HashMap::new();
            env.insert("NODE_ENV".to_string(), "production".to_string());
            env.insert("PORT".to_string(), "3000".to_string());
            env
        },
        port_mappings: vec![PortMapping {
            host_port: 8080,
            container_port: 3000,
            protocol: Protocol::Tcp,
        }],
        network_name: Some("my-network".to_string()),
        resource_limits: ResourceLimits {
            cpu_limit: Some(2.0),
            memory_limit_mb: Some(512),
            disk_limit_mb: Some(1024),
        },
        restart_policy: RestartPolicy::Always,
        log_path: PathBuf::from("./deploy.log"),
    };

    let result = runtime.deploy_container(deploy_request).await?;
    println!("Container {} deployed on port {}", result.container_name, result.host_port);

    // Container is now running and accessible at http://localhost:8080

    Ok(())
}
```

## Supported Languages (Nixpacks)

Nixpacks automatically detects and builds projects for:

- **Node.js** - npm, yarn, pnpm
- **Python** - pip, poetry, pipenv
- **Rust** - cargo
- **Go** - go modules
- **Java** - maven, gradle
- **PHP** - composer
- **Ruby** - bundler
- **C#** - dotnet
- **Elixir** - mix
- **Dart** - dart/flutter
- **Deno** - deno
- **Static Sites** - HTML/CSS/JS
- **And more...**

## Architecture

### Trait-Based Design

The library uses a trait-based architecture for maximum flexibility:

```rust
#[async_trait]
pub trait ImageBuilder: Send + Sync {
    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError>;
    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError>;
    async fn extract_from_image(&self, image_name: &str, source_path: &str, destination_path: &Path) -> Result<(), BuilderError>;
    async fn list_images(&self) -> Result<Vec<String>, BuilderError>;
    async fn remove_image(&self, image_name: &str) -> Result<(), BuilderError>;
}

#[async_trait]
pub trait ContainerDeployer: Send + Sync {
    async fn deploy_container(&self, request: DeployRequest) -> Result<DeployResult, DeployerError>;
    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError>;
    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError>;
    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError>;
    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError>;
    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError>;
    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError>;
    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError>;
    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError>;
    async fn stream_container_logs(&self, container_id: &str) -> Result<Box<dyn Stream<Item = String> + Unpin + Send>, DeployerError>;
}
```

### Error Handling

Comprehensive error types for different scenarios:

```rust
#[derive(Error, Debug)]
pub enum BuilderError {
    #[error("Build failed: {0}")]
    BuildFailed(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Invalid context: {0}")]
    InvalidContext(String),
    #[error("Missing dockerfile: {0}")]
    MissingDockerfile(String),
    #[error("Resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),
    #[error("Other error: {0}")]
    Other(String),
}

#[derive(Error, Debug)]
pub enum DeployerError {
    #[error("Deployment failed: {0}")]
    DeploymentFailed(String),
    #[error("Container not found: {0}")]
    ContainerNotFound(String),
    #[error("Image not found: {0}")]
    ImageNotFound(String),
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),
    #[error("Other error: {0}")]
    Other(String),
}
```

## Examples

Run the comprehensive example to see both Docker and Nixpacks in action:

```bash
cargo run --example nixpacks_vs_docker
```

This example demonstrates:
- Building a Node.js app with both Nixpacks and Docker
- Building a Python FastAPI app with Nixpacks
- Building a Rust web server with Nixpacks
- Performance and size comparisons

## When to Use What

### Use Nixpacks When:
- ✅ Rapid prototyping and development
- ✅ Standard project structures
- ✅ Want zero-config builds
- ✅ Multiple language support needed
- ✅ Automatic optimization and caching

### Use Docker When:
- ✅ Complex, custom build requirements
- ✅ Need full control over the build process
- ✅ Existing Dockerfiles
- ✅ Advanced multi-stage builds
- ✅ Custom base images or complex dependencies

### Use Both When:
- ✅ Different environments (dev vs prod)
- ✅ Migration scenarios
- ✅ A/B testing build approaches
- ✅ Supporting diverse team preferences

## Integration with Temps

This library is designed to integrate seamlessly with the larger Temps ecosystem:

```rust
// In your pipeline service
use temps_deployer::{nixpacks::NixpacksBuilder, docker::DockerRuntime};

enum BuildStrategy {
    Nixpacks(NixpacksBuilder),
    Docker(DockerRuntime),
}

impl BuildStrategy {
    fn choose_for_project(project_path: &Path) -> Self {
        if has_dockerfile(project_path) {
            Self::Docker(create_docker_runtime())
        } else {
            Self::Nixpacks(NixpacksBuilder::with_default_out_dir())
        }
    }
}
```

## Future Roadmap

- 🚀 **Firecracker Integration** - Serverless container runtime
- 🌐 **Remote Builder Support** - Build on remote machines
- 📊 **Build Analytics** - Performance metrics and optimization suggestions
- 🔄 **Advanced Caching** - Cross-build layer sharing
- 🛡️ **Security Scanning** - Vulnerability detection in builds
- 📦 **OCI Compliance** - Full Open Container Initiative support

## Dependencies

- `nixpacks` - Language-agnostic container builds
- `bollard` - Docker API client
- `tokio` - Async runtime
- `serde` - Serialization
- `anyhow` - Error handling
- `log` - Logging

## License

This project is part of the Temps ecosystem and follows the same licensing terms.
