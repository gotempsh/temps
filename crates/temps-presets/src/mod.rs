use std::{fmt, path::Path};

mod custom;
mod docker;
mod docusaurus;
mod nextjs;
mod react_app;
mod rsbuild;
mod vite;
mod build_system;
mod docker_custom;
use build_system::BuildSystem;
pub use custom::CustomPreset;
use docusaurus::Docusaurus;
use docker::DockerfilePreset;
pub use nextjs::NextJs;
pub use react_app::CreateReactApp;
use rsbuild::Rsbuild;
pub use vite::Vite;
pub use build_system::MonorepoTool;

#[derive(Debug, Clone, Copy)]
pub enum ProjectType {
    Server,
    Static,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectType::Server => write!(f, "server"),
            ProjectType::Static => write!(f, "static"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PackageManager {
    Bun,
    Yarn,
    Npm,
    Pnpm,
}

impl PackageManager {
    pub fn detect(local_path: &Path) -> Self {
        if local_path.join("pnpm-lock.yaml").exists() {
            PackageManager::Pnpm
        } else if local_path.join("package-lock.json").exists() {
            PackageManager::Npm
        } else if local_path.join("yarn.lock").exists() {
            PackageManager::Yarn
        } else if local_path.join("bun.lockb").exists() || local_path.join("bun.lock").exists() {
            PackageManager::Bun
        } else {
            PackageManager::Npm // Default
        }
    }

    pub fn install_command(&self) -> &'static str {
        match self {
            PackageManager::Bun => "bun install",
            PackageManager::Yarn => "yarn install --frozen-lockfile",
            PackageManager::Npm => "npm install",
            PackageManager::Pnpm => "pnpm install --frozen-lockfile",
        }
    }

    pub fn build_command(&self) -> &'static str {
        match self {
            PackageManager::Bun => "bun run build",
            PackageManager::Yarn => "yarn build",
            PackageManager::Npm => "npm run build",
            PackageManager::Pnpm => "pnpm run build",
        }
    }

    pub fn base_image(&self) -> &'static str {
        match self {
            PackageManager::Bun => "oven/bun:1.2",
            PackageManager::Pnpm => "node:22-alpine",
            PackageManager::Yarn | PackageManager::Npm => "node:22-alpine",
        }
    }
}

/// Configuration parameters for generating a Dockerfile
pub struct DockerfileConfig<'a> {
    pub root_local_path: &'a Path,
    pub local_path: &'a Path,
    pub install_command: Option<&'a str>,
    pub build_command: Option<&'a str>,
    pub output_dir: Option<&'a str>,
    pub build_vars: Option<&'a Vec<String>>,
    pub project_slug: &'a str,
}

pub trait Preset: fmt::Display + Send + Sync {
    fn project_type(&self) -> ProjectType;
    fn label(&self) -> String;
    fn icon_url(&self) -> String;
    fn dockerfile(&self, config: DockerfileConfig) -> String;
    fn dockerfile_with_build_dir(&self, local_path: &Path) -> String;
    fn install_command(&self, local_path: &Path) -> String {
        let build_system = BuildSystem::detect(local_path);
        build_system.get_install_command()
    }
    fn build_command(&self, local_path: &Path) -> String {
        let build_system = BuildSystem::detect(local_path);
        build_system.get_build_command(None)
    }
    fn dirs_to_upload(&self) -> Vec<String>;
    fn slug(&self) -> String;
}

pub fn all_presets() -> Vec<Box<dyn Preset>> {
    vec![
        Box::new(NextJs),
        Box::new(Vite),
        Box::new(CreateReactApp),
        Box::new(Rsbuild),
        Box::new(Docusaurus),
        Box::new(DockerfilePreset),
        Box::new(docker_custom::DockerCustomPreset),
    ]
}

pub fn get_preset_by_slug(slug: &str) -> Option<Box<dyn Preset>> {
    all_presets()
        .into_iter()
        .find(|preset| preset.slug() == slug)
}

