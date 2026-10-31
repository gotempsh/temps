// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resolving the effective Compose configuration and checking it, including
//! snapshotting the files a compose document includes or references, so a
//! check sees exactly what a deploy would.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use temps_entities::compose_security::ComposeSecurityCheck as PolicyCheck;
use temps_presets::compose_check::{
    ComposeCheckErrorClass, ComposeCheckReport, ComposeCheckRequest,
};

use super::diagnostics::{
    classify_compose_config_failure_with_status, command_argv, redact_compose_text,
    sanitize_config_stderr, SubstitutedValues,
};
use super::exec::isolated_docker_command;
use super::files::render_env_file;
use super::support::{COMPOSE_CONFIG_FLAGS, COMPOSE_CONSISTENCY_FLAGS};
use super::{
    ComposeCommandDiagnostics, ComposeError, ComposeExecutor, COMPOSE_CONFIG_TIMEOUT,
    MAX_RESOLVED_COMPOSE_CONFIG_BYTES,
};
use crate::compose_interpolation::{self, InterpolationError};
use yaml_serde::{Mapping, Value};

/// Owns the inspected local inputs until `compose config` finishes. Remote
/// archives are retained only after validation, because effective runtime paths
/// can refer to their assets throughout the deployment.
/// Most bytes of variables (names and values) that `include` environments
/// may hold across one resolution. Each distinct include scope holds the
/// including document's variables plus its own env files; repeating an
/// include reuses its scope and is not charged again.
const MAX_INCLUDE_ENVIRONMENT_BYTES: usize = 16 * 1024 * 1024;

/// An include scope: its variables and their identity, or `None` when the
/// include's env files add nothing and it sees the including variables.
type IncludeScope = Option<(Arc<HashMap<String, String>>, u64)>;

/// Variables for interpolating referenced files, plus what was learned while
/// doing so.
#[derive(Default)]
struct SnapshotInterpolation {
    /// Variables the document being snapshotted is interpolated with. An
    /// `include` entry layers its own `env_file` (or `.env`) underneath the
    /// including document's variables for the files it includes.
    variables: Arc<HashMap<String, String>>,
    /// Identifies `variables`, so one file included under two different
    /// environments is snapshotted once per environment.
    scope_id: u64,
    unset: BTreeSet<String>,
    referenced: BTreeSet<String>,
    /// Values loaded from include env files, redacted from diagnostics like
    /// the root variables. Each distinct include scope contributes once.
    loaded_values: Vec<String>,
    /// Include scopes already built, keyed by the including scope and the
    /// env files loaded on top of it, so a repeated include neither reparses
    /// its env files nor holds another copy of their values.
    include_scopes: HashMap<(u64, Vec<PathBuf>), IncludeScope>,
    /// Bytes held by `include_scopes`; see [`MAX_INCLUDE_ENVIRONMENT_BYTES`].
    include_bytes: usize,
    /// See [`ComposeCheckInput::defer_unknown_required`].
    defer_required: bool,
    deferred_required: BTreeSet<String>,
    /// Services the deployment removes; see
    /// [`compose_interpolation::interpolate_project_document`].
    excluded_services: Vec<String>,
    /// Referenced files as written, before interpolation; see
    /// [`ResolvedComposeConfiguration::referenced_sources`].
    sources: Vec<String>,
    sources_bytes: usize,
}

impl SnapshotInterpolation {
    fn new(variables: HashMap<String, String>, defer_required: bool) -> Self {
        Self {
            variables: Arc::new(variables),
            defer_required,
            ..Default::default()
        }
    }
}

/// Interpolate one document, deferring unset required variables when the
/// check runs without the deployment's generated variables. `excluded` lists
/// the excluded services when the document's `services` are project services
/// (the root file, the override, an include, the merged model), and is empty
/// for an `extends` source, whose services a kept service may inherit.
fn interpolate_for_check(
    document: &str,
    content: &str,
    variables: &HashMap<String, String>,
    defer_required: bool,
    excluded: &[String],
) -> Result<compose_interpolation::InterpolatedDocument, InterpolationError> {
    compose_interpolation::interpolate_project_document(
        document,
        content,
        variables,
        defer_required,
        excluded,
    )
}

struct ComposeReferenceSnapshots {
    cache: tempfile::TempDir,
    interpolation: SnapshotInterpolation,
    files: Vec<tempfile::NamedTempFile>,
    remote: HashMap<String, (PathBuf, PathBuf)>,
    /// Keyed by file, working directory, include scope, and whether the
    /// file's services are project services (see [`interpolate_for_check`]).
    resolved: HashMap<(PathBuf, PathBuf, u64, bool), PathBuf>,
    active: HashSet<PathBuf>,
    bytes: usize,
}

impl ComposeReferenceSnapshots {
    fn new(root: &Path) -> Result<Self, ComposeError> {
        Ok(Self {
            cache: tempfile::Builder::new()
                .prefix(".temps-compose-sources-")
                .tempdir_in(root)?,
            files: Vec::new(),
            remote: HashMap::new(),
            resolved: HashMap::new(),
            active: HashSet::new(),
            bytes: 0,
            interpolation: Default::default(),
        })
    }
}

/// A Compose model after interpolation and, when needed, `docker compose
/// config` resolution.
#[derive(Debug, Clone)]
pub struct ResolvedComposeConfiguration {
    pub content: String,
    pub override_content: Option<String>,
    /// Why resolution was needed; empty when the file was used directly.
    pub resolution_reasons: Vec<&'static str>,
    /// The `docker compose config` command that ran, when one did.
    pub command: Option<ComposeCommandDiagnostics>,
    pub unset_variables: BTreeSet<String>,
    pub referenced_variables: BTreeSet<String>,
    /// Required variables the check could not see and substituted as empty;
    /// see [`ComposeCheckInput::defer_unknown_required`]. Always empty for a
    /// deployment.
    pub deferred_required_variables: BTreeSet<String>,
    /// The files `include` and `extends` pulled in, as written (before
    /// interpolation), so callers can tell literal text in the merged model
    /// from substituted values. At most [`MAX_RESOLVED_COMPOSE_CONFIG_BYTES`]
    /// in total; files past that are left out.
    pub referenced_sources: Vec<String>,
    /// Values substituted into `content`, for redacting text derived from it.
    pub substituted_values: SubstitutedValues,
    /// Caveats, e.g. resolution skipped because no Compose CLI is present.
    pub notes: Vec<String>,
}

/// Output of [`ComposeExecutor::check_configuration`]: everything the deploy
/// job needs after the deploy-time checks passed.
#[derive(Debug, Clone)]
pub struct CheckedComposeConfiguration {
    /// Resolved model before excluded services are removed (the settings
    /// page lists every service so excluded ones can be re-enabled).
    pub resolved_content: String,
    /// Resolved model with excluded services removed — what deploys.
    pub content: String,
    pub override_content: Option<String>,
    pub resolution: ResolvedComposeConfiguration,
}

