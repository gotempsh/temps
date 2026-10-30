// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use std::{fmt, path::Path};

mod autopack_preset;
mod build_system;
mod compiled_workspace;
pub mod compose_check;
mod docker;
pub mod docker_compose;
mod docker_custom;
pub mod dockerfile_expose;
mod docusaurus;
pub mod env_example;
mod framework_detector;
mod go_preset;
mod java_preset;
mod nextjs;
mod nixpacks_preset;
mod pnpm_workspace;
mod preset_config;
mod python_preset;
mod react_app;
pub mod registry_prefix;
mod rsbuild;
mod rust_preset;
mod vite;
mod workspace_manifests;

// Preset configuration schemas
// Source abstraction for file access
pub mod preset_config_schema;
pub mod source;

// New preset provider system
pub mod preset_provider;
pub mod providers;

// Re-export Preset enum from temps-entities
pub use autopack_preset::{python_app_directory, AutopackPreset};
use build_system::BuildSystem;
pub use build_system::MonorepoTool;
pub use compiled_workspace::{
    compiled_workspace_app, CompiledLanguage, CompiledWorkspaceApp, CompiledWorkspaceError,
};
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
pub use pnpm_workspace::{package_workspace_contains, pnpm_workspace_contains};
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

    /// Copy optional pnpm install configuration before dependency installation.
    pub(crate) fn dependency_config_copy(&self, local_path: &Path) -> &'static str {
        if matches!(self, PackageManager::Pnpm) && local_path.join("pnpm-workspace.yaml").is_file()
        {
            "COPY pnpm-workspace.yaml ./\n"
        } else {
            ""
        }
    }

    /// Where this package manager keeps downloaded packages, as
    /// `(directory, environment variables pointing it there)`.
    ///
    /// The directory is a BuildKit cache mount on the install step, so a
    /// lockfile change only downloads what changed instead of every package.
    /// Each manager is pointed at the directory explicitly rather than
    /// relying on its default, because the defaults move between versions.
    pub(crate) fn store_cache(&self) -> (&'static str, &'static [(&'static str, &'static str)]) {
        match self {
            PackageManager::Npm => ("/cache/npm", &[("npm_config_cache", "/cache/npm")]),
            // pnpm 10 and earlier read `npm_config_*`; pnpm 11 only reads
            // `pnpm_config_*`. Each version ignores the other's variable.
            PackageManager::Pnpm => (
                "/cache/pnpm",
                &[
                    ("npm_config_store_dir", "/cache/pnpm"),
                    ("pnpm_config_store_dir", "/cache/pnpm"),
                ],
            ),
            // Yarn 1 reads YARN_CACHE_FOLDER. Yarn 2+ uses its global cache
            // under YARN_GLOBAL_FOLDER by default (enableGlobalCache).
            PackageManager::Yarn => (
                "/cache/yarn",
                &[
                    ("YARN_CACHE_FOLDER", "/cache/yarn/v1"),
                    ("YARN_GLOBAL_FOLDER", "/cache/yarn/berry"),
                ],
            ),
            PackageManager::Bun => ("/cache/bun", &[("BUN_INSTALL_CACHE_DIR", "/cache/bun")]),
        }
    }

    pub(crate) fn id(&self) -> &'static str {
        match self {
            PackageManager::Bun => "bun",
            PackageManager::Yarn => "yarn",
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
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

/// Why a preset could not produce a buildable plan for the selected source.
///
/// The [`Preset::dockerfile`] contract cannot return an error, so a preset that
/// knows up front that the build will fail still renders a Dockerfile that
/// fails loudly (for `temps build` and any other caller that only looks at the
/// content) and records the reason here. The deployment pipeline checks this
/// before starting a build and fails immediately with the message, instead of
/// spending minutes on an image build that can only end in a generic error.
///
/// Messages are written to be matched by the deployment failure classifier
/// and to tell the user exactly what to change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildPlanFailure {
    /// The package manager would fail with "Missing script: build".
    #[error(
        "Build plan failed for preset '{preset}': package.json at '{package_json}' has no \"build\" \
         script, so the build would stop with \"Missing script: build\". Add a build script to \
         package.json (for a Vite app: \"build\": \"vite build\"), or set a custom build command \
         in the project's build settings."
    )]
    MissingBuildScript {
        preset: String,
        package_json: String,
    },

    /// Autopack detected nothing it can build, or could not work out how to
    /// start what it detected.
    #[error(
        "Build plan failed for preset '{preset}': autopack could not plan this application: \
         {reason}. Select the preset that matches the application in the project's build \
         settings, add a start command, or commit a Dockerfile."
    )]
    Unplannable { preset: String, reason: String },

    /// The repository's own configuration makes the selected application
    /// unbuildable (for example an invalid pnpm workspace member path).
    #[error("Build plan failed for preset '{preset}': invalid configuration: {reason}")]
    InvalidConfiguration { preset: String, reason: String },
}

/// Dockerfile content along with build arguments
#[derive(Debug, Clone)]
pub struct DockerfileWithArgs {
    /// The Dockerfile content
    pub content: String,
    /// Build arguments to pass to `docker build --build-arg KEY=VALUE`
    /// These are key-value pairs that will be available as ARG in the Dockerfile
    pub build_args: std::collections::HashMap<String, String>,
    /// Set when the preset already knows this build cannot succeed. The
    /// `content` is then a Dockerfile that fails with the same reason.
    pub plan_failure: Option<BuildPlanFailure>,
    /// Non-fatal findings the user should see in the deployment log, e.g.
    /// legacy `nixpacks.toml` settings that could not be translated.
    pub warnings: Vec<String>,
}

impl DockerfileWithArgs {
    /// Create a new DockerfileWithArgs with just content (no build args)
    pub fn new(content: String) -> Self {
        Self {
            content,
            build_args: std::collections::HashMap::new(),
            plan_failure: None,
            warnings: Vec::new(),
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
            plan_failure: None,
            warnings: Vec::new(),
        }
    }

    /// A Dockerfile that fails with `failure` when built, carrying the typed
    /// reason so the deployment pipeline can stop before building.
    pub fn failing(failure: BuildPlanFailure) -> Self {
        let message = failure.to_string();
        let single_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
        let content = format!(
            "# {}\nFROM debian:bookworm-slim\nRUN echo '{}' >&2 && exit 1\n",
            message.replace('\n', "\n# "),
            single_line.replace('\'', r"'\''")
        );
        Self {
            plan_failure: Some(failure),
            ..Self::new(content)
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
        Box::new(NixpacksPreset::new(NixpacksProvider::Swift)),
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
        for (name, args, allow_empty) in [
            ("command", config.runtime.command.as_ref(), false),
            ("entrypoint", config.runtime.entrypoint.as_ref(), true),
        ] {
            if let Some(args) = args {
                let invalid = if args.len() > 64 || (!allow_empty && args.is_empty()) {
                    Some("must contain 1–64 arguments (entrypoint also accepts [] to clear it)")
                } else if args.first().is_some_and(|arg| arg.trim().is_empty()) {
                    Some("first argument cannot be blank")
                } else if args
                    .iter()
                    .any(|arg| arg.len() > 1024 || arg.contains('\0'))
                {
                    Some("arguments must be at most 1024 bytes and contain no null characters")
                } else {
                    None
                };
                if let Some(reason) = invalid {
                    return Err(PresetResolutionError::InvalidConfig {
                        slug: PresetType::Dockerfile.as_str().to_string(),
                        reason: format!("container {name} {reason}"),
                    });
                }
            }
        }
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
        if command
            .iter()
            .any(|part| part.is_empty() || part.len() > 1_024 || part.chars().any(char::is_control))
        {
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

    // Server languages that commonly ship Vite only for their asset pipeline
    // (Laravel, Rails via vite_ruby). Offer the server preset, mirroring the
    // archive-upload detector.
    let file_named = |name: &str| {
        files
            .iter()
            .any(|path| path.rsplit('/').next().unwrap_or(path) == name)
    };
    let has_php = file_named("composer.json");
    let has_ruby = file_named("Gemfile");
    if has_php {
        presets.push(Box::new(NixpacksPreset::new(NixpacksProvider::Php)));
    }
    if has_ruby {
        presets.push(Box::new(NixpacksPreset::new(NixpacksProvider::Ruby)));
    }
    // Languages autopack builds with no preset of their own. Only explicit
    // manifests count here: this sees one directory at a time, so it cannot
    // tell a lone Deno `main.ts` from a Node app's `src/main.ts`.
    for (manifests, provider) in [
        (&["mix.exs"][..], NixpacksProvider::Elixir),
        (&["Package.swift"][..], NixpacksProvider::Swift),
        (
            &["deno.json", "deno.jsonc", "deno.lock"][..],
            NixpacksProvider::Deno,
        ),
    ] {
        if manifests.iter().any(|name| file_named(name)) {
            presets.push(Box::new(NixpacksPreset::new(provider)));
        }
    }

    // Check for Vite. A `vite.config.*` alone does not make a static site:
    // SvelteKit, React Router 7 framework mode, Remix, TanStack Start and
    // SolidStart all build with Vite and need a Node server. Only file names
    // are available here, so their own config files are the signal; when one
    // is present the app is offered as a Node (autopack) build, which detects
    // the framework from package.json at build time, instead of an nginx image
    // that would serve nothing useful.
    if files.iter().any(|path| is_vite_config_file(path)) {
        let server_framework_config = files.iter().any(|path| is_server_framework_config(path));
        if server_framework_config {
            presets.push(Box::new(NixpacksPreset::new(NixpacksProvider::Node)));
        } else if !has_php && !has_ruby {
            presets.push(Box::new(Vite));
        }
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

/// `vite.config.{js,ts,mjs,mts,cjs,cts}` — every extension Vite resolves.
fn is_vite_config_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_prefix("vite.config.")
        .is_some_and(|extension| matches!(extension, "js" | "ts" | "mjs" | "mts" | "cjs" | "cts"))
}

/// Config files of Vite-based frameworks that need a server at runtime:
/// SvelteKit (`svelte.config.*`), React Router 7 framework mode
/// (`react-router.config.*`), Remix (`remix.config.*`) and TanStack Start /
/// SolidStart (`app.config.*`).
fn is_server_framework_config(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    [
        "svelte.config.",
        "react-router.config.",
        "remix.config.",
        "app.config.",
    ]
    .iter()
    .any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|extension| {
            matches!(extension, "js" | "ts" | "mjs" | "mts" | "cjs" | "cts")
        })
    })
}

/// Directory segments whose compose files are never a deployment target:
/// development containers, examples, vendored or installed dependencies and
/// test fixtures.
const NON_DEPLOYABLE_COMPOSE_SEGMENTS: [&str; 12] = [
    "node_modules",
    ".devcontainer",
    ".git",
    "vendor",
    "example",
    "examples",
    "test",
    "tests",
    "__tests__",
    "fixtures",
    "__fixtures__",
    "testdata",
];

/// Whether compose files in `directory` (repository-relative, `""` for the
/// root) should be ignored by compose auto-detection.
pub fn is_non_deployable_compose_dir(directory: &str) -> bool {
    directory.split('/').any(|segment| {
        NON_DEPLOYABLE_COMPOSE_SEGMENTS.contains(&segment.to_ascii_lowercase().as_str())
    })
}

/// Order compose file paths so the first entry is the one to deploy by
/// default: shallowest directory first (root files before any subdirectory),
/// then Docker Compose's own precedence within a directory (`compose.yaml`,
/// `compose.yml`, `docker-compose.yaml`, `docker-compose.yml`), then path.
pub fn order_compose_files(mut paths: Vec<String>) -> Vec<String> {
    const PRECEDENCE: [&str; 4] = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ];
    paths.sort_by(|left, right| {
        let key = |path: &String| {
            let (directory, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
            let depth = if directory.is_empty() {
                0
            } else {
                directory.matches('/').count() + 1
            };
            let rank = PRECEDENCE
                .iter()
                .position(|candidate| *candidate == name)
                .unwrap_or(PRECEDENCE.len());
            (depth, directory.to_string(), rank)
        };
        key(left).cmp(&key(right)).then_with(|| left.cmp(right))
    });
    paths.dedup();
    paths
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
    /// Explicit build provider for languages without a standalone preset.
    pub build_provider: Option<NixpacksProvider>,
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
    /// Human-readable candidate label, including an explicitly selected language.
    pub fn label(&self) -> &'static str {
        match self.build_provider {
            Some(provider) => nixpacks_preset::provider_name(provider),
            None => self.preset.display_name(),
        }
    }

