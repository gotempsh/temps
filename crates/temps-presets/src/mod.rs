// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use std::{fmt, path::Path};

mod autopack_preset;
mod build_system;
mod docker;
pub mod docker_compose;
pub mod dockerfile_expose;
mod docker_custom;
mod docusaurus;
pub mod env_example;
mod framework_detector;
mod go_preset;
mod java_preset;
mod nextjs;
mod nixpacks_preset;
mod preset_config;
mod python_preset;
mod react_app;
mod rsbuild;
mod rust_preset;
mod vite;

// Preset configuration schemas
// Source abstraction for file access
pub mod preset_config_schema;
pub mod source;

// New preset provider system
pub mod preset_provider;
pub mod providers;

// Re-export Preset enum from temps-entities
pub use autopack_preset::AutopackPreset;
use build_system::BuildSystem;
pub use build_system::MonorepoTool;
use docker::DockerfilePreset;
use docker_custom::DockerCustomPreset;
use docusaurus::Docusaurus;
pub use framework_detector::{
    detect_node_framework, detect_node_framework_from_package_json, NodeFramework,
};
pub use go_preset::GoPreset;
pub use java_preset::JavaPreset;
pub use nextjs::NextJs;
pub use nixpacks_preset::{NixpacksPreset, NixpacksProvider};
pub use preset_config::PresetConfig;
pub use python_preset::PythonPreset;
pub use react_app::CreateReactApp;
use rsbuild::Rsbuild;
pub use rust_preset::RustPreset;
pub use temps_entities::preset::Preset as PresetType;
use temps_entities::preset::{
    DockerfileVariant, ImageRuntimeConfig, NixpacksConfig, PresetConfig as StoredPresetConfig,
};
pub use vite::Vite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    Server,
    Static,
}

