// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build Image Job
//!
//! Builds container images from downloaded repository source code

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use temps_core::{
    JobResult, TempsConfig, WorkflowCancellationProvider, WorkflowContext, WorkflowError,
    WorkflowTask,
};
use temps_deployer::{BuildRequest, ImageBuilder};
use temps_entities::preset::{Preset as StoredPreset, PresetConfig as StoredPresetConfig};
use temps_logs::{LogLevel, LogService};
use temps_presets;
use tokio::time::{sleep, Duration};

// Generated JavaScript workspace builds need root lockfiles and sibling packages.
// Existing Dockerfiles return before this helper and keep their selected context.
fn preset_build_root(
    preset: &str,
    source_root: &Path,
    app: &Path,
) -> Result<PathBuf, WorkflowError> {
    if source_root == app {
        return Ok(app.to_path_buf());
    }
    if matches!(
        preset,
        "nextjs" | "vite" | "nixpacks-node" | "nixpacks" | "autopack"
    ) {
        if let Some(workspace) = read_confined_control_file(
            source_root,
            &source_root.join("pnpm-workspace.yaml"),
            1024 * 1024,
        )? {
            if !app.join("package.json").is_file() {
                return Ok(app.to_path_buf());
            }
            let relative = app.strip_prefix(source_root).map_err(|_| {
                WorkflowError::JobValidationFailed(format!(
                    "Application '{}' escapes workspace '{}'",
                    app.display(),
                    source_root.display()
                ))
            })?;
            let member = temps_presets::pnpm_workspace_contains(&workspace, relative)
                .map_err(WorkflowError::JobValidationFailed)?;
            return Ok(if member { source_root } else { app }.to_path_buf());
        }
    }
    if matches!(preset, "nixpacks-node" | "nixpacks" | "autopack")
        && app.join("package.json").is_file()
    {
        if let Some(package) =
            read_confined_control_file(source_root, &source_root.join("package.json"), 1024 * 1024)?
        {
            let manifest: serde_json::Value = serde_json::from_str(&package).map_err(|error| {
                WorkflowError::JobValidationFailed(format!(
                    "Cannot parse workspace package.json: {error}"
                ))
            })?;
            if manifest.get("workspaces").is_some() {
                let relative = app.strip_prefix(source_root).map_err(|_| {
                    WorkflowError::JobValidationFailed(
                        "Application directory escapes workspace root".to_string(),
                    )
                })?;
                let member = temps_presets::package_workspace_contains(&package, relative)
                    .map_err(WorkflowError::JobValidationFailed)?;
                return Ok(if member { source_root } else { app }.to_path_buf());
            }
        }
    }
    // A nested Go module, Cargo crate or Elixir umbrella app whose
    // `replace`/`path`/`in_umbrella` dependencies or workspace live beside it
    // builds from the repository root; the preset runs its commands in the
    // application's directory.
    if let Some(language) = temps_presets::CompiledLanguage::for_preset(preset, app) {
        if temps_presets::compiled_workspace_app(source_root, app, language)
            .map_err(|error| WorkflowError::JobValidationFailed(error.to_string()))?
            .is_some()
        {
            return Ok(source_root.to_path_buf());
        }
    }
    if matches!(
        preset,
        "python" | "nixpacks-python" | "nixpacks" | "autopack"
    ) && temps_presets::python_app_directory(source_root, app)
        .map_err(WorkflowError::JobValidationFailed)?
        .is_some()
    {
        return Ok(source_root.to_path_buf());
    }
    if preset != "nextjs" {
        return Ok(app.to_path_buf());
    }
    for marker in ["turbo.json", "nx.json", "lerna.json", "pnpm-workspace.yaml"] {
        if read_confined_control_file(source_root, &source_root.join(marker), 1024 * 1024)?
            .is_some()
        {
            return Ok(source_root.to_path_buf());
        }
    }
    if let Some(package) = read_confined_control_file(
        source_root,
        &source_root.join("package.json"),
        5 * 1024 * 1024,
    )? {
        if serde_json::from_str::<serde_json::Value>(&package)
            .ok()
            .and_then(|manifest| manifest.get("workspaces").cloned())
            .is_some_and(|workspaces| workspaces.is_array() || workspaces.is_object())
        {
            return Ok(source_root.to_path_buf());
        }
    }
    Ok(app.to_path_buf())
}

fn write_workspace_ignore(root: &Path, dockerfile: &Path) -> Result<(), WorkflowError> {
    // Widening the context must not let app-local negations expose siblings.
    // Builders recognize this internal marker and independently enforce both
    // root and specific rule sets, retaining each file's own negations.
    let mut specific = dockerfile.as_os_str().to_os_string();
    specific.push(".dockerignore");
    let path = Path::new(&specific);
    let existing = read_confined_control_file(root, path, 1024 * 1024)?;
    let create_new = existing.is_none();
    // Validate the root control file before handing either builder the context.
    read_confined_control_file(root, &root.join(".dockerignore"), 1024 * 1024)?;
    let mut ignore = existing.unwrap_or_else(|| "# SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n# SPDX-License-Identifier: MIT OR Apache-2.0\n".to_string());
    ignore.push('\n');
    ignore.push_str(temps_deployer::build_protocol::WORKSPACE_ROOT_IGNORE_MARKER);
    ignore.push_str("\n**/node_modules\n**/.git\n**/.env\n**/.env.*\n");
    write_no_follow(path, ignore.as_bytes(), create_new)
}

/// Variable name prefixes that frontend frameworks inline into the
/// application at build time (Vite, Next.js, SvelteKit/Astro, Create React
/// App, Gatsby, Nuxt, Expo). A build that does not receive such a value
/// ships an app with it empty, with nothing at runtime able to restore it.
const BUILD_INLINED_VARIABLE_PREFIXES: &[&str] = &[
    "VITE_",
    "NEXT_PUBLIC_",
    "PUBLIC_",
    "REACT_APP_",
    "GATSBY_",
    "NUXT_PUBLIC_",
    "EXPO_PUBLIC_",
];

/// Build arguments for the `index`-th platform of a multi-platform build.
///
/// The BuildKit cache-mount namespace (#1372) is per project, environment
/// and ref, not per architecture. Two architectures sharing one
/// `RUN --mount=type=cache` id would share native artifacts -- an arm64
/// `node_modules` or `target/` reused by an amd64 build -- so every platform
/// after the first gets its own namespace. The first keeps the plain one,
/// so a single-architecture build's cache key never changes.
fn platform_build_args(
    args: &HashMap<String, String>,
    index: usize,
    platform: Option<&str>,
) -> HashMap<String, String> {
    let mut args = args.clone();
    if let (true, Some(platform)) = (index > 0, platform) {
        if let Some(namespace) =
            args.get_mut(temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG)
        {
            namespace.push('-');
            namespace.push_str(&temps_deployer::platform::platform_tag_suffix(platform));
        }
    }
    args
}

/// The variables among `names` that a framework inlines at build time,
/// sorted and de-duplicated. Names only: values never leave the planner.
pub(crate) fn build_inlined_variables<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut inlined: Vec<String> = names
        .into_iter()
        .filter(|name| {
            BUILD_INLINED_VARIABLE_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix) && name.len() > prefix.len())
        })
        .map(str::to_owned)
        .collect();
    inlined.sort();
    inlined.dedup();
    inlined
}

fn validate_relative_build_path(path: &Path, label: &str) -> Result<(), WorkflowError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(WorkflowError::JobValidationFailed(format!(
            "{label} '{}' must be relative and contained by the build context",
            path.display()
        )));
    }
    Ok(())
}

fn validate_confined_regular_file(root: &Path, path: &Path) -> Result<(), WorkflowError> {
    let metadata = fs::symlink_metadata(path).map_err(WorkflowError::IoError)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build input '{}' must be a regular non-symlink file",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(WorkflowError::IoError)?;
    if !canonical.starts_with(root) {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build input '{}' escapes build context '{}'",
            canonical.display(),
            root.display()
        )));
    }
    Ok(())
}

fn write_no_follow(path: &Path, contents: &[u8], create_new: bool) -> Result<(), WorkflowError> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(WorkflowError::IoError)?;
    if !file.metadata().map_err(WorkflowError::IoError)?.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build output '{}' must be a regular file",
            path.display()
        )));
    }
    file.write_all(contents).map_err(WorkflowError::IoError)
}

pub(crate) fn read_confined_control_file(
    root: &Path,
    path: &Path,
    max_bytes: u64,
) -> Result<Option<String>, WorkflowError> {
    let canonical_root = root.canonicalize().map_err(WorkflowError::IoError)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(WorkflowError::IoError(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' must be a regular non-symlink file",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(WorkflowError::IoError)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' escapes build context '{}'",
            canonical.display(),
            canonical_root.display()
        )));
    }
    if metadata.len() > max_bytes {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' exceeds the {max_bytes} byte limit",
            path.display()
        )));
    }
    let mut contents = String::new();
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(WorkflowError::IoError)?;
    if !file.metadata().map_err(WorkflowError::IoError)?.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' must remain a regular file",
            path.display()
        )));
    }
    file.take(max_bytes + 1)
        .read_to_string(&mut contents)
        .map_err(WorkflowError::IoError)?;
    if contents.len() as u64 > max_bytes {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' exceeds the {max_bytes} byte limit",
            path.display()
        )));
    }
    Ok(Some(contents))
}

/// Install/build/output overrides from one configuration source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BuildOverrides {
    install_command: Option<String>,
    build_command: Option<String>,
    output_dir: Option<String>,
}

impl BuildOverrides {
    /// Overrides stored on the project's typed preset configuration.
    fn from_stored(config: &StoredPresetConfig) -> Self {
        let full =
            |install: &Option<String>, build: &Option<String>, output: &Option<String>| Self {
                install_command: install.clone(),
                build_command: build.clone(),
                output_dir: output.clone(),
            };
        let commands = |install: &Option<String>, build: &Option<String>| Self {
            install_command: install.clone(),
            build_command: build.clone(),
            output_dir: None,
        };
        let build_only = |build: &Option<String>| Self {
            build_command: build.clone(),
            ..Self::default()
        };
        let overrides = match config {
            StoredPresetConfig::NextJs(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::Vite(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::Astro(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::Nuxt(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::Remix(c) => commands(&c.install_command, &c.build_command),
            StoredPresetConfig::SvelteKit(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::SolidStart(c) => commands(&c.install_command, &c.build_command),
            StoredPresetConfig::Angular(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::Vue(c) => full(&c.install_command, &c.build_command, &c.output_dir),
            StoredPresetConfig::React(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::Docusaurus(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::Rsbuild(c) => {
                full(&c.install_command, &c.build_command, &c.output_dir)
            }
            StoredPresetConfig::NodeJs(c) => commands(&c.install_command, &c.build_command),
            StoredPresetConfig::Rails(c) => build_only(&c.build_command),
            StoredPresetConfig::Go(c) => build_only(&c.build_command),
            StoredPresetConfig::Rust(c) => build_only(&c.build_command),
            StoredPresetConfig::Java(c) => build_only(&c.build_command),
            StoredPresetConfig::Laravel(c) => build_only(&c.build_command),
            StoredPresetConfig::Python(_)
            | StoredPresetConfig::FastApi(_)
            | StoredPresetConfig::Flask(_)
            | StoredPresetConfig::Django(_)
            | StoredPresetConfig::Dockerfile(_)
            | StoredPresetConfig::DockerCompose(_)
            | StoredPresetConfig::Nixpacks(_)
            | StoredPresetConfig::Static(_) => Self::default(),
        };
        overrides.without_blank_values()
    }

    /// Treat empty strings as unset, so clearing a field in the API restores
    /// the detected default instead of rendering `RUN ` with no command.
    fn without_blank_values(self) -> Self {
        let keep = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
        Self {
            install_command: keep(self.install_command),
            build_command: keep(self.build_command),
            output_dir: keep(self.output_dir),
        }
    }

    /// Field-by-field fallback to `other` where `self` has no value.
    fn or(self, other: Self) -> Self {
        let this = self.without_blank_values();
        Self {
            install_command: this.install_command.or(other.install_command),
            build_command: this.build_command.or(other.build_command),
            output_dir: this.output_dir.or(other.output_dir),
        }
    }

    fn is_empty(&self) -> bool {
        self.install_command.is_none() && self.build_command.is_none() && self.output_dir.is_none()
    }

    /// Each value is spliced into a single Dockerfile instruction; a newline
    /// would start a new instruction. The output directory is also a COPY
    /// source path inside the build stage, so it must stay relative.
    fn validate(&self) -> Result<(), WorkflowError> {
        for (field, value) in [
            ("install command", &self.install_command),
            ("build command", &self.build_command),
            ("output directory", &self.output_dir),
        ] {
            if let Some(value) = value {
                if value.chars().any(char::is_control) {
                    return Err(WorkflowError::JobValidationFailed(format!(
                        "Invalid configuration: build {field} {value:?} contains a newline or \
                         control character; use a single-line value in the project's build \
                         settings or .temps.yaml"
                    )));
                }
            }
        }
        if let Some(output) = self.output_dir.as_deref() {
            validate_relative_build_path(Path::new(output), "Build output directory").map_err(
                |_| {
                    WorkflowError::JobValidationFailed(format!(
                        "Invalid configuration: build output directory '{output}' must be a \
                         relative path inside the application (for example dist or build)"
                    ))
                },
            )?;
        }
        Ok(())
    }
}

/// Typed output from DownloadRepoJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryOutput {
    pub repo_dir: PathBuf,
    pub checkout_ref: String,
    pub repo_owner: String,
    pub repo_name: String,
}

impl RepositoryOutput {
    /// Extract RepositoryOutput from WorkflowContext
    pub fn from_context(
        context: &WorkflowContext,
        download_job_id: &str,
    ) -> Result<Self, WorkflowError> {
        let repo_dir_str: String = context
            .get_output(download_job_id, "repo_dir")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("repo_dir output not found".to_string())
            })?;
        let checkout_ref: String = context
            .get_output(download_job_id, "checkout_ref")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("checkout_ref output not found".to_string())
            })?;
        let repo_owner: String = context
            .get_output(download_job_id, "repo_owner")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("repo_owner output not found".to_string())
            })?;
        let repo_name: String = context
            .get_output(download_job_id, "repo_name")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("repo_name output not found".to_string())
            })?;

        Ok(Self {
            repo_dir: PathBuf::from(repo_dir_str),
            checkout_ref,
            repo_owner,
            repo_name,
        })
    }
}

/// The parts of a worker-built image's configuration that later jobs read.
///
/// Recorded from the node that owns the image, so a job on the control plane
/// never needs a local copy to learn how the image runs. `ENV` is deliberately
/// not carried: it is whatever the Dockerfile baked in, and job outputs are
/// persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltImageConfig {
    /// Ports from `EXPOSE`, ascending.
    #[serde(default)]
    pub exposed_ports: Vec<u16>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub entrypoint: Vec<String>,
}

impl BuiltImageConfig {
    /// `None` when the node did not report the image's configuration (an
    /// agent that predates it): "unknown" must not read as "exposes nothing".
    pub fn from_image_info(info: &temps_deployer::ImageInfo) -> Option<Self> {
        let config = info.config.as_ref()?;
        Some(Self {
            exposed_ports: config.exposed_ports.clone(),
            working_dir: info.working_dir.clone(),
            user: config.user.clone(),
            entrypoint: config.entrypoint.clone(),
        })
    }
}

/// Typed output from BuildImageJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOutput {
    pub image_tag: String,
    pub image_id: String,
    pub size_bytes: u64,
    pub build_context: PathBuf,
    pub dockerfile_path: PathBuf,
    /// Every tag this build produced, keyed by canonical platform.
    ///
    /// Empty for a single-platform build (the default): `image_tag` is then
    /// the whole story. On a heterogeneous cluster it holds one entry per
    /// architecture — the first platform keeps the plain `image_tag`, the rest
    /// get `-<arch>` suffixed tags — and the deploy job picks the entry that
    /// matches each node.
    #[serde(default)]
    pub image_tags_by_platform: HashMap<String, String>,
    /// For a mixed-architecture worker build, the worker that built and
    /// holds each platform's image, keyed like `image_tags_by_platform`.
    /// Empty for every other build, whose single builder (if any) is the
    /// `builder_node_id` output.
    #[serde(default)]
    pub builder_node_ids_by_platform: HashMap<String, i32>,
    /// For a mixed-architecture worker build, the id each worker reported
    /// for the image it built, keyed like `image_tags_by_platform`. A
    /// transfer or rollback of a tag only proceeds when the source still
    /// holds exactly this image.
    #[serde(default)]
    pub image_ids_by_platform: HashMap<String, String>,
    /// Configuration of the primary image as its owning node reported it.
    /// `None` for a build on the control plane's own daemon, and for a node
    /// that predates the field.
    #[serde(default)]
    pub image_config: Option<BuiltImageConfig>,
}

/// What [`BuildImageJob::build_on_platform_workers`] produced.
struct PerArchitectureBuild {
    /// The first platform's build, which owns the plain tag.
    primary: temps_deployer::BuildResult,
    image_tags_by_platform: HashMap<String, String>,
    builder_node_ids_by_platform: HashMap<String, i32>,
    image_ids_by_platform: HashMap<String, String>,
}

/// The id a build recorded for `image_tag`: its own platform's entry on a
/// mixed-architecture build, otherwise `image_id`. `None` when none was
/// recorded.
pub(crate) fn primary_image_id<'a>(
    image_tag: &str,
    image_id: &'a str,
    image_tags_by_platform: &HashMap<String, String>,
    image_ids_by_platform: &'a HashMap<String, String>,
) -> Option<&'a str> {
    image_tags_by_platform
        .iter()
        .find(|(_, built)| built.as_str() == image_tag)
        .and_then(|(platform, _)| image_ids_by_platform.get(platform))
        .map(String::as_str)
        .or_else(|| (!image_id.trim().is_empty()).then_some(image_id))
}

impl ImageOutput {
    /// The id the build recorded for the primary image: its own platform's
    /// entry on a mixed-architecture build, otherwise the build's `image_id`.
    /// `None` when none was recorded.
    pub fn expected_image_id(&self) -> Option<&str> {
        primary_image_id(
            &self.image_tag,
            &self.image_id,
            &self.image_tags_by_platform,
            &self.image_ids_by_platform,
        )
    }

    /// Extract ImageOutput from WorkflowContext
    pub fn from_context(
        context: &WorkflowContext,
        build_job_id: &str,
    ) -> Result<Self, WorkflowError> {
        let image_tag: String =
            context
                .get_output(build_job_id, "image_tag")?
                .ok_or_else(|| {
                    WorkflowError::JobValidationFailed("image_tag output not found".to_string())
                })?;
        let image_id: String = context
            .get_output(build_job_id, "image_id")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("image_id output not found".to_string())
            })?;
        let size_bytes: u64 = context
            .get_output(build_job_id, "size_bytes")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("size_bytes output not found".to_string())
            })?;
        let build_context_str: String = context
            .get_output(build_job_id, "build_context")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("build_context output not found".to_string())
            })?;
        let dockerfile_path_str: String = context
            .get_output(build_job_id, "dockerfile_path")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("dockerfile_path output not found".to_string())
            })?;

        let image_tags_by_platform: HashMap<String, String> = context
            .get_output(build_job_id, "image_tags_by_platform")?
            .unwrap_or_default();
        let builder_node_ids_by_platform: HashMap<String, i32> = context
            .get_output(build_job_id, "builder_node_ids_by_platform")?
            .unwrap_or_default();
        let image_ids_by_platform: HashMap<String, String> = context
            .get_output(build_job_id, "image_ids_by_platform")?
            .unwrap_or_default();
        let image_config: Option<BuiltImageConfig> =
            context.get_output(build_job_id, "image_config")?.flatten();

        Ok(Self {
            image_tag,
            image_id,
            size_bytes,
            build_context: PathBuf::from(build_context_str),
            dockerfile_path: PathBuf::from(dockerfile_path_str),
            image_tags_by_platform,
            builder_node_ids_by_platform,
            image_ids_by_platform,
            image_config,
        })
    }
}

/// Configuration for building images
///
/// `Debug` lists build-argument names only: their values include every
/// resolved environment variable, secrets among them.
#[derive(Clone)]
pub struct BuildConfig {
    pub dockerfile_path: Option<String>,
    pub build_context: Option<String>,
    pub build_args: Vec<(String, String)>,
    pub build_args_buildkit: Vec<(String, String)>,
    /// Container platforms to build for, in priority order.
    ///
    /// Empty (the default) builds exactly once, on the daemon's native
    /// platform — identical to the behaviour before multi-arch support. When
    /// populated, the **first** entry produces the plain `image_tag` and each
    /// additional entry produces a `-<arch>` suffixed tag. Non-native entries
    /// are built through the daemon's `platform` option, which needs QEMU
    /// binfmt handlers registered on the host.
    pub target_platforms: Vec<String>,
    pub cache_from: Vec<String>,
}

impl std::fmt::Debug for BuildConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = |args: &[(String, String)]| {
            args.iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        f.debug_struct("BuildConfig")
            .field("dockerfile_path", &self.dockerfile_path)
            .field("build_context", &self.build_context)
            .field("build_arg_names", &names(&self.build_args))
            .field(
                "build_arg_buildkit_names",
                &names(&self.build_args_buildkit),
            )
            .field("target_platforms", &self.target_platforms)
            .field("cache_from", &self.cache_from)
            .finish()
    }
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            dockerfile_path: Some("Dockerfile".to_string()),
            build_context: Some(".".to_string()),
            build_args: Vec::new(),
            build_args_buildkit: Vec::new(),
            target_platforms: Vec::new(),
            cache_from: Vec::new(),
        }
    }
}

/// One worker of a mixed-architecture worker build: the node that builds the
/// image for `platform`, natively, because it runs that architecture itself.
#[derive(Clone)]
pub struct WorkerPlatformBuilder {
    /// Canonical platform this worker builds (`linux/arm64`).
    pub platform: String,
    pub node_id: i32,
    pub node_name: String,
    /// Authenticated agent client for `node_id`.
    pub builder: Arc<dyn ImageBuilder>,
}

impl std::fmt::Debug for WorkerPlatformBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerPlatformBuilder")
            .field("platform", &self.platform)
            .field("node_id", &self.node_id)
            .field("node_name", &self.node_name)
            .finish()
    }
}

