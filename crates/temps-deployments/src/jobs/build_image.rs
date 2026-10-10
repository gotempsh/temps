// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build Image Job
//!
//! Builds container images from downloaded repository source code

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

fn read_confined_control_file(
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
}

impl ImageOutput {
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

        Ok(Self {
            image_tag,
            image_id,
            size_bytes,
            build_context: PathBuf::from(build_context_str),
            dockerfile_path: PathBuf::from(dockerfile_path_str),
            image_tags_by_platform,
        })
    }
}

/// Configuration for building images
#[derive(Debug, Clone)]
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
        }
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
                        "⚠️ Could not copy the image to the control plane: {error}. Source \
                         maps, static assets and the vulnerability scan for this deployment \
                         may be skipped"
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

    /// How a generated Dockerfile treats project variables on a worker build.
    ///
    /// A worker never receives build-argument values and refuses a Dockerfile
    /// that declares an `ARG` (`temps_deployer::remote`), so every generated
    /// `ARG` is left out there. That is only safe for variables the build
    /// does not inline: `Err` names the ones a framework bakes into the
    /// bundle, which would silently be empty. Autopack has always built on
    /// workers without any of them and only warns, so it never refuses.
    fn worker_build_vars(
        preset: &dyn temps_presets::Preset,
        build_vars: &[String],
    ) -> Result<(), Vec<String>> {
        if preset.uses_autopack() {
            return Ok(());
        }
        let inlined = build_inlined_variables(build_vars.iter().map(String::as_str));
        if inlined.is_empty() {
            Ok(())
        } else {
            Err(inlined)
        }
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
    ) -> Result<(std::collections::HashMap<String, String>, PathBuf), WorkflowError> {
        // If Dockerfile exists, we're done (no preset build args)
        if fs::symlink_metadata(dockerfile_path).is_ok() {
            return Ok((std::collections::HashMap::new(), build_context_dir.clone()));
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

        // A worker build never receives build-argument values and refuses a
        // Dockerfile that declares an `ARG` (`temps_deployer::remote`). Every
        // build carries platform variables (HOST, telemetry, the BuildKit
        // cache namespace), so declaring them would refuse every generated
        // build; leave them out and say what the build does not receive.
        if let Some(node_id) = self.remote_builder_node_id {
            if let Err(inlined) = Self::worker_build_vars(preset.as_ref(), &build_vars) {
                // Same shape as a preset's own plan failure, so it is
                // classified as configuration rather than an invalid Dockerfile.
                let message = format!(
                    "Build plan failed for preset '{}': this build runs on worker node {}, \
                     which does not receive project variables yet, but {} {} inlined into the \
                     application at build time and would be empty. On a server that builds \
                     locally, set this environment's build location to the control plane; \
                     otherwise build the app in CI and deploy the resulting image or static \
                     bundle.",
                    preset_slug,
                    node_id,
                    inlined.join(", "),
                    if inlined.len() == 1 { "is" } else { "are" }
                );
                self.log_with_level(context, LogLevel::Error, format!("ERROR: {message}"))
                    .await?;
                return Err(WorkflowError::JobExecutionFailed(message));
            }
            let project_variables = build_vars
                .iter()
                .filter(|name| !name.starts_with("BUILDKIT_"))
                .count();
            if project_variables > 0 {
                let consequence = if preset.uses_autopack() {
                    "so a value the framework inlines at build time (VITE_*, NEXT_PUBLIC_*, \
                     PUBLIC_*) will be empty."
                } else {
                    "none of them is a variable this framework inlines at build time."
                };
                self.log(
                    context,
                    format!(
                        "Build warning: this build runs on worker node {}, which does not receive \
                         project variables yet. {} variable(s) are set when the container runs, \
                         but not while it builds; {}",
                        node_id, project_variables, consequence
                    ),
                )
                .await?;
            }
            build_vars.clear();
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
        Ok((dockerfile_with_args.build_args, preset_root))
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
        let (preset_build_args, preset_root) = self
            .ensure_dockerfile(context, &build_context, &dockerfile_path, &canonical_root)
            .await?;
        build_context = preset_root;

        // Write .npmrc into the build context when NPM_RC / NPM_TOKEN env vars
        // are provided (Vercel-compatible behavior). No-op otherwise.
        self.ensure_npmrc(context, &build_context).await?;

        // Merge preset build args with user-provided build args
        // User-provided args take precedence
        let user_arg_keys: std::collections::HashSet<String> = self
            .build_config
            .build_args
            .iter()
            .map(|(k, _)| k.clone())
            .collect();

        let mut build_args = self.build_config.build_args.clone();
        for (key, value) in preset_build_args {
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

        let mut build_args_buildkit = HashMap::new();
        for (key, value) in &self.build_config.build_args_buildkit {
            build_args_buildkit.insert(key.clone(), value.clone());
        }

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
                build_args: build_args.clone(),
                build_args_buildkit: build_args_buildkit.clone(),
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
                    .verify_built_platform(&build_result.image_name, platform, context)
                    .await
                {
                    Ok(()) => {
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
        })
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
        image_name: &str,
        requested_platform: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let built_platform = match self.image_builder.inspect_image(image_name).await {
            Ok(info) => info.platform,
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
                return Ok(());
            }
        };

        if temps_deployer::platform::platforms_match(&built_platform, requested_platform) {
            return Ok(());
        }

        let msg = format!(
            "Build for {} produced a {} image ('{}'). The Docker daemon accepted the \
             requested platform but ignored it — this is what the legacy builder does. \
             Enable BuildKit on the control plane (Docker 23+ enables it by default; \
             otherwise set DOCKER_BUILDKIT=1 or install docker-buildx) and redeploy. \
             Deploying this image would fail on the target node with 'exec format error'.",
            requested_platform, built_platform, image_name
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
        let image_output = self.build_image(&repo_output, &context).await?;
        self.copy_image_to_control_plane(&context, &image_output.image_tag)
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
        }
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
            .with_control_plane_copy(self.control_plane_copy);

        Ok(job)
    }
}

impl Default for BuildImageJobBuilder {
    fn default() -> Self {
        Self::new()
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
                    && line.contains("Source maps")),
            "{lines:?}"
        );
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
    fn worker_builds_refuse_only_build_inlined_variables() {
        let autopack = temps_presets::get_preset_by_slug("autopack").unwrap();
        let python = temps_presets::get_preset_by_slug("python").unwrap();
        let nextjs = temps_presets::get_preset_by_slug("nextjs").unwrap();
        let vite = temps_presets::get_preset_by_slug("vite").unwrap();
        // What every build carries even with no project variables at all.
        let platform: Vec<String> = ["HOST", "SENTRY_DSN", "BUILDKIT_CACHE_MOUNT_NS"]
            .into_iter()
            .map(String::from)
            .collect();
        let with_public = |name: &str| {
            let mut vars = platform.clone();
            vars.push(name.to_string());
            vars
        };

        // Platform and runtime-only variables never block a worker build.
        for preset in [&autopack, &python, &nextjs, &vite] {
            assert_eq!(
                BuildImageJob::worker_build_vars(preset.as_ref(), &platform),
                Ok(())
            );
        }
        // Autopack has always built on workers without any of them.
        assert_eq!(
            BuildImageJob::worker_build_vars(autopack.as_ref(), &with_public("VITE_API_URL")),
            Ok(())
        );
        // Other presets refuse rather than build an app whose inlined
        // variables are silently empty, and name exactly those variables.
        assert_eq!(
            BuildImageJob::worker_build_vars(vite.as_ref(), &with_public("VITE_API_URL")),
            Err(vec!["VITE_API_URL".to_string()])
        );
        assert_eq!(
            BuildImageJob::worker_build_vars(nextjs.as_ref(), &with_public("NEXT_PUBLIC_SITE")),
            Err(vec!["NEXT_PUBLIC_SITE".to_string()])
        );
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

    /// The generated Vite Dockerfile for a worker build declares no `ARG`,
    /// so the worker's context validation accepts it, while a local build
    /// keeps declaring every variable.
    #[tokio::test]
    async fn generated_vite_dockerfile_on_worker_declares_no_build_args() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"qa-vite","scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
        let repo = RepositoryOutput {
            repo_dir: dir.path().to_path_buf(),
            checkout_ref: "main".to_string(),
            repo_owner: "owner".to_string(),
            repo_name: "repo".to_string(),
        };
        let mut context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);
        context
            .set_output(
                "download_repo",
                "repo_dir",
                repo.repo_dir.display().to_string(),
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
        let platform_args = vec![
            ("HOST".to_string(), "0.0.0.0".to_string()),
            ("SENTRY_DSN".to_string(), "synthetic-dsn".to_string()),
            ("BUILDKIT_CACHE_MOUNT_NS".to_string(), "ns".to_string()),
        ];
        for (worker, expect_args) in [(Some(7), false), (None, true)] {
            let mut builder = BuildImageJobBuilder::new()
                .job_id("build".into())
                .download_job_id("download_repo".into())
                .image_tag("app:latest".into())
                .preset(StoredPreset::Vite)
                .build_args(platform_args.clone());
            if let Some(node_id) = worker {
                builder = builder.remote_builder_node_id(node_id);
            }
            let job = builder
                .build(Arc::new(RecordingImageBuilder::default()))
                .unwrap();
            let dockerfile = dir.path().join("Dockerfile");
            let _ = std::fs::remove_file(&dockerfile);
            job.ensure_dockerfile(&context, &repo.repo_dir, &dockerfile, &repo.repo_dir)
                .await
                .unwrap();
            let rendered = std::fs::read_to_string(&dockerfile).unwrap();
            assert_eq!(rendered.contains("ARG HOST"), expect_args, "{rendered}");
            assert_eq!(
                rendered.contains("ARG SENTRY_DSN"),
                expect_args,
                "{rendered}"
            );
            assert!(!rendered.contains("synthetic-dsn"), "{rendered}");
        }

        // A public build-time variable is refused before anything is built.
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("app:latest".into())
            .preset(StoredPreset::Vite)
            .remote_builder_node_id(7)
            .build_args(vec![("VITE_API_URL".to_string(), "synthetic".to_string())])
            .build(Arc::new(RecordingImageBuilder::default()))
            .unwrap();
        let dockerfile = dir.path().join("Dockerfile");
        let _ = std::fs::remove_file(&dockerfile);
        let error = job
            .ensure_dockerfile(&context, &repo.repo_dir, &dockerfile, &repo.repo_dir)
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(matches!(error, WorkflowError::JobExecutionFailed(_)));
        assert!(message.contains("VITE_API_URL"), "{message}");
        assert!(
            message.contains("Build plan failed for preset 'vite'"),
            "{message}"
        );
        assert!(!message.contains("synthetic"), "{message}");
        assert!(!dockerfile.exists());
    }

    /// What a stand-in worker received on `POST /agent/images/build`.
    #[derive(Default)]
    struct CapturedWorkerBuild {
        authorization: Option<String>,
        spec_json: String,
        spec: Option<temps_deployer::build_protocol::BuildSpec>,
        files: std::collections::BTreeMap<String, String>,
    }

    /// Stands in for a worker agent's build endpoint. Before answering it
    /// applies the checks the real agent (`temps_agent::build_handler`)
    /// applies before handing the context to Docker: the spec must come first
    /// and pass `BuildSpec::validate`, the context must be a readable tar, and
    /// the spec's Dockerfile must be in it. Answers with the same NDJSON
    /// terminal event the agent streams.
    async fn fake_worker_build(
        axum::extract::State(captured): axum::extract::State<
            Arc<std::sync::Mutex<CapturedWorkerBuild>>,
        >,
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
        let mut captured = captured.lock().unwrap();
        captured.authorization = authorization;
        captured.spec_json = spec_json;
        captured.spec = Some(spec);
        captured.files = files;
        (StatusCode::OK, format!("{event}\n"))
    }

    /// Issue #1344, end to end on the control-plane side: an ordinary Vite
    /// source with no Dockerfile, built on a worker with the platform
    /// variables every build carries plus a runtime-only secret, goes through
    /// the real `RemoteNodeDeployer` upload to a worker endpoint and builds.
    /// The worker receives a generated Dockerfile with no `ARG`, the cache
    /// namespace as the only build input, and no variable value anywhere.
    #[tokio::test]
    async fn generated_vite_build_on_a_worker_is_accepted_and_transfers_no_values() {
        let captured = Arc::new(std::sync::Mutex::new(CapturedWorkerBuild::default()));
        let app = axum::Router::new()
            .route(
                "/agent/images/build",
                axum::routing::post(fake_worker_build),
            )
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let worker = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                format!("http://{address}"),
                "worker-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap(),
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"web","private":true,"scripts":{"build":"vite build"},"devDependencies":{"vite":"^5.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            "<!doctype html><title>web</title>",
        )
        .unwrap();
        let repo = RepositoryOutput {
            repo_dir: dir.path().to_path_buf(),
            checkout_ref: "main".to_string(),
            repo_owner: "owner".to_string(),
            repo_name: "web".to_string(),
        };
        let mut context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);
        for (key, value) in [
            ("repo_dir", repo.repo_dir.display().to_string()),
            ("checkout_ref", "main".to_string()),
            ("repo_owner", "owner".to_string()),
            ("repo_name", "web".to_string()),
        ] {
            context.set_output("download_repo", key, value).unwrap();
        }
        const SECRET: &str = "postgres://synthetic-user:synthetic-secret@db.internal/app";
        let job = BuildImageJobBuilder::new()
            .job_id("build".into())
            .download_job_id("download_repo".into())
            .image_tag("web:worker".into())
            .preset(StoredPreset::Vite)
            .remote_builder_node_id(7)
            .build_args(vec![
                ("HOST".to_string(), "0.0.0.0".to_string()),
                ("SENTRY_DSN".to_string(), "synthetic-dsn-value".to_string()),
                ("DATABASE_URL".to_string(), SECRET.to_string()),
                (
                    temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG.to_string(),
                    "ns-web-main".to_string(),
                ),
            ])
            .build(worker)
            .unwrap();

        let output = job
            .build_image(&repo, &context)
            .await
            .expect("a generated Vite build is accepted by the worker");
        server.abort();

        assert_eq!(output.image_tag, "web:worker");
        let captured = captured.lock().unwrap();
        assert_eq!(
            captured.authorization.as_deref(),
            Some("Bearer worker-token")
        );
        let spec = captured.spec.as_ref().expect("the worker received a spec");
        assert_eq!(spec.image_name, "web:worker");
        assert_eq!(spec.cache_namespace.as_deref(), Some("ns-web-main"));
        let dockerfile = captured
            .files
            .get(&spec.dockerfile)
            .expect("the generated Dockerfile was uploaded");
        assert!(dockerfile.contains("FROM "), "{dockerfile}");
        let declared_args: Vec<&str> = dockerfile
            .lines()
            .filter(|line| {
                line.split_whitespace()
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("ARG"))
            })
            .collect();
        assert!(
            declared_args.is_empty(),
            "a worker build must declare no ARG: {declared_args:?}\n{dockerfile}"
        );
        assert!(captured.files.contains_key("package.json"));
        assert!(captured.files.contains_key("index.html"));
        for value in [SECRET, "synthetic-secret", "synthetic-dsn-value"] {
            assert!(
                !captured.spec_json.contains(value),
                "the spec carries '{value}'"
            );
            for (path, contents) in &captured.files {
                assert!(!contents.contains(value), "{path} carries '{value}'");
            }
        }
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
            .verify_built_platform("app:latest", "linux/arm64", &context)
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
}