    /// Return the public preset catalog slug that can be passed to project
    /// creation for this detected candidate.
    ///
    /// Some canonical framework identifiers do not have a dedicated build
    /// preset yet. Those projects are still zero-config deployable through
    /// the matching Nixpacks provider.
    pub fn catalog_slug(&self) -> &'static str {
        if let Some(provider) = self.build_provider {
            return provider.variant_slug();
        }
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

/// Whether a normalized archive directory can contain a deployable project root.
/// ZIP inspection uses the same filter so dependency/build manifests consume no
/// project-manifest budget. Archive path and credential checks still apply.
pub fn is_project_candidate_directory(directory: &str) -> bool {
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

    !directory
        .split('/')
        .any(|segment| SKIP_SEGMENTS.contains(&segment))
        && (directory == "." || directory.split('/').count() <= MAX_ROOT_DEPTH)
}

/// Manifests of server languages that have no dedicated preset and build
/// through an explicit autopack provider. Each file is the one autopack's own
/// provider detects, so a candidate offered from it is one the build plans
/// rather than one it rejects. Listed in the order alternatives are offered.
const SERVER_LANGUAGE_MANIFESTS: [(&str, NixpacksProvider, &str); 7] = [
    (
        "composer.json",
        NixpacksProvider::Php,
        "PHP composer.json found (server preset)",
    ),
    (
        "Gemfile",
        NixpacksProvider::Ruby,
        "Ruby Gemfile found (server preset)",
    ),
    (
        "mix.exs",
        NixpacksProvider::Elixir,
        "Elixir mix.exs found (server preset)",
    ),
    (
        "Package.swift",
        NixpacksProvider::Swift,
        "Swift Package.swift found (server preset)",
    ),
    ("deno.json", NixpacksProvider::Deno, "Deno deno.json found"),
    (
        "deno.jsonc",
        NixpacksProvider::Deno,
        "Deno deno.jsonc found",
    ),
    ("deno.lock", NixpacksProvider::Deno, "Deno deno.lock found"),
];

/// Entrypoints autopack's Deno provider accepts without a `deno.json`.
const DENO_ENTRYPOINTS: [&str; 2] = ["main.ts", "mod.ts"];

/// Manifests of other ecosystems that stop autopack from reading a lone
/// `main.ts` as Deno. Mirrors autopack's own foreign-manifest list, so Drop
/// never offers a Deno build that autopack would then not plan.
const NON_DENO_ECOSYSTEM_MANIFESTS: [&str; 13] = [
    "package.json",
    "composer.json",
    "Gemfile",
    "go.mod",
    "Cargo.toml",
    "mix.exs",
    "gleam.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "requirements.txt",
    "pyproject.toml",
    "Pipfile",
];

/// Whether `name` on its own makes its directory an independently buildable
/// project, whether or not a Dockerfile also lives there. This distinguishes a
/// genuine monorepo service (its own Dockerfile *and* its own manifest, e.g.
/// `apps/api/Dockerfile` + `apps/api/package.json`) from a bare Dockerfile
/// conventionally tucked into `docker/` or `.devcontainer/`, whose `COPY`/`ADD`
/// instructions typically reach back to the real repository root.
pub fn is_project_manifest(name: &str) -> bool {
    PROJECT_MANIFESTS.contains(&name)
        || SERVER_LANGUAGE_MANIFESTS
            .iter()
            .any(|(manifest, _, _)| *manifest == name)
        || name.ends_with(".csproj")
        || name.starts_with("next.config.")
        || name.starts_with("vite.config.")
        || name.starts_with("astro.config.")
}

/// Project manifests other than the server-language ones above.
const PROJECT_MANIFESTS: [&str; 13] = [
    "package.json",
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
    "Cargo.toml",
    "go.mod",
    "requirements.txt",
    "pyproject.toml",
    "pom.xml",
    "build.gradle",
    "index.html",
    "nixpacks.toml",
];

/// Every file name that can make a directory deployable, for messages that
/// tell a user what to add when nothing was found.
pub fn project_signal_names() -> Vec<&'static str> {
    let mut names = vec!["Dockerfile"];
    names.extend(PROJECT_MANIFESTS);
    names.extend(
        SERVER_LANGUAGE_MANIFESTS
            .iter()
            .map(|(manifest, _, _)| *manifest),
    );
    names.extend(["*.csproj", "index.php", "main.ts (Deno)"]);
    names
}

/// A file that names a project's entrypoint rather than describing the project.
fn is_entrypoint_file(name: &str) -> bool {
    name == "index.php" || DENO_ENTRYPOINTS.contains(&name)
}

/// The project root an entrypoint in `directory` belongs to: a `public/`
/// holding `index.php` is the document root of a PHP application, not the
/// application.
fn entrypoint_root<'a>(directory: &'a str, names: &[&str]) -> &'a str {
    if !names.contains(&"index.php") {
        return directory;
    }
    match directory.rsplit_once('/') {
        Some((parent, "public")) => parent,
        None if directory == "public" => ".",
        _ => directory,
    }
}

/// Whether `directory` is a `public/` folder whose parent can be a project
/// root, so its `index.php` can root the application at that parent.
fn is_public_dir_of_candidate(directory: &str) -> bool {
    match directory.rsplit_once('/') {
        Some((parent, "public")) => is_project_candidate_directory(parent),
        None => directory == "public",
        Some(_) => false,
    }
}

/// Whether a static file server would hand out `path` as PHP source.
fn is_php_source(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    extension == "phtml"
        || extension == "phar"
        || extension
            .strip_prefix("php")
            .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit()))
}

/// Every directory whose tree contains PHP source, named the way the caller
/// names directories (`root` is the archive or repository root).
///
/// One pass, inserting each PHP file's ancestors, so checking a candidate is
/// a set lookup rather than a rescan of the file list per root.
fn directories_with_php_source<'a>(
    paths: impl Iterator<Item = &'a str>,
    root: &'a str,
) -> std::collections::HashSet<&'a str> {
    let mut directories = std::collections::HashSet::new();
    for path in paths.filter(|path| is_php_source(path)) {
        directories.insert(root);
        for (index, _) in path.match_indices('/') {
            directories.insert(&path[..index]);
        }
    }
    directories
}

fn directory_depth(directory: &str) -> usize {
    if directory == "." {
        0
    } else {
        directory.matches('/').count() + 1
    }
}