/// Job for building container images from source code
pub struct BuildImageJob {
    job_id: String,
    download_job_id: String,
    image_tag: String,
    build_config: BuildConfig,
    image_builder: Arc<dyn ImageBuilder>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    preset: Option<StoredPreset>,
    preset_config: Option<StoredPresetConfig>,
    /// Operator-configured prefix (`AppSettings::registry_mirror_prefix`)
    /// applied to implicit Docker Hub base images in the generated
    /// Dockerfile. `None` (the default) leaves every `FROM` line untouched.
    registry_mirror_prefix: Option<String>,
    /// Whether this process may run builds/containers locally at all.
    ///
    /// Wired from the same `LocalWorkloadPolicy`/`NodeScheduler` source of
    /// truth the scheduler already uses for cross-build platform detection
    /// (see `WorkflowExecutionService`'s `BuildImageJob` construction).
    /// `true` by default so every existing caller/test keeps building
    /// locally unless a control-plane profile explicitly disables it.
    ///
    /// Checked before `image_builder` is touched at all: git-source builds
    /// are out of scope for the control-plane profile (worker-side builds
    /// are ADR-045), so this must refuse with a typed, actionable error
    /// instead of reaching `ImageBuilder::build_image_with_callback` and
    /// surfacing a raw `BuilderError::DockerUnavailable`.
    local_workloads_enabled: bool,
    /// Node whose agent owns the image produced by this job. `None` means
    /// the historical control-plane-local builder was used.
    remote_builder_node_id: Option<i32>,
    /// Why this build is not running where the deployment's build location
    /// asked for, written to the build log before the build starts so a
    /// fallback to the control plane is never silent.
    build_location_notice: Option<String>,
    /// For a build moved off the control plane: the control plane's own
    /// image store, and the name of the node building the image. The image
    /// is streamed into it after the build, because the control plane's
    /// post-build jobs (source maps, static assets, scans) read it there.
    control_plane_copy: Option<(Arc<dyn ImageBuilder>, String)>,
    /// Names of the build arguments whose values may be sent to a worker
    /// build: variables the planner recorded as not secret (the project's
    /// non-secret variables and the platform's public values, such as the
    /// browser Sentry DSN). Every other value -- secrets, linked-service
    /// credentials, platform tokens -- never leaves the control plane.
    /// Empty for a job planned before this existed: nothing is sent.
    worker_build_arg_keys: BTreeSet<String>,
    /// A mixed-architecture worker build: one worker per target platform,
    /// each building its own architecture natively, in parallel. Empty for
    /// every other build. When set, the first entry is also
    /// `image_builder`/`remote_builder_node_id` and owns the plain tag.
    platform_builders: Vec<WorkerPlatformBuilder>,
}

/// What `ensure_dockerfile` produced.
struct GeneratedDockerfile {
    /// Build arguments the preset itself asks for.
    build_args: HashMap<String, String>,
    /// The build context the Dockerfile expects.
    build_root: PathBuf,
    /// Whether Temps wrote the Dockerfile (false: the repository's own).
    generated: bool,
}

/// A worker build's build arguments, split by whether their values may leave
/// the control plane. Names are sorted.
#[derive(Default)]
struct WorkerBuildArgs {
    /// Sent to the worker (the cache namespace included).
    sent: BTreeMap<String, String>,
    /// Names whose values stay on the control plane.
    withheld: Vec<String>,
}

impl WorkerBuildArgs {
    /// Project and platform variables sent, without BuildKit's own arguments.
    fn variable_names(&self) -> Vec<String> {
        self.sent
            .keys()
            .filter(|name| temps_deployer::build_protocol::is_worker_build_arg_name(name))
            .cloned()
            .collect()
    }
}

/// `names` joined for a log line, capped so a project with hundreds of
/// variables does not produce an unreadable line.
fn name_list(names: &[String]) -> String {
    const SHOWN: usize = 20;
    let mut list = names
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        list.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    list
}

impl std::fmt::Debug for BuildImageJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildImageJob")
            .field("job_id", &self.job_id)
            .field("download_job_id", &self.download_job_id)
            .field("image_tag", &self.image_tag)
            .field("build_config", &self.build_config)
            .field("image_builder", &"<ImageBuilder>")
            .finish()
    }
}

impl BuildImageJob {
    pub fn new(
        job_id: String,
        download_job_id: String,
        image_tag: String,
        image_builder: Arc<dyn ImageBuilder>,
    ) -> Self {
        Self {
            job_id,
            download_job_id,
            image_tag,
            build_config: BuildConfig::default(),
            image_builder,
            log_id: None,
            log_service: None,
            preset: None,
            preset_config: None,
            registry_mirror_prefix: None,
            local_workloads_enabled: true,
            remote_builder_node_id: None,
            build_location_notice: None,
            control_plane_copy: None,
            worker_build_arg_keys: BTreeSet::new(),
            platform_builders: Vec::new(),
        }
    }

    /// Build each platform on its own worker. See the `platform_builders`
    /// field doc. The first builder becomes this job's primary builder.
    pub fn with_platform_builders(mut self, builders: Vec<WorkerPlatformBuilder>) -> Self {
        if let Some(primary) = builders.first() {
            self.image_builder = primary.builder.clone();
            self.remote_builder_node_id = Some(primary.node_id);
            self.build_config.target_platforms =
                builders.iter().map(|b| b.platform.clone()).collect();
        }
        self.platform_builders = builders;
        self
    }

    /// Whether this is a mixed-architecture build spread over several workers.
    fn is_per_architecture_worker_build(&self) -> bool {
        self.platform_builders.len() > 1
    }

    /// The worker(s) this build runs on, for build-log lines.
    fn worker_nodes_phrase(&self, primary_node_id: i32) -> String {
        if !self.is_per_architecture_worker_build() {
            return format!("worker node {primary_node_id}");
        }
        let nodes: Vec<String> = self
            .platform_builders
            .iter()
            .map(|b| format!("{} ({})", b.node_id, b.platform))
            .collect();
        format!("worker nodes {}", nodes.join(", "))
    }

    /// Set whether this process may build images locally. See the
    /// `local_workloads_enabled` field doc for why this exists.
    pub fn with_local_workloads_enabled(mut self, enabled: bool) -> Self {
        self.local_workloads_enabled = enabled;
        self
    }

    pub fn with_remote_builder_node_id(mut self, node_id: i32) -> Self {
        self.remote_builder_node_id = Some(node_id);
        self
    }

    pub fn with_build_location_notice(mut self, notice: Option<String>) -> Self {
        self.build_location_notice = notice;
        self
    }

    pub fn with_control_plane_copy(
        mut self,
        control_plane_copy: Option<(Arc<dyn ImageBuilder>, String)>,
    ) -> Self {
        self.control_plane_copy = control_plane_copy;
        self
    }

    pub fn with_worker_build_arg_keys(mut self, keys: BTreeSet<String>) -> Self {
        self.worker_build_arg_keys = keys;
        self
    }

    /// What happens to a worker-built image once the build succeeded: its
    /// configuration is read from the node that owns it, and the image is
    /// streamed to the control plane only when that cannot be done.
    ///
    /// The control plane reads nothing from its own copy of a node-built image
    /// any more -- the scan runs on the node, extraction reads from it, and
    /// the container port comes from the configuration recorded here -- so a
    /// replica placed on the control plane is the only thing that needs the
    /// image there, and it fetches the image itself. A node that did not
    /// report the configuration predates all of that and still gets the copy
    /// the older jobs expect.
    async fn finish_worker_build(
        &self,
        context: &WorkflowContext,
        image_tag: &str,
    ) -> Result<Option<BuiltImageConfig>, WorkflowError> {
        let config = self.read_worker_image_config(context, image_tag).await?;
        if config.is_none() {
            self.copy_image_to_control_plane(context, image_tag).await?;
        }
        Ok(config)
    }

    /// The configuration of the image a worker just built, read from the
    /// worker that owns it.
    ///
    /// `Ok(None)` for a build on the control plane's own daemon (nothing to
    /// record: its callers inspect the local image) and for a node that does
    /// not report image configuration. A failed inspection is logged and also
    /// yields `None`, which keeps the control plane's own copy of the image as
    /// the source of truth, exactly as before.
    async fn read_worker_image_config(
        &self,
        context: &WorkflowContext,
        image_tag: &str,
    ) -> Result<Option<BuiltImageConfig>, WorkflowError> {
        let Some(node_id) = self.remote_builder_node_id else {
            return Ok(None);
        };
        let info = match self.image_builder.inspect_image(image_tag).await {
            Ok(info) => info,
            Err(error) => {
                tracing::warn!(
                    image_tag = %image_tag,
                    node_id,
                    "Could not read the configuration of a worker-built image: {error}"
                );
                self.log(
                    context,
                    format!(
                        "⚠️ Could not read the image configuration from build node {node_id}: \
                         {error}"
                    ),
                )
                .await?;
                return Ok(None);
            }
        };
        let config = BuiltImageConfig::from_image_info(&info);
        match &config {
            Some(config) => {
                self.log(
                    context,
                    format!(
                        "Image '{}' stays on build node {}: exposed ports {:?}, user {}, \
                         workdir {}. It is copied to the control plane only if a replica is \
                         placed there",
                        image_tag,
                        node_id,
                        config.exposed_ports,
                        config.user.as_deref().unwrap_or("(default)"),
                        config.working_dir.as_deref().unwrap_or("(none)"),
                    ),
                )
                .await?;
            }
            None => {
                self.log(
                    context,
                    format!(
                        "Build node {node_id} did not report the image's configuration \
                         (an older agent); the image will be copied to the control plane"
                    ),
                )
                .await?;
            }
        }
        Ok(config)
    }

    /// After a build moved off the control plane, stream the image into the
    /// control plane's daemon. A failure is logged, not fatal: the image is
    /// on its worker and can still run there, and a replica placed on the
    /// control plane retries the copy before it starts.
    async fn copy_image_to_control_plane(
        &self,
        context: &WorkflowContext,
        image_tag: &str,
    ) -> Result<(), WorkflowError> {
        let Some((control_plane, node_name)) = &self.control_plane_copy else {
            return Ok(());
        };
        let started = std::time::Instant::now();
        match super::node_image::copy_node_built_image(
            self.image_builder.as_ref(),
            node_name,
            control_plane.as_ref(),
            image_tag,
            None,
        )
        .await
        {
            Ok(super::node_image::NodeImageCopy::AlreadyPresent) => Ok(()),
            Ok(super::node_image::NodeImageCopy::Transferred) => {
                self.log(
                    context,
                    format!(
                        "Image '{}' copied from build node '{}' to the control plane in {:.1}s",
                        image_tag,
                        node_name,
                        started.elapsed().as_secs_f64()
                    ),
                )
                .await
            }
            Err(error) => {
                tracing::warn!(
                    image_tag = %image_tag,
                    node_name = %node_name,
                    "Could not copy worker-built image to the control plane: {}",
                    error
                );
                self.log(
                    context,
                    format!(
                        "⚠️ Could not copy the image to the control plane: {error}. A \
                         replica placed on the control plane retries the copy before it starts"
                    ),
                )
                .await
            }
        }
    }

    pub fn with_build_config(mut self, build_config: BuildConfig) -> Self {
        self.build_config = build_config;
        self
    }

    pub fn with_dockerfile_path(mut self, dockerfile_path: String) -> Self {
        self.build_config.dockerfile_path = Some(dockerfile_path);
        self
    }

    pub fn with_build_args(mut self, build_args: Vec<(String, String)>) -> Self {
        self.build_config.build_args = build_args;
        self
    }

    pub fn with_build_args_buildkit(mut self, build_args_buildkit: Vec<(String, String)>) -> Self {
        self.build_config.build_args_buildkit = build_args_buildkit;
        self
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    pub fn with_preset(mut self, preset: StoredPreset) -> Self {
        self.preset = Some(preset);
        self
    }

    pub fn with_preset_config(mut self, preset_config: Option<StoredPresetConfig>) -> Self {
        self.preset_config = preset_config;
        self
    }

    pub fn with_registry_mirror_prefix(mut self, registry_mirror_prefix: Option<String>) -> Self {
        self.registry_mirror_prefix = registry_mirror_prefix;
        self
    }

    /// Write log message to both job-specific log file and context log writer
    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content/emojis
        let level = Self::detect_log_level(&message);
        self.log_with_level(context, level, message).await
    }

    /// Like [`Self::log`], with an explicit level for messages whose wording
    /// would mislead the keyword heuristic (a warning that quotes an error).
    async fn log_with_level(
        &self,
        context: &WorkflowContext,
        level: LogLevel,
        message: String,
    ) -> Result<(), WorkflowError> {
        // Write structured log to job-specific log file
        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        // Also write to context log writer (for real-time streaming and test capture)
        context.log(&message).await?;
        Ok(())
    }

    /// Split this build's arguments for a worker build. A value is sent only
    /// when the planner recorded its name as not secret, and it is a name a
    /// worker accepts (`BUILDKIT_*` arguments other than the cache namespace,
    /// and the `.npmrc` credentials, never are). The rest stay here.
    fn worker_build_args(&self) -> WorkerBuildArgs {
        use temps_deployer::build_protocol::{is_worker_build_arg_name, CACHE_MOUNT_NAMESPACE_ARG};
        let mut split = WorkerBuildArgs::default();
        let mut withheld = BTreeSet::new();
        for (name, value) in self
            .build_config
            .build_args
            .iter()
            .chain(self.build_config.build_args_buildkit.iter())
        {
            let sendable = name == CACHE_MOUNT_NAMESPACE_ARG
                || (self.worker_build_arg_keys.contains(name)
                    && is_worker_build_arg_name(name)
                    && !matches!(name.as_str(), "NPM_TOKEN" | "NPM_RC"));
            if sendable {
                split.sent.insert(name.clone(), value.clone());
            } else {
                split.sent.remove(name);
                withheld.insert(name.clone());
            }
        }
        split.withheld = withheld.into_iter().collect();
        split
    }