/// Canonical representation of a selectable catalog preset.
///
/// Catalog slugs are presentation-level variants such as `nixpacks-node` or
/// `react-app`. Projects persist the canonical preset plus its typed config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPreset {
    pub preset: PresetType,
    pub config: Option<StoredPresetConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresetResolutionError {
    #[error("Unknown preset: {slug}")]
    UnknownSlug { slug: String },

    #[error("Preset '{slug}' cannot be persisted")]
    NotPersistable { slug: String },

    #[error("Preset config for '{config_preset}' cannot be used with '{slug}'")]
    ConfigMismatch {
        config_preset: PresetType,
        slug: String,
    },

    #[error("Invalid config for preset '{slug}': {reason}")]
    InvalidConfig { slug: String, reason: String },
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

    pub fn start_command(&self) -> &'static str {
        match self {
            PackageManager::Bun => "[\"bun\", \"start\"]",
            PackageManager::Yarn => "[\"yarn\", \"start\"]",
            PackageManager::Npm => "[\"npm\", \"start\"]",
            PackageManager::Pnpm => "[\"pnpm\", \"start\"]",
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
    pub fn new(root_local_path: &'a Path, local_path: &'a Path, project_slug: &'a str) -> Self {
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
    pub fn with_args(
        content: String,
        build_args: std::collections::HashMap<String, String>,
    ) -> Self {
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

    /// Canonical database preset represented by this catalog entry.
    ///
    /// Most presets have identical catalog and storage slugs. Variants override
    /// this method. Entries without a persistable identity return `None`.
    fn stored_preset(&self) -> Option<PresetType> {
        self.slug().parse().ok()
    }

    /// Resolve this catalog entry with optional user configuration.
    fn resolve_storage(
        &self,
        config: Option<StoredPresetConfig>,
    ) -> Result<StoredPreset, PresetResolutionError> {
        let preset = self
            .stored_preset()
            .ok_or_else(|| PresetResolutionError::NotPersistable { slug: self.slug() })?;

        if let Some(config) = config.as_ref() {
            validate_preset_config(preset, config)?;
        }

        Ok(StoredPreset { preset, config })
    }

    /// Returns the default exposed port for this preset
    /// This is the port the application listens on inside the container
    fn default_port(&self) -> u16 {
        3000 // Default port for most web applications
    }

    /// Returns the static output directory for presets that can be deployed as static files
    /// Returns None for presets that require a runtime server
    /// For static-capable presets (Vite, React, etc.), returns Some("dist"), Some("build"), etc.
    fn static_output_dir(&self) -> Option<String> {
        None // Default: requires runtime server
    }

    /// Whether this preset needs a container build step at all.
    ///
    /// `true` for every preset that compiles something (even static-capable
    /// ones like Vite still need `npm run build` inside a container) or that
    /// runs as a long-lived server. `false` only for presets whose deployable
    /// output *is* the checked-out source with no build step — there the
    /// workflow planner skips Docker/image-build entirely and deploys
    /// directly from the downloaded repository, since building an image just
    /// to immediately discard it (or, worse, run it as a container purely to
    /// serve static files) is pure overhead.
    fn needs_container_build(&self) -> bool {
        true
    }
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
        Box::new(JavaPreset::new()),
        // Generic presets
        Box::new(docker_compose::DockerComposePreset),
        Box::new(DockerfilePreset),
        Box::new(DockerCustomPreset),
        // Nixpacks auto-detect
        Box::new(AutopackPreset::new()),
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

/// Resolve a public catalog slug to its canonical persisted representation.
pub fn resolve_preset_slug(
    slug: &str,
    config: Option<StoredPresetConfig>,
) -> Result<StoredPreset, PresetResolutionError> {
    let preset = get_preset_by_slug(slug).ok_or_else(|| PresetResolutionError::UnknownSlug {
        slug: slug.to_string(),
    })?;
    preset.resolve_storage(config)
}

/// Validate typed configuration for its canonical stored preset.
///
/// This is intentionally public so create, full-update, and config-only patch
/// paths share the same validation boundary.
pub fn validate_preset_config(
    preset: PresetType,
    config: &StoredPresetConfig,
) -> Result<(), PresetResolutionError> {
    if config.preset_type() != preset {
        return Err(PresetResolutionError::ConfigMismatch {
            config_preset: config.preset_type(),
            slug: preset.as_str().to_string(),
        });
    }

    if let StoredPresetConfig::Nixpacks(config) = config {
        NixpacksPreset::validate_config(config)?;
    }

    if let StoredPresetConfig::Dockerfile(config) = config {
        if let Some(runtime) = config.image_runtime.as_ref() {
            validate_image_runtime_config(runtime)?;
        }
    }

    Ok(())
}

/// Validate the durable runtime snapshot used by prebuilt-image templates.
///
/// These checks live at the preset persistence boundary so settings cannot be
/// saved successfully with values that every later deployment would reject.
pub fn validate_image_runtime_config(
    runtime: &ImageRuntimeConfig,
) -> Result<(), PresetResolutionError> {
    let invalid = |reason: &str| PresetResolutionError::InvalidConfig {
        slug: PresetType::Dockerfile.as_str().to_string(),
        reason: reason.to_string(),
    };

    if runtime.image_ref.is_empty() || runtime.image_ref.len() > 512 {
        return Err(invalid(
            "image reference must contain between 1 and 512 bytes",
        ));
    }
    if runtime
        .image_ref
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(invalid(
            "image reference cannot contain whitespace or control characters",
        ));
    }

    if let Some(command) = runtime.command.as_ref() {
        if command.len() > 64 {
            return Err(invalid("container command supports at most 64 arguments"));
        }
        if command.iter().any(|part| {
            part.is_empty() || part.len() > 1_024 || part.chars().any(char::is_control)
        }) {
            return Err(invalid(
                "container command arguments must be non-empty, at most 1024 bytes, and contain no control characters",
            ));
        }
    }

    if let Some(path) = runtime.health_check_path.as_deref() {
        if path.is_empty()
            || path.len() > 2_048
            || !path.starts_with('/')
            || path.contains('@')
            || path.contains("://")
            || path.chars().any(char::is_control)
        {
            return Err(invalid(
                "health-check path must be a safe relative HTTP path starting with '/'",
            ));
        }
    }

    Ok(())
}

/// Instantiate the build preset for a canonical stored project configuration.
pub fn get_preset_for_storage(
    preset: PresetType,
    config: Option<&StoredPresetConfig>,
) -> Result<Option<Box<dyn Preset>>, PresetResolutionError> {
    if let Some(config) = config {
        validate_preset_config(preset, config)?;
    }

    if preset == PresetType::Nixpacks {
        let nixpacks_config = match config {
            Some(StoredPresetConfig::Nixpacks(config)) => config.clone(),
            _ => NixpacksConfig::default(),
        };
        return Ok(Some(Box::new(NixpacksPreset::from_config(nixpacks_config))));
    }

    if preset == PresetType::Dockerfile {
        let is_custom = matches!(
            config,
            Some(StoredPresetConfig::Dockerfile(config))
                if config.variant == DockerfileVariant::Custom
        );
        let runtime_preset: Box<dyn Preset> = if is_custom {
            Box::new(DockerCustomPreset)
        } else {
            Box::new(DockerfilePreset)
        };
        return Ok(Some(runtime_preset));
    }

    Ok(all_presets()
        .into_iter()
        .find(|candidate| candidate.stored_preset() == Some(preset)))
}

/// Public catalog slug corresponding to a stored project.
///
/// Multi-provider Nixpacks configurations intentionally use the canonical
/// `nixpacks` slug because no single catalog variant can represent them.
pub fn runtime_slug(preset: PresetType, config: Option<&StoredPresetConfig>) -> String {
    get_preset_for_storage(preset, config)
        .ok()
        .flatten()
        .map(|runtime_preset| runtime_preset.slug())
        .filter(|slug| get_preset_by_slug(slug).is_some())
        .unwrap_or_else(|| preset.as_str().to_string())
}

pub fn detect_preset_from_files(files: &[String]) -> Option<Box<dyn Preset>> {
    // Returns the highest-priority preset for deployment decisions
    detect_all_presets_from_files(files).into_iter().next()
}

/// Detect ALL matching presets for a set of files in a single directory.
///
/// Unlike `detect_preset_from_files` which returns only the highest-priority match,
/// this returns every preset that matches the directory's files. This allows users
/// to choose between e.g. Dockerfile, Docker Compose, and Next.js when all three
/// config files exist in the same directory.
///
/// Results are ordered by priority (Docker Compose first, then Dockerfile, then frameworks).
pub fn detect_all_presets_from_files(files: &[String]) -> Vec<Box<dyn Preset>> {
    let mut presets: Vec<Box<dyn Preset>> = Vec::new();

    // Check for Docker Compose files
    if files.iter().any(|path| {
        docker_compose::COMPOSE_FILE_NAMES
            .iter()
            .any(|name| path.ends_with(name))
    }) {
        presets.push(Box::new(docker_compose::DockerComposePreset));
    }

    // Check for Dockerfile
    if files.iter().any(|path| path.ends_with("Dockerfile")) {
        presets.push(Box::new(DockerfilePreset));
    }

    // Check for Docusaurus
    if files.iter().any(|path| {
        path.ends_with("docusaurus.config.js") || path.ends_with("docusaurus.config.ts")
    }) {
        presets.push(Box::new(Docusaurus));
    }

    // Check for Next.js
    if files.iter().any(|path| {
        path.ends_with("next.config.js")
            || path.ends_with("next.config.mjs")
            || path.ends_with("next.config.ts")
    }) {
        presets.push(Box::new(NextJs));
    }

    // Check for Vite
    if files
        .iter()
        .any(|path| path.ends_with("vite.config.js") || path.ends_with("vite.config.ts"))
    {
        presets.push(Box::new(Vite));
    }

    // Check for Create React App
    if files.iter().any(|path| path.contains("react-scripts")) {
        presets.push(Box::new(CreateReactApp));
    }

    // Check for Rsbuild
    if files.iter().any(|path| path.ends_with("rsbuild.config.ts")) {
        presets.push(Box::new(Rsbuild));
    }

    // Check for Rust (Cargo.toml)
    if files.iter().any(|path| path.ends_with("Cargo.toml")) {
        presets.push(Box::new(RustPreset::new()));
    }

    // Check for Go (go.mod)
    if files.iter().any(|path| path.ends_with("go.mod")) {
        presets.push(Box::new(GoPreset::new()));
    }

    // Check for Python (requirements.txt, pyproject.toml, setup.py)
    if files.iter().any(|path| {
        path.ends_with("requirements.txt")
            || path.ends_with("pyproject.toml")
            || path.ends_with("setup.py")
            || path.ends_with("Pipfile")
    }) {
        presets.push(Box::new(PythonPreset::new()));
    }

    // Check for Java (pom.xml, build.gradle, build.gradle.kts)
    if files.iter().any(|path| {
        path.ends_with("pom.xml")
            || path.ends_with("build.gradle")
            || path.ends_with("build.gradle.kts")
    }) {
        presets.push(Box::new(JavaPreset::new()));
    }

    // Only detect Nixpacks if there's an explicit nixpacks.toml file
    if files.iter().any(|path| path.ends_with("nixpacks.toml")) {
        presets.push(Box::new(NixpacksPreset::auto()));
    }

    // Static site fallback: a plain index.html with no build system found above.
    // Checked last, mirroring `detect_project_candidates` and autopack's own
    // static provider (registered last, only claims what no language claims).
    if presets.is_empty() && files.iter().any(|path| path.ends_with("index.html")) {
        presets.push(Box::new(NixpacksPreset::new(NixpacksProvider::Static)));
    }

    presets
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
    /// Compose file paths found in the repository (only for docker-compose preset)
    pub compose_files: Option<Vec<String>>,
    /// Repository-root-relative path to the Dockerfile, when it does not
    /// live directly under `{path}/Dockerfile`.
    ///
    /// Set only for a `dockerfile` preset whose Dockerfile was found alone
    /// (no manifest of its own) in a subdirectory conventionally used to
    /// hold one, e.g. `docker/Dockerfile` or `.devcontainer/Dockerfile`.
    /// That Dockerfile's `COPY`/`ADD` instructions typically reach back to
    /// the real repository root, so the candidate is rooted at `path =
    /// "./"` with this field pointing at the nested file, rather than
    /// promoting the subdirectory itself to a project root (which would
    /// wrongly become the build context too). `None` for a Dockerfile at
    /// `{path}/Dockerfile` (including a genuine monorepo service directory
    /// that has both its own Dockerfile and its own manifest) and for every
    /// non-Dockerfile preset.
    pub dockerfile_path: Option<String>,
}

/// Detection result with the evidence that selected the preset. This is used
/// by archive uploads where file contents (especially package.json) are
/// available without a Git checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCandidate {
    pub path: String,
    pub preset: PresetType,
    pub confidence: &'static str,
    pub reason: String,
    /// Repository-root-relative path to the Dockerfile, when it does not
    /// live directly under `{path}/Dockerfile`. See
    /// [`DetectedPreset::dockerfile_path`] for the full explanation — this
    /// is the same concept for the archive-upload detection path.
    pub dockerfile_path: Option<String>,
}

impl ProjectCandidate {
    /// Return the public preset catalog slug that can be passed to project
    /// creation for this detected candidate.
    ///
    /// Some canonical framework identifiers do not have a dedicated build
    /// preset yet. Those projects are still zero-config deployable through
    /// the matching Nixpacks provider.
    pub fn catalog_slug(&self) -> &'static str {
        match self.preset {
            PresetType::Astro
            | PresetType::Nuxt
            | PresetType::Remix
            | PresetType::SvelteKit
            | PresetType::SolidStart
            | PresetType::Angular
            | PresetType::Vue
            | PresetType::NodeJs => "nixpacks-node",
            PresetType::Static => "nixpacks-static",
            _ => self.preset.as_str(),
        }
    }
}

/// Directory names conventionally used to hold a Dockerfile that is not
/// itself an independent project — its `COPY`/`ADD` instructions typically
/// reach back to the real repository root, unlike a Dockerfile that happens
/// to sit alone in a monorepo service directory (e.g. `apps/api/Dockerfile`
/// with no `apps/api/package.json`, which — by convention — genuinely is
/// that service's own root and build context).
///
/// Directory *contents* alone cannot tell these two shapes apart: both are
/// "a Dockerfile with no manifest next to it". The directory *name* is the
/// only reliable signal, so this list is deliberately an allowlist of
/// well-known Docker-tooling conventions rather than a broader heuristic
/// that would risk misrouting a real monorepo service.
const DOCKERFILE_ONLY_DIR_NAMES: [&str; 6] = [
    "docker",
    ".docker",
    ".devcontainer",
    "deploy",
    "deployment",
    "dockerfiles",
];

/// The final path segment of `directory` (the part after the last `/`).
fn dir_basename(directory: &str) -> &str {
    directory.rsplit('/').next().unwrap_or(directory)
}

/// Detect deployable project roots from normalized archive entries.
///
/// `files` maps slash-separated relative paths to the contents of small text
/// manifests. Binary and large files may be represented by an empty string.
pub fn detect_project_candidates(
    files: &std::collections::BTreeMap<String, String>,
) -> Vec<ProjectCandidate> {
    use std::collections::{BTreeMap, BTreeSet};

    /// Directories that never contain a *deployable* root — they hold
    /// dependencies, build output, or VCS metadata. Without this a ZIP that
    /// shipped its `node_modules` offers thousands of bogus candidates.
    const SKIP_SEGMENTS: [&str; 8] = [
        "node_modules",
        ".git",
        "dist",
        "build",
        "vendor",
        "target",
        ".next",
        "__pycache__",
    ];
    /// Deployable roots live near the top of an archive. Bounding depth keeps
    /// a pathological archive from turning detection into an O(n^2) walk.
    const MAX_ROOT_DEPTH: usize = 4;

    // Index every path by its directory ONCE. The previous implementation
    // rescanned all of `files` for each root, which is O(roots x files) — a
    // 20k-entry archive turned into ~4x10^8 string comparisons per request.
    let mut by_directory: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in files.keys() {
        let (directory, name) = match path.rsplit_once('/') {
            Some((directory, name)) => (directory, name),
            None => (".", path.as_str()),
        };
        if directory
            .split('/')
            .any(|segment| SKIP_SEGMENTS.contains(&segment))
        {
            continue;
        }
        if directory != "." && directory.split('/').count() > MAX_ROOT_DEPTH {
            continue;
        }
        by_directory.entry(directory).or_default().push(name);
    }

    // Manifests that make a directory an independently buildable project on
    // its own, regardless of whether a Dockerfile also lives there. This
    // distinguishes a genuine monorepo service (its own Dockerfile *and* its
    // own manifest, e.g. `apps/api/Dockerfile` + `apps/api/package.json`)
    // from a bare Dockerfile conventionally tucked into a subdirectory like
    // `docker/` or `.devcontainer/`, whose `COPY`/`ADD` instructions
    // typically reach back to the real repository root.
    let has_independent_manifest = |names: &[&str]| {
        names.iter().any(|name| {
            matches!(
                *name,
                "package.json"
                    | "docker-compose.yml"
                    | "docker-compose.yaml"
                    | "compose.yml"
                    | "compose.yaml"
                    | "Cargo.toml"
                    | "go.mod"
                    | "requirements.txt"
                    | "pyproject.toml"
                    | "pom.xml"
                    | "build.gradle"
                    | "index.html"
            ) || name.ends_with(".csproj")
                || name.starts_with("next.config.")
                || name.starts_with("vite.config.")
                || name.starts_with("astro.config.")
        })
    };

    let mut roots = BTreeSet::new();
    // Subdirectories whose only signal is a bare `Dockerfile`, in a
    // directory conventionally used to hold one — not promoted to their own
    // project root. Surfaced as a build option on the repository root
    // instead (see below).
    let mut orphan_dockerfile_dirs: Vec<&str> = Vec::new();
    for (directory, names) in &by_directory {
        let has_dockerfile = names.contains(&"Dockerfile");
        if *directory != "."
            && has_dockerfile
            && !has_independent_manifest(names)
            && DOCKERFILE_ONLY_DIR_NAMES.contains(&dir_basename(directory))
        {
            orphan_dockerfile_dirs.push(directory);
            continue;
        }
        if has_dockerfile || has_independent_manifest(names) {
            roots.insert(*directory);
        }
    }

    let mut candidates = Vec::new();
    for root in roots {
        let at_root = |name: &str| {
            if root == "." {
                name.to_string()
            } else {
                format!("{root}/{name}")
            }
        };
        let names = by_directory.get(root).map(Vec::as_slice).unwrap_or(&[]);
        let has = |name: &str| names.contains(&name);
        let has_extension =
            |extension: &str| names.iter().any(|name| name.ends_with(extension));

        let detected = if has("docker-compose.yml")
            || has("docker-compose.yaml")
            || has("compose.yml")
            || has("compose.yaml")
        {
            Some((
                PresetType::DockerCompose,
                "high",
                "Docker Compose file found".to_string(),
            ))
        } else if has("Dockerfile") {
            Some((
                PresetType::Dockerfile,
                "high",
                "Dockerfile found".to_string(),
            ))
        } else if let Some(package_json) = files.get(&at_root("package.json")) {
            detect_package_json_preset(package_json)
        } else if has("Cargo.toml") {
            Some((PresetType::Rust, "high", "Cargo.toml found".to_string()))
        } else if has("go.mod") {
            Some((PresetType::Go, "high", "go.mod found".to_string()))
        } else if has("requirements.txt") || has("pyproject.toml") {
            Some((
                PresetType::Python,
                "medium",
                "Python manifest found".to_string(),
            ))
        } else if has("pom.xml") || has("build.gradle") {
            Some((
                PresetType::Java,
                "high",
                "Java build manifest found".to_string(),
            ))
        } else if has_extension(".csproj") {
            Some((
                PresetType::Nixpacks,
                "high",
                ".NET project file found".to_string(),
            ))
        } else if has("index.html") {
            Some((PresetType::Static, "medium", "index.html found".to_string()))
        } else {
            None
        };

        if let Some((preset, confidence, reason)) = detected {
            candidates.push(ProjectCandidate {
                path: root.to_string(),
                preset,
                confidence,
                reason,
                dockerfile_path: None,
            });
        }
    }

    // Every orphaned Dockerfile becomes a build option rooted at the
    // repository root, not at the subdirectory it was found in — so the
    // default build context stays the root the Dockerfile's own COPY/ADD
    // paths almost always assume.
    orphan_dockerfile_dirs.sort_unstable();
    for dir in orphan_dockerfile_dirs {
        candidates.push(ProjectCandidate {
            path: ".".to_string(),
            preset: PresetType::Dockerfile,
            confidence: "medium",
            reason: format!("Dockerfile found in {dir}/ (build context defaults to the repository root)"),
            dockerfile_path: Some(format!("{dir}/Dockerfile")),
        });
    }

    candidates.sort_by(|left, right| {
        let left_root = left.path == ".";
        let right_root = right.path == ".";
        right_root
            .cmp(&left_root)
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates
}

fn detect_package_json_preset(content: &str) -> Option<(PresetType, &'static str, String)> {
    let package: serde_json::Value = serde_json::from_str(content).ok()?;
    let has_dependency = |name: &str| {
        package
            .get("dependencies")
            .and_then(|value| value.get(name))
            .is_some()
            || package
                .get("devDependencies")
                .and_then(|value| value.get(name))
                .is_some()
    };

    let (preset, label) = if has_dependency("next") {
        (PresetType::NextJs, "next")
    } else if has_dependency("astro") {
        (PresetType::Astro, "astro")
    } else if has_dependency("nuxt") {
        (PresetType::Nuxt, "nuxt")
    } else if has_dependency("@remix-run/react") {
        (PresetType::Remix, "@remix-run/react")
    } else if has_dependency("@sveltejs/kit") {
        (PresetType::SvelteKit, "@sveltejs/kit")
    } else if has_dependency("vite") {
        (PresetType::Vite, "vite")
    } else {
        return Some((
            PresetType::NodeJs,
            "medium",
            "package.json found".to_string(),
        ));
    };

    Some((
        preset,
        "high",
        format!("{label} dependency found in package.json"),
    ))
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

        let detected = detect_all_presets_from_files(dir_files);
        // A subdirectory whose only detected preset is a bare Dockerfile (no
        // manifest of its own — that would have produced additional entries
        // here) AND whose name is a known Docker-tooling convention (e.g.
        // `docker/Dockerfile`, `.devcontainer/Dockerfile`) typically has
        // COPY/ADD instructions that reach back to the real repository root.
        // Root the candidate at "./" and record the nested path instead of
        // promoting the subdirectory to its own project root, which would
        // wrongly become the build context too.
        //
        // A directory with its own manifest alongside the Dockerfile (a
        // genuine monorepo service) produces more than one entry here and
        // keeps today's behaviour. So does a directory with only a
        // Dockerfile whose name is NOT one of those conventions — e.g.
        // `apps/api/Dockerfile` with no `apps/api/package.json` is, by
        // monorepo convention, that service's own root; directory contents
        // alone cannot distinguish it from the `docker/` case, so the
        // directory name is the deciding signal.
        let has_only_dockerfile = !dir.is_empty()
            && detected.len() == 1
            && detected[0].slug() == "dockerfile"
            && DOCKERFILE_ONLY_DIR_NAMES.contains(&dir_basename(dir));

        for preset in detected {
            // Use relative paths: "./" for root, subdirectory name for others
            let (path, dockerfile_path) = if has_only_dockerfile {
                ("./".to_string(), Some(format!("{dir}/Dockerfile")))
            } else if dir.is_empty() {
                ("./".to_string(), None)
            } else {
                (dir.clone(), None)
            };

            // For docker-compose presets, collect all compose file paths in the repo
            let compose_files = if preset.slug() == "docker-compose" {
                let mut files_found: Vec<String> = Vec::new();
                for (d, d_files) in &directory_files {
                    for file_path in d_files {
                        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
                        if docker_compose::COMPOSE_FILE_NAMES.contains(&filename) {
                            // Build relative path from repo root
                            let relative = if d.is_empty() {
                                filename.to_string()
                            } else {
                                file_path.clone()
                            };
                            files_found.push(relative);
                        }
                    }
                }
                files_found.sort();
                Some(files_found)
            } else {
                None
            };

            presets.push(DetectedPreset {
                path,
                slug: preset.slug(),
                label: preset.label(),
                exposed_port: None, // Port will be determined during deployment
                compose_files,
                dockerfile_path,
            });
        }
    }

    // Sort presets by path for consistent output (root "./" comes first), then by slug
    presets.sort_by(|a, b| {
        // Root should come first
        let path_ord = if a.path == "./" && b.path != "./" {
            std::cmp::Ordering::Less
        } else if a.path != "./" && b.path == "./" {
            std::cmp::Ordering::Greater
        } else {
            a.path.cmp(&b.path)
        };
        path_ord.then_with(|| a.slug.cmp(&b.slug))
    });

    presets
}

#[cfg(test)]
mod uploaded_source_detection_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn image_runtime() -> ImageRuntimeConfig {
        ImageRuntimeConfig {
            image_ref: "quay.io/keycloak/keycloak:26.7.2".to_string(),
            command: Some(vec!["start".to_string()]),
            health_check_path: Some("/realms/master".to_string()),
        }
    }

    #[test]
    fn image_runtime_validation_accepts_safe_runtime() {
        assert!(validate_image_runtime_config(&image_runtime()).is_ok());
    }

    #[test]
    fn image_runtime_validation_rejects_values_that_deploy_cannot_run() {
        let mut runtime = image_runtime();
        runtime.command = Some(vec!["part".to_string(); 65]);
        assert!(validate_image_runtime_config(&runtime).is_err());

        let mut runtime = image_runtime();
        runtime.command = Some(vec!["bad\nargument".to_string()]);
        assert!(validate_image_runtime_config(&runtime).is_err());

        let mut runtime = image_runtime();
        runtime.health_check_path = Some("https://attacker.example".to_string());
        assert!(validate_image_runtime_config(&runtime).is_err());

        let mut runtime = image_runtime();
        runtime.image_ref = "registry.example/image:tag with-space".to_string();
        assert!(validate_image_runtime_config(&runtime).is_err());
    }

    #[test]
    fn detects_next_without_a_next_config_file() {
        let files = BTreeMap::from([(
            "package.json".to_string(),
            r#"{"dependencies":{"next":"15.0.0","react":"19.0.0"}}"#.to_string(),
        )]);

        let candidates = detect_project_candidates(&files);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].preset, PresetType::NextJs);
        assert_eq!(candidates[0].path, ".");
        assert_eq!(candidates[0].confidence, "high");
    }

    #[test]
    fn detects_nested_node_and_vite_projects_in_stable_order() {
        let files = BTreeMap::from([
            (
                "apps/api/package.json".to_string(),
                r#"{"dependencies":{"express":"5.0.0"}}"#.to_string(),
            ),
            (
                "apps/web/package.json".to_string(),
                r#"{"devDependencies":{"vite":"7.0.0"}}"#.to_string(),
            ),
        ]);

        let candidates = detect_project_candidates(&files);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].path, "apps/api");
        assert_eq!(candidates[0].preset, PresetType::NodeJs);
        assert_eq!(candidates[1].path, "apps/web");
        assert_eq!(candidates[1].preset, PresetType::Vite);
    }

    #[test]
    fn dockerfile_takes_priority_over_package_json() {
        let files = BTreeMap::from([
            ("Dockerfile".to_string(), "FROM node:22".to_string()),
            ("package.json".to_string(), "{}".to_string()),
        ]);

        let candidates = detect_project_candidates(&files);

        assert_eq!(candidates[0].preset, PresetType::Dockerfile);
    }

    #[test]
    fn a_bare_dockerfile_in_a_conventional_subdirectory_roots_at_the_repository_root() {
        // Mirrors a real repository (JupyterLab) that ships a `docker/Dockerfile`
        // whose COPY instructions reach back to files at the repository root
        // (pyproject.toml, LICENSE, README.md, ...). Rooting the candidate at
        // "docker" instead would make "docker" the default build context and
        // break every one of those COPY paths.
        let files = BTreeMap::from([
            ("pyproject.toml".to_string(), "[project]\nname = \"x\"\n".to_string()),
            ("docker/Dockerfile".to_string(), "FROM debian\nCOPY pyproject.toml ./\n".to_string()),
        ]);

        let candidates = detect_project_candidates(&files);

        let dockerfile_candidate = candidates
            .iter()
            .find(|c| c.preset == PresetType::Dockerfile)
            .expect("Dockerfile should still be offered as a candidate");
        assert_eq!(dockerfile_candidate.path, ".");
        assert_eq!(
            dockerfile_candidate.dockerfile_path.as_deref(),
            Some("docker/Dockerfile")
        );
        // The root's own Python detection must survive alongside it.
        assert!(candidates.iter().any(|c| c.preset == PresetType::Python));
    }

    #[test]
    fn a_bare_dockerfile_in_a_monorepo_service_directory_keeps_its_own_root() {
        // `apps/api/Dockerfile` with no manifest of its own is, by monorepo
        // convention, that service's own root — "apps" is not one of the
        // Docker-tooling directory names, so it must NOT be treated as an
        // orphan even though the directory contains only a Dockerfile.
        let files = BTreeMap::from([(
            "apps/api/Dockerfile".to_string(),
            "FROM node:22".to_string(),
        )]);

        let candidates = detect_project_candidates(&files);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, "apps/api");
        assert_eq!(candidates[0].preset, PresetType::Dockerfile);
        assert_eq!(candidates[0].dockerfile_path, None);
    }

    #[test]
    fn detects_dotnet_project_in_nested_directory() {
        let files = BTreeMap::from([(
            "services/api/Api.csproj".to_string(),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web" />"#.to_string(),
        )]);

        let candidates = detect_project_candidates(&files);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, "services/api");
        assert_eq!(candidates[0].preset, PresetType::Nixpacks);
        assert!(candidates[0].reason.contains(".NET"));
    }

    #[test]
    fn every_detected_candidate_exposes_a_resolvable_catalog_slug() {
        let fixtures = [
            BTreeMap::from([(
                "package.json".to_string(),
                r#"{"dependencies":{"express":"5.0.0"}}"#.to_string(),
            )]),
            BTreeMap::from([(
                "package.json".to_string(),
                r#"{"dependencies":{"astro":"5.0.0"}}"#.to_string(),
            )]),
            BTreeMap::from([("index.html".to_string(), "<!doctype html>".to_string())]),
        ];

        for files in fixtures {
            let candidate = detect_project_candidates(&files)
                .into_iter()
                .next()
                .expect("fixture should produce a candidate");

            resolve_preset_slug(candidate.catalog_slug(), None).unwrap_or_else(|error| {
                panic!(
                    "detected {:?} emitted invalid catalog slug '{}': {error}",
                    candidate.preset,
                    candidate.catalog_slug()
                )
            });
        }
    }

    #[test]
    fn detects_docker_compose_at_the_archive_root() {
        for name in [
            "docker-compose.yml",
            "docker-compose.yaml",
            "compose.yml",
            "compose.yaml",
        ] {
            let files = BTreeMap::from([
                (name.to_string(), "services:\n  web:\n    image: nginx".to_string()),
                ("package.json".to_string(), "{}".to_string()),
            ]);

            let candidates = detect_project_candidates(&files);

            assert_eq!(candidates.len(), 1, "{name} should yield one candidate");
            assert_eq!(
                candidates[0].preset,
                PresetType::DockerCompose,
                "{name} should win over package.json"
            );
            assert_eq!(candidates[0].path, ".");
        }
    }

    #[test]
    fn detects_project_wrapped_in_a_single_top_level_directory() {
        // GitHub-style archives wrap everything in `<repo>-<ref>/`.
        let files = BTreeMap::from([(
            "my-app-main/package.json".to_string(),
            r#"{"dependencies":{"next":"15.0.0"}}"#.to_string(),
        )]);

        let candidates = detect_project_candidates(&files);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, "my-app-main");
        assert_eq!(candidates[0].preset, PresetType::NextJs);
    }

    #[test]
    fn ignores_dependency_and_build_output_directories() {
        let files = BTreeMap::from([
            (
                "package.json".to_string(),
                r#"{"dependencies":{"vite":"7.0.0"}}"#.to_string(),
            ),
            ("node_modules/left-pad/package.json".to_string(), "{}".to_string()),
            ("dist/index.html".to_string(), "<!doctype html>".to_string()),
            ("vendor/thing/go.mod".to_string(), "module thing".to_string()),
            ("target/debug/Cargo.toml".to_string(), "[package]".to_string()),
            (".git/config".to_string(), String::new()),
        ]);

        let candidates = detect_project_candidates(&files);

        assert_eq!(
            candidates.len(),
            1,
            "only the real root is deployable, got {candidates:?}"
        );
        assert_eq!(candidates[0].path, ".");
        assert_eq!(candidates[0].preset, PresetType::Vite);
    }

    #[test]
    fn deeply_nested_directories_do_not_become_candidates() {
        let files = BTreeMap::from([(
            "a/b/c/d/e/package.json".to_string(),
            r#"{"dependencies":{"express":"5.0.0"}}"#.to_string(),
        )]);

        assert!(detect_project_candidates(&files).is_empty());
    }

    /// Regression guard for the O(roots x files) scan: this fixture used to
    /// cost ~4x10^8 string comparisons. It must now stay linear enough to
    /// finish effectively instantly.
    #[test]
    fn wide_archives_do_not_blow_up_detection() {
        let mut files = BTreeMap::new();
        for i in 0..5_000 {
            files.insert(format!("site{i}/index.html"), "<!doctype html>".to_string());
        }

        let started = std::time::Instant::now();
        let candidates = detect_project_candidates(&files);
        let elapsed = started.elapsed();

        assert_eq!(candidates.len(), 5_000);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "detection took {elapsed:?} — the per-root full scan is back"
        );
    }
}
