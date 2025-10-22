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

impl ToString for ProjectType {
    fn to_string(&self) -> String {
        match self {
            ProjectType::Server => "server".to_string(),
            ProjectType::Static => "static".to_string(),
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

pub trait Preset: fmt::Display + Send + Sync {
    fn project_type(&self) -> ProjectType;
    fn label(&self) -> String;
    fn icon_url(&self) -> String;
    fn dockerfile(
        &self,
        root_local_path: &Path,
        local_path: &Path,
        install_command: Option<&str>,
        build_command: Option<&str>,
        output_dir: Option<&str>,
        build_vars: Option<&Vec<String>>,
        project_slug: &str,
    ) -> String;
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

// Add a function to create a custom preset
pub fn create_custom_preset(
    label: String,
    icon_url: String,
    project_type: ProjectType,
    dockerfile: String,
    slug: String,
    install_command: String,
    build_command: String,
    dockerfile_with_build_dir: String,
) -> Box<dyn Preset> {
    Box::new(CustomPreset::new(
        label,
        icon_url,
        project_type,
        dockerfile,
        dockerfile_with_build_dir,
        slug,
        install_command,
        build_command,
    ))
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

    Some(create_custom_preset(
        "Custom".to_string(),
        "".to_string(),
        ProjectType::Server,
        "".to_string(),
        "custom".to_string(),
        "".to_string(),
        "".to_string(),
        "".to_string(),
    ))
}

// Add this function to register the new docker_custom preset
pub fn register_docker_custom_preset() -> custom::CustomPreset {
    custom::CustomPreset::new(
        "Docker Custom".to_string(),
        "/images/presets/docker.svg".to_string(), // Adjust icon path as needed
        ProjectType::Server,
        r#"FROM alpine:latest

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
        r#"FROM alpine:latest

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
        "docker_custom".to_string(),
        "{{INSTALL_COMMAND}}".to_string(),
        "{{BUILD_COMMAND}}".to_string()
    )
}