/// Whether `directory` is `root` or lies beneath it.
fn is_within(directory: &str, root: &str) -> bool {
    root == "."
        || directory == root
        || directory
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn offer_provider(
    candidates: &mut Vec<ProjectCandidate>,
    root: &str,
    provider: NixpacksProvider,
    confidence: &'static str,
    reason: &str,
) {
    if candidates
        .iter()
        .any(|candidate| candidate.build_provider == Some(provider))
    {
        return;
    }
    candidates.push(ProjectCandidate {
        build_provider: Some(provider),
        path: root.to_string(),
        preset: PresetType::Nixpacks,
        confidence,
        reason: reason.to_string(),
        dockerfile_path: None,
    });
}

/// Detect deployable project roots from normalized archive entries.
///
/// `files` maps slash-separated relative paths to the contents of small text
/// manifests. Binary and large files may be represented by an empty string.
pub fn detect_project_candidates(
    files: &std::collections::BTreeMap<String, String>,
) -> Vec<ProjectCandidate> {
    use std::collections::{BTreeMap, BTreeSet};

    // Index every path by its directory ONCE. The previous implementation
    // rescanned all of `files` for each root, which is O(roots x files) — a
    // 20k-entry archive turned into ~4x10^8 string comparisons per request.
    let mut by_directory: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in files.keys() {
        let (directory, name) = match path.rsplit_once('/') {
            Some((directory, name)) => (directory, name),
            None => (".", path.as_str()),
        };
        // A `public/` document root may sit one level below the depth cap
        // when its application root is at the cap; it is indexed so that
        // root can be found, but never becomes a root itself (below).
        if !is_project_candidate_directory(directory)
            && !(name == "index.php" && is_public_dir_of_candidate(directory))
        {
            continue;
        }
        by_directory.entry(directory).or_default().push(name);
    }
    // Directories whose tree holds PHP source. Built from every archive path,
    // including ones detection itself skips (`vendor/`, deep folders), since
    // a static deployment publishes all of them.
    let php_source_dirs = directories_with_php_source(files.keys().map(String::as_str), ".");

    let has_independent_manifest =
        |names: &[&str]| names.iter().any(|name| is_project_manifest(name));

    let mut roots = BTreeSet::new();
    // Subdirectories whose only signal is a bare `Dockerfile`, in a
    // directory conventionally used to hold one — not promoted to their own
    // project root. Surfaced as a build option on the repository root
    // instead (see below).
    let mut orphan_dockerfile_dirs: Vec<&str> = Vec::new();
    for (directory, names) in &by_directory {
        if !is_project_candidate_directory(directory) {
            continue;
        }
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

    // An entrypoint file (`index.php`, a lone `main.ts`) is how a project
    // without a manifest announces itself, but the same files appear all over
    // projects that have one: WordPress puts an `index.php` in nearly every
    // directory, and a Vite app has `src/main.ts`. Such a file only starts a
    // project when no enclosing directory already is one. Shallowest first,
    // so an accepted entrypoint root also claims the entrypoints beneath it.
    let mut entrypoint_roots: Vec<&str> = by_directory
        .iter()
        .filter(|(_, names)| names.iter().any(|name| is_entrypoint_file(name)))
        .map(|(directory, names)| entrypoint_root(directory, names))
        .collect();
    entrypoint_roots.sort_by_key(|directory| (directory_depth(directory), *directory));
    for directory in entrypoint_roots {
        if is_project_candidate_directory(directory)
            && !roots.iter().any(|root| is_within(directory, root))
        {
            roots.insert(directory);
        }
    }

    // Rsbuild's src/index.html is an input template. Claim that conventional
    // source directory from its enclosing buildable app; independent nested
    // manifests and sibling static sites still receive their own candidates.
    let rsbuild_roots: BTreeSet<&str> = roots
        .iter()
        .copied()
        .filter(|root| {
            let manifest = if *root == "." {
                "package.json".to_string()
            } else {
                format!("{root}/package.json")
            };
            files
                .get(&manifest)
                .and_then(|contents| detect_package_json_preset(contents))
                .is_some_and(|(preset, _, _)| preset == PresetType::Rsbuild)
        })
        .collect();
    roots.retain(|root| {
        let names = by_directory.get(root).map(Vec::as_slice).unwrap_or(&[]);
        if !names.contains(&"index.html")
            || names
                .iter()
                .any(|name| *name != "index.html" && is_project_manifest(name))
            || names.contains(&"Dockerfile")
            || names.iter().any(|name| is_entrypoint_file(name))
        {
            return true;
        }
        // Only the conventional direct src/index.html is claimed. Descendant
        // sites can belong to independent apps, including public/ document roots.
        let (parent, name) = root.rsplit_once('/').unwrap_or((".", *root));
        !(name == "src" && rsbuild_roots.contains(parent))
    });

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
        let has_extension = |extension: &str| names.iter().any(|name| name.ends_with(extension));

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

        let explicit_docker = detected.as_ref().is_some_and(|(preset, _, _)| {
            matches!(preset, PresetType::DockerCompose | PresetType::Dockerfile)
        });
        // Preserve specific framework detection when another language manifest
        // is only tooling. Offer each language independently rather than force
        // a single provider on an ambiguous directory.
        let mut root_candidates = Vec::new();
        if let Some((preset, confidence, reason)) = detected {
            root_candidates.push(ProjectCandidate {
                build_provider: None,
                path: root.to_string(),
                preset,
                confidence,
                reason,
                dockerfile_path: None,
            });
        }
        if !explicit_docker {
            let has_public_index_php = by_directory
                .get(at_root("public").as_str())
                .is_some_and(|names| names.contains(&"index.php"));
            for (manifest, provider, reason) in SERVER_LANGUAGE_MANIFESTS {
                if has(manifest) {
                    offer_provider(&mut root_candidates, root, provider, "high", reason);
                }
            }
            // Plain PHP needs no Composer: autopack serves `index.php` (or
            // `public/index.php`) through FrankenPHP. The static preset would
            // publish PHP source anywhere in its tree as text (`index.html`
            // beside `api/index.php`), so it is never offered for such a tree;
            // the PHP build serves the HTML and runs the PHP instead.
            if php_source_dirs.contains(root)
                && root_candidates
                    .iter()
                    .any(|candidate| candidate.preset == PresetType::Static)
            {
                root_candidates.retain(|candidate| candidate.preset != PresetType::Static);
                offer_provider(
                    &mut root_candidates,
                    root,
                    NixpacksProvider::Php,
                    "high",
                    "PHP source found beside the HTML (server preset, so it runs \
                     instead of being published as text)",
                );
            }
            if has("index.php") {
                offer_provider(
                    &mut root_candidates,
                    root,
                    NixpacksProvider::Php,
                    "high",
                    "PHP index.php found (server preset)",
                );
            } else if has_public_index_php {
                offer_provider(
                    &mut root_candidates,
                    root,
                    NixpacksProvider::Php,
                    "high",
                    "PHP public/index.php found (server preset)",
                );
            }
            // A lone `main.ts`/`mod.ts` is Deno's convention, and it is the
            // rule autopack applies: only when no other ecosystem's manifest
            // is present, since Node projects have a `main.ts` too.
            if DENO_ENTRYPOINTS.iter().any(|name| has(name))
                && !NON_DENO_ECOSYSTEM_MANIFESTS.iter().any(|name| has(name))
            {
                offer_provider(
                    &mut root_candidates,
                    root,
                    NixpacksProvider::Deno,
                    "medium",
                    "Deno entrypoint found with no other manifest; add deno.json \
                     with a `start` task to choose the start command explicitly",
                );
            }
            // An explicit Nixpacks build plan with no language Temps can name
            // still builds: autopack reads it and detects the provider.
            if root_candidates.is_empty() && has("nixpacks.toml") {
                root_candidates.push(ProjectCandidate {
                    build_provider: None,
                    path: root.to_string(),
                    preset: PresetType::Nixpacks,
                    confidence: "medium",
                    reason: "nixpacks.toml found (language detected at build time)".to_string(),
                    dockerfile_path: None,
                });
            }
            // Vite assets and a generic JS manifest are common in server apps.
            // Prefer the server language, while keeping the JS option available.
            root_candidates.sort_by_key(|candidate| {
                candidate.preset == PresetType::Vite
                    || candidate.preset == PresetType::Static
                    || (candidate.preset == PresetType::NodeJs && candidate.confidence == "medium")
            });
        }
        candidates.extend(root_candidates);
    }

    // Every orphaned Dockerfile becomes a build option rooted at the
    // repository root, not at the subdirectory it was found in — so the
    // default build context stays the root the Dockerfile's own COPY/ADD
    // paths almost always assume.
    orphan_dockerfile_dirs.sort_unstable();
    for dir in orphan_dockerfile_dirs {
        candidates.push(ProjectCandidate {
            build_provider: None,
            path: ".".to_string(),
            preset: PresetType::Dockerfile,
            confidence: "medium",
            reason: format!(
                "Dockerfile found in {dir}/ (build context defaults to the repository root)"
            ),
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
    } else if has_dependency("@tanstack/react-start") {
        (PresetType::NodeJs, "@tanstack/react-start")
    } else if has_dependency("@tanstack/solid-start") {
        (PresetType::NodeJs, "@tanstack/solid-start")
    } else if has_dependency("@solidjs/start") {
        (PresetType::SolidStart, "@solidjs/start")
    } else if has_dependency("@react-router/dev") {
        // React Router 7 framework mode builds a server bundle by default.
        (PresetType::NodeJs, "@react-router/dev")
    } else if has_dependency("@builder.io/qwik-city") {
        (PresetType::NodeJs, "@builder.io/qwik-city")
    } else if has_dependency("@rsbuild/core") {
        (PresetType::Rsbuild, "@rsbuild/core")
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

    let php_source_dirs = directories_with_php_source(files.iter().map(String::as_str), "");
    let js_tree = JsTree::new(&directory_files);

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

        let mut detected = detect_all_presets_from_files(dir_files);
        // A static deployment of this directory would publish the PHP source
        // in its tree as text; build it with PHP instead.
        if php_source_dirs.contains(dir.as_str())
            && detected
                .iter()
                .any(|preset| preset.slug() == "nixpacks-static")
        {
            detected.retain(|preset| preset.slug() != "nixpacks-static");
            detected.push(Box::new(NixpacksPreset::new(NixpacksProvider::Php)));
        }
        // An `index.html` beside a bundler config, in the `src/` under one,
        // or in a bundler's or JavaScript app's `public/` is an input the build
        // fills in or the server serves. Publishing it as-is serves a page with
        // no bundle.
        if js_tree.is_build_input_dir(dir) {
            detected.retain(|preset| preset.slug() != "nixpacks-static");
        }
        // A `package.json` no framework claimed is still something autopack
        // builds: an Express server, or a webpack bundle served as static files.
        if detected.is_empty()
            && !php_source_dirs.contains(dir.as_str())
            && js_tree.offers_generic_node(dir)
        {
            detected.push(Box::new(NixpacksPreset::new(NixpacksProvider::Node)));
        }
        // A compose file under `.devcontainer/`, `examples/`, a test fixture
        // directory or vendored code describes a development or sample
        // environment, not this repository's deployment.
        if is_non_deployable_compose_dir(dir) {
            detected.retain(|preset| preset.slug() != "docker-compose");
        }
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
                    if is_non_deployable_compose_dir(d) {
                        continue;
                    }
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
                // The console pre-selects the first entry, so the order is
                // the default deployment target: root files first, by
                // Compose's own name precedence, then shallower directories.
                Some(order_compose_files(files_found))
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

/// Config files of bundlers that build an app from an `index.html` template.
const BUNDLER_CONFIG_PREFIXES: [&str; 4] = [
    "webpack.config.",
    "rspack.config.",
    "rsbuild.config.",
    "vite.config.",
];

/// Files whose presence marks a JavaScript workspace root rather than an app.
const JS_WORKSPACE_MARKERS: [&str; 5] = [
    "pnpm-workspace.yaml",
    "lerna.json",
    "turbo.json",
    "rush.json",
    "nx.json",
];

/// Workspace directories that by convention hold shared libraries, not apps.
const JS_LIBRARY_DIRS: [&str; 2] = ["packages", "libs"];

/// Directory names that hold tooling with its own `package.json` — browser
/// tests, infrastructure code, scripts, documentation — not a deployable app.
const JS_TOOLING_DIRS: [&str; 10] = [
    "e2e",
    "cypress",
    "playwright",
    "infra",
    "infrastructure",
    "cdk",
    "scripts",
    "tools",
    "docs",
    "website",
];

/// Files that make a directory tooling or another ecosystem's project even
/// when it has a `package.json`: CDK apps, browser-test suites, Django
/// (`manage.py`), Hugo sites.
const NON_NODE_APP_MARKERS: [&str; 7] = [
    "cdk.json",
    "manage.py",
    "hugo.toml",
    "hugo.yaml",
    "hugo.json",
    "playwright.config.",
    "cypress.config.",
];

/// The last segment of a slash-separated path.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `name` inside `dir`, where the repository root is the empty string.
fn child_dir(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The directory containing `dir`, where the repository root is the empty
/// string and has no parent.
fn parent_dir(dir: &str) -> Option<&str> {
    if dir.is_empty() {
        return None;
    }
    Some(dir.rsplit_once('/').map_or("", |(parent, _)| parent))
}

/// JavaScript or TypeScript source, as opposed to tooling around it: config
/// (`eslint.config.js`, `.prettierrc.js`), type declarations, test setup,
/// presets, workspace definitions and tests.
fn is_js_source_file(name: &str) -> bool {
    let is_script = [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts", ".jsx", ".tsx"]
        .iter()
        .any(|extension| name.ends_with(extension));
    let is_tooling = name.starts_with('.')
        || name.ends_with(".d.ts")
        || [
            ".config.",
            ".setup.",
            ".preset.",
            ".workspace.",
            ".test.",
            ".spec.",
        ]
        .iter()
        .any(|infix| name.contains(infix));
    is_script && !is_tooling
}

/// Whether a directory can never hold a deployable JavaScript app, wherever
/// it sits: dependencies, hidden directories, tests, examples and vendored
/// code.
fn is_ignored_js_dir(dir: &str) -> bool {
    is_non_deployable_compose_dir(dir)
        || dir
            .split('/')
            .any(|segment| segment.starts_with('.') || segment == "node_modules")
}

/// What JavaScript preset detection needs to know about a whole file tree,
/// computed once per request so per-directory checks are lookups.
struct JsTree<'a> {
    directory_files: &'a std::collections::HashMap<String, Vec<String>>,
    /// Directories with a `package.json` of their own.
    package_dirs: std::collections::HashSet<&'a str>,
    /// Directories with a `package.json` in a deployable directory beneath:
    /// tooling packages (`e2e/`, `docs/`) do not count.
    has_nested_package: std::collections::HashSet<&'a str>,
    /// Directories with JavaScript source somewhere beneath them that
    /// belongs to them rather than to a nested package, ignoring `public/`
    /// assets and ignored directories.
    has_own_source_below: std::collections::HashSet<&'a str>,
}

impl<'a> JsTree<'a> {
    /// Index `directory_files`, the repository's files grouped by directory.
    fn new(directory_files: &'a std::collections::HashMap<String, Vec<String>>) -> Self {
        let package_dirs: std::collections::HashSet<&str> = directory_files
            .iter()
            .filter(|(_, files)| files.iter().any(|path| file_name(path) == "package.json"))
            .map(|(dir, _)| dir.as_str())
            .collect();
        let mut tree = Self {
            directory_files,
            package_dirs,
            has_nested_package: std::collections::HashSet::new(),
            has_own_source_below: std::collections::HashSet::new(),
        };

        let mut has_nested_package = std::collections::HashSet::new();
        for dir in tree.package_dirs.iter().copied() {
            if dir.is_empty() || is_ignored_js_dir(dir) || tree.is_tooling_dir(dir) {
                continue;
            }
            let mut ancestor = parent_dir(dir);
            while let Some(current) = ancestor {
                has_nested_package.insert(current);
                ancestor = parent_dir(current);
            }
        }

        // Each source file marks its directory and the ancestors up to, and
        // including, the nearest package that owns it. Source in a tooling
        // directory of that package (`scripts/`, `e2e/`) is not the app's.
        let mut has_own_source_below = std::collections::HashSet::new();
        for (dir, files) in directory_files {
            let dir = dir.as_str();
            if is_ignored_js_dir(dir)
                || dir.split('/').any(|segment| segment == "public")
                || tree.is_tooling_source_of_owner(dir)
                || !files.iter().any(|path| is_js_source_file(file_name(path)))
            {
                continue;
            }
            let mut current = Some(dir);
            while let Some(directory) = current {
                if !has_own_source_below.insert(directory) || tree.package_dirs.contains(directory) {
                    break;
                }
                current = parent_dir(directory);
            }
        }

        tree.has_nested_package = has_nested_package;
        tree.has_own_source_below = has_own_source_below;
        tree
    }

    /// Whether any file directly in `dir` satisfies `predicate` by name.
    fn has_file(&self, dir: &str, predicate: impl Fn(&str) -> bool) -> bool {
        self.directory_files
            .get(dir)
            .is_some_and(|files| files.iter().any(|path| predicate(file_name(path))))
    }

    /// Whether `dir` holds a webpack, rspack, rsbuild or Vite config.
    fn has_bundler_config(&self, dir: &str) -> bool {
        self.has_file(dir, |name| {
            BUNDLER_CONFIG_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
    }

    /// Whether `dir` holds tooling rather than an app: browser tests,
    /// infrastructure code, scripts or documentation, by name or by marker.
    fn is_tooling_dir(&self, dir: &str) -> bool {
        dir.split('/')
            .any(|segment| JS_TOOLING_DIRS.contains(&segment))
            || self.has_file(dir, |name| {
                NON_NODE_APP_MARKERS.iter().any(|marker| {
                    name == *marker || (marker.ends_with('.') && name.starts_with(marker))
                })
            })
    }

    /// Whether `dir` sits in a tooling directory of the package that owns it,
    /// the nearest package at or above it (the repository root when none):
    /// `scripts/build.js` belongs to the root's tooling, while a package that
    /// itself lives under `website/` still owns its `src/`.
    fn is_tooling_source_of_owner(&self, dir: &str) -> bool {
        let mut owner = Some(dir);
        while let Some(current) = owner {
            if self.package_dirs.contains(current) {
                break;
            }
            owner = parent_dir(current);
        }
        let relative = match owner {
            Some("") | None => dir,
            Some(owner) => dir
                .strip_prefix(owner)
                .map_or(dir, |rest| rest.trim_start_matches('/')),
        };
        !relative.is_empty()
            && relative
                .split('/')
                .any(|segment| JS_TOOLING_DIRS.contains(&segment))
    }

    /// Whether the package in `dir` has the source of a runnable app: a
    /// source file at its top level, or source of its own under `src/` or
    /// under a `server/` that is not a package itself. Code elsewhere —
    /// build scripts, serverless functions in `api/`, helpers in `lib/` —
    /// does not make a package an app.
    fn has_app_source(&self, dir: &str) -> bool {
        let server_dir = child_dir(dir, "server");
        self.has_file(dir, is_js_source_file)
            || self.has_own_source_below.contains(child_dir(dir, "src").as_str())
            || (!self.package_dirs.contains(server_dir.as_str())
                && self.has_own_source_below.contains(server_dir.as_str()))
    }

    /// Whether `dir`'s `index.html` is a build input rather than a site: it
    /// sits beside a bundler config, in the `src/` directly beneath one, or in
    /// the `public/` of a bundler config or a JavaScript app. Create React App
    /// and Vue CLI hide their bundler config and fill `public/index.html` in
    /// at build time; an Express app serves `public/` beside its API.
    /// `public/` under a `package.json` with no app source is the site.
    fn is_build_input_dir(&self, dir: &str) -> bool {
        if self.has_bundler_config(dir) {
            return true;
        }
        let (parent, name) = dir.rsplit_once('/').unwrap_or(("", dir));
        match name {
            "src" => self.has_bundler_config(parent),
            "public" => {
                self.has_bundler_config(parent)
                    || (self.package_dirs.contains(parent) && self.has_app_source(parent))
            }
            _ => false,
        }
    }

    /// Whether `dir` is a JavaScript workspace root: it declares one with a
    /// marker file, or it has a `package.json`, nested packages and no app
    /// source of its own. An npm or yarn workspace is declared inside
    /// `package.json`, which a file-name listing cannot read, hence the
    /// second rule.
    fn is_workspace_root(&self, dir: &str) -> bool {
        if self.has_file(dir, |name| JS_WORKSPACE_MARKERS.contains(&name)) {
            return true;
        }
        self.package_dirs.contains(dir)
            && self.has_nested_package.contains(dir)
            && !self.has_app_source(dir)
    }

    /// Whether to offer a Node build for a directory no framework claimed.
    ///
    /// Needs a `package.json` and JavaScript source of its own, and skips
    /// what is not a deployable Node app: workspace roots (which would be the
    /// pre-selected card and fail with no start command), shared libraries
    /// under a workspace's `packages/` or `libs/`, tooling (tests,
    /// infrastructure, scripts, docs), another ecosystem's project that uses
    /// npm for assets, and ignored directories. A `package.json` beside only
    /// static files is tooling for a static site, which keeps its own card;
    /// so is one whose only source is outside the app layout (see
    /// `has_app_source`), such as serverless functions in `api/`, when a
    /// `public/index.html` is the site.
    fn offers_generic_node(&self, dir: &str) -> bool {
        if !self.package_dirs.contains(dir)
            || !self.has_own_source_below.contains(dir)
            || (!self.has_app_source(dir)
                && self.has_file(&child_dir(dir, "public"), |name| name == "index.html"))
            || is_ignored_js_dir(dir)
            || self.is_tooling_dir(dir)
            || self
                .directory_files
                .contains_key(&child_dir(dir, "requirements"))
            || self.is_workspace_root(dir)
        {
            return false;
        }
        let Some((parent, _)) = dir.rsplit_once('/') else {
            return true;
        };
        if !JS_LIBRARY_DIRS.contains(&dir_basename(parent)) {
            return true;
        }
        let mut ancestors =
            std::iter::once("").chain(dir.match_indices('/').map(|(index, _)| &dir[..index]));
        !ancestors.any(|ancestor| self.is_workspace_root(ancestor))
    }
}

#[cfg(test)]
mod file_tree_detection_tests {
    use super::detect_presets_from_file_tree;

    /// Detected `(path, slug)` pairs for a list of repository paths.
    fn detect(files: &[&str]) -> Vec<(String, String)> {
        let files: Vec<String> = files.iter().map(|file| file.to_string()).collect();
        detect_presets_from_file_tree(&files)
            .into_iter()
            .map(|preset| (preset.path, preset.slug))
            .collect()
    }

    /// An expected `(path, slug)` pair.
    fn pair(path: &str, slug: &str) -> (String, String) {
        (path.to_string(), slug.to_string())
    }

    /// One Node card per app and service; no root, library or template card.
    #[test]
    fn npm_workspace_of_webpack_apps_and_express_services() {
        // npm workspaces declared inside the root package.json: webpack apps
        // whose index.html is a template in src/, Express services, a shared
        // library, and a development-only compose file.
        let presets = detect(&[
            "package.json",
            "package-lock.json",
            "README.md",
            "apps/web/package.json",
            "apps/web/webpack.config.js",
            "apps/web/src/index.html",
            "apps/web/src/bootstrap.tsx",
            "apps/admin/package.json",
            "apps/admin/webpack.config.js",
            "apps/admin/src/index.html",
            "apps/admin/src/index.ts",
            "services/orders-api/package.json",
            "services/orders-api/src/index.mjs",
            "services/users-api/package.json",
            "services/users-api/src/index.mjs",
            "packages/shared/package.json",
            "packages/shared/src/index.ts",
            "infra/docker-compose.yml",
            "infra/seed/seed.mjs",
        ]);

        assert_eq!(
            presets,
            vec![
                pair("apps/admin", "nixpacks-node"),
                pair("apps/web", "nixpacks-node"),
                pair("infra", "docker-compose"),
                pair("services/orders-api", "nixpacks-node"),
                pair("services/users-api", "nixpacks-node"),
            ]
        );
    }

    /// A package.json with source and no framework config is a Node app.
    #[test]
    fn package_json_alone_is_a_node_app() {
        assert_eq!(
            detect(&["package.json", "server.js"]),
            vec![pair("./", "nixpacks-node")]
        );
    }

    /// A framework or another language's preset is not joined by a Node card.
    #[test]
    fn framework_presets_are_not_doubled_with_generic_node() {
        assert_eq!(
            detect(&["package.json", "vite.config.ts", "index.html", "src/main.ts"]),
            vec![pair("./", "vite")]
        );
        // Another language owns a directory whose package.json is tooling.
        assert_eq!(
            detect(&["composer.json", "package.json", "artisan"]),
            vec![pair("./", "nixpacks-php")]
        );
    }

    /// A bundler's index.html template is not offered as static files.
    #[test]
    fn index_html_beside_a_bundler_config_is_a_template_not_a_site() {
        assert_eq!(
            detect(&["package.json", "webpack.config.js", "index.html", "src/app.js"]),
            vec![pair("./", "nixpacks-node")]
        );
        assert_eq!(
            detect(&[
                "package.json",
                "rspack.config.ts",
                "src/index.html",
                "src/index.ts"
            ]),
            vec![pair("./", "nixpacks-node")]
        );
    }

    /// With nothing building it, src/ is the site.
    #[test]
    fn src_index_html_without_a_bundler_stays_a_static_site() {
        // Nothing builds src/, so it is the site itself.
        assert_eq!(
            detect(&["README.md", "src/index.html", "src/style.css"]),
            vec![pair("src", "nixpacks-static")]
        );
    }

    /// Declared and undeclared workspace roots get no card of their own.
    #[test]
    fn workspace_roots_are_not_offered_as_apps() {
        // Declared by a marker file, even with source of its own.
        assert_eq!(
            detect(&[
                "package.json",
                "pnpm-workspace.yaml",
                "scripts/release.ts",
                "apps/api/package.json",
                "apps/api/src/index.ts",
            ]),
            vec![pair("apps/api", "nixpacks-node")]
        );
        // Undeclared: nested packages and no source of its own. Tooling
        // config at the root is not source.
        assert_eq!(
            detect(&[
                "package.json",
                "eslint.config.js",
                "apps/api/package.json",
                "apps/api/index.js",
            ]),
            vec![pair("apps/api", "nixpacks-node")]
        );
    }

    /// A root with its own source stays an app beside a nested package.
    #[test]
    fn an_app_with_a_nested_package_is_still_an_app() {
        // An Express server at the root with its own client package below.
        assert_eq!(
            detect(&[
                "package.json",
                "server.js",
                "client/package.json",
                "client/src/index.js",
            ]),
            vec![pair("./", "nixpacks-node"), pair("client", "nixpacks-node")]
        );
    }

    /// Only the app gets a card in a workspace full of non-apps.
    #[test]
    fn workspace_libraries_tests_and_hidden_directories_are_skipped() {
        assert_eq!(
            detect(&[
                "package.json",
                "turbo.json",
                "apps/site/package.json",
                "apps/site/index.js",
                "packages/ui/package.json",
                "packages/ui/src/index.ts",
                "libs/utils/package.json",
                "libs/utils/index.js",
                "examples/demo/package.json",
                "examples/demo/index.js",
                "tests/e2e/package.json",
                "tests/e2e/run.js",
                ".config/tool/package.json",
                ".config/tool/index.js",
            ]),
            vec![pair("apps/site", "nixpacks-node")]
        );
    }

    /// Nx roots and tooling files at a root do not make it an app.
    #[test]
    fn nx_and_tooling_files_do_not_make_a_workspace_root_an_app() {
        assert_eq!(
            detect(&[
                "package.json",
                "nx.json",
                "jest.preset.js",
                "apps/web/vite.config.ts",
                "apps/web/index.html",
            ]),
            vec![pair("apps/web", "vite")]
        );
        // Type declarations, test setup and workspace files are not source.
        assert_eq!(
            detect(&[
                "package.json",
                "global.d.ts",
                "jest.setup.js",
                "vitest.workspace.ts",
                "apps/api/package.json",
                "apps/api/src/index.ts",
                "packages/ui/package.json",
                "packages/ui/src/index.ts",
            ]),
            vec![pair("apps/api", "nixpacks-node")]
        );
    }

    /// Browser tests and infrastructure code never displace the real app.
    #[test]
    fn tooling_directories_are_not_offered_as_apps() {
        assert_eq!(
            detect(&[
                "web/package.json",
                "web/Dockerfile",
                "e2e/package.json",
                "e2e/playwright.config.ts",
                "e2e/tests/a.spec.ts",
                "browser-tests/package.json",
                "browser-tests/cypress.config.ts",
                "scripts/package.json",
                "scripts/release.mjs",
                "tools/codegen/package.json",
                "tools/codegen/index.js",
                "docs/package.json",
                "docs/index.md",
            ]),
            vec![pair("web", "dockerfile")]
        );
        assert_eq!(
            detect(&[
                "server/go.mod",
                "server/main.go",
                "deploy/package.json",
                "deploy/cdk.json",
                "deploy/bin/app.ts",
            ]),
            vec![pair("server", "go")]
        );
    }

    /// PHP, Django and Hugo projects that use npm for assets get no Node
    /// card, even with JavaScript source of their own; so does a project
    /// whose only Python marker is a `requirements/` directory.
    #[test]
    fn another_ecosystem_using_npm_for_assets_gets_no_node_card() {
        for files in [
            &["index.php", "package.json", "tailwind.config.js", "assets/app.js"][..],
            &["manage.py", "requirements/base.txt", "package.json", "assets/app.js"][..],
            &["requirements/base.txt", "package.json", "assets/app.js"][..],
            &["hugo.toml", "package.json", "content/_index.md", "assets/app.js"][..],
        ] {
            assert!(
                !detect(files)
                    .iter()
                    .any(|(_, slug)| slug == "nixpacks-node"),
                "{files:?}"
            );
        }
    }

    /// A bundler app's public/ is a build input; a lone public/ is a site.
    #[test]
    fn public_assets_of_a_javascript_app_are_not_a_static_site() {
        assert_eq!(
            detect(&[
                "package.json",
                "webpack.config.js",
                "public/index.html",
                "src/index.js",
            ]),
            vec![pair("./", "nixpacks-node")]
        );
        // Without a package.json above it, public/ is the site itself.
        assert_eq!(
            detect(&["public/index.html", "README.md"]),
            vec![pair("public", "nixpacks-static")]
        );
    }

    /// Fixtures and installed dependencies are not nested packages.
    #[test]
    fn fixtures_and_dependencies_do_not_make_an_app_a_workspace_root() {
        assert_eq!(
            detect(&[
                "package.json",
                "lib/app.js",
                "test/fixtures/x/package.json",
                "node_modules/dep/package.json",
            ]),
            vec![pair("./", "nixpacks-node")]
        );
    }

    /// A JavaScript app's public/ is not a separate site even with no
    /// bundler config in sight: Create React App and Vue CLI hide theirs, and
    /// an Express app serves public/ beside its API.
    #[test]
    fn public_of_a_javascript_app_without_a_bundler_config_is_not_a_site() {
        for files in [
            &["package.json", "public/index.html", "src/index.js", "src/App.js"][..],
            &["package.json", "server.js", "public/index.html"][..],
            &["package.json", "vue.config.js", "public/index.html", "src/main.js"][..],
        ] {
            assert_eq!(detect(files), vec![pair("./", "nixpacks-node")], "{files:?}");
        }
    }

    /// A static site with a tooling package.json keeps its static card.
    #[test]
    fn a_static_site_using_npm_for_tooling_keeps_its_static_card() {
        // package.json only carries devDependencies such as a formatter; the
        // site is public/, and there is no JavaScript to build or run.
        assert_eq!(
            detect(&["package.json", "public/index.html", "public/style.css"]),
            vec![pair("public", "nixpacks-static")]
        );
        assert_eq!(
            detect(&[
                "package.json",
                "pnpm-workspace.yaml",
                "public/index.html",
                "public/style.css",
            ]),
            vec![pair("public", "nixpacks-static")]
        );
    }

    /// Build scripts beside a static site are tooling, not the site's own
    /// source: no Node card appears, and public/ keeps its static card. A
    /// package that lives under a tooling-named directory still owns its src/.
    #[test]
    fn tooling_scripts_are_not_a_static_sites_own_source() {
        for scripts in [
            &["scripts/build.js"][..],
            &["tools/minify.mjs", "e2e/home.spec.ts", "e2e/helpers.ts"][..],
        ] {
            let mut files = vec!["package.json", "public/index.html", "public/style.css"];
            files.extend_from_slice(scripts);
            assert_eq!(
                detect(&files),
                vec![pair("public", "nixpacks-static")],
                "{files:?}"
            );
        }
        assert_eq!(
            detect(&[
                "website/package.json",
                "website/public/index.html",
                "website/src/index.js",
            ]),
            Vec::<(String, String)>::new()
        );
    }

    /// Serverless functions in api/ or helpers in lib/ beside a public/ site
    /// are not an app: public/ keeps its static card and no Node card with
    /// no start command appears. Source in src/ or server/ is an app.
    #[test]
    fn code_outside_the_app_layout_keeps_public_a_static_site() {
        for code in [&["api/hello.js"][..], &["lib/analytics.js", "lib/util/format.mjs"][..]] {
            let mut files = vec!["package.json", "public/index.html", "public/style.css"];
            files.extend_from_slice(code);
            assert_eq!(
                detect(&files),
                vec![pair("public", "nixpacks-static")],
                "{files:?}"
            );
        }
        for app in [&["server/index.js"][..], &["src/app/main.js"][..]] {
            let mut files = vec!["package.json", "public/index.html", "api/hello.js"];
            files.extend_from_slice(app);
            assert_eq!(detect(&files), vec![pair("./", "nixpacks-node")], "{files:?}");
        }
    }

    /// Source nested under src/ makes a package an app, not a workspace
    /// root, even with a nested package beside it.
    #[test]
    fn nested_src_source_is_not_a_workspace_root() {
        assert_eq!(
            detect(&[
                "package.json",
                "src/app/main.js",
                "client/package.json",
                "client/src/index.js",
            ]),
            vec![pair("./", "nixpacks-node"), pair("client", "nixpacks-node")]
        );
    }

    /// An e2e or docs package does not hide the app above it.
    #[test]
    fn tooling_packages_do_not_turn_an_app_into_a_workspace_root() {
        for tooling in [
            &["e2e/package.json", "e2e/playwright.config.ts"][..],
            &["docs/package.json", "docs/index.md"][..],
        ] {
            let mut files = vec!["package.json", "server/index.js"];
            files.extend_from_slice(tooling);
            assert_eq!(
                detect(&files),
                vec![pair("./", "nixpacks-node")],
                "{tooling:?}"
            );
        }
    }

    /// Source in a non-package server/ belongs to the root app.
    #[test]
    fn server_source_with_a_nested_client_package_is_an_app() {
        assert_eq!(
            detect(&[
                "package.json",
                "server/index.js",
                "client/package.json",
                "client/src/index.js",
            ]),
            vec![pair("./", "nixpacks-node"), pair("client", "nixpacks-node")]
        );
    }

    /// A server/ with its own package.json is a workspace member.
    #[test]
    fn a_server_member_package_keeps_the_root_a_workspace_root() {
        assert_eq!(
            detect(&[
                "package.json",
                "server/package.json",
                "server/index.js",
                "client/package.json",
                "client/src/index.js",
            ]),
            vec![pair("client", "nixpacks-node"), pair("server", "nixpacks-node")]
        );
    }

    /// A package.json alone, with no source, offers nothing.
    #[test]
    fn a_package_json_with_no_javascript_is_not_a_node_app() {
        assert_eq!(detect(&["package.json", "README.md"]), vec![]);
    }

    /// Without a workspace above it, packages/ is just a folder name.
    #[test]
    fn a_packages_directory_outside_a_workspace_holds_apps() {
        // Without a workspace root above it, `packages/` is just a folder name.
        assert_eq!(
            detect(&["packages/api/package.json", "packages/api/index.js"]),
            vec![pair("packages/api", "nixpacks-node")]
        );
    }
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
    fn rsbuild_is_a_buildable_static_preset_and_source_html_is_not_a_candidate() {
        for prefix in ["", "apps/web/"] {
            let package = format!("{prefix}package.json");
            let source = format!("{prefix}src/index.html");
            let mut files = BTreeMap::from([(package, r#"{"scripts":{"build":"rsbuild build"},"devDependencies":{"@rsbuild/core":"1.3.22","@rsbuild/plugin-react":"1.3.2"}}"#.to_string()), (source, r#"<div id="root"></div>"#.to_string())]);
            files.insert("docs/index.html".to_string(), "<h1>Docs</h1>".to_string());
            let candidates = detect_project_candidates(&files);
            let root = if prefix.is_empty() { "." } else { "apps/web" };
            assert!(candidates.iter().any(|c| c.path == root
                && c.preset == PresetType::Rsbuild
                && c.confidence == "high"));
            assert!(!candidates.iter().any(|c| c.path == format!("{prefix}src")));
            assert!(candidates
                .iter()
                .any(|c| c.path == "docs" && c.preset == PresetType::Static));
            files.insert(
                format!("{prefix}src/independent/package.json"),
                r#"{"scripts":{"start":"node server.js"}}"#.to_string(),
            );
            files.insert(
                format!("{prefix}src/independent/index.html"),
                "<h1>Independent</h1>".to_string(),
            );
            assert!(detect_project_candidates(&files).iter().any(|c| {
                c.path == format!("{prefix}src/independent") && c.preset == PresetType::NodeJs
            }));
        }
    }

    #[test]
    fn rsbuild_source_filter_preserves_independent_html_and_server_entrypoints() {
        let base = BTreeMap::from([
            (
                "web/package.json".to_string(),
                r#"{"devDependencies":{"@rsbuild/core":"1.3.22"}}"#.to_string(),
            ),
            (
                "web/src/index.html".to_string(),
                "<div id='root'></div>".to_string(),
            ),
            (
                "web/src/independent/package.json".to_string(),
                r#"{"scripts":{"start":"node server.js"}}"#.to_string(),
            ),
            (
                "web/src/independent/public/index.html".to_string(),
                "<h1>Independent</h1>".to_string(),
            ),
        ]);
        let candidates = detect_project_candidates(&base);
        assert!(!candidates.iter().any(|c| c.path == "web/src"));
        assert!(candidates
            .iter()
            .any(|c| c.path == "web/src/independent/public" && c.preset == PresetType::Static));
        for (entry, provider) in [
            ("index.php", NixpacksProvider::Php),
            ("main.ts", NixpacksProvider::Deno),
        ] {
            let mut files = base.clone();
            files.insert(format!("web/src/{entry}"), "server entrypoint".to_string());
            assert!(detect_project_candidates(&files)
                .iter()
                .any(|c| c.path == "web/src" && c.build_provider == Some(provider)));
        }
    }

    #[test]
    fn dockerfile_runtime_override_validation() {
        for (value, valid) in [
            (
                serde_json::json!({"command": ["python", "-c", "print(1)", ""], "entrypoint": []}),
                true,
            ),
            (
                serde_json::json!({"command": null, "entrypoint": null}),
                true,
            ),
            (serde_json::json!({"command": []}), false),
            (serde_json::json!({"entrypoint": [""]}), false),
            (serde_json::json!({"command": ["python", "\u{0}"]}), false),
            (serde_json::json!({"command": ["a".repeat(1025)]}), false),
            (serde_json::json!({"entrypoint": vec!["a"; 65]}), false),
        ] {
            let config =
                StoredPresetConfig::parse_for_preset(&PresetType::Dockerfile, &value).unwrap();
            assert_eq!(
                validate_preset_config(PresetType::Dockerfile, &config).is_ok(),
                valid,
                "{value}"
            );
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
    fn server_manifests_win_over_vite_and_resolve_to_explicit_language_providers() {
        for (manifest, content, slug, label) in [
            ("Gemfile", "gem 'rails'", "nixpacks-ruby", "Ruby"),
            (
                "composer.json",
                r#"{"require":{"laravel/framework":"^12"}}"#,
                "nixpacks-php",
                "PHP",
            ),
        ] {
            for root in [".", "apps/server"] {
                let prefix = if root == "." {
                    String::new()
                } else {
                    format!("{root}/")
                };
                let files = BTreeMap::from([
                    (format!("{prefix}{manifest}"), content.to_string()),
                    (
                        format!("{prefix}package.json"),
                        r#"{"devDependencies":{"vite":"7"}}"#.to_string(),
                    ),
                ]);
                let candidates = detect_project_candidates(&files);
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].path, root);
                assert_eq!(candidates[0].catalog_slug(), slug);
                assert_eq!(candidates[0].label(), label);
                let resolved = resolve_preset_slug(slug, None).unwrap();
                assert_eq!(resolved.preset, PresetType::Nixpacks);
                let Some(StoredPresetConfig::Nixpacks(config)) = resolved.config else {
                    panic!("candidate must persist its explicit build provider");
                };
                assert_eq!(
                    config.providers,
                    vec![candidates[0].build_provider.unwrap()]
                );
                // A user-authored Dockerfile still takes precedence.
                let mut with_docker = files.clone();
                with_docker.insert(format!("{prefix}Dockerfile"), "FROM scratch".to_string());
                assert_eq!(
                    detect_project_candidates(&with_docker)[0].catalog_slug(),
                    "dockerfile"
                );
                // Ruby/PHP-only projects are deployable without a JS manifest.
                let only_manifest =
                    BTreeMap::from([(format!("{prefix}{manifest}"), content.to_string())]);
                assert_eq!(
                    detect_project_candidates(&only_manifest)[0].catalog_slug(),
                    slug
                );
            }
        }
    }

    #[test]
    fn incidental_language_manifests_do_not_hide_specific_node_frameworks() {
        for dependency in ["next", "@tanstack/react-start"] {
            let files = BTreeMap::from([
                ("Gemfile".to_string(), "gem 'tooling'".to_string()),
                ("composer.json".to_string(), "{}".to_string()),
                (
                    "package.json".to_string(),
                    serde_json::json!({
                        "dependencies": {dependency: "1"}, "devDependencies": {"vite": "7"}
                    })
                    .to_string(),
                ),
            ]);
            let candidates = detect_project_candidates(&files);
            assert_eq!(candidates.len(), 3);
            assert_eq!(candidates[0].build_provider, None);
            assert_eq!(
                candidates[0].catalog_slug(),
                if dependency == "next" {
                    "nextjs"
                } else {
                    "nixpacks-node"
                }
            );
            assert!(candidates
                .iter()
                .any(|candidate| candidate.catalog_slug() == "nixpacks-ruby"));
            assert!(candidates
                .iter()
                .any(|candidate| candidate.catalog_slug() == "nixpacks-php"));
        }
        let candidates = detect_project_candidates(&BTreeMap::from([
            ("Gemfile".to_string(), "gem 'rails'".to_string()),
            ("composer.json".to_string(), "{}".to_string()),
        ]));
        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .any(|candidate| candidate.build_provider == Some(NixpacksProvider::Ruby)));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.build_provider == Some(NixpacksProvider::Php)));
    }

    #[test]
    fn tanstack_start_is_a_server_but_vite_spa_remains_static() {
        for dependency in ["@tanstack/react-start", "@tanstack/solid-start"] {
            let files = BTreeMap::from([(
                "package.json".to_string(),
                serde_json::json!({
                    "dependencies": {dependency: "1"},
                    "devDependencies": {"vite": "7"}
                })
                .to_string(),
            )]);
            assert_eq!(
                detect_project_candidates(&files)[0].catalog_slug(),
                "nixpacks-node"
            );
        }
        let files = BTreeMap::from([(
            "package.json".to_string(),
            r#"{"devDependencies":{"vite":"7"}}"#.to_string(),
        )]);
        assert_eq!(detect_project_candidates(&files)[0].catalog_slug(), "vite");
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
            (
                "pyproject.toml".to_string(),
                "[project]\nname = \"x\"\n".to_string(),
            ),
            (
                "docker/Dockerfile".to_string(),
                "FROM debian\nCOPY pyproject.toml ./\n".to_string(),
            ),
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
                (
                    name.to_string(),
                    "services:\n  web:\n    image: nginx".to_string(),
                ),
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
            (
                "node_modules/left-pad/package.json".to_string(),
                "{}".to_string(),
            ),
            ("dist/index.html".to_string(), "<!doctype html>".to_string()),
            (
                "vendor/thing/go.mod".to_string(),
                "module thing".to_string(),
            ),
            (
                "target/debug/Cargo.toml".to_string(),
                "[package]".to_string(),
            ),
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

#[cfg(test)]
mod git_tree_detection_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn slugs(files: &[&str]) -> Vec<String> {
        let files: Vec<String> = files.iter().map(|file| file.to_string()).collect();
        detect_all_presets_from_files(&files)
            .into_iter()
            .map(|preset| preset.slug())
            .collect()
    }

    #[test]
    fn every_vite_config_extension_is_detected_as_static_vite() {
        for config in [
            "vite.config.js",
            "vite.config.ts",
            "vite.config.mjs",
            "vite.config.mts",
            "vite.config.cjs",
            "vite.config.cts",
        ] {
            assert_eq!(slugs(&["package.json", config]), vec!["vite"], "{config}");
        }
        assert!(slugs(&["package.json", "vite.config.json"]).is_empty());
        assert!(slugs(&["package.json", "myvite.config.ts.bak"]).is_empty());
    }

    #[test]
    fn vite_based_server_frameworks_are_not_static_vite() {
        for marker in [
            "svelte.config.js",
            "react-router.config.ts",
            "remix.config.js",
            "app.config.ts",
        ] {
            let detected = slugs(&["package.json", "vite.config.ts", marker]);
            assert_eq!(detected, vec!["nixpacks-node"], "{marker}");
        }
    }

    #[test]
    fn php_and_ruby_apps_with_vite_assets_are_server_presets() {
        assert_eq!(
            slugs(&["composer.json", "package.json", "vite.config.js"]),
            vec!["nixpacks-php"]
        );
        assert_eq!(
            slugs(&["Gemfile", "package.json", "vite.config.mts"]),
            vec!["nixpacks-ruby"]
        );
        assert_eq!(slugs(&["apps/server/Gemfile"]), vec!["nixpacks-ruby"]);
    }

    #[test]
    fn upload_detector_treats_react_router_and_solid_start_as_servers() {
        for (dependency, expected_slug) in [
            ("@react-router/dev", "nixpacks-node"),
            ("@solidjs/start", "nixpacks-node"),
            ("@builder.io/qwik-city", "nixpacks-node"),
        ] {
            let files = BTreeMap::from([(
                "package.json".to_string(),
                format!(r#"{{"devDependencies":{{"vite":"6","{dependency}":"1"}}}}"#),
            )]);
            let candidates = detect_project_candidates(&files);
            assert_eq!(candidates[0].catalog_slug(), expected_slug, "{dependency}");
            assert_ne!(candidates[0].preset, PresetType::Vite, "{dependency}");
        }
        let spa = BTreeMap::from([(
            "package.json".to_string(),
            r#"{"devDependencies":{"vite":"6","react-router-dom":"7"}}"#.to_string(),
        )]);
        assert_eq!(detect_project_candidates(&spa)[0].preset, PresetType::Vite);
    }

    #[test]
    fn compose_files_prefer_root_and_skip_development_directories() {
        let files: Vec<String> = [
            ".devcontainer/docker-compose.yml",
            "examples/basic/compose.yaml",
            "node_modules/pkg/docker-compose.yml",
            "test/fixtures/docker-compose.yml",
            "deploy/prod/compose.yml",
            "deploy/compose.yaml",
            "docker-compose.yml",
            "compose.yaml",
            "package.json",
        ]
        .iter()
        .map(|path| path.to_string())
        .collect();
        let presets = detect_presets_from_file_tree(&files);
        let compose: Vec<&DetectedPreset> = presets
            .iter()
            .filter(|preset| preset.slug == "docker-compose")
            .collect();
        assert!(
            compose
                .iter()
                .all(|preset| !preset.path.contains(".devcontainer")
                    && !preset.path.contains("examples")),
            "{compose:?}"
        );
        assert_eq!(
            compose[0].compose_files.as_deref().unwrap(),
            [
                "compose.yaml",
                "docker-compose.yml",
                "deploy/compose.yaml",
                "deploy/prod/compose.yml"
            ]
        );
    }

    #[test]
    fn devcontainer_only_compose_is_not_offered() {
        let files = vec![
            ".devcontainer/docker-compose.yml".to_string(),
            "package.json".to_string(),
        ];
        assert!(detect_presets_from_file_tree(&files)
            .iter()
            .all(|preset| preset.slug != "docker-compose"));
    }

    #[test]
    fn compose_ordering_is_stable_and_deduplicated() {
        assert_eq!(
            order_compose_files(vec![
                "b/docker-compose.yml".into(),
                "a/compose.yml".into(),
                "docker-compose.yaml".into(),
                "compose.yml".into(),
                "compose.yml".into(),
            ]),
            vec![
                "compose.yml",
                "docker-compose.yaml",
                "a/compose.yml",
                "b/docker-compose.yml"
            ]
        );
        assert!(is_non_deployable_compose_dir(".devcontainer"));
        assert!(is_non_deployable_compose_dir("services/Examples/demo"));
        assert!(!is_non_deployable_compose_dir(""));
        assert!(!is_non_deployable_compose_dir("deploy"));
    }

    /// File lists of the official starters, as a Drop archive presents them:
    /// every path is present, only recognised manifests carry contents.
    fn archive(paths: &[&str]) -> BTreeMap<String, String> {
        paths
            .iter()
            .map(|path| (path.to_string(), String::new()))
            .collect()
    }

    fn wrapped(prefix: &str, paths: &[&str]) -> Vec<String> {
        paths
            .iter()
            .map(|path| {
                if prefix == "." {
                    path.to_string()
                } else {
                    format!("{prefix}/{path}")
                }
            })
            .collect()
    }

    #[test]
    fn server_languages_without_a_js_manifest_are_deployable_from_drop() {
        let phoenix = [
            "config/config.exs",
            "config/runtime.exs",
            "lib/hello/application.ex",
            "lib/hello_web.ex",
            "lib/hello_web/controllers/page_controller.ex",
            "lib/hello_web/endpoint.ex",
            "lib/hello_web/router.ex",
            "mix.exs",
        ];
        let vapor = ["Package.swift", "Sources/App/main.swift"];
        let plain_php = ["index.php", "nixpacks.toml"];
        let deno_config = ["deno.json", "main.ts"];
        for (paths, slug, label) in [
            (&phoenix[..], "nixpacks-elixir", "Elixir"),
            (&vapor[..], "nixpacks-swift", "Swift"),
            (&plain_php[..], "nixpacks-php", "PHP"),
            (&deno_config[..], "nixpacks-deno", "Deno"),
        ] {
            // At the archive root, and wrapped in a folder as a zipped
            // directory arrives.
            for root in [".", "starter"] {
                let files: BTreeMap<String, String> = wrapped(root, paths)
                    .into_iter()
                    .map(|path| (path, String::new()))
                    .collect();
                let candidates = detect_project_candidates(&files);
                assert_eq!(candidates.len(), 1, "{slug} at {root}: {candidates:?}");
                let candidate = &candidates[0];
                assert_eq!(candidate.path, root, "{slug}");
                assert_eq!(candidate.catalog_slug(), slug);
                assert_eq!(candidate.label(), label);
                assert_eq!(candidate.confidence, "high", "{slug}");
                // The slug must create a project that builds with exactly
                // that provider, not one that re-detects at build time.
                let resolved = resolve_preset_slug(slug, None).unwrap();
                let Some(StoredPresetConfig::Nixpacks(config)) = resolved.config else {
                    panic!("{slug} must persist its explicit build provider");
                };
                assert_eq!(config.providers, vec![candidate.build_provider.unwrap()]);
            }
        }
    }

    #[test]
    fn a_lone_deno_entrypoint_is_offered_with_how_to_make_it_explicit() {
        for entrypoint in ["main.ts", "mod.ts"] {
            let candidates = detect_project_candidates(&archive(&[entrypoint]));
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].catalog_slug(), "nixpacks-deno");
            // Inferred from a file name, not declared: say so, and say how
            // to declare it.
            assert_eq!(candidates[0].confidence, "medium");
            assert!(candidates[0].reason.contains("deno.json"));
        }
    }

    #[test]
    fn typescript_inside_another_project_is_not_mistaken_for_deno() {
        // A Vite app's entrypoint, a Node server's `main.ts`, and arbitrary
        // TypeScript files are not Deno projects.
        let vite = archive(&["package.json", "vite.config.ts", "src/main.ts"]);
        let vite = BTreeMap::from_iter(vite.into_iter().map(|(path, contents)| {
            if path == "package.json" {
                (path, r#"{"devDependencies":{"vite":"7"}}"#.to_string())
            } else {
                (path, contents)
            }
        }));
        let candidates = detect_project_candidates(&vite);
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].preset, PresetType::Vite);

        let node = archive(&["package.json", "main.ts"]);
        let candidates = detect_project_candidates(&node);
        assert!(candidates
            .iter()
            .all(|candidate| candidate.build_provider != Some(NixpacksProvider::Deno)));

        assert!(detect_project_candidates(&archive(&["lib/util.ts", "types.ts"])).is_empty());
    }

    #[test]
    fn php_source_is_never_offered_as_a_static_site() {
        let candidates = detect_project_candidates(&archive(&["index.php", "index.html"]));
        // The static preset would publish the PHP source as text.
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].catalog_slug(), "nixpacks-php");
    }

    #[test]
    fn public_index_php_roots_the_application_at_its_parent() {
        for root in [".", "app"] {
            let files: BTreeMap<String, String> =
                wrapped(root, &["public/index.php", "src/Kernel.php"])
                    .into_iter()
                    .map(|path| (path, String::new()))
                    .collect();
            let candidates = detect_project_candidates(&files);
            assert_eq!(candidates.len(), 1, "{candidates:?}");
            assert_eq!(candidates[0].path, root);
            assert_eq!(candidates[0].catalog_slug(), "nixpacks-php");
        }
    }

    #[test]
    fn nested_entrypoints_inside_a_project_do_not_become_projects() {
        // WordPress ships an `index.php` in nearly every directory.
        let wordpress = archive(&[
            "index.php",
            "wp-config.php",
            "wp-content/index.php",
            "wp-content/plugins/index.php",
            "wp-content/themes/index.php",
        ]);
        let candidates = detect_project_candidates(&wordpress);
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].path, ".");
        // Laravel: Composer at the root, the document root beneath it.
        let laravel = archive(&["composer.json", "artisan", "public/index.php"]);
        let candidates = detect_project_candidates(&laravel);
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].catalog_slug(), "nixpacks-php");
    }

    #[test]
    fn a_nixpacks_plan_alone_is_offered_for_build_time_detection() {
        let candidates = detect_project_candidates(&archive(&["nixpacks.toml", "app.sh"]));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].catalog_slug(), "nixpacks");
        resolve_preset_slug(candidates[0].catalog_slug(), None).unwrap();
    }

    #[test]
    fn explicit_language_manifests_still_yield_to_a_dockerfile() {
        for manifest in ["mix.exs", "Package.swift", "deno.json", "index.php"] {
            let candidates = detect_project_candidates(&archive(&[manifest, "Dockerfile"]));
            assert_eq!(candidates.len(), 1, "{manifest}: {candidates:?}");
            assert_eq!(candidates[0].catalog_slug(), "dockerfile", "{manifest}");
        }
    }

    #[test]
    fn git_detection_recognises_the_same_explicit_language_manifests() {
        assert_eq!(slugs(&["mix.exs"]), vec!["nixpacks-elixir"]);
        assert_eq!(slugs(&["Package.swift"]), vec!["nixpacks-swift"]);
        for manifest in ["deno.json", "deno.jsonc", "deno.lock"] {
            assert_eq!(slugs(&[manifest, "main.ts"]), vec!["nixpacks-deno"]);
        }
        // Per-directory detection cannot see context, so a bare `main.ts`
        // stays unclaimed there.
        assert!(slugs(&["main.ts"]).is_empty());
        for slug in ["nixpacks-elixir", "nixpacks-swift", "nixpacks-deno"] {
            assert!(get_preset_by_slug(slug).is_some(), "{slug}");
        }
    }

    #[test]
    fn every_project_signal_is_named_for_the_not_found_message() {
        let names = project_signal_names();
        for expected in [
            "Dockerfile",
            "mix.exs",
            "Package.swift",
            "deno.json",
            "index.php",
            "nixpacks.toml",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
    }

    #[test]
    fn php_source_anywhere_under_a_static_root_is_never_published_as_files() {
        // `api/index.php` sits inside the static root, so it is not a root of
        // its own; a static deployment of "." would serve its source.
        for (paths, root) in [
            (&["index.html", "api/index.php"][..], "."),
            (&["site/index.html", "site/lib/view.phtml"][..], "site"),
            // Paths detection skips are still published by a static deploy.
            (&["index.html", "vendor/acme/Mailer.PHP"][..], "."),
            (&["index.html", "a/b/c/d/e/f/deep.php"][..], "."),
        ] {
            let candidates = detect_project_candidates(&archive(paths));
            assert!(
                candidates
                    .iter()
                    .all(|candidate| candidate.preset != PresetType::Static),
                "{paths:?}: {candidates:?}"
            );
            assert_eq!(candidates[0].path, root, "{paths:?}");
            assert_eq!(candidates[0].catalog_slug(), "nixpacks-php", "{paths:?}");
        }
        // HTML with no PHP anywhere stays a static site, and PHP that lives
        // outside the static root does not change it.
        let site = detect_project_candidates(&archive(&["site/index.html", "tools/gen.php"]));
        assert_eq!(site[0].path, "site");
        assert_eq!(site[0].preset, PresetType::Static);
    }

    #[test]
    fn git_detection_never_offers_static_for_a_tree_with_php_source() {
        let files: Vec<String> = ["index.html", "api/index.php"]
            .iter()
            .map(|path| path.to_string())
            .collect();
        let root: Vec<String> = detect_presets_from_file_tree(&files)
            .into_iter()
            .filter(|preset| preset.path == "./")
            .map(|preset| preset.slug)
            .collect();
        assert_eq!(root, vec!["nixpacks-php"]);
        let static_only: Vec<String> = vec!["index.html".to_string(), "about.html".to_string()];
        assert_eq!(
            detect_presets_from_file_tree(&static_only)[0].slug,
            "nixpacks-static"
        );
    }

    #[test]
    fn a_php_app_at_the_depth_cap_is_found_through_its_public_directory() {
        let candidates = detect_project_candidates(&archive(&["a/b/c/app/public/index.php"]));
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].path, "a/b/c/app");
        assert_eq!(candidates[0].catalog_slug(), "nixpacks-php");
        // One level deeper is past the cap, like every other project.
        assert!(detect_project_candidates(&archive(&["a/b/c/d/app/public/index.php"])).is_empty());
        // The document root is indexed, never promoted to a root itself.
        let deep_public = archive(&[
            "a/b/c/app/public/index.php",
            "a/b/c/app/public/package.json",
        ]);
        assert!(detect_project_candidates(&deep_public)
            .iter()
            .all(|candidate| candidate.path == "a/b/c/app"));
        // Dependency folders stay excluded even with a public/ document root.
        assert!(detect_project_candidates(&archive(&["vendor/pkg/public/index.php"])).is_empty());
    }
}