/// Configuration for creating a custom preset
pub struct CreateCustomPresetConfig {
    pub label: String,
    pub icon_url: String,
    pub project_type: ProjectType,
    pub dockerfile: String,
    pub slug: String,
    pub install_command: String,
    pub build_command: String,
    pub dockerfile_with_build_dir: String,
}

/// Create a custom preset with the given configuration
pub fn create_custom_preset(config: CreateCustomPresetConfig) -> Box<dyn Preset> {
    Box::new(CustomPreset::new(custom::CustomPresetConfig {
        label: config.label,
        icon_url: config.icon_url,
        project_type: config.project_type,
        dockerfile: config.dockerfile,
        dockerfile_with_build_dir: config.dockerfile_with_build_dir,
        slug: config.slug,
        install_command: config.install_command,
        build_command: config.build_command,
    }))
}

pub fn detect_preset_from_files(files: &[String]) -> Option<Box<dyn Preset>> {
    // Check for Dockerfile first
    if files.iter().any(|path| path.ends_with("Dockerfile")) {
        return Some(Box::new(DockerfilePreset));
    }

    // Check for Docusaurus
    if files.iter().any(|path| {
        path.ends_with("docusaurus.config.js") || path.ends_with("docusaurus.config.ts")
    }) {
        return Some(Box::new(Docusaurus));
    }

    // Check for Next.js
    if files.iter().any(|path| {
        path.ends_with("next.config.js")
            || path.ends_with("next.config.mjs")
            || path.ends_with("next.config.ts")
    }) {
        return Some(Box::new(NextJs));
    }

    // Check for Vite
    if files
        .iter()
        .any(|path| path.ends_with("vite.config.js") || path.ends_with("vite.config.ts"))
    {
        return Some(Box::new(Vite));
    }

    // Check for Create React App
    if files.iter().any(|path| path.contains("react-scripts")) {
        return Some(Box::new(CreateReactApp));
    }

    // Check for Rsbuild
    if files.iter().any(|path| path.ends_with("rsbuild.config.ts")) {
        return Some(Box::new(Rsbuild));
    }

    Some(create_custom_preset(CreateCustomPresetConfig {
        label: "Custom".to_string(),
        icon_url: "".to_string(),
        project_type: ProjectType::Server,
        dockerfile: "".to_string(),
        slug: "custom".to_string(),
        install_command: "".to_string(),
        build_command: "".to_string(),
        dockerfile_with_build_dir: "".to_string(),
    }))
}

// Add this function to register the new docker_custom preset
pub fn register_docker_custom_preset() -> custom::CustomPreset {
    custom::CustomPreset::new(custom::CustomPresetConfig {
        label: "Docker Custom".to_string(),
        icon_url: "/images/presets/docker.svg".to_string(), // Adjust icon path as needed
        project_type: ProjectType::Server,
        slug: "docker_custom".to_string(),
        install_command: "{{INSTALL_COMMAND}}".to_string(),
        build_command: "{{BUILD_COMMAND}}".to_string(),
        dockerfile: r#"FROM alpine:latest

ARG PROJECT_SLUG
ARG INSTALL_COMMAND
ARG BUILD_COMMAND

# Install needed packages
RUN apk add --no-cache nodejs npm git curl

# Set up working directory
WORKDIR /app

# Copy project files
COPY . .

# Run install and build commands
RUN sh -c "$INSTALL_COMMAND"
RUN sh -c "$BUILD_COMMAND"

# Use a lightweight web server
RUN apk add --no-cache nginx

# Set up nginx
COPY nginx.conf /etc/nginx/nginx.conf

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]"#.to_string(),
        dockerfile_with_build_dir: r#"FROM alpine:latest

ARG PROJECT_SLUG
ARG INSTALL_COMMAND
ARG BUILD_COMMAND

# Install needed packages
RUN apk add --no-cache nodejs npm git curl

# Set up working directory
WORKDIR /app

# Copy project files
COPY . .

# Run install and build commands
RUN sh -c "$INSTALL_COMMAND"
RUN sh -c "$BUILD_COMMAND"

# Use a lightweight web server
RUN apk add --no-cache nginx

# Set up nginx
COPY nginx.conf /etc/nginx/nginx.conf

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]"#.to_string(),
    })
}