    /// Detect log level from message content
    fn detect_log_level(message: &str) -> LogLevel {
        // The deployer prefixes its own failure lines with `ERROR:`. Check
        // that before the substring heuristics below, which would otherwise
        // file a daemon error that says "did not complete successfully"
        // under `success`.
        if message.trim_start().starts_with("ERROR:") {
            return LogLevel::Error;
        }
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌")
            || message.contains("Failed")
            || message.contains("Error")
            || message.contains("error")
        {
            LogLevel::Error
        } else if message.contains("⏳")
            || message.contains("Waiting")
            || message.contains("warning")
        {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }

    /// Load and parse .temps.yaml from the build context directory.
    /// Returns None if the file does not exist or cannot be parsed.
    fn load_temps_config(
        &self,
        build_context_dir: &Path,
    ) -> Result<Option<TempsConfig>, WorkflowError> {
        let config_path = build_context_dir.join(".temps.yaml");
        let Some(contents) =
            read_confined_control_file(build_context_dir, &config_path, 1024 * 1024)?
        else {
            return Ok(None);
        };
        TempsConfig::from_yaml(&contents)
            .map(Some)
            .map_err(|error| {
                WorkflowError::JobValidationFailed(format!(
                    "Invalid .temps.yaml at '{}': {error}",
                    config_path.display()
                ))
            })
    }

    async fn ensure_dockerfile(
        &self,
        context: &WorkflowContext,
        build_context_dir: &PathBuf,
        dockerfile_path: &PathBuf,
        source_root: &Path,
    ) -> Result<GeneratedDockerfile, WorkflowError> {
        // If Dockerfile exists, we're done (no preset build args)
        if fs::symlink_metadata(dockerfile_path).is_ok() {
            return Ok(GeneratedDockerfile {
                build_args: HashMap::new(),
                build_root: build_context_dir.clone(),
                generated: false,
            });
        }

        // Resolve the canonical stored preset and typed config, or auto-detect
        // a catalog preset when the job did not receive an explicit selection.
        let preset = if let Some(stored_preset) = self.preset {
            let runtime_slug =
                temps_presets::runtime_slug(stored_preset, self.preset_config.as_ref());
            self.log(
                context,
                format!(
                    "Dockerfile not found, generating from preset: {}",
                    runtime_slug
                ),
            )
            .await?;

            temps_presets::get_preset_for_storage(stored_preset, self.preset_config.as_ref())
                .map_err(|error| WorkflowError::JobExecutionFailed(error.to_string()))?
                .ok_or_else(|| {
                    WorkflowError::JobExecutionFailed(format!(
                        "No build preset registered for stored preset '{}'",
                        stored_preset
                    ))
                })?
        } else {
            self.log(
                context,
                "No preset specified, auto-detecting project type...".to_string(),
            )
            .await?;

            // Read directory to get list of files
            let files: Vec<String> = fs::read_dir(build_context_dir)
                .map_err(WorkflowError::IoError)?
                .filter_map(|entry| {
                    entry
                        .ok()
                        .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
                })
                .collect();

            // Try to read package.json for more accurate detection
            let package_json_path = build_context_dir.join("package.json");
            let package_json_content =
                read_confined_control_file(build_context_dir, &package_json_path, 5 * 1024 * 1024)?;

            // Check for Create React App by looking for react-scripts in package.json
            let detected_slug = if let Some(content) = &package_json_content {
                if content.contains("\"react-scripts\"") {
                    self.log(
                        context,
                        "Detected project type: react-app (found react-scripts in package.json)"
                            .to_string(),
                    )
                    .await?;
                    "react-app".to_string()
                } else {
                    // Fall back to file-based detection
                    let detected_preset = temps_presets::detect_preset_from_files(&files)
                        .ok_or_else(|| {
                            WorkflowError::JobExecutionFailed(
                                format!("Could not auto-detect project type from files: {:?}. Please specify a preset explicitly.",
                                files.iter().take(5).collect::<Vec<_>>())
                            )
                        })?;

                    let slug = detected_preset.slug().to_string();
                    self.log(context, format!("Detected project type: {}", slug))
                        .await?;
                    slug
                }
            } else {
                // No package.json, use file-based detection
                let detected_preset = temps_presets::detect_preset_from_files(&files)
                    .ok_or_else(|| {
                        WorkflowError::JobExecutionFailed(
                            format!("Could not auto-detect project type from files: {:?}. Please specify a preset explicitly.",
                            files.iter().take(5).collect::<Vec<_>>())
                        )
                    })?;

                let slug = detected_preset.slug().to_string();
                self.log(context, format!("Detected project type: {}", slug))
                    .await?;
                slug
            };

            temps_presets::get_preset_by_slug(&detected_slug).ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Unknown detected preset: {}",
                    detected_slug
                ))
            })?
        };

        let preset_slug = preset.slug();
        let preset_root = preset_build_root(&preset_slug, source_root, build_context_dir)?;
        if preset_root != *build_context_dir {
            self.log(context, format!("Workspace build context: {}; selected application: {}. Retaining root configuration and sibling packages.", preset_root.display(), build_context_dir.display())).await?;
        }

        // Convert build args to build_vars format (Vec<String> of "KEY" for ARG directives)
        let mut build_vars: Vec<String> = self
            .build_config
            .build_args
            .iter()
            .map(|(key, _)| key.clone())
            .collect();

        // A worker build receives the values of the variables that are not
        // secret; the generated Dockerfile declares exactly those, so it
        // never asks a worker for a value that stays on the control plane.
        if let Some(node_id) = self.remote_builder_node_id {
            let split = self.worker_build_args();
            build_vars = split.variable_names();
            self.log_worker_build_args(context, node_id, &split).await?;
        }

        // Get repository output to extract repo name for project slug
        let repo_output = RepositoryOutput::from_context(context, &self.download_job_id)?;

        // Use repo name as project slug (sanitized: lowercase, hyphens to underscores)
        let project_slug = repo_output.repo_name.replace("-", "_").to_lowercase();

        // Build overrides, most specific first: the repository's .temps.yaml,
        // then the project's stored preset settings, then whatever the preset
        // detects on its own (None here).
        let temps_config = self.load_temps_config(build_context_dir)?;
        let repository_overrides = temps_config
            .as_ref()
            .and_then(|c| c.build.as_ref())
            .map(|build| BuildOverrides {
                install_command: build.install_command.clone(),
                build_command: build.build_command.clone(),
                output_dir: build.output_dir.clone(),
            })
            .unwrap_or_default();
        let project_overrides = self
            .preset_config
            .as_ref()
            .map(BuildOverrides::from_stored)
            .unwrap_or_default();
        let effective_overrides = repository_overrides.clone().or(project_overrides.clone());
        effective_overrides.validate()?;

        if !repository_overrides.is_empty() {
            self.log(
                context,
                format!(
                    "Found .temps.yaml build overrides: install={:?}, build={:?}, output_dir={:?}",
                    repository_overrides.install_command,
                    repository_overrides.build_command,
                    repository_overrides.output_dir
                ),
            )
            .await?;
        }
        if !project_overrides.is_empty() {
            self.log(
                context,
                format!(
                    "Project build settings: install={:?}, build={:?}, output_dir={:?} \
                     (.temps.yaml values take precedence)",
                    project_overrides.install_command,
                    project_overrides.build_command,
                    project_overrides.output_dir
                ),
            )
            .await?;
        }
        let install_cmd_owned = effective_overrides.install_command;
        let build_cmd_owned = effective_overrides.build_command;
        let output_dir_owned = effective_overrides.output_dir;

        // Generate Dockerfile content with build args and .temps.yaml overrides
        // Workspace installs need the root lockfile and sibling packages.
        let mut dockerfile_with_args = preset
            .dockerfile(temps_presets::DockerfileConfig {
                root_local_path: &preset_root,
                local_path: build_context_dir,
                install_command: install_cmd_owned.as_deref(),
                build_command: build_cmd_owned.as_deref(),
                output_dir: output_dir_owned.as_deref(),
                build_vars: Some(&build_vars), // ARG directives for env vars
                project_slug: &project_slug,
                use_buildkit: true, // Enable BuildKit for faster builds and caching
            })
            .await;

        // Non-fatal planning findings (e.g. nixpacks.toml settings autopack
        // could not translate) belong in the deployment log, not only in the
        // server's tracing output.
        for warning in &dockerfile_with_args.warnings {
            self.log_with_level(
                context,
                LogLevel::Warning,
                format!("Build warning: {warning}"),
            )
            .await?;
        }

        // The preset already knows this build cannot succeed. Stop here with
        // its explanation instead of running an image build that can only end
        // in a generic failure minutes later.
        if let Some(failure) = dockerfile_with_args.plan_failure.as_ref() {
            let message = failure.to_string();
            self.log_with_level(context, LogLevel::Error, format!("ERROR: {message}"))
                .await?;
            return Err(WorkflowError::JobExecutionFailed(message));
        }

        // Route implicit Docker Hub base images (`FROM node:22-slim`) through
        // the operator's configured registry mirror/prefix, if any. Only
        // applies to preset-generated Dockerfiles -- an existing Dockerfile
        // in the repo never reaches this function at all (see
        // `ensure_dockerfile`'s early return above), so a user's own FROM
        // lines are never rewritten out from under them.
        if let Some(prefix) = self
            .registry_mirror_prefix
            .as_deref()
            .filter(|p| !p.is_empty())
        {
            dockerfile_with_args.content =
                temps_presets::apply_registry_prefix(&dockerfile_with_args.content, prefix);
        }

        // Write the Dockerfile
        write_no_follow(
            dockerfile_path,
            dockerfile_with_args.content.as_bytes(),
            true,
        )?;

        if preset_root != *build_context_dir && preset_slug != "nextjs" {
            write_workspace_ignore(&preset_root, dockerfile_path)?;
        }

        self.log(
            context,
            format!(
                "Generated Dockerfile at: {} ({} build args from preset)",
                dockerfile_path.display(),
                dockerfile_with_args.build_args.len()
            ),
        )
        .await?;

        // Return the preset build args so the caller can merge them
        Ok(GeneratedDockerfile {
            build_args: dockerfile_with_args.build_args,
            build_root: preset_root,
            generated: true,
        })
    }

    /// A repository's own Dockerfile on a worker: refuse when one of its
    /// `ARG`s would read a value that stays on the control plane, rather than
    /// build an image that silently lacks it, and otherwise say which values
    /// it receives. Names only.
    async fn check_worker_dockerfile_args(
        &self,
        context: &WorkflowContext,
        node_id: i32,
        dockerfile_path: &Path,
        split: &WorkerBuildArgs,
    ) -> Result<(), WorkflowError> {
        let nodes = self.worker_nodes_phrase(node_id);
        let plural = self.is_per_architecture_worker_build();
        use temps_deployer::build_protocol::{
            dockerfile_build_arg_names, dockerfile_reads_build_arg,
        };
        let contents = fs::read_to_string(dockerfile_path).map_err(WorkflowError::IoError)?;
        let shown = self
            .build_config
            .dockerfile_path
            .as_deref()
            .unwrap_or("Dockerfile");
        let declared = dockerfile_build_arg_names(&contents);
        let message = match &declared {
            Some(declared) => {
                let lost: Vec<String> = split
                    .withheld
                    .iter()
                    .filter(|name| dockerfile_reads_build_arg(declared, name))
                    .cloned()
                    .collect();
                let one = lost.len() == 1;
                (!lost.is_empty()).then(|| {
                    format!(
                        "Worker builds cannot pass secret values: '{shown}' declares ARG {}, \
                         but {} a secret, a platform credential or a BuildKit setting, which \
                         never leaves the control plane, so this build on {nodes} would \
                         silently lose {}. Read {} when the container runs \
                         instead of at build time, recreate {} as a non-secret variable if {} \
                         not sensitive, or, on a server that builds locally, set this \
                         environment's build location to the control plane.",
                        name_list(&lost),
                        if one { "it is" } else { "they are each" },
                        if one { "it" } else { "them" },
                        if one { "it" } else { "them" },
                        if one { "it" } else { "them" },
                        if one { "it is" } else { "they are" },
                    )
                })
            }
            None if split
                .withheld
                .iter()
                .any(|name| !name.to_ascii_uppercase().starts_with("BUILDKIT_")) =>
            {
                Some(format!(
                    "Worker builds cannot tell which build arguments '{shown}' reads: an ARG \
                     instruction names a variable reference, and secret values never leave \
                     the control plane. Name each ARG literally, or, on a server that builds \
                     locally, set this environment's build location to the control plane."
                ))
            }
            None => None,
        };
        if let Some(message) = message {
            self.log_with_level(context, LogLevel::Error, format!("ERROR: {message}"))
                .await?;
            return Err(WorkflowError::JobExecutionFailed(message));
        }
        let received: Vec<String> = split
            .variable_names()
            .into_iter()
            .filter(|name| {
                declared
                    .as_ref()
                    .is_none_or(|declared| dockerfile_reads_build_arg(declared, name))
            })
            .collect();
        if !received.is_empty() {
            self.log(
                context,
                format!(
                    "Build arguments: {nodes} {} {} value(s) for the Dockerfile's ARG \
                     instructions over {} authenticated connection: {}",
                    if plural { "each receive" } else { "receives" },
                    received.len(),
                    if plural { "its own" } else { "its" },
                    name_list(&received)
                ),
            )
            .await?;
        }
        Ok(())
    }

    /// Say in the build log what a generated build on a worker receives, and
    /// what it does not. Names only: no value reaches a log.
    async fn log_worker_build_args(
        &self,
        context: &WorkflowContext,
        node_id: i32,
        split: &WorkerBuildArgs,
    ) -> Result<(), WorkflowError> {
        let nodes = self.worker_nodes_phrase(node_id);
        let plural = self.is_per_architecture_worker_build();
        let sent = split.variable_names();
        if !sent.is_empty() {
            self.log(
                context,
                format!(
                    "Build arguments: this build runs on {nodes}, which {} {} variable(s) \
                     over {} authenticated connection: {}",
                    if plural { "each receive" } else { "receives" },
                    sent.len(),
                    if plural { "its own" } else { "its" },
                    name_list(&sent)
                ),
            )
            .await?;
        }
        let withheld: Vec<String> = split
            .withheld
            .iter()
            .filter(|name| !name.to_ascii_uppercase().starts_with("BUILDKIT_"))
            .cloned()
            .collect();
        if withheld.is_empty() {
            return Ok(());
        }
        // Every build carries the platform's own credentials (the deployment
        // token, the cron secret), so this line is routine, not a warning.
        self.log_with_level(
            context,
            LogLevel::Info,
            format!(
                "Build note: {} variable(s) are secrets or platform credentials, which never \
                 leave the control plane. They are set when the container runs, but not while \
                 this build runs on {nodes}: {}",
                withheld.len(),
                name_list(&withheld)
            ),
        )
        .await?;
        let inlined = build_inlined_variables(withheld.iter().map(String::as_str));
        if !inlined.is_empty() {
            let one = inlined.len() == 1;
            self.log_with_level(
                context,
                LogLevel::Warning,
                format!(
                    "Build warning: {} {} inlined into the application at build time, but {} \
                     secret, so {} will be empty in this build. A value the browser bundle needs \
                     is public anyway: recreate {} as a non-secret variable, or, on a server that \
                     builds locally, set this environment's build location to the control plane.",
                    name_list(&inlined),
                    if one { "is" } else { "are" },
                    if one { "it is" } else { "they are" },
                    if one { "it" } else { "they" },
                    if one { "it" } else { "them" },
                ),
            )
            .await?;
        }
        Ok(())
    }

    /// Write a `.npmrc` file into the build context when the user provides
    /// `NPM_RC` or `NPM_TOKEN` as a build env var.
    ///
    /// Matches the Vercel behavior: `NPM_RC` wins over `NPM_TOKEN` when both
    /// are set. The file contents are never logged; only the source env var
    /// name ("NPM_RC" or "NPM_TOKEN") is logged.
    async fn ensure_npmrc(
        &self,
        context: &WorkflowContext,
        build_context_dir: &Path,
    ) -> Result<(), WorkflowError> {
        // `build_config.build_args` is the flat map of user env vars / build args
        // that workflow_planner.rs assembles for this job.
        let env: HashMap<String, String> = self
            .build_config
            .build_args
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let Some(plan) = super::npmrc::plan_npmrc(&env) else {
            return Ok(());
        };

        if self.remote_builder_node_id.is_some() {
            return Err(WorkflowError::JobValidationFailed(
                "Worker builds cannot generate or transfer NPM_TOKEN/NPM_RC credentials; deploy a prebuilt registry image until build-secret handling is supported".into(),
            ));
        }

        let npmrc_path = build_context_dir.join(".npmrc");
        if let Ok(metadata) = fs::symlink_metadata(&npmrc_path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(WorkflowError::JobValidationFailed(format!(
                    ".npmrc '{}' must be a regular non-symlink file",
                    npmrc_path.display()
                )));
            }
        }
        write_no_follow(&npmrc_path, plan.contents.as_bytes(), false).map_err(|e| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to write .npmrc to {}: {}",
                npmrc_path.display(),
                e
            ))
        })?;

        self.log(
            context,
            format!(
                "Generated .npmrc from {} env var at {}",
                plan.source.as_str(),
                npmrc_path.display()
            ),
        )
        .await?;

        Ok(())
    }

    /// Build the container image with real-time logging
    async fn build_image(
        &self,
        repo_output: &RepositoryOutput,
        context: &WorkflowContext,
    ) -> Result<ImageOutput, WorkflowError> {
        self.log(
            context,
            format!("Starting image build for {}", self.image_tag),
        )
        .await?;

        // Determine build context first (needed for Dockerfile path)
        let build_context = if let Some(ref context_path) = self.build_config.build_context {
            let context_path = Path::new(context_path);
            if context_path.is_absolute()
                || context_path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                return Err(WorkflowError::JobValidationFailed(format!(
                    "Build context '{}' must be relative and contained by the source root",
                    context_path.display()
                )));
            }
            repo_output.repo_dir.join(context_path)
        } else {
            repo_output.repo_dir.clone()
        };

        let canonical_root = repo_output
            .repo_dir
            .canonicalize()
            .map_err(WorkflowError::IoError)?;
        let canonical_context = build_context.canonicalize().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                // Name the path instead of a bare "No such file or directory".
                WorkflowError::JobValidationFailed(format!(
                    "Invalid configuration: build context '{}' is not in the checked-out source \
                     at '{}'. Check the project's root directory and build context settings",
                    self.build_config.build_context.as_deref().unwrap_or("."),
                    repo_output.repo_dir.display()
                ))
            } else {
                WorkflowError::IoError(error)
            }
        })?;
        if !canonical_context.starts_with(&canonical_root) {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Build context '{}' escapes source root '{}'",
                canonical_context.display(),
                canonical_root.display()
            )));
        }

        let mut build_context = canonical_context.clone();

        // Determine dockerfile path relative to build context
        let dockerfile_relative = self
            .build_config
            .dockerfile_path
            .as_deref()
            .map(Path::new)
            .unwrap_or_else(|| Path::new("Dockerfile"));
        validate_relative_build_path(dockerfile_relative, "Dockerfile path")?;
        let dockerfile_path = build_context.join(dockerfile_relative);
        let dockerfile_parent = dockerfile_path.parent().ok_or_else(|| {
            WorkflowError::JobValidationFailed("Dockerfile path has no parent".to_string())
        })?;
        let canonical_parent = dockerfile_parent
            .canonicalize()
            .map_err(WorkflowError::IoError)?;
        if !canonical_parent.starts_with(&canonical_context) {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Dockerfile parent '{}' escapes build context '{}'",
                canonical_parent.display(),
                canonical_context.display()
            )));
        }
        if fs::symlink_metadata(&dockerfile_path).is_ok() {
            validate_confined_regular_file(&canonical_context, &dockerfile_path)?;
        }

        self.log(
            context,
            format!("Using Dockerfile: {}", dockerfile_path.display()),
        )
        .await?;

        // Ensure Dockerfile exists (generate from preset if needed)
        // This returns build args from the preset
        let dockerfile = self
            .ensure_dockerfile(context, &build_context, &dockerfile_path, &canonical_root)
            .await?;
        build_context = dockerfile.build_root;

        // Write .npmrc into the build context when NPM_RC / NPM_TOKEN env vars
        // are provided (Vercel-compatible behavior). No-op otherwise.
        self.ensure_npmrc(context, &build_context).await?;

        // A worker build receives only the values that may leave the control
        // plane; a local build receives every value, as it always has.
        let (mut build_args, buildkit_args) = match self.remote_builder_node_id {
            Some(node_id) => {
                let split = self.worker_build_args();
                if !dockerfile.generated {
                    self.check_worker_dockerfile_args(context, node_id, &dockerfile_path, &split)
                        .await?;
                }
                (split.sent.into_iter().collect::<Vec<_>>(), Vec::new())
            }
            None => (
                self.build_config.build_args.clone(),
                self.build_config.build_args_buildkit.clone(),
            ),
        };

        // Merge preset build args with user-provided build args. User-provided
        // args take precedence, including one a worker does not receive: a
        // preset default never stands in for a withheld value.
        let user_arg_keys: std::collections::HashSet<String> = self
            .build_config
            .build_args
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        for (key, value) in dockerfile.build_args {
            if !user_arg_keys.contains(&key) {
                build_args.push((key, value));
            }
        }

        self.log(
            context,
            format!("Build context: {}", build_context.display()),
        )
        .await?;

        // Create a temporary log file for the build
        let log_path = std::env::temp_dir().join(format!("build_{}.log", self.job_id));

        // Build the image using ImageBuilder trait
        self.log(context, "Building container image...".to_string())
            .await?;

        let build_args: HashMap<String, String> = build_args.into_iter().collect();

        let build_args_buildkit: HashMap<String, String> = buildkit_args.into_iter().collect();

        // Create log callback to stream Docker build output to job logs with structured logging
        let log_service = self.log_service.clone();
        let log_id = self.log_id.clone();
        let log_callback: Option<temps_deployer::LogCallback> =
            if let (Some(log_svc), Some(log_id_str)) = (log_service, log_id) {
                Some(std::sync::Arc::new(move |line: String| {
                    let log_svc_clone = log_svc.clone();
                    let log_id_clone = log_id_str.clone();
                    Box::pin(async move {
                        // Detect log level from Docker build output
                        let level = Self::detect_log_level(&line);
                        let _ = log_svc_clone
                            .append_structured_log(&log_id_clone, level, line)
                            .await;
                    })
                }))
            } else {
                None
            };

        // A mixed-architecture worker build: every platform on its own
        // worker, natively and in parallel. Every other build -- including
        // every single-architecture one -- takes the loop below unchanged.
        if self.is_per_architecture_worker_build() {
            let built = self
                .build_on_platform_workers(
                    context,
                    &build_context,
                    &dockerfile_path,
                    &build_args,
                    &build_args_buildkit,
                )
                .await?;
            return Ok(ImageOutput {
                image_tag: built.primary.image_name,
                image_id: built.primary.image_id,
                size_bytes: built.primary.size_bytes,
                build_context,
                dockerfile_path,
                image_tags_by_platform: built.image_tags_by_platform,
                builder_node_ids_by_platform: built.builder_node_ids_by_platform,
                image_ids_by_platform: built.image_ids_by_platform,
                image_config: None,
            });
        }

        // One build per requested platform. The empty case (`None` platform)
        // is the single-architecture path every existing deployment takes.
        let platforms: Vec<Option<String>> = if self.build_config.target_platforms.is_empty() {
            vec![None]
        } else {
            self.build_config
                .target_platforms
                .iter()
                .map(|p| Some(p.clone()))
                .collect()
        };

        let mut image_tags_by_platform: HashMap<String, String> = HashMap::new();
        let mut primary: Option<temps_deployer::BuildResult> = None;

        for (index, platform) in platforms.iter().enumerate() {
            // The first platform owns the plain tag; the rest are suffixed so
            // several architectures can coexist in one Docker image store
            // without a registry or manifest list.
            let tag = match platform {
                Some(platform) if index > 0 => format!(
                    "{}-{}",
                    self.image_tag,
                    temps_deployer::platform::platform_tag_suffix(platform)
                ),
                _ => self.image_tag.clone(),
            };

            if let Some(platform) = platform {
                self.log(
                    context,
                    format!("Building '{}' for platform {}...", tag, platform),
                )
                .await?;
            }

            let build_request = BuildRequest {
                cache_from: self.build_config.cache_from.clone(),
                image_name: tag.clone(),
                context_path: build_context.clone(),
                dockerfile_path: Some(dockerfile_path.clone()),
                build_args: platform_build_args(&build_args, index, platform.as_deref()),
                build_args_buildkit: platform_build_args(
                    &build_args_buildkit,
                    index,
                    platform.as_deref(),
                ),
                platform: platform.clone(),
                log_path: log_path.clone(),
            };

            let build_request_with_callback = temps_deployer::BuildRequestWithCallback {
                request: build_request,
                log_callback: log_callback.clone(),
            };

            let build_result = match self
                .image_builder
                .build_image_with_callback(build_request_with_callback)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    // The *daemon's* platform, not the binary's: with a
                    // cross-architecture DOCKER_HOST those differ, and using
                    // the binary's would either recommend QEMU for a
                    // daemon-native build or omit that advice for a real
                    // cross-build.
                    let build_host_platform = self.image_builder.get_native_platform();
                    let message =
                        Self::describe_build_failure(platform.as_deref(), &build_host_platform, &e);

                    // Only the primary build is fatal. A secondary platform
                    // failing — most often because QEMU isn't installed for it
                    // — must not take the whole cluster's deployments down: it
                    // simply isn't in `image_tags_by_platform`, so the
                    // scheduler's architecture filter excludes those nodes and
                    // the deployment proceeds on the rest (or fails with a
                    // clean `NoCompatibleNode` if there is no rest).
                    if index == 0 {
                        self.log(context, format!("ERROR: {}", message)).await?;
                        return Err(WorkflowError::JobExecutionFailed(message));
                    }

                    self.log(
                        context,
                        format!(
                            "WARNING: {} Nodes running {} will be excluded from this deployment.",
                            message,
                            platform.as_deref().unwrap_or("that platform")
                        ),
                    )
                    .await?;
                    continue;
                }
            };

            self.log(
                context,
                format!(
                    "Image built successfully: {} ({})",
                    build_result.image_name, build_result.image_id
                ),
            )
            .await?;
            self.log(
                context,
                format!(
                    "📊 Image size: {} MB",
                    build_result.size_bytes / (1024 * 1024)
                ),
            )
            .await?;
            self.log(
                context,
                format!("Build time: {} ms", build_result.build_duration_ms),
            )
            .await?;

            if let Some(platform) = platform {
                // Same rule: a mislabelled secondary image drops its platform
                // rather than failing every deployment in the cluster.
                match self
                    .verify_built_platform(
                        self.image_builder.as_ref(),
                        &build_result.image_name,
                        platform,
                        context,
                    )
                    .await
                {
                    Ok(_) => {
                        image_tags_by_platform.insert(
                            temps_deployer::platform::canonicalize_platform(platform),
                            build_result.image_name.clone(),
                        );
                    }
                    Err(e) if index > 0 => {
                        // The tag exists but holds the wrong architecture.
                        // Leaving it behind would let a mislabelled image be
                        // picked up by hand later, so drop it — best-effort,
                        // since failing here would defeat the degradation.
                        if let Err(remove_err) = self
                            .image_builder
                            .remove_image(&build_result.image_name)
                            .await
                        {
                            tracing::debug!(
                                image = %build_result.image_name,
                                "Could not remove the mislabelled image: {}",
                                remove_err
                            );
                        }
                        self.log(
                            context,
                            format!(
                                "WARNING: {} Nodes running {} will be excluded from this \
                                 deployment.",
                                e, platform
                            ),
                        )
                        .await?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            if index == 0 {
                primary = Some(build_result);
            }
        }

        // `platforms` is never empty, so the first iteration always ran.
        let primary = primary.ok_or_else(|| {
            WorkflowError::JobExecutionFailed(
                "Internal error: image build produced no result".to_string(),
            )
        })?;

        Ok(ImageOutput {
            image_tag: primary.image_name,
            image_id: primary.image_id,
            size_bytes: primary.size_bytes,
            build_context,
            dockerfile_path,
            image_tags_by_platform,
            builder_node_ids_by_platform: HashMap::new(),
            image_ids_by_platform: HashMap::new(),
            image_config: None,
        })
    }

    /// The tag `platform_builders[index]` produces: the first platform owns
    /// the plain tag, the rest are `-<arch>` suffixed, exactly as the
    /// control plane's own multi-platform build names them.
    fn platform_image_tag(&self, index: usize, platform: &str) -> String {
        if index == 0 {
            self.image_tag.clone()
        } else {
            format!(
                "{}-{}",
                self.image_tag,
                temps_deployer::platform::platform_tag_suffix(platform)
            )
        }
    }

    /// A log callback that writes each build line under a `[platform]`
    /// prefix, so parallel builds stay readable in one job log.
    fn prefixed_log_callback(&self, platform: &str) -> Option<temps_deployer::LogCallback> {
        let (Some(log_service), Some(log_id)) = (self.log_service.clone(), self.log_id.clone())
        else {
            return None;
        };
        let prefix = format!("[{platform}] ");
        Some(Arc::new(move |line: String| {
            let log_service = log_service.clone();
            let log_id = log_id.clone();
            let prefix = prefix.clone();
            Box::pin(async move {
                let level = Self::detect_log_level(&line);
                let _ = log_service
                    .append_structured_log(&log_id, level, format!("{prefix}{line}"))
                    .await;
            })
        }))
    }

    /// Build every platform of a mixed-architecture deployment on its own
    /// worker, natively, in parallel.
    ///
    /// Unlike the control plane's own cross-build, no platform is optional
    /// here: a failure on any architecture fails the build, and the builds
    /// still running are stopped, so a deployment never rolls out on only
    /// some of its architectures. Nothing is emulated: each worker builds
    /// the architecture it runs, which [`Self::verify_built_platform`] then
    /// confirms from the image itself.
    async fn build_on_platform_workers(
        &self,
        context: &WorkflowContext,
        build_context: &Path,
        dockerfile_path: &Path,
        build_args: &HashMap<String, String>,
        build_args_buildkit: &HashMap<String, String>,
    ) -> Result<PerArchitectureBuild, WorkflowError> {
        use futures::stream::{FuturesUnordered, StreamExt};

        let plan: Vec<String> = self
            .platform_builders
            .iter()
            .map(|b| {
                format!(
                    "{} on worker '{}' (id={})",
                    b.platform, b.node_name, b.node_id
                )
            })
            .collect();
        self.log(
            context,
            format!(
                "Mixed-architecture deployment: building {} images in parallel, each natively on \
                 a worker of its own architecture: {}",
                plan.len(),
                plan.join("; ")
            ),
        )
        .await?;

        let mut builds: FuturesUnordered<_> = self
            .platform_builders
            .iter()
            .enumerate()
            .map(|(index, target)| async move {
                let result = self
                    .build_on_platform_worker(
                        context,
                        index,
                        target,
                        build_context,
                        dockerfile_path,
                        build_args,
                        build_args_buildkit,
                    )
                    .await;
                (index, result)
            })
            .collect();

        let mut results: Vec<Option<(temps_deployer::BuildResult, String)>> =
            (0..self.platform_builders.len()).map(|_| None).collect();
        let mut failure: Option<(usize, WorkflowError)> = None;
        while let Some((index, result)) = builds.next().await {
            match result {
                Ok(built) => results[index] = Some(built),
                Err(error) => {
                    failure = Some((index, error));
                    break;
                }
            }
        }
        // Dropping the remaining futures closes their build streams, which
        // stops those builds on their workers.
        drop(builds);

        if let Some((failed_index, error)) = failure {
            let failed = &self.platform_builders[failed_index];
            for (index, target) in self.platform_builders.iter().enumerate() {
                if index != failed_index && results[index].is_none() {
                    self.log_with_level(
                        context,
                        LogLevel::Warning,
                        format!(
                            "[{}] Build on worker '{}' stopped: the {} build failed",
                            target.platform, target.node_name, failed.platform
                        ),
                    )
                    .await?;
                }
            }
            self.log_with_level(
                context,
                LogLevel::Error,
                format!(
                    "ERROR: The {} image could not be built on worker '{}' (id={}), so this \
                     deployment is not rolled out on any architecture: {}",
                    failed.platform, failed.node_name, failed.node_id, error
                ),
            )
            .await?;
            return Err(error);
        }

        let mut image_tags_by_platform = HashMap::new();
        let mut builder_node_ids_by_platform = HashMap::new();
        let mut image_ids_by_platform = HashMap::new();
        let mut primary = None;
        for (index, (target, built)) in self.platform_builders.iter().zip(results).enumerate() {
            let built = built.ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Internal error: the {} build on worker '{}' (id={}) produced no result",
                    target.platform, target.node_name, target.node_id
                ))
            })?;
            let (built, image_id) = built;
            let platform = temps_deployer::platform::canonicalize_platform(&target.platform);
            image_tags_by_platform.insert(platform.clone(), built.image_name.clone());
            image_ids_by_platform.insert(platform.clone(), image_id);
            builder_node_ids_by_platform.insert(platform, target.node_id);
            if index == 0 {
                primary = Some(built);
            }
        }
        let primary = primary.ok_or_else(|| {
            WorkflowError::JobExecutionFailed(
                "Internal error: a mixed-architecture build had no platforms".to_string(),
            )
        })?;
        self.log(
            context,
            format!(
                "All {} architecture images built: {}",
                image_tags_by_platform.len(),
                plan.join("; ")
            ),
        )
        .await?;
        Ok(PerArchitectureBuild {
            primary,
            image_tags_by_platform,
            builder_node_ids_by_platform,
            image_ids_by_platform,
        })
    }

    /// One platform of [`Self::build_on_platform_workers`].
    #[allow(clippy::too_many_arguments)]
    async fn build_on_platform_worker(
        &self,
        context: &WorkflowContext,
        index: usize,
        target: &WorkerPlatformBuilder,
        build_context: &Path,
        dockerfile_path: &Path,
        build_args: &HashMap<String, String>,
        build_args_buildkit: &HashMap<String, String>,
    ) -> Result<(temps_deployer::BuildResult, String), WorkflowError> {
        let platform = target.platform.as_str();
        let tag = self.platform_image_tag(index, platform);
        self.log(
            context,
            format!(
                "[{platform}] Building '{tag}' on worker '{}' (id={})...",
                target.node_name, target.node_id
            ),
        )
        .await?;
        let request = temps_deployer::BuildRequestWithCallback {
            request: BuildRequest {
                cache_from: self.build_config.cache_from.clone(),
                image_name: tag.clone(),
                context_path: build_context.to_path_buf(),
                dockerfile_path: Some(dockerfile_path.to_path_buf()),
                build_args: platform_build_args(build_args, index, Some(platform)),
                build_args_buildkit: platform_build_args(
                    build_args_buildkit,
                    index,
                    Some(platform),
                ),
                platform: Some(platform.to_string()),
                log_path: std::env::temp_dir().join(format!(
                    "build_{}_{}.log",
                    self.job_id,
                    temps_deployer::platform::platform_tag_suffix(platform)
                )),
            },
            log_callback: self.prefixed_log_callback(platform),
        };
        let built = target
            .builder
            .build_image_with_callback(request)
            .await
            .map_err(|error| {
                let reason = Self::describe_build_failure(
                    Some(platform),
                    &target.builder.get_native_platform(),
                    &error,
                );
                WorkflowError::JobExecutionFailed(format!(
                    "{reason} (built on worker '{}', id={})",
                    target.node_name, target.node_id
                ))
            })?;
        // The id the worker reports for the image it just built: what every
        // later transfer of this tag, and every rollback reusing it, must
        // match before the image is run anywhere else.
        let image_id = self
            .verify_built_platform(
                target.builder.as_ref(),
                &built.image_name,
                platform,
                context,
            )
            .await?
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Cannot verify the {platform} image '{}' built on worker '{}' (id={})",
                    built.image_name, target.node_name, target.node_id
                ))
            })?;
        self.log(
            context,
            format!(
                "[{platform}] Image built on worker '{}': {} ({}), {} MB in {} ms",
                target.node_name,
                built.image_name,
                built.image_id,
                built.size_bytes / (1024 * 1024),
                built.build_duration_ms
            ),
        )
        .await?;
        Ok((built, image_id))
    }

    /// Confirm the image the daemon produced is actually for the platform we
    /// asked for.
    ///
    /// This is not paranoia: Docker's **legacy builder silently ignores**
    /// `platform`. It accepts the parameter, reports success, and hands back
    /// an image of the host's architecture — so a cross-build would produce
    /// `myapp:latest-arm64` containing amd64 binaries. Without this check the
    /// mislabelled image travels to the arm64 node and fails much later, with
    /// an error pointing at the node instead of at the build.
    ///
    /// BuildKit honours the platform correctly, so the fix is to enable it.
    ///
    /// An `inspect_image` that fails is not treated as a mismatch — some
    /// `ImageBuilder` implementations don't support inspection at all, and a
    /// missing check must not block an otherwise fine build.
    async fn verify_built_platform(
        &self,
        builder: &dyn ImageBuilder,
        image_name: &str,
        requested_platform: &str,
        context: &WorkflowContext,
    ) -> Result<Option<String>, WorkflowError> {
        let (built_platform, image_id) = match builder.inspect_image(image_name).await {
            Ok(info) => (info.platform, info.id),
            Err(e) if self.remote_builder_node_id.is_some() => {
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Cannot verify worker-built image '{image_name}' for {requested_platform}: {e}"
                )));
            }
            Err(e) => {
                tracing::debug!(
                    image = %image_name,
                    "Could not inspect the built image to verify its platform: {}",
                    e
                );
                return Ok(None);
            }
        };

        if temps_deployer::platform::platforms_match(&built_platform, requested_platform) {
            return Ok(Some(image_id));
        }

        let build_host = if self.remote_builder_node_id.is_some() {
            "the worker that built it"
        } else {
            "the control plane"
        };
        let msg = format!(
            "Build for {} produced a {} image ('{}'). The Docker daemon accepted the \
             requested platform but ignored it — this is what the legacy builder does. \
             Enable BuildKit on {} (Docker 23+ enables it by default; \
             otherwise set DOCKER_BUILDKIT=1 or install docker-buildx) and redeploy. \
             Deploying this image would fail on the target node with 'exec format error'.",
            requested_platform, built_platform, image_name, build_host
        );
        self.log(context, format!("ERROR: {}", msg)).await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

    /// Turn a build failure into a message that says what to do about it.
    ///
    /// Cross-architecture builds go through the daemon's `platform` option,
    /// which silently requires QEMU binfmt handlers on the build host. Without
    /// them the failure surfaces as `exec format error` from inside the build —
    /// a message that reads like a broken Dockerfile and sends people looking
    /// in the wrong place. When we asked for a platform the build host doesn't
    /// run natively, say so and give the one command that fixes it.
    ///
    /// `build_host_platform` must be the **daemon's** platform (from
    /// `ImageBuilder::get_native_platform`), not this process's: they differ
    /// whenever `DOCKER_HOST` points at another machine, and the advice would
    /// then be aimed at the wrong host.
    fn describe_build_failure(
        platform: Option<&str>,
        build_host_platform: &str,
        error: &temps_deployer::BuilderError,
    ) -> String {
        if let temps_deployer::BuilderError::BuildOutOfMemory { message, diagnosis } = error {
            return Self::describe_out_of_memory(platform, message, diagnosis);
        }
        if let Some(message) = Self::describe_registry_rate_limit(platform, error) {
            return message;
        }

        let Some(platform) = platform else {
            return format!("Failed to build image: {}", error);
        };

        if temps_deployer::platform::platforms_match(platform, build_host_platform) {
            return format!("Failed to build image for {}: {}", platform, error);
        }

        let text = error.to_string().to_lowercase();
        let looks_like_missing_emulation = text.contains("exec format error")
            || text.contains("no match for platform")
            || text.contains("cannot execute binary file")
            || text.contains("unknown operating system or architecture");

        if looks_like_missing_emulation {
            format!(
                "Failed to build image for {} on a {} host: {}. \
                 Cross-architecture builds need QEMU emulation registered on the build host. \
                 Install it with: docker run --privileged --rm tonistiigi/binfmt --install {}. \
                 Alternatively, restrict this environment to {} nodes via target nodes/labels.",
                platform,
                build_host_platform,
                error,
                temps_deployer::platform::platform_arch(platform),
                build_host_platform
            )
        } else {
            format!("Failed to build image for {}: {}", platform, error)
        }
    }

    /// Explain a build step that ran out of memory and what to do about it.
    ///
    /// The deployer already established the facts (`BuildMemoryDiagnosis`);
    /// this adds the part a self-hoster cannot see from the log: on a
    /// BuildKit host the per-build memory cap from Settings is not applied,
    /// so the step competes with everything else on the host and the kernel
    /// ends the largest process, usually a compiler or bundler, whose parent
    /// then exits with an ordinary status. Without this, the log ends in a
    /// bare `exit code: 1` after minutes of silence.
    fn describe_out_of_memory(
        platform: Option<&str>,
        message: &str,
        diagnosis: &temps_deployer::BuildMemoryDiagnosis,
    ) -> String {
        let scope = match platform {
            Some(platform) => format!(" for {}", platform),
            None => String::new(),
        };
        let cap_explanation = if diagnosis.cap_enforced {
            format!(
                "The per-build memory cap of {} MB (Settings > Build Limits) was reached. \
                 Raise it, or reduce the build's memory use.",
                diagnosis.requested_cap_mb
            )
        } else {
            "This host builds with BuildKit, which does not apply the per-build memory cap \
             from Settings > Build Limits, so a build can use all of the host's RAM; when the \
             host runs out, the kernel ends the largest process (usually the compiler or \
             bundler) and the build tool exits with a generic status."
                .to_string()
        };
        format!(
            "Failed to build image{scope}: {diagnosis}. {cap_explanation} Options: run this \
             build on a host with more RAM, lower the maximum number of concurrent builds, \
             reduce the build's memory use (for Node.js builds set the build variable \
             NODE_OPTIONS=--max-old-space-size=<MB>), or build the image in CI and deploy it \
             as an image (https://temps.sh/docs/set-up-ci-cd-pipeline). Underlying error: \
             {message}"
        )
    }

    /// Recognise a Docker Hub anonymous-pull rate limit and explain the fix.
    ///
    /// Autopack's generated Dockerfiles always reference unqualified base
    /// images (`FROM node:22-slim`, `FROM debian:bookworm-slim`), so the
    /// daemon resolves every one of them against `docker.io` with no
    /// credentials attached — Temps has no registry auth or mirror
    /// configuration of its own. Docker Hub throttles anonymous pulls per
    /// source IP, and that limit is shared across every build this host (or,
    /// on multi-node clusters, every node behind the same WAN IP) runs. The
    /// daemon reports this as a generic pull failure buried in build output;
    /// without translation it reads like a broken Dockerfile or a transient
    /// network blip and sends people looking in the wrong place.
    ///
    /// Returns `None` for any other failure so the caller falls through to
    /// its existing handling.
    fn describe_registry_rate_limit(
        platform: Option<&str>,
        error: &temps_deployer::BuilderError,
    ) -> Option<String> {
        let text = error.to_string().to_lowercase();
        let looks_like_rate_limit = text.contains("toomanyrequests")
            || text.contains("pull rate limit")
            || text.contains("429 too many requests")
            || (text.contains("failed to authorize") && text.contains("429"));

        if !looks_like_rate_limit {
            return None;
        }

        let scope = match platform {
            Some(platform) => format!(" for {}", platform),
            None => String::new(),
        };

        Some(format!(
            "Failed to build image{scope}: Docker Hub rate-limited an anonymous image pull ({error}). \
             This build host pulls base images from docker.io without authentication, and Docker \
             Hub throttles anonymous pulls by source IP — the limit is shared across every build \
             this host runs, and across every node behind the same WAN IP on a multi-node cluster. \
             Configure a registry mirror on this host's Docker daemon (the `registry-mirrors` option \
             in /etc/docker/daemon.json) and restart Docker — see \
             https://temps.sh/docs/configure-a-docker-registry-mirror for the steps, including how \
             to also raise the pull ceiling with an authenticated pull-through cache."
        ))
    }
}