/// Inputs for one deploy-time configuration check.
#[derive(Debug, Clone, Copy)]
pub struct ComposeCheckInput<'a> {
    pub project_name: &'a str,
    /// Repository checkout; `None` for inline projects and previews.
    pub project_dir: Option<&'a Path>,
    pub compose_file: &'a str,
    pub content: &'a str,
    pub override_content: Option<&'a str>,
    pub excluded_services: &'a [String],
    pub environment: &'a HashMap<String, String>,
    /// Repository `.env` text when there is no checkout to read it from.
    pub repository_env: Option<&'a str>,
    /// Whether filesystem confinement is checked against `project_dir`.
    pub filesystem_checks: bool,
    /// Whether `docker compose config` may run. When false and resolution
    /// would be needed, the files are validated separately and a note says so.
    pub allow_cli: bool,
    /// For checks outside a deployment, which cannot load the variables a
    /// deployment generates (linked services, secrets bindings, deployment
    /// tokens): a required `${VAR:?}` whose variable is not set at all is
    /// reported in `deferred_required_variables` instead of failing. Always
    /// false for a deployment, which fails on it.
    pub defer_unknown_required: bool,
}

impl ComposeExecutor {
    /// Why this model needs `docker compose config` before validation. Only
    /// file features count: an override to merge, `include`, `extends`,
    /// Compose merge tags (`!reset`/`!override`), or YAML merge keys that do
    /// not expand. Policy settings never add a resolution step, so turning a
    /// check off can never introduce a failure.
    fn resolution_reasons(content: &str, override_content: Option<&str>) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if override_content.is_some_and(|content| !content.trim().is_empty()) {
            reasons.push("project override");
        }
        let Ok(mut root) = yaml_serde::from_str::<Value>(content) else {
            // Invalid YAML is reported by validation with its own message.
            return reasons;
        };
        if root.apply_merge().is_err() {
            reasons.push("YAML merge keys");
            return reasons;
        }
        if root.get("include").is_some() {
            reasons.push("include");
        }
        if root
            .get("services")
            .and_then(Value::as_mapping)
            .is_some_and(|services| {
                services
                    .values()
                    .any(|service| service.get("extends").is_some())
            })
        {
            reasons.push("extends");
        }
        if Self::contains_compose_tag(&root) {
            reasons.push("Compose merge tags");
        }
        reasons
    }

    /// Variables Compose would interpolate with: the repository `.env`,
    /// overridden by the project environment (the same precedence as the
    /// `--env-file .env --env-file .env.temps` pair used at deploy time).
    fn interpolation_variables(
        input: &ComposeCheckInput<'_>,
    ) -> Result<HashMap<String, String>, ComposeError> {
        let repository_env = match (input.repository_env, input.project_dir) {
            (Some(content), _) => Some(content.to_string()),
            (None, Some(dir)) => Self::read_repository_dotenv(dir)?,
            (None, None) => None,
        };
        let mut variables = match repository_env.as_deref() {
            Some(content) => compose_interpolation::parse_dotenv(content)?,
            None => HashMap::new(),
        };
        variables.extend(
            input
                .environment
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        Ok(variables)
    }

    /// The checkout's root `.env`, confined to the checkout and size-limited.
    /// A missing file is not an error: Compose treats it as empty.
    fn read_repository_dotenv(dir: &Path) -> Result<Option<String>, ComposeError> {
        let path = dir.join(".env");
        if path.symlink_metadata().is_err() {
            return Ok(None);
        }
        let root = std::fs::canonicalize(dir)?;
        let path = Self::confined_reference(&root, &root, ".env")?;
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_file() {
            return Ok(None);
        }
        if metadata.len() > compose_interpolation::MAX_DOTENV_BYTES as u64 {
            return Err(ComposeError::InvalidComposePath {
                field: ".env".to_string(),
                path: ".env".to_string(),
                reason: format!(
                    "the repository .env file is {} bytes; at most {} bytes are read for Compose interpolation",
                    metadata.len(),
                    compose_interpolation::MAX_DOTENV_BYTES
                ),
            });
        }
        Ok(Some(std::fs::read_to_string(&path)?))
    }

    /// Resolve the user-owned model before discovery, exclusions, or generated
    /// overrides, so validation sees the values that will actually deploy.
    ///
    /// 1. `${VAR}` / `${VAR:-default}` / `${VAR:?}` are substituted in-process
    ///    from the repository `.env` and project environment, and every
    ///    remaining `$` is escaped, so Compose performs no later substitution.
    /// 2. Override list fields are de-duplicated against the repository file.
    /// 3. Only when the files use override/include/extends/merge tags does
    ///    `docker compose config` merge them (with `--no-interpolate`; values
    ///    are already final). No build/pull/up runs here.
    ///
    /// Policy errors raised on substituted values never echo variable values.
    pub async fn resolve_configuration(
        &self,
        input: &ComposeCheckInput<'_>,
    ) -> Result<ResolvedComposeConfiguration, ComposeError> {
        let variables = Self::interpolation_variables(input)?;
        let result = self.resolve_configuration_inner(input, &variables).await;
        result.map_err(|error| Self::redact_variable_values(error, &variables))
    }

    async fn resolve_configuration_inner(
        &self,
        input: &ComposeCheckInput<'_>,
        variables: &HashMap<String, String>,
    ) -> Result<ResolvedComposeConfiguration, ComposeError> {
        let defer = input.defer_unknown_required;
        // Excluded services a kept service inherits from (through `extends`
        // in the file or the override) still run, so they keep strict checks.
        let excluded = &compose_interpolation::excluded_services_not_inherited(
            &[input.content, input.override_content.unwrap_or_default()],
            input.excluded_services,
        );
        let content =
            interpolate_for_check("compose file", input.content, variables, defer, excluded)?;
        let mut unset_variables = content.unset_variables;
        let mut referenced_variables = content.referenced_variables;
        let mut deferred_required_variables = content.deferred_required;
        let content = content.content;
        let override_content = match input.override_content {
            Some(override_content) if !override_content.trim().is_empty() => {
                let interpolated = interpolate_for_check(
                    "compose override",
                    override_content,
                    variables,
                    defer,
                    excluded,
                )?;
                unset_variables.extend(interpolated.unset_variables);
                referenced_variables.extend(interpolated.referenced_variables);
                deferred_required_variables.extend(interpolated.deferred_required);
                Some(Self::deduplicate_override(
                    input.project_name,
                    &content,
                    &interpolated.content,
                )?)
            }
            _ => None,
        };
        let resolution_reasons = Self::resolution_reasons(&content, override_content.as_deref());
        let mut substituted_values = SubstitutedValues::default();
        substituted_values.extend(variables.values());
        let mut resolved = ResolvedComposeConfiguration {
            content,
            override_content,
            resolution_reasons,
            command: None,
            unset_variables,
            referenced_variables,
            deferred_required_variables,
            referenced_sources: Vec::new(),
            substituted_values,
            notes: Vec::new(),
        };
        if resolved.resolution_reasons.is_empty() {
            return Ok(resolved);
        }
        if !input.allow_cli {
            resolved.notes.push(format!(
                "Docker Compose is not available here, so the {} step was not run; the files were validated separately. Deployment merges them with `docker compose config` on the deploying host.",
                resolved.resolution_reasons.join(", ")
            ));
            return Ok(resolved);
        }
        let mut snapshots_variables = SnapshotInterpolation::new(variables.clone(), defer);
        snapshots_variables.excluded_services = excluded.clone();
        let (merged, command) = self
            .run_compose_config(
                input,
                &resolved.content,
                resolved.override_content.as_deref(),
                &mut snapshots_variables,
            )
            .await
            // Values loaded from include env files are redacted like the
            // root variables (which the caller redacts).
            .map_err(|error| Self::redact_values(error, &snapshots_variables.loaded_values))?;
        resolved.unset_variables.extend(snapshots_variables.unset);
        resolved
            .deferred_required_variables
            .extend(snapshots_variables.deferred_required);
        resolved
            .referenced_variables
            .extend(snapshots_variables.referenced);
        resolved.referenced_sources = snapshots_variables.sources;
        resolved
            .substituted_values
            .extend(&snapshots_variables.loaded_values);
        resolved.content = merged;
        resolved.override_content = None;
        resolved.command = Some(command);
        Ok(resolved)
    }

    /// Remove override list items (env_file, volumes, environment, ports)
    /// that repeat the repository file, so a pasted copy of the repo file
    /// does not duplicate mounts or env files when Compose appends lists.
    fn deduplicate_override(
        project_name: &str,
        content: &str,
        override_content: &str,
    ) -> Result<String, ComposeError> {
        let (Ok(mut base), Ok(mut overlay)) = (
            yaml_serde::from_str::<Value>(content),
            yaml_serde::from_str::<Value>(override_content),
        ) else {
            // Malformed YAML is reported by validation with its own message.
            return Ok(override_content.to_string());
        };
        if base.apply_merge().is_err() {
            return Ok(override_content.to_string());
        }
        let before = overlay.clone();
        temps_presets::compose_check::deduplicate_override_lists(&base, &mut overlay);
        if overlay == before {
            return Ok(override_content.to_string());
        }
        yaml_serde::to_string(&overlay).map_err(|error| ComposeError::InvalidOverride {
            project: project_name.to_string(),
            reason: format!(
                "failed to re-serialize the override after removing duplicate list items: {error}"
            ),
        })
    }

    /// Policy errors raised on substituted values must not reveal the value of
    /// a project variable to readers of the deployment. Values shorter than
    /// three bytes are left alone: they cannot hide a secret and redacting
    /// them would mangle the message.
    fn redact_variable_values(
        error: ComposeError,
        variables: &HashMap<String, String>,
    ) -> ComposeError {
        let values: Vec<String> = variables.values().cloned().collect();
        Self::redact_values(error, &values)
    }

    /// [`Self::redact_variable_values`] for a list of values.
    fn redact_values(error: ComposeError, values: &[String]) -> ComposeError {
        let values: Vec<String> = values
            .iter()
            .filter(|value| value.len() >= 3)
            .cloned()
            .collect();
        if values.is_empty() {
            return error;
        }
        let redact = |text: String| redact_compose_text(&text, &values);
        match error {
            ComposeError::ConfigResolutionFailed {
                project,
                class,
                exit_status,
                cause,
                diagnostics,
            } => {
                let diagnostics = *diagnostics;
                ComposeError::ConfigResolutionFailed {
                    project,
                    class,
                    exit_status,
                    cause: redact(cause),
                    diagnostics: Box::new(ComposeCommandDiagnostics {
                        argv: diagnostics.argv.into_iter().map(redact).collect(),
                        exit_code: diagnostics.exit_code,
                        stderr: redact(diagnostics.stderr),
                    }),
                }
            }
            ComposeError::SecurityPolicyViolation {
                service,
                field,
                reason,
            } => ComposeError::SecurityPolicyViolation {
                service,
                field,
                reason: redact(reason),
            },
            ComposeError::InvalidComposePath {
                field,
                path,
                reason,
            } => ComposeError::InvalidComposePath {
                field,
                path: redact(path),
                reason: redact(reason),
            },
            ComposeError::InvalidOverride { project, reason } => ComposeError::InvalidOverride {
                project,
                reason: redact(reason),
            },
            ComposeError::InvalidComposeYaml {
                compose_source,
                reason,
            } => ComposeError::InvalidComposeYaml {
                compose_source,
                reason: redact(reason),
            },
            other => other,
        }
    }

    /// Every deploy-time configuration check, in deploy order and without
    /// touching Docker state: resolution (see [`Self::resolve_configuration`]),
    /// removal of excluded services, the security policy, and — for a
    /// checkout — filesystem confinement. The deploy job, the "Run compose
    /// check" action and the settings preview all call this, so they accept
    /// and reject the same files with the same error class.
    pub async fn check_configuration(
        &self,
        input: &ComposeCheckInput<'_>,
    ) -> Result<CheckedComposeConfiguration, ComposeError> {
        let variables = Self::interpolation_variables(input)?;
        let result = async {
            let resolution = self.resolve_configuration_inner(input, &variables).await?;
            let content =
                self.strip_excluded_services(&resolution.content, input.excluded_services)?;
            let override_content = resolution.override_content.clone();
            self.preflight_validate(&content, override_content.as_deref())?;
            if input.filesystem_checks {
                if let Some(project_dir) = input.project_dir {
                    self.preflight_validate_filesystem(
                        project_dir,
                        input.compose_file,
                        &content,
                        override_content.as_deref(),
                    )?;
                }
            }
            Ok(CheckedComposeConfiguration {
                resolved_content: resolution.content.clone(),
                content,
                override_content,
                resolution,
            })
        }
        .await;
        result.map_err(|error| Self::redact_variable_values(error, &variables))
    }

    /// [`Self::check_configuration`] on in-memory files, as a typed report.
    /// Runs in a throwaway directory with no checkout, so filesystem
    /// confinement is not checked; `docker compose config` runs only when the
    /// local Compose CLI is usable, otherwise the report notes the skip.
    pub async fn check_report(&self, request: ComposeCheckRequest) -> ComposeCheckReport {
        let workspace = match tempfile::Builder::new()
            .prefix(".temps-compose-check-")
            .tempdir()
        {
            Ok(workspace) => workspace,
            Err(error) => {
                return ComposeCheckReport::failure(
                    ComposeCheckErrorClass::Internal,
                    format!(
                        "Failed to create a temporary directory for the Compose check of project '{}': {error}",
                        request.project_name
                    ),
                )
            }
        };
        // `docker compose config` reads files only; it needs the CLI, not a daemon.
        let allow_cli = self.require_compose_v2().await.is_ok();
        let compose_file = if request.compose_file.trim().is_empty() {
            "docker-compose.yml".to_string()
        } else {
            request.compose_file.clone()
        };
        let input = ComposeCheckInput {
            project_name: &request.project_name,
            project_dir: Some(workspace.path()),
            compose_file: &compose_file,
            content: &request.content,
            override_content: request.compose_override.as_deref(),
            excluded_services: &request.excluded_services,
            environment: &request.environment,
            repository_env: Some(request.repository_env.as_deref().unwrap_or_default()),
            filesystem_checks: false,
            allow_cli,
            defer_unknown_required: true,
        };
        // The scratch directory only gives the config step somewhere to
        // write its temporary inputs; nothing in it is a checkout.
        let parent_ok = Self::validate_relative_path(&compose_file, "compose_path").is_ok()
            && Path::new(&compose_file).parent().is_none_or(|parent| {
                std::fs::create_dir_all(workspace.path().join(parent)).is_ok()
            });
        let executor = ComposeExecutor {
            policy: request.policy.clone(),
            policy_authoritative: true,
            ..self.clone_for_check()
        };
        let result = if parent_ok {
            match Self::write_reference_files(workspace.path(), &request.reference_files) {
                Ok(()) => executor.check_configuration(&input).await,
                Err(error) => Err(error),
            }
        } else {
            Err(ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_file.clone(),
                reason: "the Compose file path must be a relative path inside the repository"
                    .to_string(),
            })
        };
        match result {
            Ok(checked) => {
                let mut resolution = checked.resolution;
                if !resolution.deferred_required_variables.is_empty() {
                    let names: Vec<&str> = resolution
                        .deferred_required_variables
                        .iter()
                        .map(String::as_str)
                        .collect();
                    resolution.notes.push(format!(
                        "Required variable(s) {} are not set in the project environment or the repository .env. This check cannot see the values a deployment generates (linked services, secrets bindings, deployment tokens), so they were treated as supplied; the deployment fails if none of those provides them.",
                        names.join(", ")
                    ));
                    resolution
                        .unset_variables
                        .extend(resolution.deferred_required_variables.iter().cloned());
                }
                ComposeCheckReport {
                    passed: true,
                    error_class: None,
                    error: None,
                    resolution_reasons: resolution
                        .resolution_reasons
                        .iter()
                        .map(|reason| reason.to_string())
                        .collect(),
                    resolution_ran: resolution.command.is_some(),
                    command: resolution
                        .command
                        .map(ComposeCommandDiagnostics::into_report),
                    unset_variables: resolution.unset_variables.into_iter().collect(),
                    referenced_variables: resolution.referenced_variables.into_iter().collect(),
                    notes: resolution.notes,
                }
            }
            Err(error) => {
                let command = match &error {
                    ComposeError::ConfigResolutionFailed { diagnostics, .. } => {
                        Some((**diagnostics).clone().into_report())
                    }
                    _ => None,
                };
                let resolution_reasons =
                    Self::resolution_reasons(&request.content, request.compose_override.as_deref())
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                ComposeCheckReport {
                    resolution_ran: command.is_some(),
                    command,
                    resolution_reasons,
                    ..ComposeCheckReport::failure(error.class(), error.to_string())
                }
            }
        }
    }

    /// Place the repository files a check fetched (include/extends targets
    /// and include env files) into its scratch directory, so references
    /// resolve as they would in a checkout. Paths are relative to the
    /// Compose project directory and are written through the confined-write
    /// guard.
    fn write_reference_files(
        workspace: &Path,
        files: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), ComposeError> {
        if files.is_empty() {
            return Ok(());
        }
        let root = std::fs::canonicalize(workspace)?;
        for (path, content) in files {
            Self::validate_relative_path(path, "Compose reference")?;
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(root.join(parent))?;
            }
            let target = Self::confined_write_path(&root, Path::new(path), "Compose reference")?;
            std::fs::write(target, content)?;
        }
        Ok(())
    }

    /// An executor sharing this one's Docker handle and data directory.
    fn clone_for_check(&self) -> Self {
        Self {
            docker: self.docker.clone(),
            data_dir: self.data_dir.clone(),
            policy: self.policy.clone(),
            policy_authoritative: self.policy_authoritative,
            published_host_ip: self.published_host_ip,
        }
    }

    /// [`Self::check_configuration`] returning only the resolved files.
    pub async fn resolve_security_configuration(
        &self,
        project_name: &str,
        project_dir: Option<&Path>,
        compose_file: &str,
        content: &str,
        override_content: Option<&str>,
        environment: &HashMap<String, String>,
    ) -> Result<(String, Option<String>), ComposeError> {
        let checked = self
            .check_configuration(&ComposeCheckInput {
                project_name,
                project_dir,
                compose_file,
                content,
                override_content,
                excluded_services: &[],
                environment,
                repository_env: None,
                filesystem_checks: project_dir.is_some(),
                allow_cli: true,
                defer_unknown_required: false,
            })
            .await?;
        Ok((checked.content, checked.override_content))
    }

    /// Merge the (already interpolated) files with `docker compose config`
    /// and validate the merged model. Referenced files are snapshotted and
    /// interpolated with the same variables first.
    async fn run_compose_config(
        &self,
        input: &ComposeCheckInput<'_>,
        content: &str,
        override_content: Option<&str>,
        interpolation: &mut SnapshotInterpolation,
    ) -> Result<(String, ComposeCommandDiagnostics), ComposeError> {
        let project_name = input.project_name;
        let project_dir = input.project_dir;
        let compose_file = input.compose_file;
        let inline_root;
        let root = match project_dir {
            Some(root) => root,
            None => {
                Self::validate_service_dir_name(project_name)?;
                inline_root = self.project_dir(project_name);
                std::fs::create_dir_all(&inline_root)?;
                &inline_root
            }
        };
        Self::validate_relative_path(compose_file, "compose_path")?;
        let canonical_root = std::fs::canonicalize(root)?;
        let base = canonical_root
            .join(compose_file)
            .parent()
            .ok_or_else(|| ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_file.to_string(),
                reason: "missing parent directory".to_string(),
            })?
            .to_path_buf();
        // File preparation still uses the non-bypassable confined-write guard.
        Self::confined_write_path(&canonical_root, Path::new(compose_file), "compose_path")?;
        std::fs::create_dir_all(&base)?;
        let base = std::fs::canonicalize(&base)?;
        let mut snapshots = ComposeReferenceSnapshots::new(&canonical_root)?;
        std::mem::swap(&mut snapshots.interpolation, interpolation);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let snapshot_result = async {
            let input = tokio::time::timeout_at(
                deadline,
                self.snapshot_compose_document(&canonical_root, &base, content, &mut snapshots),
            )
            .await
            .map_err(|_| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: "Compose reference resolution exceeded 120 seconds".to_string(),
            })??;
            let inline = if let Some(override_content) = override_content {
                Self::validate_compose_override_with_policy(
                    project_name,
                    content,
                    override_content,
                    &self.policy,
                )?;
                let override_content =
                    Self::without_duplicate_extends(project_name, content, override_content)?;
                Some(
                    tokio::time::timeout_at(
                        deadline,
                        self.snapshot_compose_document(
                            &canonical_root,
                            &base,
                            &override_content,
                            &mut snapshots,
                        ),
                    )
                    .await
                    .map_err(|_| ComposeError::CommandFailed {
                        project: project_name.to_string(),
                        reason: "Compose reference resolution exceeded 120 seconds".to_string(),
                    })??,
                )
            } else {
                None
            };
            Ok::<_, ComposeError>((input, inline))
        }
        .await;
        std::mem::swap(&mut snapshots.interpolation, interpolation);
        let (input_file, inline) = snapshot_result?;
        // Inline port changes replace the repository and inherited ports.
        // Compose normally appends port lists, and after this resolution the
        // inline file is flattened away, so the later write-time strip cannot
        // remove ports that came through `extends`. Reset them between the
        // source and inline files while Compose still knows their provenance.
        let port_reset = if let (Some(override_content), Some(_)) = (override_content, &inline) {
            if let Some(content) = Self::port_reset_override(project_name, override_content)? {
                let file = tempfile::Builder::new()
                    .prefix(".temps-compose-port-reset-")
                    .suffix(".yml")
                    .tempfile_in(&base)?;
                std::fs::write(file.path(), content)?;
                Some(file)
            } else {
                None
            }
        } else {
            None
        };
        let variables = tempfile::Builder::new()
            .prefix(".temps-compose-env-")
            .tempfile_in(&base)?;
        std::fs::write(variables.path(), render_env_file(input.environment)?)?;
        let repository_env = if canonical_root.join(".env").exists() {
            Some(Self::confined_reference(
                &canonical_root,
                &canonical_root,
                ".env",
            )?)
        } else {
            None
        };
        let config_command = |flags: &[&str]| {
            let mut cmd = isolated_docker_command();
            cmd.args(["compose", "-p", project_name, "--project-directory"])
                .arg(&base)
                .arg("-f")
                .arg(&input_file);
            if let Some(reset) = &port_reset {
                cmd.arg("-f").arg(reset.path());
            }
            if let Some(inline) = &inline {
                cmd.arg("-f").arg(inline);
            }
            if let Some(env) = &repository_env {
                cmd.arg("--env-file").arg(env);
            }
            // Platform environment values override repository defaults, matching deployment.
            cmd.arg("--env-file").arg(variables.path());
            cmd.arg("config").args(flags).current_dir(&base);
            cmd
        };
        // Every value Compose could echo back: the repository `.env` values
        // as well as the project environment, since both were substituted.
        let redact_values: Vec<String> = interpolation
            .variables
            .values()
            .chain(&interpolation.loaded_values)
            .chain(input.environment.values())
            .filter(|value| !value.is_empty())
            .cloned()
            .collect();
        // Values were substituted (and `$` escaped) before this step;
        // `--no-interpolate` keeps Compose from substituting a second time.
        let (resolved, diagnostics) = self
            .run_config_pass(
                config_command(COMPOSE_CONFIG_FLAGS),
                project_name,
                &redact_values,
            )
            .await?;
        // `--no-interpolate` also skips Compose's model consistency checks
        // (undefined `depends_on` targets, networks, volumes, secrets), so
        // the same files are validated once more with interpolation on.
        // Nothing is left to substitute, so this only adds those checks.
        self.run_config_pass(
            config_command(COMPOSE_CONSISTENCY_FLAGS),
            project_name,
            &redact_values,
        )
        .await?;
        let resolved =
            String::from_utf8(resolved).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: project_name.to_string(),
                reason: format!("resolved configuration is not UTF-8: {error}"),
            })?;
        // Idempotent for the substituted files; resolves anything Compose
        // pulled in that was not snapshotted (none today, defensively).
        let reinterpolated = interpolate_for_check(
            "resolved Compose configuration",
            &resolved,
            &interpolation.variables,
            interpolation.defer_required,
            &interpolation.excluded_services,
        )?;
        interpolation
            .deferred_required
            .extend(reinterpolated.deferred_required);
        interpolation.unset.extend(reinterpolated.unset_variables);
        interpolation
            .referenced
            .extend(reinterpolated.referenced_variables);
        let resolved = reinterpolated.content;
        self.validate_compose_security_policy("resolved Compose configuration", &resolved)?;
        if input.filesystem_checks || project_dir.is_none() {
            Self::validate_compose_filesystem_confinement_with_policy(
                &canonical_root,
                compose_file,
                "resolved Compose configuration",
                &resolved,
                &self.policy,
            )?;
        }
        // Effective file/build paths may point into these archives. Keep them with
        // the deployment checkout; its lifecycle owns their cleanup.
        if let Some(cache_name) = snapshots
            .cache
            .path()
            .file_name()
            .and_then(|name| name.to_str())
        {
            self.validate_remote_runtime_assets(&resolved, cache_name)?;
            if resolved.contains(cache_name) {
                if project_dir.is_none() {
                    return Err(ComposeError::InvalidComposePath {
                        field: "Compose reference".to_string(), path: "<remote assets>".to_string(),
                        reason: "remote build contexts and environment files require a repository-backed deployment; vendor these assets into a repository".to_string(),
                    });
                }
                let _ = snapshots.cache.keep();
            }
        }
        Ok((resolved, diagnostics))
    }

    /// Run one `docker compose config` invocation with a bounded timeout.
    /// Returns stdout and the sanitized diagnostics, or a classified
    /// [`ComposeError::ConfigResolutionFailed`] carrying them.
    async fn run_config_pass(
        &self,
        cmd: tokio::process::Command,
        project_name: &str,
        redact_values: &[String],
    ) -> Result<(Vec<u8>, ComposeCommandDiagnostics), ComposeError> {
        let argv = command_argv(&cmd, redact_values);
        let output = Self::bounded_command_output(
            cmd,
            COMPOSE_CONFIG_TIMEOUT,
            project_name,
            "resolve Compose security configuration",
        )
        .await?;
        let diagnostics = ComposeCommandDiagnostics {
            argv,
            exit_code: output.status.code(),
            stderr: sanitize_config_stderr(&output.stderr, redact_values),
        };
        if !output.status.success() {
            let (class, cause) =
                classify_compose_config_failure_with_status(&output.stderr, output.status.code());
            return Err(ComposeError::ConfigResolutionFailed {
                project: project_name.to_string(),
                class,
                exit_status: output
                    .status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string()),
                cause,
                diagnostics: Box::new(diagnostics),
            });
        }
        Ok((output.stdout, diagnostics))
    }

    /// The workflow removes its checkout after deployment. Image builds and env
    /// files are consumed before cleanup, but bind/config/secret mounts must live
    /// for the entire container lifetime and cannot point into fetched archives.
    fn validate_remote_runtime_assets(
        &self,
        content: &str,
        cache_name: &str,
    ) -> Result<(), ComposeError> {
        let yaml: Value =
            yaml_serde::from_str(content).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "resolved Compose configuration".to_string(),
                reason: error.to_string(),
            })?;
        let reject = |service: &str, field: &str, value: &Value| -> Result<(), ComposeError> {
            if value.as_str().is_some_and(|path| path.contains(cache_name)) {
                return Err(ComposeError::InvalidComposePath {
                    field: format!("{service}.{field}"), path: "<remote runtime asset>".to_string(),
                    reason: "remote Compose bind mounts, config files and secret files cannot outlive the deployment checkout; vendor runtime files locally or use named volumes".to_string(),
                });
            }
            Ok(())
        };
        for field in ["configs", "secrets"] {
            if let Some(definitions) = yaml.get(field).and_then(Value::as_mapping) {
                for (name, definition) in definitions {
                    if let Some(file) = definition.get("file") {
                        reject(name.as_str().unwrap_or("<resource>"), field, file)?;
                    }
                }
            }
        }
        if let Some(services) = yaml.get("services").and_then(Value::as_mapping) {
            for (name, service) in services {
                if let Some(volumes) = service.get("volumes").and_then(Value::as_sequence) {
                    for volume in volumes {
                        reject(
                            name.as_str().unwrap_or("<service>"),
                            "volumes",
                            volume.get("source").unwrap_or(volume),
                        )?;
                    }
                }
            }
        }
        if let Some(volumes) = yaml.get("volumes").and_then(Value::as_mapping) {
            for (name, volume) in volumes {
                if let Some(device) = volume
                    .get("driver_opts")
                    .and_then(|options| options.get("device"))
                {
                    reject(
                        name.as_str().unwrap_or("<volume>"),
                        "driver_opts.device",
                        device,
                    )?;
                }
            }
        }
        Ok(())
    }

    async fn snapshot_compose_document(
        &self,
        scope: &Path,
        base: &Path,
        content: &str,
        snapshots: &mut ComposeReferenceSnapshots,
    ) -> Result<PathBuf, ComposeError> {
        let canonical_scope = std::fs::canonicalize(scope)?;
        let scope = canonical_scope.as_path();
        let canonical_base = std::fs::canonicalize(base)?;
        let base = canonical_base.as_path();
        snapshots.bytes = snapshots.bytes.saturating_add(content.len());
        if snapshots.bytes > MAX_RESOLVED_COMPOSE_CONFIG_BYTES
            || snapshots.files.len() + snapshots.active.len() >= 128
        {
            return Err(ComposeError::InvalidComposePath {
                field: "Compose reference".to_string(),
                path: "<references>".to_string(),
                reason: "Compose references exceed the resolution size/file limit".to_string(),
            });
        }
        self.validate_source_security(content)?;
        if self.enforced(PolicyCheck::EnvFiles) && base.join(".env").symlink_metadata().is_ok() {
            Self::confined_reference(scope, base, ".env")?;
        }
        let mut yaml: Value =
            yaml_serde::from_str(content).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "Compose reference".to_string(),
                reason: error.to_string(),
            })?;
        yaml.apply_merge()
            .map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "Compose reference".to_string(),
                reason: error.to_string(),
            })?;
        if let Some(services) = yaml.get_mut("services").and_then(Value::as_mapping_mut) {
            for (name, definition) in services.iter_mut() {
                // Some Compose versions read inherited env_file values even
                // with --no-env-resolution. Check every existing path prefix
                // before config can consume a symlink and erase its origin.
                if self.enforced(PolicyCheck::EnvFiles) {
                    if let Some(env) = definition.get("env_file") {
                        for file in env.as_str().into_iter().chain(
                            env.as_sequence().into_iter().flatten().filter_map(|entry| {
                                entry
                                    .as_str()
                                    .or_else(|| entry.get("path").and_then(Value::as_str))
                            }),
                        ) {
                            Self::validate_confined_bind_path(
                                scope,
                                base,
                                file,
                                name.as_str().unwrap_or("<source>"),
                                "env_file",
                            )?;
                        }
                    }
                }
                Self::tolerate_missing_env_files(base, definition);
                if let Some(file) = definition
                    .get_mut("extends")
                    .and_then(|v| v.get_mut("file"))
                {
                    self.snapshot_compose_reference(scope, base, file, None, false, snapshots)
                        .await?;
                }
            }
        }
        if let Some(includes) = yaml.get_mut("include").and_then(Value::as_sequence_mut) {
            for include in includes {
                self.snapshot_include(scope, base, include, snapshots)
                    .await?;
            }
        }
        let file = tempfile::Builder::new()
            .prefix(".temps-compose-resolve-")
            .suffix(".yml")
            .tempfile_in(base)?;
        let path = file.path().to_path_buf();
        let serialized =
            yaml_serde::to_string(&yaml).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "Compose reference".to_string(),
                reason: error.to_string(),
            })?;
        std::fs::write(&path, serialized)?;
        snapshots.files.push(file);
        Ok(path)
    }

    /// Snapshot the files one `include` item pulls in. They are interpolated
    /// with this document's variables plus the item's own env files, as
    /// Compose does. The main file is located first: a remote include's
    /// project directory, and so its `.env`, exist only once materialized.
    async fn snapshot_include(
        &self,
        scope: &Path,
        base: &Path,
        include: &mut Value,
        snapshots: &mut ComposeReferenceSnapshots,
    ) -> Result<(), ComposeError> {
        let main = include
            .as_str()
            .or_else(|| {
                include.get("path").and_then(|path| {
                    path.as_str()
                        .or_else(|| path.as_sequence()?.first()?.as_str())
                })
            })
            .map(str::to_string);
        let located = match &main {
            Some(file) => Some(self.locate_reference(scope, base, file, snapshots).await?),
            None => None,
        };
        let scoped = Self::include_variables(scope, base, include, located.as_ref(), snapshots)?;
        let previous = scoped.map(|(variables, scope_id)| {
            (
                std::mem::replace(&mut snapshots.interpolation.variables, variables),
                std::mem::replace(&mut snapshots.interpolation.scope_id, scope_id),
            )
        });
        let result = self
            .snapshot_include_files(scope, base, include, snapshots)
            .await;
        if let Some((variables, scope_id)) = previous {
            snapshots.interpolation.variables = variables;
            snapshots.interpolation.scope_id = scope_id;
        }
        result
    }

    async fn snapshot_include_files(
        &self,
        scope: &Path,
        base: &Path,
        include: &mut Value,
        snapshots: &mut ComposeReferenceSnapshots,
    ) -> Result<(), ComposeError> {
        if include.is_string() {
            self.snapshot_compose_reference(scope, base, include, None, true, snapshots)
                .await?;
            return Ok(());
        }
        let project_directory = include
            .get("project_directory")
            .and_then(Value::as_str)
            .map(|directory| Self::confined_reference(scope, base, directory))
            .transpose()?;
        if let Some(directory) = &project_directory {
            if self.enforced(PolicyCheck::EnvFiles)
                && directory.join(".env").symlink_metadata().is_ok()
            {
                Self::confined_reference(scope, directory, ".env")?;
            }
        }
        if self.enforced(PolicyCheck::EnvFiles) {
            for file in Self::include_env_files(include) {
                Self::confined_reference(scope, base, file)?;
            }
        }
        if let Some(path) = include.get_mut("path") {
            if let Some(paths) = path.as_sequence_mut() {
                let mut working_directory = project_directory.clone();
                for path in paths {
                    let snapshot = self
                        .snapshot_compose_reference(
                            scope,
                            base,
                            path,
                            working_directory.as_deref(),
                            true,
                            snapshots,
                        )
                        .await?;
                    if working_directory.is_none() {
                        working_directory = snapshot.parent().map(Path::to_path_buf);
                    }
                }
            } else {
                self.snapshot_compose_reference(
                    scope,
                    base,
                    path,
                    project_directory.as_deref(),
                    true,
                    snapshots,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// The `env_file` entries of a long-form `include` item.
    fn include_env_files(include: &Value) -> Vec<&str> {
        let Some(env) = include.get("env_file") else {
            return Vec::new();
        };
        env.as_str()
            .into_iter()
            .chain(
                env.as_sequence()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            )
            .collect()
    }

    /// Variables for the files an `include` item pulls in: the including
    /// document's variables, plus the item's `env_file` entries (relative to
    /// the including file) or else the `.env` in the item's project directory
    /// (`project_directory`, or the main file's directory, which for a remote
    /// include is inside its materialized source). As in Compose, values the
    /// including document already sets win, and each env file sees the values
    /// earlier ones loaded. `None` when no env file adds anything, in which
    /// case the included files see the including document's variables.
    ///
    /// Scopes are cached for the whole resolution and their combined size is
    /// bounded by [`MAX_INCLUDE_ENVIRONMENT_BYTES`].
    fn include_variables(
        scope: &Path,
        base: &Path,
        include: &Value,
        main: Option<&(PathBuf, PathBuf)>,
        snapshots: &mut ComposeReferenceSnapshots,
    ) -> Result<IncludeScope, ComposeError> {
        let Some((main_path, main_scope)) = main else {
            return Ok(None);
        };
        let explicit = Self::include_env_files(include);
        let files: Vec<PathBuf> = if explicit.is_empty() {
            let (directory, directory_scope) =
                match include.get("project_directory").and_then(Value::as_str) {
                    Some(directory) => (
                        Self::confined_reference(scope, base, directory)?,
                        scope.to_path_buf(),
                    ),
                    None => match main_path.parent() {
                        Some(parent) => (parent.to_path_buf(), std::fs::canonicalize(main_scope)?),
                        None => return Ok(None),
                    },
                };
            if directory.join(".env").symlink_metadata().is_err() {
                return Ok(None);
            }
            vec![Self::confined_reference(
                &directory_scope,
                &directory,
                ".env",
            )?]
        } else {
            explicit
                .into_iter()
                .filter(|file| *file != "/dev/null")
                .map(|file| Self::confined_reference(scope, base, file))
                .collect::<Result<_, _>>()?
        };
        let interpolation = &mut snapshots.interpolation;
        let key = (interpolation.scope_id, files);
        if let Some(cached) = interpolation.include_scopes.get(&key) {
            return Ok(cached.clone());
        }
        let mut loaded: HashMap<String, String> = HashMap::new();
        for file in &key.1 {
            let metadata = std::fs::metadata(file)?;
            if !metadata.is_file()
                || metadata.len() > compose_interpolation::MAX_DOTENV_BYTES as u64
            {
                return Err(ComposeError::InvalidComposePath {
                    field: "include.env_file".to_string(),
                    path: file
                        .strip_prefix(scope)
                        .unwrap_or(file)
                        .display()
                        .to_string(),
                    reason: format!(
                        "include env files must be regular files of at most {} bytes",
                        compose_interpolation::MAX_DOTENV_BYTES
                    ),
                });
            }
            let content = std::fs::read_to_string(file)?;
            loaded = compose_interpolation::parse_dotenv_layered(
                &content,
                &interpolation.variables,
                loaded,
            )?;
        }
        let scoped = if loaded.is_empty() {
            None
        } else {
            let mut variables = HashMap::clone(&interpolation.variables);
            let mut values = Vec::new();
            for (key, value) in loaded {
                if !value.is_empty() {
                    values.push(value.clone());
                }
                variables.entry(key).or_insert(value);
            }
            let size: usize = variables
                .iter()
                .map(|(key, value)| key.len() + value.len())
                .chain(values.iter().map(String::len))
                .sum();
            interpolation.include_bytes = interpolation.include_bytes.saturating_add(size);
            if interpolation.include_bytes > MAX_INCLUDE_ENVIRONMENT_BYTES {
                return Err(ComposeError::Interpolation(InterpolationError::TooLarge {
                    location: "include env_file".to_string(),
                    limit: MAX_INCLUDE_ENVIRONMENT_BYTES,
                }));
            }
            interpolation.loaded_values.extend(values);
            let mut entries: Vec<(&String, &String)> = variables.iter().collect();
            entries.sort();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&entries, &mut hasher);
            Some((Arc::new(variables), std::hash::Hasher::finish(&hasher)))
        };
        interpolation.include_scopes.insert(key, scoped.clone());
        Ok(scoped)
    }

    /// Where a Compose reference lives: `(file, scope)`, with remote sources
    /// materialized (once per resolution) and local files confined to the
    /// checkout.
    async fn locate_reference(
        &self,
        scope: &Path,
        base: &Path,
        file: &str,
        snapshots: &mut ComposeReferenceSnapshots,
    ) -> Result<(PathBuf, PathBuf), ComposeError> {
        if Self::contains_interpolation(file) {
            return Err(ComposeError::InvalidComposePath {
                field: "Compose reference".to_string(),
                path: "<reference>".to_string(),
                reason: "Compose reference paths must not contain interpolation".to_string(),
            });
        }
        if file.contains("://") || file.starts_with("git@") {
            if let Some(paths) = snapshots.remote.get(file) {
                return Ok(paths.clone());
            }
            if snapshots.remote.len() >= 4 {
                return Err(ComposeError::InvalidComposePath {
                    field: "Compose reference".to_string(), path: "<remote sources>".to_string(),
                    reason: "at most four remote Compose sources may be resolved per deployment; vendor additional sources into the checkout".to_string(),
                });
            }
            let paths = crate::compose_remote::materialize(file, snapshots.cache.path()).await?;
            snapshots.remote.insert(file.to_string(), paths.clone());
            return Ok(paths);
        }
        Ok((
            Self::confined_reference(scope, base, file)?,
            scope.to_path_buf(),
        ))
    }

    async fn snapshot_compose_reference(
        &self,
        scope: &Path,
        base: &Path,
        reference: &mut Value,
        working_directory: Option<&Path>,
        project_services: bool,
        snapshots: &mut ComposeReferenceSnapshots,
    ) -> Result<PathBuf, ComposeError> {
        let file = reference
            .as_str()
            .ok_or_else(|| ComposeError::InvalidComposePath {
                field: "Compose reference".to_string(),
                path: "<reference>".to_string(),
                reason: "Compose reference must be a literal file path or supported HTTPS Git URL"
                    .to_string(),
            })?;
        let (path, scope) = self.locate_reference(scope, base, file, snapshots).await?;
        let scope = std::fs::canonicalize(scope)?;
        let directory = working_directory.or_else(|| path.parent()).ok_or_else(|| {
            ComposeError::InvalidComposePath {
                field: "Compose reference".to_string(),
                path: file.to_string(),
                reason: "Compose reference has no working directory".to_string(),
            }
        })?;
        let directory = Self::confined_reference(&scope, &scope, &directory.to_string_lossy())?;
        let key = (
            path.clone(),
            directory.clone(),
            snapshots.interpolation.scope_id,
            project_services,
        );
        let snapshot = if let Some(snapshot) = snapshots.resolved.get(&key) {
            snapshot.clone()
        } else {
            if !snapshots.active.insert(path.clone()) {
                return Err(ComposeError::InvalidComposePath {
                    field: "Compose reference".to_string(),
                    path: file.to_string(),
                    reason: "cyclic Compose reference".to_string(),
                });
            }
            let metadata = std::fs::metadata(&path)?;
            if !metadata.is_file() {
                return Err(ComposeError::InvalidComposePath {
                    field: "Compose reference".to_string(),
                    path: file.to_string(),
                    reason: "Compose references must be regular files".to_string(),
                });
            }
            if metadata.len() > MAX_RESOLVED_COMPOSE_CONFIG_BYTES as u64 {
                return Err(ComposeError::InvalidComposePath {
                    field: "Compose reference".to_string(),
                    path: file.to_string(),
                    reason: "Compose reference exceeds the file size limit".to_string(),
                });
            }
            let content = std::fs::read_to_string(&path)?;
            let interpolation = &mut snapshots.interpolation;
            if interpolation.sources_bytes + content.len() <= MAX_RESOLVED_COMPOSE_CONFIG_BYTES {
                interpolation.sources_bytes += content.len();
                interpolation.sources.push(content.clone());
            }
            // Referenced files are interpolated with the variables in scope
            // (the root file's, plus an include's env file), so their values
            // are validated as deployed.
            let excluded: &[String] = if project_services {
                &snapshots.interpolation.excluded_services
            } else {
                &[]
            };
            let interpolated = interpolate_for_check(
                "Compose reference",
                &content,
                &snapshots.interpolation.variables,
                snapshots.interpolation.defer_required,
                excluded,
            )?;
            snapshots
                .interpolation
                .deferred_required
                .extend(interpolated.deferred_required);
            snapshots
                .interpolation
                .unset
                .extend(interpolated.unset_variables);
            snapshots
                .interpolation
                .referenced
                .extend(interpolated.referenced_variables);
            let content = interpolated.content;
            let snapshot =
                Box::pin(self.snapshot_compose_document(&scope, &directory, &content, snapshots))
                    .await?;
            snapshots.active.remove(&path);
            snapshots.resolved.insert(key, snapshot.clone());
            snapshot
        };
        *reference = Value::String(snapshot.to_string_lossy().into_owned());
        Ok(snapshot)
    }

    fn confined_reference(root: &Path, base: &Path, file: &str) -> Result<PathBuf, ComposeError> {
        if Self::contains_interpolation(file) {
            return Err(ComposeError::InvalidComposePath {
                field: "Compose reference".to_string(),
                path: file.to_string(),
                reason: "Compose reference paths must be literal project files".to_string(),
            });
        }
        let path = std::fs::canonicalize(base.join(file)).map_err(|error| {
            ComposeError::InvalidComposePath {
                field: "Compose reference".to_string(),
                path: file.to_string(),
                reason: format!("cannot resolve referenced file: {error}"),
            }
        })?;
        if !path.starts_with(root) {
            return Err(ComposeError::InvalidComposePath {
                field: "Compose reference".to_string(),
                path: file.to_string(),
                reason: "Compose references must remain inside the project checkout".to_string(),
            });
        }
        Ok(path)
    }

    /// Rewrite each `env_file` entry naming a file absent from `base` to
    /// `{path, required: false}` for the config step. Deployment writes the
    /// file (synthesized from the project environment when the repository
    /// lacks it), so a `.env` that is only created at deploy time must not
    /// fail resolution. Existing files keep their entries unchanged.
    fn tolerate_missing_env_files(base: &Path, definition: &mut Value) {
        let Some(env_file) = definition.get_mut("env_file") else {
            return;
        };
        let missing = |path: &str| {
            !path.is_empty()
                && !Self::contains_interpolation(path)
                && base.join(path).symlink_metadata().is_err()
        };
        let optional = |path: &str| {
            let mut entry = Mapping::new();
            entry.insert(Value::from("path"), Value::from(path));
            entry.insert(Value::from("required"), Value::Bool(false));
            Value::Mapping(entry)
        };
        match env_file {
            Value::String(path) if missing(path) => {
                *env_file = Value::Sequence(vec![optional(path)]);
            }
            Value::Sequence(entries) => {
                for entry in entries.iter_mut() {
                    match entry {
                        Value::String(path) if missing(path) => *entry = optional(path),
                        Value::Mapping(mapping) => {
                            let absent = mapping
                                .get("path")
                                .and_then(Value::as_str)
                                .is_some_and(missing);
                            if absent {
                                mapping.insert(Value::from("required"), Value::Bool(false));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
