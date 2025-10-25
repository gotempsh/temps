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
mod rust_preset;
mod go_preset;
mod python_preset;

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
pub use rust_preset::RustPreset;
pub use go_preset::GoPreset;
pub use python_preset::PythonPreset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        // Node.js / TypeScript frameworks
        Box::new(NextJs),
        Box::new(Vite),
        Box::new(CreateReactApp),
        Box::new(Rsbuild),
        Box::new(Docusaurus),
        // Language-specific presets (using Nixpacks)
        Box::new(RustPreset::new()),
        Box::new(GoPreset::new()),
        Box::new(PythonPreset::new()),
        // Generic presets
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

    // Check for Rust (Cargo.toml)
    if files.iter().any(|path| path.ends_with("Cargo.toml")) {
        return Some(Box::new(RustPreset::new()));
    }

    // Check for Go (go.mod)
    if files.iter().any(|path| path.ends_with("go.mod")) {
        return Some(Box::new(GoPreset::new()));
    }

    // Check for Python (requirements.txt, pyproject.toml, setup.py)
    if files.iter().any(|path| {
        path.ends_with("requirements.txt")
            || path.ends_with("pyproject.toml")
            || path.ends_with("setup.py")
            || path.ends_with("Pipfile")
    }) {
        return Some(Box::new(PythonPreset::new()));
    }

    // Only detect Nixpacks if there's an explicit nixpacks.toml file
    // This prevents Nixpacks from being auto-detected for every path
    // Users can still manually select Nixpacks when creating a project
    if files.iter().any(|path| path.ends_with("nixpacks.toml")) {
        return Some(Box::new(NixpacksPreset::auto()));
    }

    // No preset detected - return None
    // This prevents false positives for directories like "src", "public", etc.
    None
}

/// Information about a detected preset in a specific directory
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPreset {
    /// Relative path from repository root (e.g., "./", "apps/web", "packages/api")
    pub path: String,
    /// Preset slug (e.g., "nextjs", "vite", "dockerfile")
    pub slug: String,
    /// Human-readable preset name (e.g., "Next.js", "Vite", "Dockerfile")
    pub label: String,
    /// Exposed port if applicable
    pub exposed_port: Option<u16>,
}

/// Detect all presets in a file tree
///
/// This function analyzes a complete file tree and identifies presets in different directories.
/// It groups files by directory, detects presets for each directory, and returns a list of
/// detected presets with their locations.
///
/// # Arguments
/// * `files` - Complete list of file paths from repository root (e.g., ["src/main.rs", "apps/web/next.config.js"])
///
/// # Returns
/// A vector of detected presets, sorted by path (root first, then subdirectories)
///
/// # Example
/// ```
/// use temps_presets::detect_presets_from_file_tree;
///
/// let files = vec![
///     "package.json".to_string(),
///     "next.config.js".to_string(),
///     "apps/api/Dockerfile".to_string(),
///     "apps/web/vite.config.ts".to_string(),
/// ];
///
/// let presets = detect_presets_from_file_tree(&files);
/// // Returns presets for root (Next.js), apps/api (Dockerfile), apps/web (Vite)
/// ```
pub fn detect_presets_from_file_tree(files: &[String]) -> Vec<DetectedPreset> {
    use std::collections::HashMap;

    if files.is_empty() {
        return Vec::new();
    }

    // Group files by directory
    let mut directory_files: HashMap<String, Vec<String>> = HashMap::new();

    for path in files {
        let directory = match path.rfind('/') {
            Some(idx) => path[..idx].to_string(),
            None => String::new(), // Root directory
        };

        directory_files
            .entry(directory)
            .or_default()
            .push(path.clone());
    }

    let mut presets = Vec::new();

    // Check each directory for presets
    for (dir, dir_files) in &directory_files {
        // Limit directory depth to avoid detecting presets in deeply nested node_modules, etc.
        // Depth is the number of slashes: "" = 0, "a" = 0, "a/b" = 1, "a/b/c" = 2, etc.
        let depth = dir.matches('/').count();
        if depth >= 4 {
            continue;
        }

        // Skip common directories that shouldn't have presets
        let dir_lower = dir.to_lowercase();
        if dir_lower.contains("node_modules")
            || dir_lower.contains(".git")
            || dir_lower.contains("dist")
            || dir_lower.contains("build")
            || dir_lower.ends_with("/public")
            || dir_lower.ends_with("/static")
            || dir_lower.ends_with("/assets")
        {
            continue;
        }

        if let Some(preset) = detect_preset_from_files(dir_files) {
            // Use relative paths: "./" for root, subdirectory name for others
            let path = if dir.is_empty() {
                "./".to_string()
            } else {
                dir.clone()
            };

            presets.push(DetectedPreset {
                path,
                slug: preset.slug(),
                label: preset.label(),
                exposed_port: None, // Port will be determined during deployment
            });
        }
    }

    // Sort presets by path for consistent output (root "./" comes first)
    presets.sort_by(|a, b| {
        // Root should come first
        if a.path == "./" && b.path != "./" {
            std::cmp::Ordering::Less
        } else if a.path != "./" && b.path == "./" {
            std::cmp::Ordering::Greater
        } else {
            a.path.cmp(&b.path)
        }
    });

    presets
}
