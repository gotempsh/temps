use std::{fmt, path::Path};
use async_trait::async_trait;

mod docker;
mod docusaurus;
mod nextjs;
mod nixpacks_preset;
mod react_app;
mod rsbuild;
mod vite;
mod build_system;
mod docker_custom;
mod preset_config;
mod framework_detector;

// Preset configuration schemas
// Source abstraction for file access
pub mod source;
pub mod preset_config_schema;

// New preset provider system
pub mod preset_provider;
pub mod providers;

// Re-export Preset enum from temps-entities
pub use temps_entities::preset::Preset as PresetType;
use build_system::BuildSystem;
use docusaurus::Docusaurus;
use docker::DockerfilePreset;
pub use nextjs::NextJs;
pub use nixpacks_preset::{NixpacksPreset, NixpacksProvider};
pub use react_app::CreateReactApp;
use rsbuild::Rsbuild;
pub use vite::Vite;
pub use build_system::MonorepoTool;
pub use preset_config::PresetConfig;
pub use framework_detector::{detect_node_framework, NodeFramework};

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
    /// Whether BuildKit is available for use
    /// If true, Dockerfiles can use --mount syntax for caching
    /// If false, Dockerfiles must be compatible with standard Docker (default: false)
    pub use_buildkit: bool,
}

impl<'a> DockerfileConfig<'a> {
    /// Create a new DockerfileConfig with default values (BuildKit disabled)
    pub fn new(
        root_local_path: &'a Path,
        local_path: &'a Path,
        project_slug: &'a str,
    ) -> Self {
        Self {
            root_local_path,
            local_path,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug,
            use_buildkit: false, // Default to false for compatibility
        }
    }

    /// Enable BuildKit support (allows --mount syntax in Dockerfiles)
    pub fn with_buildkit(mut self, enabled: bool) -> Self {
        self.use_buildkit = enabled;
        self
    }

    /// Set install command
    pub fn with_install_command(mut self, cmd: &'a str) -> Self {
        self.install_command = Some(cmd);
        self
    }

    /// Set build command
    pub fn with_build_command(mut self, cmd: &'a str) -> Self {
        self.build_command = Some(cmd);
        self
    }

    /// Set output directory
    pub fn with_output_dir(mut self, dir: &'a str) -> Self {
        self.output_dir = Some(dir);
        self
    }

    /// Set build variables
    pub fn with_build_vars(mut self, vars: &'a Vec<String>) -> Self {
        self.build_vars = Some(vars);
        self
    }
}

/// Dockerfile content along with build arguments
#[derive(Debug, Clone)]
pub struct DockerfileWithArgs {
    /// The Dockerfile content
    pub content: String,
    /// Build arguments to pass to `docker build --build-arg KEY=VALUE`
    /// These are key-value pairs that will be available as ARG in the Dockerfile
    pub build_args: std::collections::HashMap<String, String>,
}

impl DockerfileWithArgs {
    /// Create a new DockerfileWithArgs with just content (no build args)
    pub fn new(content: String) -> Self {
        Self {
            content,
            build_args: std::collections::HashMap::new(),
        }
    }

    /// Create a new DockerfileWithArgs with content and build args
    pub fn with_args(content: String, build_args: std::collections::HashMap<String, String>) -> Self {
        Self {
            content,
            build_args,
        }
    }

    /// Add a build argument
    pub fn add_arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.build_args.insert(key.into(), value.into());
        self
    }
}

#[async_trait]
pub trait Preset: fmt::Display + Send + Sync {
    fn project_type(&self) -> ProjectType;
    fn label(&self) -> String;
    fn icon_url(&self) -> String;
    fn description(&self) -> String {
        // Default implementation - presets can override
        format!("Optimized for {} applications", self.label())
    }
    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs;
    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs;
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
        // Nixpacks auto-detect
        Box::new(NixpacksPreset::auto()),
        // Nixpacks provider-specific variants
        Box::new(NixpacksPreset::new(NixpacksProvider::Node)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Python)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Rust)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Go)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Java)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Php)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Ruby)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Deno)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Elixir)),
        Box::new(NixpacksPreset::new(NixpacksProvider::CSharp)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Dart)),
        Box::new(NixpacksPreset::new(NixpacksProvider::Static)),
    ]
}

pub fn get_preset_by_slug(slug: &str) -> Option<Box<dyn Preset>> {
    all_presets()
        .into_iter()
        .find(|preset| preset.slug() == slug)
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

    // Fall back to Nixpacks for auto-detection (will be checked at generation time)
    // This allows zero-config deployment for Python, Java, .NET, Go, etc.
    Some(Box::new(NixpacksPreset::auto()))
}