#[async_trait]
impl WorkflowTask for BuildImageJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Build Image"
    }

    fn description(&self) -> &str {
        "Builds a container image from repository source code"
    }

    fn depends_on(&self) -> Vec<String> {
        vec![self.download_job_id.clone()]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Never fall through to the control plane's daemon when local
        // workloads are disabled. A remote builder must be selected first.
        if !self.local_workloads_enabled && self.remote_builder_node_id.is_none() {
            let message = "This control plane runs no builds; deploy from a registry image, \
                or run the full profile on a node with Docker"
                .to_string();
            self.log(&context, format!("ERROR: {}", message)).await?;
            return Err(WorkflowError::LocalWorkloadsDisabled(message));
        }

        if let Some(notice) = &self.build_location_notice {
            self.log(&context, notice.clone()).await?;
        }

        // Get typed output from the download job
        let repo_output = RepositoryOutput::from_context(&context, &self.download_job_id)?;

        // Build the image (logs written in real-time)
        let mut image_output = self.build_image(&repo_output, &context).await?;
        image_output.image_config = self
            .finish_worker_build(&context, &image_output.image_tag)
            .await?;

        // Set typed job outputs
        context.set_output(&self.job_id, "image_tag", &image_output.image_tag)?;
        context.set_output(&self.job_id, "image_id", &image_output.image_id)?;
        context.set_output(&self.job_id, "size_bytes", image_output.size_bytes)?;
        context.set_output(
            &self.job_id,
            "build_context",
            image_output.build_context.to_string_lossy().to_string(),
        )?;
        context.set_output(
            &self.job_id,
            "dockerfile_path",
            image_output.dockerfile_path.to_string_lossy().to_string(),
        )?;
        // Only meaningful for a multi-arch build; an empty map downstream means
        // "one tag covers everything", which is what single-arch builds want.
        context.set_output(
            &self.job_id,
            "image_tags_by_platform",
            &image_output.image_tags_by_platform,
        )?;
        context.set_output(&self.job_id, "builder_node_id", self.remote_builder_node_id)?;
        if let Some(config) = &image_output.image_config {
            context.set_output(&self.job_id, "image_config", config)?;
        }
        // Only a mixed-architecture worker build has more than one builder.
        if !image_output.builder_node_ids_by_platform.is_empty() {
            context.set_output(
                &self.job_id,
                "builder_node_ids_by_platform",
                &image_output.builder_node_ids_by_platform,
            )?;
            context.set_output(
                &self.job_id,
                "image_ids_by_platform",
                &image_output.image_ids_by_platform,
            )?;
        }

        // Read .temps.yaml health config and pass it to downstream jobs
        // The DeployImageJob will use this to configure its health check path
        let build_context_dir = &image_output.build_context;
        if let Some(temps_config) = self.load_temps_config(build_context_dir)? {
            if let Some(health) = &temps_config.health {
                context.set_output(&self.job_id, "health_check_path", &health.path)?;
                context.set_output(&self.job_id, "health_check_timeout", health.timeout)?;
            }
        }

        // Set artifacts
        context.set_artifact(
            &self.job_id,
            "container_image",
            PathBuf::from(&image_output.image_tag),
        );

        Ok(JobResult::success(context))
    }

    async fn execute_with_cancellation(
        &self,
        context: WorkflowContext,
        cancellation_provider: &dyn WorkflowCancellationProvider,
    ) -> Result<JobResult, WorkflowError> {
        let workflow_run_id = context.workflow_run_id.clone();

        // Check if already cancelled before starting
        if cancellation_provider.is_cancelled(&workflow_run_id).await? {
            if let (Some(log_service), Some(log_id)) = (&self.log_service, &self.log_id) {
                log_service
                    .log_warning(
                        log_id,
                        "Build cancelled before starting - deployment was cancelled by user",
                    )
                    .await
                    .ok();
            }
            return Err(WorkflowError::BuildCancelled);
        }

        // Create cancellation check future that polls every 2 seconds
        let cancellation_check = async {
            loop {
                sleep(Duration::from_secs(2)).await;

                match cancellation_provider.is_cancelled(&workflow_run_id).await {
                    Ok(true) => {
                        // Cancellation detected
                        return;
                    }
                    Ok(false) => {
                        // Continue checking
                    }
                    Err(_) => {
                        // Error checking cancellation - stop polling
                        break;
                    }
                }
            }
        };

        // Race between build execution and cancellation detection
        let build_future = self.execute(context.clone());

        tokio::select! {
            result = build_future => {
                // Build completed (success or failure)
                result
            }
            _ = cancellation_check => {
                // Cancellation detected during build
                if let (Some(log_service), Some(log_id)) = (&self.log_service, &self.log_id) {
                    log_service
                        .log_warning(
                            log_id,
                            "🚫 Docker build cancelled by user - stopping image build",
                        )
                        .await
                        .ok();
                }

                Err(WorkflowError::BuildCancelled)
            }
        }
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Verify that the download job output is available
        RepositoryOutput::from_context(context, &self.download_job_id)?;

        // Basic validation
        if self.image_tag.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "image_tag cannot be empty".to_string(),
            ));
        }
        if self.download_job_id.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "download_job_id cannot be empty".to_string(),
            ));
        }

        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Container images persist beyond job completion
        // Could implement cleanup logic here if needed (e.g., remove intermediate layers)
        Ok(())
    }
}

/// Builder for BuildImageJob
pub struct BuildImageJobBuilder {
    job_id: Option<String>,
    download_job_id: Option<String>,
    image_tag: Option<String>,
    build_config: BuildConfig,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    preset: Option<StoredPreset>,
    preset_config: Option<StoredPresetConfig>,
    registry_mirror_prefix: Option<String>,
    local_workloads_enabled: bool,
    remote_builder_node_id: Option<i32>,
    build_location_notice: Option<String>,
    control_plane_copy: Option<(Arc<dyn ImageBuilder>, String)>,
    worker_build_arg_keys: BTreeSet<String>,
    platform_builders: Vec<WorkerPlatformBuilder>,
}

impl BuildImageJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            download_job_id: None,
            image_tag: None,
            build_config: BuildConfig::default(),
            log_id: None,
            log_service: None,
            preset: None,
            preset_config: None,
            registry_mirror_prefix: None,
            local_workloads_enabled: true,
            remote_builder_node_id: None,
            build_location_notice: None,
            control_plane_copy: None,
            worker_build_arg_keys: BTreeSet::new(),
            platform_builders: Vec::new(),
        }
    }

    /// Build each platform natively on its own worker (a mixed-architecture
    /// deployment in the control-plane profile). The first builder owns the
    /// plain tag and replaces the builder passed to [`Self::build`].
    pub fn platform_builders(mut self, builders: Vec<WorkerPlatformBuilder>) -> Self {
        self.platform_builders = builders;
        self
    }

    /// Names of the build arguments whose values may be sent to a worker
    /// build (the planner's `worker_build_arg_keys`). See the field doc on
    /// `BuildImageJob`.
    pub fn worker_build_arg_keys(mut self, keys: impl IntoIterator<Item = String>) -> Self {
        self.worker_build_arg_keys = keys.into_iter().collect();
        self
    }

    /// Whether this process may build images locally. Defaults to `true` so
    /// every existing caller keeps building locally unless a control-plane
    /// profile explicitly disables it. See `BuildImageJob`'s field doc.
    pub fn local_workloads_enabled(mut self, enabled: bool) -> Self {
        self.local_workloads_enabled = enabled;
        self
    }

    pub fn remote_builder_node_id(mut self, node_id: i32) -> Self {
        self.remote_builder_node_id = Some(node_id);
        self
    }

    /// A line for the build log explaining where this build runs when that
    /// differs from the configured build location.
    pub fn build_location_notice(mut self, notice: impl Into<String>) -> Self {
        self.build_location_notice = Some(notice.into());
        self
    }

    /// Stream the image built on node `node_name` into `control_plane`
    /// once the build finishes (build location `node`).
    pub fn copy_image_to_control_plane(
        mut self,
        control_plane: Arc<dyn ImageBuilder>,
        node_name: impl Into<String>,
    ) -> Self {
        self.control_plane_copy = Some((control_plane, node_name.into()));
        self
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn download_job_id(mut self, download_job_id: String) -> Self {
        self.download_job_id = Some(download_job_id);
        self
    }

    pub fn image_tag(mut self, image_tag: String) -> Self {
        self.image_tag = Some(image_tag);
        self
    }

    pub fn dockerfile_path(mut self, dockerfile_path: String) -> Self {
        self.build_config.dockerfile_path = Some(dockerfile_path);
        self
    }

    pub fn build_context(mut self, build_context: String) -> Self {
        self.build_config.build_context = Some(build_context);
        self
    }

    pub fn build_args(mut self, build_args: Vec<(String, String)>) -> Self {
        self.build_config.build_args = build_args;
        self
    }

    pub fn build_args_buildkit(mut self, build_args_buildkit: Vec<(String, String)>) -> Self {
        self.build_config.build_args_buildkit = build_args_buildkit;
        self
    }

    pub fn target_platforms(mut self, target_platforms: Vec<String>) -> Self {
        self.build_config.target_platforms = target_platforms;
        self
    }

    pub fn cache_from(mut self, cache_from: Vec<String>) -> Self {
        self.build_config.cache_from = cache_from;
        self
    }

    pub fn log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    pub fn preset(mut self, preset: StoredPreset) -> Self {
        self.preset = Some(preset);
        self
    }

    pub fn preset_config(mut self, preset_config: Option<StoredPresetConfig>) -> Self {
        self.preset_config = preset_config;
        self
    }

    pub fn registry_mirror_prefix(mut self, registry_mirror_prefix: Option<String>) -> Self {
        self.registry_mirror_prefix = registry_mirror_prefix;
        self
    }

    pub fn build(
        self,
        image_builder: Arc<dyn ImageBuilder>,
    ) -> Result<BuildImageJob, WorkflowError> {
        let job_id = self.job_id.unwrap_or_else(|| "build_image".to_string());
        let download_job_id = self.download_job_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("download_job_id is required".to_string())
        })?;
        let image_tag = self.image_tag.ok_or_else(|| {
            WorkflowError::JobValidationFailed("image_tag is required".to_string())
        })?;

        let mut job = BuildImageJob::new(job_id, download_job_id, image_tag, image_builder)
            .with_build_config(self.build_config.clone());

        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }
        if let Some(preset) = self.preset {
            job = job.with_preset(preset);
        }
        job = job.with_preset_config(self.preset_config);
        job = job.with_registry_mirror_prefix(self.registry_mirror_prefix);
        job = job.with_local_workloads_enabled(self.local_workloads_enabled);
        if let Some(node_id) = self.remote_builder_node_id {
            job = job.with_remote_builder_node_id(node_id);
        }
        job = job
            .with_build_location_notice(self.build_location_notice)
            .with_control_plane_copy(self.control_plane_copy)
            .with_worker_build_arg_keys(self.worker_build_arg_keys)
            .with_platform_builders(self.platform_builders);

        Ok(job)
    }
}

impl Default for BuildImageJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// A stand-in worker agent for tests that drive a real `RemoteNodeDeployer`
/// upload end to end. Shared with the planner's tests, which feed it the
/// build arguments a real plan produces.
#[cfg(test)]
pub(crate) mod fake_worker {
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    use temps_deployer::BuildResult;

    /// What a stand-in worker received on `POST /agent/images/build`.
    #[derive(Default)]
    pub(crate) struct CapturedWorkerBuild {
        pub(crate) authorization: Option<String>,
        pub(crate) spec_json: String,
        pub(crate) spec: Option<temps_deployer::build_protocol::BuildSpec>,
        pub(crate) files: std::collections::BTreeMap<String, String>,
    }

    /// A running stand-in worker. Dropping it stops the server.
    pub(crate) struct FakeWorker {
        pub(crate) captured: Arc<Mutex<CapturedWorkerBuild>>,
        pub(crate) deployer: Arc<temps_deployer::remote::RemoteNodeDeployer>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for FakeWorker {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    /// Start a stand-in worker on a loopback port, and a real
    /// `RemoteNodeDeployer` (bearer `worker-token`) pointed at it.
    pub(crate) async fn spawn() -> FakeWorker {
        let captured = Arc::new(Mutex::new(CapturedWorkerBuild::default()));
        let app = axum::Router::new()
            .route("/agent/images/build", axum::routing::post(build))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let address = listener.local_addr().expect("local address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let deployer = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                format!("http://{address}"),
                "worker-token".to_string(),
                "worker-1".to_string(),
            )
            .expect("remote deployer"),
        );
        FakeWorker {
            captured,
            deployer,
            server,
        }
    }

    /// Applies the checks the real agent (`temps_agent::build_handler`)
    /// applies before handing the context to Docker: the spec must come first
    /// and pass `BuildSpec::validate`, the context must be a readable tar, and
    /// the spec's Dockerfile must be in it. Answers with the same NDJSON
    /// terminal event the agent streams.
    async fn build(
        axum::extract::State(captured): axum::extract::State<Arc<Mutex<CapturedWorkerBuild>>>,
        headers: axum::http::HeaderMap,
        mut multipart: axum::extract::Multipart,
    ) -> (axum::http::StatusCode, String) {
        use axum::http::StatusCode;
        let refuse = |message: String| (StatusCode::BAD_REQUEST, message);
        let authorization = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let Ok(Some(spec_field)) = multipart.next_field().await else {
            return refuse("expected spec field first".into());
        };
        if spec_field.name() != Some("spec") {
            return refuse("expected spec field first".into());
        }
        let Ok(spec_json) = spec_field.text().await else {
            return refuse("unreadable spec".into());
        };
        let spec: temps_deployer::build_protocol::BuildSpec = match serde_json::from_str(&spec_json)
        {
            Ok(spec) => spec,
            Err(error) => return refuse(format!("spec is not valid JSON: {error}")),
        };
        if let Err(message) = spec.validate() {
            return refuse(message);
        }
        let Ok(Some(context_field)) = multipart.next_field().await else {
            return refuse("expected context field after spec".into());
        };
        let Ok(archive) = context_field.bytes().await else {
            return refuse("unreadable context".into());
        };
        let mut files = std::collections::BTreeMap::new();
        let mut tar = tar::Archive::new(std::io::Cursor::new(archive.to_vec()));
        let Ok(entries) = tar.entries() else {
            return refuse("context is not a tar archive".into());
        };
        for entry in entries {
            let Ok(mut entry) = entry else {
                return refuse("corrupt context entry".into());
            };
            let Ok(path) = entry.path().map(|path| path.to_string_lossy().into_owned()) else {
                return refuse("unreadable context path".into());
            };
            let mut body = Vec::new();
            if entry.read_to_end(&mut body).is_err() {
                return refuse(format!("unreadable context file {path}"));
            }
            files.insert(path, String::from_utf8_lossy(&body).into_owned());
        }
        if !files.contains_key(&spec.dockerfile) {
            return refuse(format!(
                "Dockerfile '{}' is absent from the uploaded context",
                spec.dockerfile
            ));
        }
        let result = temps_deployer::build_protocol::BuildEvent::Result(BuildResult {
            image_id: "sha256:worker-built".to_string(),
            image_name: spec.image_name.clone(),
            size_bytes: 1,
            build_duration_ms: 1,
        });
        let event = match serde_json::to_string(&result) {
            Ok(event) => event,
            Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        };
        let Ok(mut captured) = captured.lock() else {
            return (StatusCode::INTERNAL_SERVER_ERROR, "poisoned".into());
        };
        captured.authorization = authorization;
        captured.spec_json = spec_json;
        captured.spec = Some(spec);
        captured.files = files;
        (StatusCode::OK, format!("{event}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::Path;

    use temps_deployer::{
        BuildRequest, BuildRequestWithCallback, BuildResult, BuilderError, ImageBuilder,
    };

    #[test]
    fn nextjs_workspace_context_is_limited_to_generated_workspace_builds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let app = root.join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        assert_eq!(preset_build_root("nextjs", root, &app).unwrap(), app);
        std::fs::write(root.join("turbo.json"), "{}").unwrap();
        assert_eq!(preset_build_root("nextjs", root, &app).unwrap(), root);
        assert_eq!(preset_build_root("autopack", root, &app).unwrap(), app);
        assert_eq!(preset_build_root("nextjs", root, root).unwrap(), root);
    }

    #[test]
    fn nextjs_plain_workspace_context_uses_root_lockfiles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let app = root.join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(root.join("pnpm-workspace.yaml"), "packages: [apps/*]").unwrap();
        assert_eq!(preset_build_root("nextjs", root, &app).unwrap(), root);
        std::fs::remove_file(root.join("pnpm-workspace.yaml")).unwrap();
        std::fs::write(root.join("package.json"), r#"{"workspaces":["apps/*"]}"#).unwrap();
        assert_eq!(preset_build_root("nextjs", root, &app).unwrap(), root);
    }

    #[cfg(unix)]
    #[test]
    fn nextjs_workspace_context_rejects_symlink_markers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let app = root.join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(root.join("outside.json"), "{}").unwrap();
        std::os::unix::fs::symlink(root.join("outside.json"), root.join("turbo.json")).unwrap();
        assert!(preset_build_root("nextjs", root, &app).is_err());
    }

    /// Nested Go/Cargo apps with sibling dependencies build from the
    /// repository root, only for presets that generate their Dockerfile.
    #[test]
    fn compiled_sibling_dependencies_use_repository_context_for_generated_presets() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let go_app = root.join("go/apps/api");
        let cargo_app = root.join("rust/apps/api");
        for (path, contents) in [
            ("go/apps/api/go.mod", "module a\nreplace b => ../../packages/shared\n"),
            ("go/packages/shared/go.mod", "module b\n"),
            (
                "rust/apps/api/Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nb = { path = \"../../packages/shared\" }\n",
            ),
            (
                "rust/packages/shared/Cargo.toml",
                "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
            ),
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        for (app, presets) in [
            (&go_app, ["go", "nixpacks-go", "nixpacks", "autopack"]),
            (
                &cargo_app,
                ["rust", "nixpacks-rust", "nixpacks", "autopack"],
            ),
        ] {
            for preset in presets {
                assert_eq!(
                    preset_build_root(preset, &root, app).unwrap(),
                    root,
                    "{preset}"
                );
            }
            assert_eq!(preset_build_root("dockerfile", &root, app).unwrap(), *app);
        }

        // A dependency leaving the repository is refused before any build.
        std::fs::write(
            go_app.join("go.mod"),
            "module a\nreplace b => ../../../../outside\n",
        )
        .unwrap();
        assert!(matches!(
            preset_build_root("go", &root, &go_app),
            Err(WorkflowError::JobValidationFailed(_))
        ));

        // The same broken go.mod beside a JavaScript application is never
        // read when the build is JavaScript: the application builds alone.
        std::fs::write(
            go_app.join("package.json"),
            r#"{"name":"web","scripts":{"start":"node server.js"}}"#,
        )
        .unwrap();
        for preset in ["nixpacks", "autopack", "nixpacks-node"] {
            assert_eq!(
                preset_build_root(preset, &root, &go_app).unwrap(),
                go_app,
                "{preset}"
            );
        }
        assert!(preset_build_root("go", &root, &go_app).is_err());
    }

    /// #1386: an Elixir umbrella child with an `in_umbrella` sibling builds
    /// from the repository root for the presets that generate its
    /// Dockerfile; the umbrella root and a standalone Mix app keep their own
    /// directory.
    #[test]
    fn elixir_umbrella_child_uses_repository_context_for_generated_presets() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let api = root.join("apps/api");
        for (path, contents) in [
            ("mix.exs", "defmodule U.MixProject do\n  use Mix.Project\n  def project, do: [apps_path: \"apps\", deps: []]\nend\n"),
            (
                "apps/api/mix.exs",
                "defmodule Api.MixProject do\n  use Mix.Project\n  def project, do: [app: :api, deps: [{:shared, in_umbrella: true}]]\nend\n",
            ),
            (
                "apps/shared/mix.exs",
                "defmodule Shared.MixProject do\n  use Mix.Project\n  def project, do: [app: :shared, deps: []]\nend\n",
            ),
            ("tools/worker/mix.exs", "defmodule W.MixProject do\n  use Mix.Project\n  def project, do: [app: :worker, deps: [{:jason, \"~> 1.4\"}]]\nend\n"),
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        for preset in ["nixpacks-elixir", "nixpacks", "autopack"] {
            assert_eq!(
                preset_build_root(preset, &root, &api).unwrap(),
                root,
                "{preset}"
            );
            let worker = root.join("tools/worker");
            assert_eq!(
                preset_build_root(preset, &root, &worker).unwrap(),
                worker,
                "{preset}"
            );
            assert_eq!(
                preset_build_root(preset, &root, &root).unwrap(),
                root,
                "{preset}"
            );
        }
        assert_eq!(preset_build_root("dockerfile", &root, &api).unwrap(), api);

        // A sibling that is not in the repository fails before any build.
        std::fs::remove_dir_all(root.join("apps/shared")).unwrap();
        assert!(matches!(
            preset_build_root("nixpacks-elixir", &root, &api),
            Err(WorkflowError::JobValidationFailed(_))
        ));
    }

    #[test]
    fn python_sibling_context_is_limited_to_generated_python_builds() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/api");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(repo.path().join("packages/shared")).unwrap();
        std::fs::write(app.join("requirements.txt"), "../../packages/shared").unwrap();
        for preset in ["python", "nixpacks-python", "nixpacks", "autopack"] {
            assert_eq!(
                preset_build_root(preset, repo.path(), &app).unwrap(),
                repo.path()
            );
        }
        assert_eq!(
            preset_build_root("dockerfile", repo.path(), &app).unwrap(),
            app
        );
        std::fs::write(app.join("requirements.txt"), "../../../outside").unwrap();
        assert!(preset_build_root("python", repo.path(), &app).is_err());
    }

    #[test]
    fn package_workspace_context_requires_membership_and_generated_presets() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/api");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        for workspaces in [
            serde_json::json!(["apps/*", "packages/*"]),
            serde_json::json!({"packages":["apps/*", "packages/*"]}),
        ] {
            std::fs::write(
                repo.path().join("package.json"),
                serde_json::json!({"workspaces":workspaces}).to_string(),
            )
            .unwrap();
            for preset in ["nixpacks-node", "nixpacks", "autopack"] {
                assert_eq!(
                    preset_build_root(preset, repo.path(), &app).unwrap(),
                    repo.path()
                );
            }
            assert_eq!(
                preset_build_root("dockerfile", repo.path(), &app).unwrap(),
                app
            );
        }
        std::fs::write(
            repo.path().join("package.json"),
            r#"{"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        for preset in ["nixpacks-node", "autopack"] {
            assert_eq!(preset_build_root(preset, repo.path(), &app).unwrap(), app);
        }
    }

    #[test]
    fn pnpm_nested_vite_and_node_use_workspace_context_only_for_generated_presets() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: [apps/*, packages/*]",
        )
        .unwrap();
        for preset in ["vite", "nixpacks-node", "nixpacks", "autopack"] {
            assert_eq!(
                preset_build_root(preset, repo.path(), &app).unwrap(),
                repo.path()
            );
        }
        for preset in ["dockerfile", "nixpacks-ruby", "nixpacks-php"] {
            assert_eq!(preset_build_root(preset, repo.path(), &app).unwrap(), app);
        }
        std::fs::remove_file(repo.path().join("pnpm-workspace.yaml")).unwrap();
        assert_eq!(preset_build_root("vite", repo.path(), &app).unwrap(), app);
    }

    #[test]
    fn pnpm_extglob_members_select_root_context_and_exclusions_keep_app_context() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: ['apps/@(web|api|private)', '!apps/@(private|internal)']",
        )
        .unwrap();
        for (name, member) in [
            ("web", true),
            ("api", true),
            ("private", false),
            ("mobile", false),
        ] {
            let app = repo.path().join("apps").join(name);
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(app.join("package.json"), "{}").unwrap();
            for preset in ["nextjs", "vite", "nixpacks-node", "autopack"] {
                assert_eq!(
                    preset_build_root(preset, repo.path(), &app).unwrap(),
                    if member {
                        repo.path().to_path_buf()
                    } else {
                        app.clone()
                    },
                    "{preset}: {name}"
                );
            }
        }
    }

    #[test]
    fn pnpm_nonmembers_and_excluded_apps_keep_their_selected_context() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("turbo.json"), "{}").unwrap();
        let manifestless_app = repo.path().join("apps/empty");
        std::fs::create_dir_all(&manifestless_app).unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: [apps/*]",
        )
        .unwrap();
        for preset in ["nextjs", "vite", "nixpacks-node", "autopack"] {
            assert_eq!(
                preset_build_root(preset, repo.path(), &manifestless_app).unwrap(),
                manifestless_app
            );
        }
        for (patterns, path) in [
            ("packages: [packages/*]", "apps/web"),
            ("packages: ['apps/*', '!apps/web']", "apps/web"),
            ("packages: ['!apps/web', 'apps/*']", "apps/web"),
            ("packages: ['apps/*']", "apps/nested/web"),
            ("sharedWorkspaceLockfile: true", "apps/web"),
            ("# empty config", "apps/web"),
        ] {
            let app = repo.path().join(path);
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(app.join("package.json"), "{}").unwrap();
            std::fs::write(repo.path().join("pnpm-workspace.yaml"), patterns).unwrap();
            for preset in ["nextjs", "vite", "nixpacks-node", "nixpacks", "autopack"] {
                assert_eq!(
                    preset_build_root(preset, repo.path(), &app).unwrap(),
                    app,
                    "{preset}: {patterns}"
                );
            }
        }
    }

    #[test]
    fn widened_workspace_context_keeps_ignore_rules_and_excludes_sibling_secrets() {
        use temps_deployer::build_protocol::DockerIgnore;
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            repo.path().join(".dockerignore"),
            "private-data\n!**/.env\n",
        )
        .unwrap();
        let dockerfile = app.join("Dockerfile");
        write_workspace_ignore(repo.path(), &dockerfile).unwrap();
        let contents = std::fs::read_to_string(app.join("Dockerfile.dockerignore")).unwrap();
        let rules = DockerIgnore::parse(&contents, "Dockerfile.dockerignore").unwrap();
        let root_rules = DockerIgnore::parse(
            &std::fs::read_to_string(repo.path().join(".dockerignore")).unwrap(),
            ".dockerignore",
        )
        .unwrap();
        assert!(contents.contains(temps_deployer::build_protocol::WORKSPACE_ROOT_IGNORE_MARKER));
        assert!(contents.contains("SPDX-License-Identifier: MIT OR Apache-2.0"));
        for path in [
            ".env",
            "apps/api/.env.production",
            "apps/web/.env.example",
            "packages/shared/node_modules/x.js",
            ".git/config",
            "private-data/secret",
        ] {
            assert!(
                rules.is_excluded(Path::new(path)) || root_rules.is_excluded(Path::new(path)),
                "included {path}"
            );
        }
        for path in [
            "pnpm-lock.yaml",
            "pnpm-workspace.yaml",
            "packages/shared/index.js",
        ] {
            assert!(!rules.is_excluded(Path::new(path)), "excluded {path}");
        }
        std::fs::write(app.join("Dockerfile.dockerignore"), "specific-private\n").unwrap();
        write_workspace_ignore(repo.path(), &dockerfile).unwrap();
        let contents = std::fs::read_to_string(app.join("Dockerfile.dockerignore")).unwrap();
        assert!(contents.starts_with("specific-private\n"));
    }

    // Mock ImageBuilder for testing
    struct MockImageBuilder;

    #[async_trait]
    impl ImageBuilder for MockImageBuilder {
        async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
            Ok(BuildResult {
                image_id: "sha256:test123".to_string(),
                image_name: request.image_name,
                size_bytes: 104857600, // 100MB
                build_duration_ms: 5000,
            })
        }

        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, BuilderError> {
            Ok("sha256:imported".to_string())
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &Path,
        ) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            Ok(vec!["test:latest".to_string()])
        }

        async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn build_image_with_callback(
            &self,
            request: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            // Delegate to regular build_image since we don't need callback in tests
            self.build_image(request.request).await
        }

        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, BuilderError> {
            Ok(temps_deployer::ImageInfo {
                id: "sha256:test123".to_string(),
                architecture: "amd64".to_string(),
                os: "linux".to_string(),
                platform: "linux/amd64".to_string(),
                size_bytes: 104857600,
                tags: vec!["test:latest".to_string()],
                created: None,
                working_dir: None,
                config: None,
            })
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &Path,
        ) -> Result<(), BuilderError> {
            Ok(())
        }

        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }

    /// An `ImageBuilder` that panics if any method is called, so a test can
    /// prove a code path never reaches the daemon at all.
    struct PanicsIfCalledImageBuilder;

    #[async_trait]
    impl ImageBuilder for PanicsIfCalledImageBuilder {
        async fn build_image(&self, _request: BuildRequest) -> Result<BuildResult, BuilderError> {
            panic!("ImageBuilder::build_image must not be called under a disabled local-workload policy");
        }

        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, BuilderError> {
            panic!("ImageBuilder::import_image must not be called under a disabled local-workload policy");
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &Path,
        ) -> Result<(), BuilderError> {
            panic!("ImageBuilder::extract_from_image must not be called under a disabled local-workload policy");
        }

        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            panic!("ImageBuilder::list_images must not be called under a disabled local-workload policy");
        }

        async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
            panic!("ImageBuilder::remove_image must not be called under a disabled local-workload policy");
        }

        async fn build_image_with_callback(
            &self,
            _request: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            panic!(
                "ImageBuilder::build_image_with_callback must not be called under a disabled \
                 local-workload policy"
            );
        }

        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, BuilderError> {
            panic!("ImageBuilder::inspect_image must not be called under a disabled local-workload policy");
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &Path,
        ) -> Result<(), BuilderError> {
            panic!("ImageBuilder::save_image must not be called under a disabled local-workload policy");
        }

        fn get_native_platform(&self) -> String {
            panic!("ImageBuilder::get_native_platform must not be called under a disabled local-workload policy");
        }
    }

    /// The control-plane profile (`local_workloads_enabled(false)`) must
    /// refuse a git-source build with a typed, actionable error *before*
    /// touching the download job's output or the image builder at all --
    /// worker-side builds are deferred to ADR-045
    /// (`docs/adr/045-worker-side-image-builds.md`); today this is a hard
    /// refusal, not a degraded attempt. Uses a context with no download-job
    /// output set and an `ImageBuilder` that panics if invoked, so either
    /// reaching past the guard fails the test immediately.
    #[tokio::test]
    async fn control_plane_profile_refuses_before_touching_the_image_builder() {
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .local_workloads_enabled(false)
            .build(Arc::new(PanicsIfCalledImageBuilder))
            .unwrap();

        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let error = job.execute(context).await.unwrap_err();
        match error {
            WorkflowError::LocalWorkloadsDisabled(message) => {
                assert!(message.contains("registry image"), "{message}");
                assert!(message.contains("full profile"), "{message}");
                assert!(message.contains("Docker"), "{message}");
            }
            other => panic!("expected LocalWorkloadsDisabled, got {other:?}"),
        }
    }

    /// Captures every line a job writes to its log.
    #[derive(Default)]
    struct CapturingLogWriter {
        lines: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl temps_core::LogWriter for CapturingLogWriter {
        async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
            self.lines
                .lock()
                .map_err(|error| WorkflowError::Other(error.to_string()))?
                .push(message);
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            1
        }
    }

    /// A build that could not go where its build location asked for says so
    /// in the deployment's own build log, before it starts — falling back to
    /// the control plane must never be silent.
    #[tokio::test]
    async fn build_location_notice_is_written_to_the_build_log() {
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build_location_notice("Build location is set to 'node', but no worker is free")
            .build(Arc::new(RecordingImageBuilder::default()))
            .unwrap();

        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".to_string(), 1, 1, 1, writer.clone());

        // No download output in the context, so the job stops right after the
        // notice; what matters is that the notice came first.
        assert!(job.execute(context).await.is_err());
        let lines = writer.lines.lock().unwrap().clone();
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Build location is set to 'node', but no worker is free"),
            "{lines:?}"
        );
    }

    /// Without a notice nothing extra is logged.
    #[tokio::test]
    async fn no_build_location_notice_by_default() {
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(Arc::new(RecordingImageBuilder::default()))
            .unwrap();

        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".to_string(), 1, 1, 1, writer.clone());

        assert!(job.execute(context).await.is_err());
        assert!(writer
            .lines
            .lock()
            .unwrap()
            .iter()
            .all(|line| !line.contains("Build location")),);
    }

    /// A build moved to a worker streams its image back to the control
    /// plane, whose source-map, static-asset and scan jobs read it there.
    #[tokio::test]
    async fn node_build_copies_its_image_to_the_control_plane() {
        use super::super::node_image::fake::FakeImageStore;

        let worker = Arc::new(FakeImageStore::holding("myapp:latest", "sha256:built"));
        let control_plane = Arc::new(FakeImageStore::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .remote_builder_node_id(7)
            .copy_image_to_control_plane(control_plane.clone(), "builder-1")
            .build(worker)
            .unwrap();

        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".to_string(), 1, 1, 1, writer.clone());
        job.copy_image_to_control_plane(&context, "myapp:latest")
            .await
            .unwrap();

        assert_eq!(
            control_plane.images.lock().unwrap().get("myapp:latest"),
            Some(&"sha256:built".to_string())
        );
        let lines = writer.lines.lock().unwrap().clone();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("copied from build node 'builder-1'")),
            "{lines:?}"
        );
    }

    /// A failed copy does not fail a build that succeeded on the worker,
    /// but the build log says what the deployment will be missing.
    #[tokio::test]
    async fn failed_copy_to_the_control_plane_is_logged_not_fatal() {
        use super::super::node_image::fake::FakeImageStore;

        let worker = Arc::new(FakeImageStore::default());
        let control_plane = Arc::new(FakeImageStore::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .remote_builder_node_id(7)
            .copy_image_to_control_plane(control_plane.clone(), "builder-1")
            .build(worker)
            .unwrap();

        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".to_string(), 1, 1, 1, writer.clone());
        job.copy_image_to_control_plane(&context, "myapp:latest")
            .await
            .unwrap();

        assert!(control_plane.images.lock().unwrap().is_empty());
        let lines = writer.lines.lock().unwrap().clone();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Could not copy the image")
                    && line.contains("builder-1")
                    && line.contains("replica placed on the control plane")),
            "{lines:?}"
        );
    }

    /// A node that reports the image's configuration keeps the image: the
    /// control plane records the configuration and copies nothing.
    #[tokio::test]
    async fn node_reporting_image_config_keeps_the_image_off_the_control_plane() {
        use super::super::node_image::fake::FakeImageStore;

        let mut worker = FakeImageStore::holding("myapp:latest", "sha256:built");
        worker.config = Some(temps_deployer::ImageConfig {
            exposed_ports: vec![8080],
            user: Some("app".to_string()),
            env: vec!["SYNTHETIC_SETTING=1".to_string()],
            entrypoint: vec!["/app/server".to_string()],
        });
        let worker = Arc::new(worker);
        let control_plane = Arc::new(FakeImageStore::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .remote_builder_node_id(7)
            .copy_image_to_control_plane(control_plane.clone(), "builder-1")
            .build(worker)
            .unwrap();

        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".to_string(), 1, 1, 1, writer.clone());
        let config = job
            .finish_worker_build(&context, "myapp:latest")
            .await
            .unwrap()
            .expect("the node reported a configuration");

        assert_eq!(config.exposed_ports, vec![8080]);
        assert_eq!(config.working_dir.as_deref(), Some("/app"));
        assert_eq!(config.user.as_deref(), Some("app"));
        assert_eq!(config.entrypoint, vec!["/app/server".to_string()]);
        assert!(
            control_plane.images.lock().unwrap().is_empty(),
            "the image must stay on its node"
        );
        let lines = writer.lines.lock().unwrap().clone();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("stays on build node 7")),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|line| !line.contains("SYNTHETIC_SETTING")),
            "image ENV must never reach the build log: {lines:?}"
        );
    }

    /// An agent that predates the field keeps the previous behaviour, so the
    /// older jobs that read the control plane's copy still find it.
    #[tokio::test]
    async fn node_without_image_config_still_copies_the_image() {
        use super::super::node_image::fake::FakeImageStore;

        let worker = Arc::new(FakeImageStore::holding("myapp:latest", "sha256:built"));
        let control_plane = Arc::new(FakeImageStore::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .remote_builder_node_id(7)
            .copy_image_to_control_plane(control_plane.clone(), "builder-1")
            .build(worker)
            .unwrap();

        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".to_string(), 1, 1, 1, writer.clone());
        let config = job
            .finish_worker_build(&context, "myapp:latest")
            .await
            .unwrap();

        assert!(config.is_none());
        assert_eq!(
            control_plane.images.lock().unwrap().get("myapp:latest"),
            Some(&"sha256:built".to_string())
        );
    }

    /// A build on the control plane's own daemon records no worker config.
    #[tokio::test]
    async fn control_plane_build_records_no_image_config() {
        use super::super::node_image::fake::FakeImageStore;

        let mut builder = FakeImageStore::holding("myapp:latest", "sha256:built");
        builder.config = Some(temps_deployer::ImageConfig::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(Arc::new(builder))
            .unwrap();

        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);
        let config = job
            .finish_worker_build(&context, "myapp:latest")
            .await
            .unwrap();
        assert!(config.is_none());
    }

    #[test]
    fn built_image_config_is_unknown_when_the_node_did_not_report_it() {
        let mut info = temps_deployer::ImageInfo {
            id: "sha256:built".to_string(),
            architecture: "amd64".to_string(),
            os: "linux".to_string(),
            platform: "linux/amd64".to_string(),
            size_bytes: 1,
            tags: Vec::new(),
            created: None,
            working_dir: Some("/srv".to_string()),
            config: None,
        };
        assert_eq!(BuiltImageConfig::from_image_info(&info), None);

        info.config = Some(temps_deployer::ImageConfig::default());
        let config = BuiltImageConfig::from_image_info(&info).expect("reported");
        assert!(
            config.exposed_ports.is_empty(),
            "an image that exposes nothing is a known answer"
        );
        assert_eq!(config.working_dir.as_deref(), Some("/srv"));
    }

    #[test]
    fn expected_image_id_prefers_the_platform_entry() {
        let mut output = ImageOutput {
            image_tag: "app:1".to_string(),
            image_id: "sha256:primary".to_string(),
            size_bytes: 1,
            build_context: PathBuf::from("."),
            dockerfile_path: PathBuf::from("Dockerfile"),
            image_tags_by_platform: HashMap::new(),
            builder_node_ids_by_platform: HashMap::new(),
            image_ids_by_platform: HashMap::new(),
            image_config: None,
        };
        assert_eq!(output.expected_image_id(), Some("sha256:primary"));

        output
            .image_tags_by_platform
            .insert("linux/arm64".to_string(), "app:1".to_string());
        output
            .image_ids_by_platform
            .insert("linux/arm64".to_string(), "sha256:arm".to_string());
        assert_eq!(output.expected_image_id(), Some("sha256:arm"));

        output.image_id = " ".to_string();
        output.image_tags_by_platform.clear();
        assert_eq!(output.expected_image_id(), None);
    }

    /// A build on the control plane has nothing to copy.
    #[tokio::test]
    async fn control_plane_build_copies_nothing() {
        use super::super::node_image::fake::FakeImageStore;

        let builder = Arc::new(FakeImageStore::holding("myapp:latest", "sha256:built"));
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(builder.clone())
            .unwrap();

        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".to_string(), 1, 1, 1, writer.clone());
        job.copy_image_to_control_plane(&context, "myapp:latest")
            .await
            .unwrap();

        assert!(builder.imports.lock().unwrap().is_empty());
        assert!(writer.lines.lock().unwrap().is_empty());
    }

    /// The default (`local_workloads_enabled(true)`, the historical
    /// single-binary behaviour) must be unaffected by the new guard.
    #[tokio::test]
    async fn full_profile_still_reaches_the_image_builder() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        job.build_image(&repo, &context).await.unwrap();
        assert_eq!(builder.builds(), vec![("myapp:latest".to_string(), None)]);
    }

    #[test]
    fn test_build_image_job_builder() {
        let image_builder: Arc<dyn ImageBuilder> = Arc::new(MockImageBuilder);

        let job = BuildImageJobBuilder::new()
            .job_id("test_build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .dockerfile_path("docker/Dockerfile".to_string())
            .build_args(vec![("ENV".to_string(), "production".to_string())])
            .build(image_builder)
            .unwrap();

        assert_eq!(job.job_id(), "test_build");
        assert_eq!(job.download_job_id, "download_repo");
        assert_eq!(job.image_tag, "myapp:latest");
        assert_eq!(job.depends_on(), vec!["download_repo".to_string()]);
    }

    #[test]
    fn test_build_image_job_builder_preserves_typed_preset_config() {
        let image_builder: Arc<dyn ImageBuilder> = Arc::new(MockImageBuilder);
        let config = StoredPresetConfig::Nixpacks(temps_entities::preset::NixpacksConfig {
            nixpacks_config: None,
            providers: vec![
                temps_entities::preset::NixpacksProvider::Auto,
                temps_entities::preset::NixpacksProvider::Python,
            ],
        });

        let job = BuildImageJobBuilder::new()
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .preset(StoredPreset::Nixpacks)
            .preset_config(Some(config.clone()))
            .build(image_builder)
            .unwrap();

        assert_eq!(job.preset, Some(StoredPreset::Nixpacks));
        assert_eq!(job.preset_config, Some(config));
    }

    /// Records every platform the job asked for, so a test can assert what a
    /// multi-arch build actually did.
    #[derive(Default)]
    struct RecordingImageBuilder {
        builds: std::sync::Mutex<Vec<(String, Option<String>)>>,
        requests: std::sync::Mutex<Vec<BuildRequest>>,
        /// Platforms whose build should fail, with this error text.
        fail_platform: Option<(String, String)>,
    }

    impl RecordingImageBuilder {
        fn builds(&self) -> Vec<(String, Option<String>)> {
            self.builds.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ImageBuilder for RecordingImageBuilder {
        async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
            self.requests.lock().unwrap().push(request.clone());
            self.builds
                .lock()
                .unwrap()
                .push((request.image_name.clone(), request.platform.clone()));

            if let (Some((fail_platform, message)), Some(requested)) =
                (self.fail_platform.as_ref(), request.platform.as_deref())
            {
                if fail_platform == requested {
                    return Err(BuilderError::BuildFailed(message.clone()));
                }
            }

            Ok(BuildResult {
                image_id: format!("sha256:{}", request.image_name),
                image_name: request.image_name,
                size_bytes: 1024 * 1024,
                build_duration_ms: 1,
            })
        }

        async fn build_image_with_callback(
            &self,
            request: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            self.build_image(request.request).await
        }

        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, BuilderError> {
            Ok("sha256:imported".to_string())
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &Path,
        ) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            Ok(vec![])
        }

        async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, BuilderError> {
            Err(BuilderError::ImageNotFound("not used in this test".into()))
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &Path,
        ) -> Result<(), BuilderError> {
            Ok(())
        }

        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }

    /// Build context with a trivial Dockerfile, so `build_image` doesn't try to
    /// generate one from a preset.
    fn repo_with_dockerfile() -> (tempfile::TempDir, RepositoryOutput) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        let repo = RepositoryOutput {
            repo_dir: dir.path().to_path_buf(),
            checkout_ref: "main".to_string(),
            repo_owner: "owner".to_string(),
            repo_name: "repo".to_string(),
        };
        (dir, repo)
    }

    #[tokio::test]
    async fn generated_javascript_subfolder_build_uses_workspace_context() {
        for preset in [
            StoredPreset::NextJs,
            StoredPreset::Vite,
            StoredPreset::Nixpacks,
        ] {
            for custom_dockerfile in [false, true] {
                let builder = Arc::new(RecordingImageBuilder::default());
                let job = BuildImageJobBuilder::new()
                    .job_id("build".into())
                    .download_job_id("download_repo".into())
                    .image_tag("app:latest".into())
                    .build_context("apps/web".into())
                    .preset(preset)
                    .preset_config(if preset == StoredPreset::Nixpacks {
                        Some(StoredPresetConfig::Nixpacks(
                            temps_entities::preset::NixpacksConfig {
                                nixpacks_config: None,
                                providers: vec![temps_entities::preset::NixpacksProvider::Node],
                            },
                        ))
                    } else {
                        None
                    })
                    .cache_from(vec!["app:previous".into()])
                    .build(builder.clone())
                    .unwrap();
                let dir = tempfile::tempdir().unwrap();
                let root = dir.path();
                let app = root.join("apps/web");
                std::fs::create_dir_all(&app).unwrap();
                std::fs::write(root.join("package.json"), "{}").unwrap();
                std::fs::write(root.join("turbo.json"), "{}").unwrap();
                std::fs::write(root.join("pnpm-workspace.yaml"), "packages: [apps/*]").unwrap();
                std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").unwrap();
                std::fs::write(
                    app.join("package.json"),
                    r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
                )
                .unwrap();
                if custom_dockerfile {
                    std::fs::write(app.join("Dockerfile"), "FROM scratch\n").unwrap();
                }
                let repo = RepositoryOutput {
                    repo_dir: root.into(),
                    checkout_ref: "main".into(),
                    repo_owner: "owner".into(),
                    repo_name: "repo".into(),
                };
                let mut context = crate::test_utils::create_test_context("wf".into(), 1, 1, 1);
                context
                    .set_output(
                        "download_repo",
                        "repo_dir",
                        root.to_string_lossy().to_string(),
                    )
                    .unwrap();
                context
                    .set_output("download_repo", "checkout_ref", "main")
                    .unwrap();
                context
                    .set_output("download_repo", "repo_owner", "owner")
                    .unwrap();
                context
                    .set_output("download_repo", "repo_name", "repo")
                    .unwrap();
                job.build_image(&repo, &context).await.unwrap();
                let requests = builder.requests.lock().unwrap();
                let request = &requests[0];
                let expected = if custom_dockerfile { &app } else { root };
                assert_eq!(request.context_path, expected.canonicalize().unwrap());
                assert_eq!(request.cache_from, vec!["app:previous"]);
                if !custom_dockerfile {
                    let dockerfile = std::fs::read_to_string(app.join("Dockerfile")).unwrap();
                    if preset == StoredPreset::NextJs {
                        assert!(dockerfile.contains("pnpm-lock.yaml"));
                        assert!(
                            dockerfile.find("pnpm install").unwrap()
                                < dockerfile.find("WORKDIR /repo/apps/web").unwrap()
                        );
                    } else {
                        assert!(dockerfile.contains("--frozen-lockfile"), "{dockerfile}");
                        assert!(dockerfile.contains("./apps/web..."), "{dockerfile}");
                        assert!(app.join("Dockerfile.dockerignore").is_file());
                    }
                }
            }
        }
    }

    #[test]
    fn worker_build_args_send_only_values_recorded_as_public() {
        let args = |names: &[&str]| -> Vec<(String, String)> {
            names
                .iter()
                .map(|name| (name.to_string(), format!("value-of-{name}")))
                .collect()
        };
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("app:latest".into())
            .remote_builder_node_id(7)
            .build_args(args(&[
                "HOST",
                "VITE_SENTRY_DSN",
                "VITE_API_URL",
                "TEMPS_API_TOKEN",
                "NPM_TOKEN",
                "BUILDKIT_SYNTAX",
                temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG,
            ]))
            .build_args_buildkit(args(&["LATE_PUBLIC"]))
            .worker_build_arg_keys(
                [
                    "HOST",
                    "VITE_SENTRY_DSN",
                    "VITE_API_URL",
                    "LATE_PUBLIC",
                    // Recorded or not, these never go to a worker.
                    "NPM_TOKEN",
                    "BUILDKIT_SYNTAX",
                ]
                .map(String::from),
            )
            .build(Arc::new(RecordingImageBuilder::default()))
            .unwrap();
        let split = job.worker_build_args();
        assert_eq!(
            split.sent.keys().cloned().collect::<Vec<_>>(),
            [
                "BUILDKIT_CACHE_MOUNT_NS",
                "HOST",
                "LATE_PUBLIC",
                "VITE_API_URL",
                "VITE_SENTRY_DSN"
            ]
            .map(String::from)
        );
        assert_eq!(
            split.sent.get("VITE_API_URL").map(String::as_str),
            Some("value-of-VITE_API_URL")
        );
        assert_eq!(
            split.withheld,
            ["BUILDKIT_SYNTAX", "NPM_TOKEN", "TEMPS_API_TOKEN"].map(String::from)
        );
        assert_eq!(
            split.variable_names(),
            ["HOST", "LATE_PUBLIC", "VITE_API_URL", "VITE_SENTRY_DSN"].map(String::from)
        );

        // A job planned before the planner recorded public names sends none.
        let legacy = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("app:latest".into())
            .remote_builder_node_id(7)
            .build_args(args(&["VITE_SENTRY_DSN"]))
            .build(Arc::new(RecordingImageBuilder::default()))
            .unwrap();
        assert!(legacy.worker_build_args().variable_names().is_empty());
    }

    #[test]
    fn build_inlined_variables_matches_framework_prefixes_only() {
        assert_eq!(
            build_inlined_variables([
                "VITE_B",
                "VITE_A",
                "VITE_A",
                "VITE_",
                "HOST",
                "PUBLICATION",
                "PUBLIC_X",
                "REACT_APP_Y",
                "DATABASE_URL",
            ]),
            vec!["PUBLIC_X", "REACT_APP_Y", "VITE_A", "VITE_B"]
        );
        assert!(build_inlined_variables([]).is_empty());
    }

    /// The variables a planned build carries for `preset`: every value the
    /// planner injects, from its own catalog (`managed_environment_variables`,
    /// which names each framework's public Sentry DSN and tunnel variables),
    /// plus project variables -- public ones for every framework prefix, a
    /// secret inlined one and a secret runtime one -- and the cache
    /// namespace. Values are synthetic and unique per name. Returns the build
    /// arguments and the names the planner records as public.
    fn planned_build_args(preset: StoredPreset) -> (Vec<(String, String)>, BTreeSet<String>) {
        let mut args = Vec::new();
        let mut public = BTreeSet::new();
        for variable in crate::services::managed_environment_variables(preset) {
            // Placement values are set per container when it starts.
            if matches!(
                variable.name.as_str(),
                "TEMPS_NODE_NAME" | "TEMPS_NODE_ID" | "TEMPS_REPLICA"
            ) {
                continue;
            }
            let value = format!("synthetic-{}-value", variable.name.to_ascii_lowercase());
            if !variable.is_secret {
                public.insert(variable.name.clone());
            }
            args.push((variable.name, value));
        }
        for (name, secret) in [
            ("VITE_API_URL", false),
            ("NEXT_PUBLIC_SITE_NAME", false),
            ("PUBLIC_SITE_TITLE", false),
            ("REACT_APP_FEATURE", false),
            ("VITE_PRIVATE_TOKEN", true),
            ("DATABASE_URL", true),
        ] {
            args.push((
                name.to_string(),
                format!("synthetic-{}-value", name.to_ascii_lowercase()),
            ));
            if !secret {
                public.insert(name.to_string());
            }
        }
        args.push((
            temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG.to_string(),
            "ns-test".to_string(),
        ));
        (args, public)
    }

    /// A checkout holding `files`, and a context whose log is captured.
    fn worker_checkout(
        files: &[(&str, &str)],
    ) -> (
        tempfile::TempDir,
        RepositoryOutput,
        WorkflowContext,
        Arc<CapturingLogWriter>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = dir.path().join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
        let repo = RepositoryOutput {
            repo_dir: dir.path().to_path_buf(),
            checkout_ref: "main".to_string(),
            repo_owner: "owner".to_string(),
            repo_name: "web".to_string(),
        };
        let logs = Arc::new(CapturingLogWriter::default());
        let mut context = WorkflowContext::new("wf".to_string(), 1, 1, 1, logs.clone());
        for (key, value) in [
            ("repo_dir", repo.repo_dir.display().to_string()),
            ("checkout_ref", "main".to_string()),
            ("repo_owner", "owner".to_string()),
            ("repo_name", "web".to_string()),
        ] {
            context.set_output("download_repo", key, value).unwrap();
        }
        (dir, repo, context, logs)
    }

    fn declared_args(dockerfile: &str) -> BTreeSet<String> {
        temps_deployer::build_protocol::dockerfile_build_arg_names(dockerfile)
            .expect("generated ARGs are literal")
    }

    const VITE_APP: &[(&str, &str)] = &[
        (
            "package.json",
            r#"{"name":"web","private":true,"scripts":{"build":"vite build"},"devDependencies":{"vite":"^5.0.0"}}"#,
        ),
        ("index.html", "<!doctype html><title>web</title>"),
    ];

    /// Issue #1344: the generated Vite Dockerfile for a worker build declares
    /// the variables the worker receives -- the platform's public Sentry DSN
    /// and tunnel among them -- and none whose value stays on the control
    /// plane, while a local build keeps declaring every variable. A secret
    /// inlined variable does not refuse the build: the log names it.
    #[tokio::test]
    async fn generated_vite_dockerfile_on_worker_declares_the_values_it_receives() {
        let (args, public) = planned_build_args(StoredPreset::Vite);
        for worker in [Some(7), None] {
            let (dir, repo, context, logs) = worker_checkout(VITE_APP);
            let mut builder = BuildImageJobBuilder::new()
                .job_id("build".into())
                .download_job_id("download_repo".into())
                .image_tag("app:latest".into())
                .preset(StoredPreset::Vite)
                .build_args(args.clone())
                .worker_build_arg_keys(public.clone());
            if let Some(node_id) = worker {
                builder = builder.remote_builder_node_id(node_id);
            }
            let job = builder
                .build(Arc::new(RecordingImageBuilder::default()))
                .unwrap();
            let dockerfile = dir.path().join("Dockerfile");
            let generated = job
                .ensure_dockerfile(&context, &repo.repo_dir, &dockerfile, &repo.repo_dir)
                .await
                .expect("a generated Vite build is never refused");
            assert!(generated.generated);
            let rendered = std::fs::read_to_string(&dockerfile).unwrap();
            let declared = declared_args(&rendered);
            for name in [
                "VITE_SENTRY_DSN",
                "VITE_SENTRY_TUNNEL",
                "VITE_API_URL",
                "HOST",
            ] {
                assert!(declared.contains(name), "{name} undeclared:\n{rendered}");
            }
            for name in ["VITE_PRIVATE_TOKEN", "DATABASE_URL", "TEMPS_API_TOKEN"] {
                assert_eq!(
                    declared.contains(name),
                    worker.is_none(),
                    "{name} (worker: {worker:?}):\n{rendered}"
                );
            }
            // Values are never rendered into a Dockerfile, wherever it builds.
            for (_, value) in &args {
                assert!(!rendered.contains(value.as_str()), "{value}:\n{rendered}");
            }
            let lines = logs.lines.lock().unwrap().join("\n");
            if worker.is_some() {
                assert!(lines.contains("worker node 7"), "{lines}");
                assert!(
                    lines.contains("VITE_PRIVATE_TOKEN is inlined into the application"),
                    "{lines}"
                );
                assert!(lines.contains("DATABASE_URL"), "{lines}");
            }
            for (_, value) in &args {
                assert!(!lines.contains(value.as_str()), "log quotes {value}");
            }
        }
    }

    /// Issue #1344, end to end on the control-plane side (replacing the #1381
    /// test, whose variables left out the public Sentry ones every Vite build
    /// carries): an ordinary Vite source with no Dockerfile and the variables
    /// a real plan produces goes through the real `RemoteNodeDeployer` upload
    /// to a worker endpoint and builds. The worker receives every public
    /// value its generated ARGs read, the cache namespace, and no secret;
    /// the build log quotes no value at all.
    #[tokio::test]
    async fn generated_vite_build_on_a_worker_receives_public_values_and_no_secret() {
        let worker = fake_worker::spawn().await;
        let (_dir, repo, context, logs) = worker_checkout(VITE_APP);
        let (args, public) = planned_build_args(StoredPreset::Vite);
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("web:worker".into())
            .preset(StoredPreset::Vite)
            .remote_builder_node_id(7)
            .build_args(args.clone())
            .worker_build_arg_keys(public.clone())
            .build(worker.deployer.clone())
            .unwrap();

        let output = job
            .build_image(&repo, &context)
            .await
            .expect("a generated Vite build is accepted by the worker");

        assert_eq!(output.image_tag, "web:worker");
        let captured = worker.captured.lock().unwrap();
        assert_eq!(
            captured.authorization.as_deref(),
            Some("Bearer worker-token")
        );
        let spec = captured.spec.as_ref().expect("the worker received a spec");
        assert_eq!(spec.image_name, "web:worker");
        assert_eq!(spec.cache_namespace.as_deref(), Some("ns-test"));
        assert_eq!(
            spec.version,
            temps_deployer::build_protocol::BUILD_PROTOCOL_VERSION_BUILD_ARGS
        );
        let dockerfile = captured
            .files
            .get(&spec.dockerfile)
            .expect("the generated Dockerfile was uploaded");
        assert!(dockerfile.contains("FROM "), "{dockerfile}");
        for name in ["VITE_SENTRY_DSN", "VITE_SENTRY_TUNNEL", "VITE_API_URL"] {
            let expected = args
                .iter()
                .find(|(arg, _)| arg == name)
                .map(|(_, value)| value.as_str());
            assert_eq!(
                spec.build_args.get(name).map(String::as_str),
                expected,
                "{name}"
            );
        }
        assert!(captured.files.contains_key("package.json"));
        assert!(captured.files.contains_key("index.html"));
        let lines = logs.lines.lock().unwrap().join("\n");
        for (name, value) in &args {
            // The cache namespace travels in its own field (asserted above).
            if name == temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG {
                continue;
            }
            if !public.contains(name) {
                assert!(
                    !spec.build_args.contains_key(name),
                    "{name} is secret but was sent"
                );
                assert!(!captured.spec_json.contains(value.as_str()), "{name}");
                for (path, contents) in &captured.files {
                    assert!(!contents.contains(value.as_str()), "{path} carries {name}");
                }
            }
            assert!(!lines.contains(value.as_str()), "the log quotes {name}");
        }
    }

    /// A small checkout every catalog preset can plan, keyed by its slug.
    fn preset_fixture(slug: &str) -> Vec<(&'static str, &'static str)> {
        const NODE_SERVER: &[(&str, &str)] = &[
            (
                "package.json",
                r#"{"name":"web","scripts":{"build":"node build.js","start":"node server.js"}}"#,
            ),
            ("package-lock.json", "{}"),
            ("build.js", "console.log('built')"),
            (
                "server.js",
                "require('http').createServer((_, r) => r.end('ok')).listen(3000)",
            ),
        ];
        match slug {
            "nextjs" => vec![
                (
                    "package.json",
                    r#"{"name":"web","scripts":{"build":"next build","start":"next start"},"dependencies":{"next":"14.2.3","react":"18.3.1","react-dom":"18.3.1"}}"#,
                ),
                ("package-lock.json", "{}"),
                ("pages/index.js", "export default () => null"),
            ],
            "vite" => VITE_APP.to_vec(),
            "react-app" => vec![
                (
                    "package.json",
                    r#"{"name":"web","scripts":{"build":"react-scripts build"},"dependencies":{"react-scripts":"5.0.1"}}"#,
                ),
                ("package-lock.json", "{}"),
            ],
            "rsbuild" => vec![
                (
                    "package.json",
                    r#"{"name":"web","scripts":{"build":"rsbuild build"},"devDependencies":{"@rsbuild/core":"^1.0.0"}}"#,
                ),
                ("package-lock.json", "{}"),
            ],
            "docusaurus" => vec![
                (
                    "package.json",
                    r#"{"name":"docs","scripts":{"build":"docusaurus build"},"dependencies":{"@docusaurus/core":"3.5.0"}}"#,
                ),
                ("package-lock.json", "{}"),
            ],
            "python" | "nixpacks-python" => vec![
                ("requirements.txt", "flask\n"),
                ("main.py", "print('ok')\n"),
                ("Procfile", "web: python main.py\n"),
            ],
            "go" | "nixpacks-go" => vec![
                ("go.mod", "module example.com/app\n\ngo 1.22\n"),
                ("main.go", "package main\n\nfunc main() {}\n"),
            ],
            "rust" | "nixpacks-rust" => vec![
                (
                    "Cargo.toml",
                    "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                ("src/main.rs", "fn main() {}\n"),
            ],
            "java" | "nixpacks-java" => vec![(
                "pom.xml",
                "<project><modelVersion>4.0.0</modelVersion><groupId>a</groupId><artifactId>app</artifactId><version>1</version></project>\n",
            )],
            "nixpacks-php" => vec![("composer.json", "{}"), ("index.php", "<?php echo 'ok';\n")],
            "nixpacks-ruby" => vec![
                ("Gemfile", "source 'https://rubygems.org'\n"),
                ("Gemfile.lock", "GEM\n"),
                ("config.ru", "run ->(_) { [200, {}, ['ok']] }\n"),
            ],
            "nixpacks-deno" => vec![("deno.json", "{}"), ("main.ts", "Deno.serve(() => new Response('ok'))\n")],
            "nixpacks-elixir" => vec![(
                "mix.exs",
                "defmodule App.MixProject do\n  use Mix.Project\n  def project, do: [app: :app, version: \"0.1.0\"]\nend\n",
            )],
            "nixpacks-csharp" => vec![
                ("app.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
                ("Program.cs", "return;\n"),
            ],
            "nixpacks-dart" => vec![
                ("pubspec.yaml", "name: app\n"),
                ("bin/app.dart", "void main() {}\n"),
            ],
            "nixpacks-swift" => vec![
                ("Package.swift", "// swift-tools-version:5.9\nimport PackageDescription\nlet package = Package(name: \"app\")\n"),
                ("Sources/app/main.swift", "print(\"ok\")\n"),
            ],
            "nixpacks-static" => vec![("index.html", "<!doctype html><title>ok</title>")],
            // A repository's own Dockerfile, reading a public variable.
            "dockerfile" | "custom" => vec![(
                "Dockerfile",
                "FROM node:22-alpine\nARG VITE_API_URL\nRUN echo \"$VITE_API_URL\" > /api-url\n",
            )],
            _ => NODE_SERVER.to_vec(),
        }
    }

    /// Issue #1344, for every preset in the catalog (`temps_presets::all_presets`,
    /// not a hand-picked list) with the variables a real plan carries: the
    /// build runs on a worker -- it is never refused, and never for the
    /// platform's own variables -- and every inlined public value the
    /// Dockerfile reads arrives in the worker's build request, while no
    /// secret value reaches the worker or any log line.
    #[tokio::test]
    async fn every_preset_builds_on_a_worker_with_the_planned_variables() {
        let mut built = Vec::new();
        for preset in temps_presets::all_presets() {
            let slug = preset.slug();
            let stored = preset
                .resolve_storage(None)
                .unwrap_or_else(|error| panic!("{slug} cannot be stored: {error}"));
            if stored.preset == StoredPreset::DockerCompose {
                // Planned as a Compose deployment, never a BuildImageJob.
                continue;
            }
            let worker = fake_worker::spawn().await;
            let fixture = preset_fixture(&slug);
            let (_dir, repo, context, logs) = worker_checkout(&fixture);
            let (args, public) = planned_build_args(stored.preset);
            let job = BuildImageJobBuilder::new()
                .job_id("build".into())
                .download_job_id("download_repo".into())
                .image_tag(format!("{slug}:worker"))
                .preset(stored.preset)
                .preset_config(stored.config.clone())
                .remote_builder_node_id(7)
                .build_args(args.clone())
                .worker_build_arg_keys(public.clone())
                .build(worker.deployer.clone())
                .unwrap();
            let result = job.build_image(&repo, &context).await;
            let lines = logs.lines.lock().unwrap().join("\n");
            if let Err(error) = &result {
                panic!("{slug}: the worker build was refused: {error}\n{lines}");
            }
            let captured = worker.captured.lock().unwrap();
            let spec = captured
                .spec
                .as_ref()
                .unwrap_or_else(|| panic!("{slug}: the worker received no spec"));
            let dockerfile = captured
                .files
                .get(&spec.dockerfile)
                .unwrap_or_else(|| panic!("{slug}: no Dockerfile uploaded"));
            assert!(!lines.contains("Invalid Dockerfile"), "{slug}: {lines}");
            let declared = declared_args(dockerfile);
            assert_eq!(spec.cache_namespace.as_deref(), Some("ns-test"), "{slug}");
            for (name, value) in &args {
                // The cache namespace travels in its own field.
                if name == temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG {
                    continue;
                }
                let reads =
                    temps_deployer::build_protocol::dockerfile_reads_build_arg(&declared, name);
                if public.contains(name) && reads {
                    assert_eq!(
                        spec.build_args.get(name),
                        Some(value),
                        "{slug}: {name} is read by the Dockerfile but was not sent"
                    );
                }
                if !public.contains(name) {
                    assert!(
                        !captured.spec_json.contains(value.as_str()),
                        "{slug}: the secret {name} reached the worker"
                    );
                    assert!(
                        !dockerfile.contains(value.as_str()),
                        "{slug}: the Dockerfile carries {name}"
                    );
                }
                assert!(
                    !lines.contains(value.as_str()),
                    "{slug}: the build log quotes {name}"
                );
            }
            // Every inlined public value the generated build declares,
            // including the platform's own public Sentry variables, arrives.
            for name in build_inlined_variables(public.iter().map(String::as_str)) {
                if declared.contains(&name) {
                    assert!(spec.build_args.contains_key(&name), "{slug}: {name}");
                }
            }
            built.push(slug);
        }
        for slug in [
            "nextjs",
            "vite",
            "rsbuild",
            "docusaurus",
            "autopack",
            "dockerfile",
        ] {
            assert!(
                built.iter().any(|built| built == slug),
                "{slug} was not built"
            );
        }
    }

    /// A repository's own Dockerfile on a worker: its `ARG` receives the
    /// project variable's value, and only the values it declares are sent.
    #[tokio::test]
    async fn user_dockerfile_arg_on_a_worker_receives_its_value() {
        let worker = fake_worker::spawn().await;
        let (_dir, repo, context, logs) = worker_checkout(&[(
            "Dockerfile",
            "FROM node:22-alpine\nARG APP_GREETING\nARG VITE_SENTRY_DSN\nRUN echo \"$APP_GREETING\" > /greeting\n",
        )]);
        let (mut args, mut public) = planned_build_args(StoredPreset::Dockerfile);
        args.push(("APP_GREETING".into(), "synthetic-greeting".into()));
        public.insert("APP_GREETING".into());
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("custom:worker".into())
            .preset(StoredPreset::Dockerfile)
            .remote_builder_node_id(7)
            .build_args(args)
            .worker_build_arg_keys(public)
            .build(worker.deployer.clone())
            .unwrap();
        job.build_image(&repo, &context)
            .await
            .expect("a Dockerfile ARG builds on a worker");
        let captured = worker.captured.lock().unwrap();
        let spec = captured.spec.as_ref().expect("spec");
        // The Dockerfile preset carries no browser DSN, so only the one value
        // it declares and the project set is sent.
        assert_eq!(
            spec.build_args,
            BTreeMap::from([("APP_GREETING".to_string(), "synthetic-greeting".to_string())])
        );
        let lines = logs.lines.lock().unwrap().join("\n");
        assert!(lines.contains("APP_GREETING"), "{lines}");
        assert!(!lines.contains("synthetic-greeting"), "{lines}");
    }

    /// A Dockerfile `ARG` that would read a secret is refused on a worker by
    /// name -- the value never leaves the control plane -- with a remedy,
    /// and it is classified as configuration, never an invalid Dockerfile.
    #[tokio::test]
    async fn user_dockerfile_arg_for_a_secret_is_refused_on_a_worker() {
        let worker = fake_worker::spawn().await;
        let (_dir, repo, context, _logs) = worker_checkout(&[(
            "Dockerfile",
            "FROM node:22-alpine\nARG DATABASE_URL\nRUN true\n",
        )]);
        let (args, public) = planned_build_args(StoredPreset::Dockerfile);
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("custom:worker".into())
            .preset(StoredPreset::Dockerfile)
            .remote_builder_node_id(7)
            .build_args(args)
            .worker_build_arg_keys(public)
            .build(worker.deployer.clone())
            .unwrap();
        let error = job.build_image(&repo, &context).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Worker builds cannot"), "{message}");
        assert!(message.contains("ARG DATABASE_URL"), "{message}");
        assert!(message.contains("build location"), "{message}");
        assert!(
            !message.contains("synthetic-database_url-value"),
            "{message}"
        );
        assert!(worker.captured.lock().unwrap().spec.is_none());
        let class = crate::services::failure_classifier::classify_failure_reason(Some(&format!(
            "Required job 'build_image' failed: {message}"
        )));
        assert_eq!(
            class.code,
            crate::services::failure_classifier::DeploymentFailureCode::InvalidConfiguration
        );
    }

    #[tokio::test]
    async fn worker_build_refuses_npm_credentials_before_writing_context() {
        for key in ["NPM_TOKEN", "NPM_RC"] {
            let builder = Arc::new(RecordingImageBuilder::default());
            let job = BuildImageJobBuilder::new()
                .job_id("build".into())
                .download_job_id("download_repo".into())
                .image_tag("app:latest".into())
                .remote_builder_node_id(7)
                .build_args(vec![(key.into(), "synthetic-secret".into())])
                .build(builder.clone())
                .unwrap();
            let (dir, repo) = repo_with_dockerfile();
            let context = crate::test_utils::create_test_context("wf".into(), 1, 1, 1);
            let error = job.build_image(&repo, &context).await.unwrap_err();
            assert!(matches!(error, WorkflowError::JobValidationFailed(_)));
            assert!(!error.to_string().contains("synthetic-secret"));
            assert!(!dir.path().join(".npmrc").exists());
            assert!(builder.builds().is_empty());
        }
    }

    #[tokio::test]
    async fn worker_build_requires_successful_image_inspection() {
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("app:latest".into())
            .remote_builder_node_id(7)
            .build(Arc::new(RecordingImageBuilder::default()))
            .unwrap();
        let context = crate::test_utils::create_test_context("wf".into(), 1, 1, 1);
        assert!(job
            .verify_built_platform(
                job.image_builder.as_ref(),
                "app:latest",
                "linux/arm64",
                &context
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn rejects_dockerfile_path_traversal() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .dockerfile_path("../Dockerfile".to_string())
            .build(builder)
            .unwrap();
        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let error = job.build_image(&repo, &context).await.unwrap_err();
        assert!(matches!(error, WorkflowError::JobValidationFailed(_)));
    }

    /// A configured nested directory missing from the checkout names the
    /// path and the setting to fix, not a bare "No such file or directory".
    #[tokio::test]
    async fn missing_build_context_names_the_configured_directory() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build_context("examples/starters/go/gin".to_string())
            .build(builder.clone())
            .unwrap();
        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let error = job.build_image(&repo, &context).await.unwrap_err();
        let message = error.to_string();
        assert!(
            matches!(error, WorkflowError::JobValidationFailed(_)),
            "{message}"
        );
        assert!(message.contains("examples/starters/go/gin"), "{message}");
        assert!(message.contains("Invalid configuration"), "{message}");
        assert!(builder.builds().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_dockerfile_and_npmrc_symlinks() {
        use std::os::unix::fs::symlink;

        let builder = Arc::new(RecordingImageBuilder::default());
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("outside");
        std::fs::write(&outside_file, "unchanged").unwrap();
        symlink(&outside_file, dir.path().join("Dockerfile")).unwrap();
        let repo = RepositoryOutput {
            repo_dir: dir.path().to_path_buf(),
            checkout_ref: "main".to_string(),
            repo_owner: "owner".to_string(),
            repo_name: "repo".to_string(),
        };
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(builder.clone())
            .unwrap();
        assert!(matches!(
            job.build_image(&repo, &context).await,
            Err(WorkflowError::JobValidationFailed(_))
        ));

        std::fs::remove_file(dir.path().join("Dockerfile")).unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        symlink(&outside_file, dir.path().join(".npmrc")).unwrap();
        let npm_job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build_args(vec![("NPM_TOKEN".to_string(), "secret".to_string())])
            .build(builder)
            .unwrap();
        assert!(matches!(
            npm_job.build_image(&repo, &context).await,
            Err(WorkflowError::JobValidationFailed(_))
        ));
        assert_eq!(std::fs::read_to_string(outside_file).unwrap(), "unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_build_control_files_before_reading_them() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        for name in [
            "package.json",
            ".temps.yaml",
            "nixpacks.toml",
            ".nixpacks.toml",
        ] {
            let path = directory.path().join(name);
            symlink("/dev/zero", &path).unwrap();
            assert!(matches!(
                read_confined_control_file(directory.path(), &path, 1024),
                Err(WorkflowError::JobValidationFailed(_))
            ));
        }
    }

    /// Single-platform builds must keep behaving exactly as before: one build,
    /// the plain tag, no per-platform map.
    #[tokio::test]
    async fn test_build_without_target_platforms_builds_once() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let output = job.build_image(&repo, &context).await.unwrap();

        assert_eq!(builder.builds(), vec![("myapp:latest".to_string(), None)]);
        assert_eq!(output.image_tag, "myapp:latest");
        assert!(output.image_tags_by_platform.is_empty());
    }

    /// A heterogeneous cluster builds one image per architecture. The first
    /// platform keeps the plain tag so single-arch consumers are unaffected;
    /// the rest are suffixed so both can coexist in one image store without a
    /// registry or manifest list.
    #[tokio::test]
    async fn test_multi_platform_build_produces_one_tag_per_platform() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .target_platforms(vec!["linux/amd64".to_string(), "linux/arm64".to_string()])
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let output = job.build_image(&repo, &context).await.unwrap();

        assert_eq!(
            builder.builds(),
            vec![
                ("myapp:latest".to_string(), Some("linux/amd64".to_string())),
                (
                    "myapp:latest-arm64".to_string(),
                    Some("linux/arm64".to_string())
                ),
            ]
        );

        // The primary output stays the native tag...
        assert_eq!(output.image_tag, "myapp:latest");
        // ...and every platform is resolvable for the deploy job.
        assert_eq!(
            output.image_tags_by_platform.get("linux/amd64").unwrap(),
            "myapp:latest"
        );
        assert_eq!(
            output.image_tags_by_platform.get("linux/arm64").unwrap(),
            "myapp:latest-arm64"
        );
    }

    /// A secondary platform failing must NOT fail the deployment.
    ///
    /// `required_build_platforms` is driven by cluster topology, so an
    /// operator who joins an arm64 worker without installing QEMU would
    /// otherwise break every deployment in the cluster — strictly worse than
    /// the broken ARM replicas they had before. The platform drops out of
    /// `image_tags_by_platform` instead, and the scheduler's architecture
    /// filter excludes those nodes.
    #[tokio::test]
    async fn test_secondary_platform_failure_degrades_instead_of_aborting() {
        let builder = Arc::new(RecordingImageBuilder {
            builds: Default::default(),
            requests: Default::default(),
            fail_platform: Some(("linux/arm64".to_string(), "exec format error".to_string())),
        });
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .target_platforms(vec!["linux/amd64".to_string(), "linux/arm64".to_string()])
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let output = job
            .build_image(&repo, &context)
            .await
            .expect("the native build succeeded, so the deployment must proceed");

        // The native image is there and usable...
        assert_eq!(output.image_tag, "myapp:latest");
        assert_eq!(
            output.image_tags_by_platform.get("linux/amd64").unwrap(),
            "myapp:latest"
        );
        // ...and the platform that failed is absent, which is what makes the
        // scheduler exclude arm64 nodes rather than send them a broken image.
        assert!(
            !output.image_tags_by_platform.contains_key("linux/arm64"),
            "a failed platform must not be advertised: {:?}",
            output.image_tags_by_platform
        );
    }

    /// The primary build is still fatal — without it there is nothing to
    /// deploy anywhere.
    #[tokio::test]
    async fn test_primary_platform_failure_still_fails_the_job() {
        let builder = Arc::new(RecordingImageBuilder {
            builds: Default::default(),
            requests: Default::default(),
            fail_platform: Some(("linux/amd64".to_string(), "boom".to_string())),
        });
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .target_platforms(vec!["linux/amd64".to_string(), "linux/arm64".to_string()])
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        assert!(job.build_image(&repo, &context).await.is_err());
    }

    #[test]
    fn test_describe_build_failure_mentions_emulation_only_for_cross_builds() {
        let host = "linux/amd64";

        // Native build: plain error, no misleading emulation advice.
        let native_msg = BuildImageJob::describe_build_failure(
            Some(host),
            host,
            &BuilderError::BuildFailed("exec format error".into()),
        );
        assert!(!native_msg.contains("binfmt"), "got: {}", native_msg);

        // Cross build failing for an unrelated reason: no emulation advice
        // either — don't send people chasing the wrong fix.
        let unrelated = BuildImageJob::describe_build_failure(
            Some("linux/arm64"),
            host,
            &BuilderError::BuildFailed("npm ERR! missing script: build".into()),
        );
        assert!(!unrelated.contains("binfmt"), "got: {}", unrelated);
        assert!(unrelated.contains("linux/arm64"), "got: {}", unrelated);

        // No platform requested at all (single-arch path).
        let plain = BuildImageJob::describe_build_failure(
            None,
            host,
            &BuilderError::BuildFailed("boom".into()),
        );
        assert_eq!(plain, "Failed to build image: Build failed: boom");
    }

    /// The advice must be aimed at the machine that runs the build. With a
    /// cross-architecture `DOCKER_HOST` the daemon's platform and this
    /// process's differ, and using the latter would recommend QEMU for a build
    /// the daemon runs natively — or stay silent about a genuine cross-build.
    #[test]
    fn test_describe_build_failure_judges_against_the_build_host_not_the_binary() {
        let emulation_failure = BuilderError::BuildFailed("exec format error".into());

        // Daemon is arm64 (via DOCKER_HOST); an arm64 build is native there,
        // whatever architecture this process was compiled for.
        let native_on_remote_daemon = BuildImageJob::describe_build_failure(
            Some("linux/arm64"),
            "linux/arm64",
            &emulation_failure,
        );
        assert!(
            !native_on_remote_daemon.contains("binfmt"),
            "a daemon-native build must not be blamed on missing emulation: {}",
            native_on_remote_daemon
        );

        // Same daemon, amd64 build: that IS a cross-build for it.
        let cross_on_remote_daemon = BuildImageJob::describe_build_failure(
            Some("linux/amd64"),
            "linux/arm64",
            &emulation_failure,
        );
        assert!(
            cross_on_remote_daemon.contains("binfmt"),
            "a real cross-build must carry the install command: {}",
            cross_on_remote_daemon
        );
        assert!(
            cross_on_remote_daemon.contains("--install amd64"),
            "the command must name the architecture to install: {}",
            cross_on_remote_daemon
        );
    }

    /// A Docker Hub anonymous-pull rate limit must be named explicitly, with
    /// the registry-mirror fix, instead of surfacing as an opaque pull
    /// failure that reads like a broken Dockerfile.
    #[test]
    fn test_describe_build_failure_names_docker_hub_rate_limit() {
        let host = "linux/amd64";
        let rate_limited = BuilderError::BuildFailed(
            "failed to resolve source metadata for docker.io/library/node:22-slim: \
             toomanyrequests: You have reached your pull rate limit"
                .into(),
        );

        let msg = BuildImageJob::describe_build_failure(Some(host), host, &rate_limited);
        assert!(msg.contains("Docker Hub rate-limited"), "got: {}", msg);
        assert!(msg.contains("registry-mirrors"), "got: {}", msg);
        assert!(msg.contains("daemon.json"), "got: {}", msg);
        assert!(!msg.contains("binfmt"), "not a QEMU problem: {}", msg);

        // Also recognised with no platform requested (single-arch path).
        let plain = BuildImageJob::describe_build_failure(None, host, &rate_limited);
        assert!(plain.contains("Docker Hub rate-limited"), "got: {}", plain);

        // An authorize failure that happens to 429 is the same underlying
        // problem, just surfaced by BuildKit's resolver instead of the
        // classic builder.
        let buildkit_phrasing = BuilderError::BuildFailed(
            "failed to authorize: failed to fetch anonymous token: unexpected status \
             from GET request to https://auth.docker.io/token: 429 Too Many Requests"
                .into(),
        );
        let buildkit_msg =
            BuildImageJob::describe_build_failure(Some(host), host, &buildkit_phrasing);
        assert!(
            buildkit_msg.contains("Docker Hub rate-limited"),
            "got: {}",
            buildkit_msg
        );

        // An unrelated failure must not be misdiagnosed as a rate limit.
        let unrelated = BuilderError::BuildFailed("npm ERR! missing script: build".into());
        let unrelated_msg = BuildImageJob::describe_build_failure(Some(host), host, &unrelated);
        assert!(
            !unrelated_msg.contains("Docker Hub rate-limited"),
            "got: {}",
            unrelated_msg
        );
    }

    #[test]
    fn test_repository_output_from_context() {
        let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);

        // Set up outputs as the download job would
        context
            .set_output("download_repo", "repo_dir", "/tmp/repo")
            .unwrap();
        context
            .set_output("download_repo", "checkout_ref", "main")
            .unwrap();
        context
            .set_output("download_repo", "repo_owner", "user")
            .unwrap();
        context
            .set_output("download_repo", "repo_name", "project")
            .unwrap();

        let repo_output = RepositoryOutput::from_context(&context, "download_repo").unwrap();
        assert_eq!(repo_output.repo_dir, PathBuf::from("/tmp/repo"));
        assert_eq!(repo_output.checkout_ref, "main");
        assert_eq!(repo_output.repo_owner, "user");
        assert_eq!(repo_output.repo_name, "project");
    }

    #[test]
    fn test_describe_build_failure_explains_out_of_memory() {
        let host = "linux/amd64";
        let oom = BuilderError::BuildOutOfMemory {
            message: "Docker stream error: process \"/bin/sh -c npm run build\" did not \
                      complete successfully: exit code: 1"
                .into(),
            diagnosis: temps_deployer::BuildMemoryDiagnosis {
                attribution: temps_deployer::OomAttribution::OnlyBuildRunning,
                victim: None,
                host_oom_kills: Some(1),
                exit_code: Some(1),
                host_memory_mb: Some(3902),
                requested_cap_mb: 2047,
                cap_enforced: false,
            },
        };

        let msg = BuildImageJob::describe_build_failure(Some(host), host, &oom);
        assert!(
            msg.starts_with(
                "Failed to build image for linux/amd64: The build step most likely ran out of memory"
            ),
            "got: {}",
            msg
        );
        assert!(
            msg.contains("; no other build was running;"),
            "got: {}",
            msg
        );
        assert!(
            msg.contains("terminated 1 process on this host"),
            "got: {}",
            msg
        );
        assert!(msg.contains("host RAM 3902 MB"), "got: {}", msg);
        assert!(
            msg.contains("does not apply the per-build memory cap"),
            "got: {}",
            msg
        );
        assert!(
            msg.contains("NODE_OPTIONS=--max-old-space-size"),
            "got: {}",
            msg
        );
        assert!(
            msg.contains("https://temps.sh/docs/set-up-ci-cd-pipeline"),
            "got: {}",
            msg
        );
        assert!(
            msg.ends_with("exit code: 1"),
            "keeps the builder's own text: {}",
            msg
        );
        assert!(!msg.contains("Docker Hub rate-limited"), "got: {}", msg);
        assert!(!msg.contains("binfmt"), "got: {}", msg);

        // No platform requested: same explanation without the scope.
        let plain = BuildImageJob::describe_build_failure(None, host, &oom);
        assert!(
            plain
                .starts_with("Failed to build image: The build step most likely ran out of memory"),
            "got: {}",
            plain
        );

        // Under the legacy builder the cap is real, so the advice is to raise it.
        let capped = BuilderError::BuildOutOfMemory {
            message: "returned a non-zero code: 137".into(),
            diagnosis: temps_deployer::BuildMemoryDiagnosis {
                attribution: temps_deployer::OomAttribution::StepKilled,
                victim: None,
                host_oom_kills: Some(1),
                exit_code: Some(137),
                host_memory_mb: Some(3902),
                requested_cap_mb: 512,
                cap_enforced: true,
            },
        };
        let capped_msg = BuildImageJob::describe_build_failure(None, host, &capped);
        assert!(
            capped_msg.contains("memory cap of 512 MB (Settings > Build Limits) was reached"),
            "got: {}",
            capped_msg
        );
        assert!(
            !capped_msg.contains("does not apply"),
            "got: {}",
            capped_msg
        );
    }

    #[test]
    fn test_detect_log_level_files_deployer_error_lines_as_errors() {
        // The daemon's step failure text contains "successfully"; the
        // `ERROR:` prefix the deployer adds must win over that substring.
        let daemon_error = "ERROR: Build failed: Docker stream error: process \"/bin/sh -c npm \
                            run build\" did not complete successfully: exit code: 1";
        assert!(matches!(
            BuildImageJob::detect_log_level(daemon_error),
            LogLevel::Error
        ));
        let memory_line = "ERROR: The build step ran out of memory: the kernel's OOM killer \
                           terminated 1 process on this host while the step ran";
        assert!(matches!(
            BuildImageJob::detect_log_level(memory_line),
            LogLevel::Error
        ));
        // Ordinary step output keeps the existing heuristics.
        assert!(matches!(
            BuildImageJob::detect_log_level("Image built successfully: app:latest"),
            LogLevel::Success
        ));
        assert!(matches!(
            BuildImageJob::detect_log_level("Creating an optimized production build ..."),
            LogLevel::Info
        ));
    }

    /// Run a preset-generated build of `files` and return the job result, the
    /// recording builder, the Dockerfile Temps generated (if any) and the
    /// checkout directory.
    async fn generated_build(
        preset: StoredPreset,
        preset_config: Option<StoredPresetConfig>,
        files: &[(&str, &str)],
    ) -> (
        Result<ImageOutput, WorkflowError>,
        Arc<RecordingImageBuilder>,
        Option<String>,
        tempfile::TempDir,
    ) {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("app:latest".into())
            .preset(preset)
            .preset_config(preset_config)
            .build(builder.clone())
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        let repo = RepositoryOutput {
            repo_dir: dir.path().into(),
            checkout_ref: "main".into(),
            repo_owner: "owner".into(),
            repo_name: "sample-app".into(),
        };
        let mut context = crate::test_utils::create_test_context("wf".into(), 1, 1, 1);
        for (key, value) in [
            ("repo_dir", dir.path().to_string_lossy().to_string()),
            ("checkout_ref", "main".to_string()),
            ("repo_owner", "owner".to_string()),
            ("repo_name", "sample-app".to_string()),
        ] {
            context.set_output("download_repo", key, value).unwrap();
        }
        let result = job.build_image(&repo, &context).await;
        let dockerfile = std::fs::read_to_string(dir.path().join("Dockerfile")).ok();
        (result, builder, dockerfile, dir)
    }

    const VITE_PACKAGE: &str =
        r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"6"}}"#;

    #[test]
    fn build_overrides_prefer_repository_then_project_values() {
        let repository = BuildOverrides {
            install_command: Some("pnpm install".into()),
            build_command: Some("  ".into()),
            output_dir: None,
        };
        let project = BuildOverrides::from_stored(&StoredPresetConfig::Vite(
            temps_entities::preset::ViteConfig {
                install_command: Some("npm ci".into()),
                build_command: Some("npm run build:prod".into()),
                output_dir: Some("build".into()),
            },
        ));
        let effective = repository.or(project);
        assert_eq!(effective.install_command.as_deref(), Some("pnpm install"));
        assert_eq!(
            effective.build_command.as_deref(),
            Some("npm run build:prod")
        );
        assert_eq!(effective.output_dir.as_deref(), Some("build"));
        assert!(
            BuildOverrides::from_stored(&StoredPresetConfig::Nixpacks(Default::default()))
                .is_empty()
        );
    }

    #[test]
    fn build_overrides_reject_multiline_values_and_escaping_output_dirs() {
        for overrides in [
            BuildOverrides {
                build_command: Some("npm run build\nRUN curl attacker".into()),
                ..Default::default()
            },
            BuildOverrides {
                output_dir: Some("../outside".into()),
                ..Default::default()
            },
            BuildOverrides {
                output_dir: Some("/usr/share".into()),
                ..Default::default()
            },
        ] {
            let error = overrides.validate().unwrap_err().to_string();
            assert!(error.contains("Invalid configuration"), "{error}");
        }
        assert!(BuildOverrides {
            output_dir: Some("./build".into()),
            ..Default::default()
        }
        .validate()
        .is_ok());
    }

    #[tokio::test]
    async fn project_vite_output_dir_reaches_the_generated_dockerfile() {
        let (result, builder, dockerfile, _dir) = generated_build(
            StoredPreset::Vite,
            Some(StoredPresetConfig::Vite(
                temps_entities::preset::ViteConfig {
                    output_dir: Some("build".into()),
                    ..Default::default()
                },
            )),
            &[("package.json", VITE_PACKAGE)],
        )
        .await;
        result.unwrap();
        assert_eq!(builder.builds().len(), 1);
        let dockerfile = dockerfile.unwrap();
        assert!(
            dockerfile.contains("COPY --from=builder /app/build /usr/share/nginx/html"),
            "{dockerfile}"
        );
    }

    #[tokio::test]
    async fn temps_yaml_output_dir_beats_project_settings() {
        let (result, _builder, dockerfile, _dir) = generated_build(
            StoredPreset::Vite,
            Some(StoredPresetConfig::Vite(
                temps_entities::preset::ViteConfig {
                    output_dir: Some("build".into()),
                    ..Default::default()
                },
            )),
            &[
                ("package.json", VITE_PACKAGE),
                (".temps.yaml", "build:\n  output_dir: out\n"),
            ],
        )
        .await;
        result.unwrap();
        assert!(dockerfile
            .unwrap()
            .contains("/app/out /usr/share/nginx/html"));
    }

    #[tokio::test]
    async fn vite_without_build_script_fails_before_building() {
        let (result, builder, dockerfile, _dir) = generated_build(
            StoredPreset::Vite,
            None,
            &[("package.json", r#"{"scripts":{"dev":"vite"}}"#)],
        )
        .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Missing script: build"), "{error}");
        assert!(builder.builds().is_empty(), "image builder must not run");
        assert!(dockerfile.is_none(), "no Dockerfile should be written");
        let wrapped = format!(
            "Job execution failed: Required job 'build_image' failed: {:?}",
            Some(error)
        );
        assert_eq!(
            crate::services::failure_classifier::classify_failure_reason(Some(&wrapped)).code,
            crate::services::failure_classifier::DeploymentFailureCode::MissingBuildScript
        );
    }

    #[tokio::test]
    async fn unplannable_autopack_app_fails_before_building() {
        let (result, builder, _dockerfile, _dir) =
            generated_build(StoredPreset::Autopack, None, &[("notes.txt", "nothing")]).await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("could not plan this application"), "{error}");
        assert!(builder.builds().is_empty(), "image builder must not run");
        let wrapped = format!(
            "Job execution failed: Required job 'build_image' failed: {:?}",
            Some(error)
        );
        let classified =
            crate::services::failure_classifier::classify_failure_reason(Some(&wrapped));
        assert_eq!(
            classified.code,
            crate::services::failure_classifier::DeploymentFailureCode::InvalidConfiguration
        );
    }

    #[tokio::test]
    async fn nixpacks_builds_no_longer_write_a_framework_nixpacks_toml() {
        let (result, builder, _dockerfile, dir) = generated_build(
            StoredPreset::Nixpacks,
            Some(StoredPresetConfig::Nixpacks(
                temps_entities::preset::NixpacksConfig {
                    nixpacks_config: None,
                    providers: vec![temps_entities::preset::NixpacksProvider::Node],
                },
            )),
            &[
                ("package.json", r#"{"scripts":{"build":"vite build","start":"vite preview"},"devDependencies":{"vite":"6"}}"#),
                ("index.html", "<div id=app></div>"),
            ],
        )
        .await;
        result.unwrap();
        assert_eq!(builder.builds().len(), 1);
        assert!(
            !dir.path().join("nixpacks.toml").exists(),
            "a nixpacks.toml written after planning never influences the build"
        );
    }

    /// A worker of one architecture, for mixed-architecture build tests: it
    /// builds natively, streams one build line, and reports `built_platform`
    /// when the control plane inspects the result.
    struct ArchWorker {
        platform: &'static str,
        built_platform: &'static str,
        failure: Option<&'static str>,
        /// Never finish, until the build future is dropped.
        hang: bool,
        stopped: Arc<std::sync::atomic::AtomicBool>,
        requests: std::sync::Mutex<Vec<BuildRequest>>,
    }

    impl ArchWorker {
        fn new(platform: &'static str) -> Self {
            Self {
                platform,
                built_platform: platform,
                failure: None,
                hang: false,
                stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn request(&self) -> BuildRequest {
            self.requests.lock().unwrap()[0].clone()
        }
    }

    /// Sets its flag when dropped: proves a hanging build was stopped.
    struct SetOnDrop(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl ImageBuilder for ArchWorker {
        async fn build_image(&self, _request: BuildRequest) -> Result<BuildResult, BuilderError> {
            unreachable!("worker builds stream their output")
        }

        async fn build_image_with_callback(
            &self,
            request: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            self.requests.lock().unwrap().push(request.request.clone());
            if let Some(callback) = request.log_callback.as_ref() {
                callback(format!("#1 building on {}", self.platform)).await;
            }
            if self.hang {
                let _stopped = SetOnDrop(self.stopped.clone());
                std::future::pending::<()>().await;
            }
            if let Some(failure) = self.failure {
                tokio::time::sleep(Duration::from_millis(20)).await;
                return Err(BuilderError::BuildFailed(failure.to_string()));
            }
            Ok(BuildResult {
                image_id: format!("sha256:{}", self.platform.replace('/', "-")),
                image_name: request.request.image_name,
                size_bytes: 2 * 1024 * 1024,
                build_duration_ms: 5,
            })
        }

        async fn import_image(&self, _: PathBuf, _: &str) -> Result<String, BuilderError> {
            unreachable!()
        }

        async fn extract_from_image(&self, _: &str, _: &str, _: &Path) -> Result<(), BuilderError> {
            unreachable!()
        }

        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            Ok(vec![])
        }

        async fn remove_image(&self, _: &str) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn inspect_image(
            &self,
            image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, BuilderError> {
            Ok(temps_deployer::ImageInfo {
                id: format!("sha256:{}", self.platform.replace('/', "-")),
                architecture: temps_deployer::platform::platform_arch(self.built_platform),
                os: "linux".to_string(),
                platform: self.built_platform.to_string(),
                size_bytes: 1,
                tags: vec![image_name.to_string()],
                created: None,
                working_dir: None,
                config: None,
            })
        }

        async fn save_image(&self, _: &str, _: &Path) -> Result<(), BuilderError> {
            unreachable!()
        }

        fn get_native_platform(&self) -> String {
            self.platform.to_string()
        }
    }

    const CACHE_NS: &str = "0123456789abcdef";

    fn per_architecture_job(
        workers: &[(i32, &str, Arc<ArchWorker>)],
        log_service: Option<Arc<LogService>>,
    ) -> BuildImageJob {
        let mut builder = BuildImageJobBuilder::new()
            .job_id("build_image".into())
            .download_job_id("download_repo".into())
            .image_tag("app:latest".into())
            .local_workloads_enabled(false)
            .build_args(vec![
                (
                    temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG.to_string(),
                    CACHE_NS.to_string(),
                ),
                ("PUBLIC_GREETING".to_string(), "hello".to_string()),
                ("DATABASE_PASSWORD".to_string(), "do-not-send".to_string()),
            ])
            .worker_build_arg_keys(["PUBLIC_GREETING".to_string()])
            .platform_builders(
                workers
                    .iter()
                    .map(|(node_id, name, worker)| WorkerPlatformBuilder {
                        platform: worker.platform.to_string(),
                        node_id: *node_id,
                        node_name: name.to_string(),
                        builder: worker.clone(),
                    })
                    .collect(),
            );
        if let Some(log_service) = log_service {
            builder = builder.log_id("build-log".into()).log_service(log_service);
        }
        builder.build(Arc::new(PanicsIfCalledImageBuilder)).unwrap()
    }

    fn per_architecture_context() -> (WorkflowContext, Arc<CapturingLogWriter>) {
        let writer = Arc::new(CapturingLogWriter::default());
        let context = WorkflowContext::new("wf".into(), 1, 1, 1, writer.clone());
        (context, writer)
    }

    /// A mixed-architecture deployment builds each platform once, natively,
    /// on the worker of that architecture: the first platform owns the plain
    /// tag, each image records the worker that holds it, every build line is
    /// prefixed with its platform, and each architecture gets its own
    /// BuildKit cache namespace. No secret value reaches either worker.
    #[tokio::test]
    async fn mixed_architecture_build_runs_natively_on_one_worker_per_platform() {
        let amd = Arc::new(ArchWorker::new("linux/amd64"));
        let arm = Arc::new(ArchWorker::new("linux/arm64"));
        let logs_dir = tempfile::tempdir().unwrap();
        let log_service = Arc::new(LogService::new(logs_dir.path().to_path_buf()));
        let job = per_architecture_job(
            &[(4, "w-amd", amd.clone()), (5, "w-arm", arm.clone())],
            Some(log_service.clone()),
        );
        assert_eq!(job.remote_builder_node_id, Some(4));
        let (dir, repo) = repo_with_dockerfile();
        let (context, writer) = per_architecture_context();

        let output = job.build_image(&repo, &context).await.unwrap();
        drop(dir);

        assert_eq!(output.image_tag, "app:latest");
        assert_eq!(output.image_id, "sha256:linux-amd64");
        assert_eq!(
            output.image_tags_by_platform,
            HashMap::from([
                ("linux/amd64".to_string(), "app:latest".to_string()),
                ("linux/arm64".to_string(), "app:latest-arm64".to_string()),
            ])
        );
        assert_eq!(
            output.builder_node_ids_by_platform,
            HashMap::from([
                ("linux/amd64".to_string(), 4),
                ("linux/arm64".to_string(), 5)
            ])
        );
        // The id each worker reported for its own image, which every later
        // transfer of that tag must match.
        assert_eq!(
            output.image_ids_by_platform,
            HashMap::from([
                ("linux/amd64".to_string(), "sha256:linux-amd64".to_string()),
                ("linux/arm64".to_string(), "sha256:linux-arm64".to_string()),
            ])
        );

        let (amd_request, arm_request) = (amd.request(), arm.request());
        assert_eq!(amd_request.platform.as_deref(), Some("linux/amd64"));
        assert_eq!(arm_request.platform.as_deref(), Some("linux/arm64"));
        assert_eq!(amd_request.image_name, "app:latest");
        assert_eq!(arm_request.image_name, "app:latest-arm64");
        let ns = temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG;
        assert_eq!(
            amd_request.build_args.get(ns).map(String::as_str),
            Some(CACHE_NS)
        );
        assert_eq!(
            arm_request.build_args.get(ns).cloned(),
            Some(format!("{CACHE_NS}-arm64"))
        );
        for request in [&amd_request, &arm_request] {
            assert_eq!(
                request
                    .build_args
                    .get("PUBLIC_GREETING")
                    .map(String::as_str),
                Some("hello")
            );
            assert!(!request.build_args.contains_key("DATABASE_PASSWORD"));
            assert!(request.build_args_buildkit.is_empty());
        }

        let lines = writer.lines.lock().unwrap().join("\n");
        assert!(lines.contains("building 2 images in parallel"), "{lines}");
        assert!(
            lines.contains("[linux/arm64] Building 'app:latest-arm64' on worker 'w-arm' (id=5)"),
            "{lines}"
        );
        assert!(lines.contains("All 2 architecture images built"), "{lines}");
        assert!(!lines.contains("do-not-send"), "{lines}");
        let log = log_service
            .get_structured_logs("build-log")
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            log.contains("[linux/amd64] #1 building on linux/amd64"),
            "{log}"
        );
        assert!(
            log.contains("[linux/arm64] #1 building on linux/arm64"),
            "{log}"
        );
        assert!(!log.contains("do-not-send"), "{log}");
    }

    /// One architecture failing fails the whole build with that
    /// architecture's reason, and stops the builds still running: a
    /// deployment never rolls out on only some of its architectures.
    #[tokio::test]
    async fn mixed_architecture_build_fails_whole_when_one_architecture_fails() {
        let amd = Arc::new(ArchWorker {
            hang: true,
            ..ArchWorker::new("linux/amd64")
        });
        let arm = Arc::new(ArchWorker {
            failure: Some("RUN npm ci exited with code 1"),
            ..ArchWorker::new("linux/arm64")
        });
        let job = per_architecture_job(&[(4, "w-amd", amd.clone()), (5, "w-arm", arm)], None);
        let (dir, repo) = repo_with_dockerfile();
        let (context, writer) = per_architecture_context();

        let error = tokio::time::timeout(Duration::from_secs(10), job.build_image(&repo, &context))
            .await
            .expect("a failed architecture must not wait for the others")
            .unwrap_err();
        drop(dir);

        let message = error.to_string();
        assert!(message.contains("linux/arm64"), "{message}");
        assert!(
            message.contains("RUN npm ci exited with code 1"),
            "{message}"
        );
        assert!(message.contains("worker 'w-arm', id=5"), "{message}");
        assert!(
            amd.stopped.load(std::sync::atomic::Ordering::SeqCst),
            "the amd64 build still running must be stopped"
        );
        let lines = writer.lines.lock().unwrap().join("\n");
        assert!(
            lines.contains(
                "[linux/amd64] Build on worker 'w-amd' stopped: the linux/arm64 build failed"
            ),
            "{lines}"
        );
        assert!(
            lines.contains("not rolled out on any architecture"),
            "{lines}"
        );
    }

    /// A worker whose image is not the architecture it was asked to build is
    /// a failure, never a mislabelled image deployed to the wrong nodes.
    #[tokio::test]
    async fn mixed_architecture_build_refuses_an_image_of_the_wrong_architecture() {
        let amd = Arc::new(ArchWorker::new("linux/amd64"));
        let arm = Arc::new(ArchWorker {
            built_platform: "linux/amd64",
            ..ArchWorker::new("linux/arm64")
        });
        let job = per_architecture_job(&[(4, "w-amd", amd), (5, "w-arm", arm)], None);
        let (dir, repo) = repo_with_dockerfile();
        let (context, _writer) = per_architecture_context();

        let error = job.build_image(&repo, &context).await.unwrap_err();
        drop(dir);
        let message = error.to_string();
        assert!(
            message.contains("Build for linux/arm64 produced a linux/amd64 image"),
            "{message}"
        );
    }

    /// The job's outputs carry each architecture's builder, which the deploy
    /// and rollback paths read back; a single-architecture build records no
    /// such map, exactly as before.
    #[tokio::test]
    async fn mixed_architecture_build_records_each_builder_in_its_outputs() {
        let amd = Arc::new(ArchWorker::new("linux/amd64"));
        let arm = Arc::new(ArchWorker::new("linux/arm64"));
        let job = per_architecture_job(&[(4, "w-amd", amd), (5, "w-arm", arm)], None);
        let (dir, _repo) = repo_with_dockerfile();
        let (mut context, _writer) = per_architecture_context();
        for (key, value) in [
            ("repo_dir", dir.path().to_string_lossy().to_string()),
            ("checkout_ref", "main".to_string()),
            ("repo_owner", "owner".to_string()),
            ("repo_name", "repo".to_string()),
        ] {
            context.set_output("download_repo", key, value).unwrap();
        }

        let result = job.execute(context).await.unwrap();
        let outputs = &result.context;
        let by_platform: HashMap<String, i32> = outputs
            .get_output("build_image", "builder_node_ids_by_platform")
            .unwrap()
            .unwrap();
        assert_eq!(
            by_platform,
            HashMap::from([
                ("linux/amd64".to_string(), 4),
                ("linux/arm64".to_string(), 5)
            ])
        );
        let primary: Option<i32> = outputs
            .get_output("build_image", "builder_node_id")
            .unwrap();
        assert_eq!(primary, Some(4));
        let image_ids: HashMap<String, String> = outputs
            .get_output("build_image", "image_ids_by_platform")
            .unwrap()
            .unwrap();
        assert_eq!(image_ids.len(), 2);
        let image = ImageOutput::from_context(outputs, "build_image").unwrap();
        assert_eq!(image.builder_node_ids_by_platform, by_platform);
        assert_eq!(image.image_ids_by_platform, image_ids);
    }

    /// A single-architecture build -- one platform, or the control plane's
    /// own multi-platform loop -- keeps the plain cache namespace and is not
    /// spread across workers.
    #[test]
    fn single_architecture_builds_keep_their_cache_namespace() {
        let ns = temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG;
        let args = HashMap::from([(ns.to_string(), CACHE_NS.to_string())]);
        assert_eq!(platform_build_args(&args, 0, Some("linux/arm64")), args);
        assert_eq!(platform_build_args(&args, 0, None), args);
        assert_eq!(
            platform_build_args(&args, 1, Some("linux/arm/v7"))[ns],
            format!("{CACHE_NS}-arm-v7")
        );
        let no_namespace = HashMap::from([("A".to_string(), "1".to_string())]);
        assert_eq!(
            platform_build_args(&no_namespace, 1, Some("linux/arm64")),
            no_namespace
        );

        let one = per_architecture_job(
            &[(4, "w-amd", Arc::new(ArchWorker::new("linux/amd64")))],
            None,
        );
        assert!(!one.is_per_architecture_worker_build());
        assert_eq!(one.worker_nodes_phrase(4), "worker node 4");
    }
}
