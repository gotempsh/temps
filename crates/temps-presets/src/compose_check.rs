// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The contract shared by every place that asks "would this Compose
//! configuration deploy?": the deploy job, the settings preview, the
//! "Run compose check" action, and the post-upgrade revalidation.
//!
//! The implementation lives next to the deploy executor (it needs the Docker
//! CLI and the executor's policy validator); crates that cannot depend on the
//! executor receive an `Arc<dyn ComposeConfigurationChecker>` through the
//! plugin service registry instead, so every caller runs the same code.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use temps_entities::compose_security::ComposeSecurityPolicy;
use thiserror::Error;

/// Bumped whenever Compose validation changes in a way that can turn a
/// previously accepted configuration into a rejected one (or the reverse).
/// Projects whose stored check ran under an older revision are re-checked
/// once after an upgrade, so a behaviour change is reported before the next
/// deployment rather than by it.
pub const COMPOSE_VALIDATION_REVISION: i32 = 2;

/// Stable, user-facing class of a Compose check failure. The deploy job, the
/// preview and the check action all report the same class for the same input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ComposeCheckErrorClass {
    /// A `${VAR}` expression could not be resolved (`${VAR:?}` unset, or
    /// malformed syntax).
    Interpolation,
    /// A security check rejected a resolved value.
    PolicyViolation,
    /// A path escapes the project or names a Temps-generated file.
    InvalidPath,
    /// A Compose document is not valid YAML.
    InvalidYaml,
    /// The project override changes something an override may not change.
    InvalidOverride,
    /// The Docker Compose CLI on the host is too old for the options Temps
    /// passes.
    UnsupportedComposeVersion,
    /// `docker compose config` rejected the model for another reason.
    ConfigResolution,
    /// The Docker Compose CLI is not installed or not runnable.
    ComposeUnavailable,
    /// Temps itself failed (I/O, timeouts).
    Internal,
}

impl ComposeCheckErrorClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Interpolation => "interpolation",
            Self::PolicyViolation => "policy_violation",
            Self::InvalidPath => "invalid_path",
            Self::InvalidYaml => "invalid_yaml",
            Self::InvalidOverride => "invalid_override",
            Self::UnsupportedComposeVersion => "unsupported_compose_version",
            Self::ConfigResolution => "config_resolution",
            Self::ComposeUnavailable => "compose_unavailable",
            Self::Internal => "internal",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "interpolation" => Self::Interpolation,
            "policy_violation" => Self::PolicyViolation,
            "invalid_path" => Self::InvalidPath,
            "invalid_yaml" => Self::InvalidYaml,
            "invalid_override" => Self::InvalidOverride,
            "unsupported_compose_version" => Self::UnsupportedComposeVersion,
            "config_resolution" => Self::ConfigResolution,
            "compose_unavailable" => Self::ComposeUnavailable,
            "internal" => Self::Internal,
            _ => return None,
        })
    }
}

impl std::fmt::Display for ComposeCheckErrorClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `docker compose` invocation a check ran, sanitized for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ComposeCheckCommand {
    /// Program and arguments. Contains paths and option names only; no
    /// variable values are passed on the command line.
    pub argv: Vec<String>,
    /// Process exit code; `None` when it was killed or did not start.
    pub exit_code: Option<i32>,
    /// The tail of stderr with variable values, quoted text, assignments,
    /// URLs and absolute paths removed. Line numbers and error wording are
    /// kept.
    pub stderr: Option<String>,
}

/// Result of running the deploy-time resolution and policy validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ComposeCheckReport {
    /// Whether the configuration would pass the deploy-time checks.
    pub passed: bool,
    pub error_class: Option<ComposeCheckErrorClass>,
    /// The same message the deployment would fail with. Variable values are
    /// redacted.
    pub error: Option<String>,
    /// Why `docker compose config` had to run (project override, include,
    /// extends, YAML tags). Empty when the file was validated directly.
    pub resolution_reasons: Vec<String>,
    /// Whether `docker compose config` actually ran for this check.
    pub resolution_ran: bool,
    pub command: Option<ComposeCheckCommand>,
    /// Variables referenced without a default that are not set; Compose
    /// substitutes an empty string for each.
    pub unset_variables: Vec<String>,
    /// Variables the file references, in sorted order. Names only.
    pub referenced_variables: Vec<String>,
    /// Caveats about what this check could not cover.
    pub notes: Vec<String>,
}

impl ComposeCheckReport {
    pub fn failure(class: ComposeCheckErrorClass, error: impl Into<String>) -> Self {
        Self {
            passed: false,
            error_class: Some(class),
            error: Some(error.into()),
            resolution_reasons: Vec::new(),
            resolution_ran: false,
            command: None,
            unset_variables: Vec::new(),
            referenced_variables: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// Everything a check needs. Callers load it from the same sources the
/// deployment uses.
#[derive(Debug, Clone, Default)]
pub struct ComposeCheckRequest {
    /// Compose project name used for the `-p` option.
    pub project_name: String,
    /// Compose file path relative to the project directory.
    pub compose_file: String,
    pub content: String,
    pub compose_override: Option<String>,
    pub excluded_services: Vec<String>,
    pub policy: ComposeSecurityPolicy,
    /// Project environment values. Used to resolve `${VAR}` and to redact
    /// every value from the report; never returned.
    pub environment: HashMap<String, String>,
    /// Contents of the repository `.env`, when known.
    pub repository_env: Option<String>,
    /// Where the files came from. When set, the checker fetches the files
    /// `include` and `extends` reference (and include env files) at the same
    /// revision before checking, so they resolve as they do in a checkout.
    pub repository: Option<ComposeRepositorySource>,
    /// Files the Compose file references, keyed by path relative to the
    /// Compose project directory. Filled from `repository` by the checker.
    pub reference_files: BTreeMap<String, String>,
}

/// A repository revision a check reads its files from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeRepositorySource {
    pub connection_id: i32,
    pub owner: String,
    pub name: String,
    pub git_ref: String,
    /// Compose project directory inside the repository; `.` for the root.
    pub directory: String,
}

/// A repository file a Compose document refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeLocalReference {
    /// Normalized path relative to the Compose project directory.
    pub path: String,
    /// Whether the file may be absent (an include's implicit `.env`).
    pub optional: bool,
}

/// One `include` item's local files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeIncludeReference {
    /// The included Compose files in order, normalized relative to the
    /// Compose project directory. Remote and out-of-project paths are left
    /// out; resolution fetches or rejects those itself.
    pub paths: Vec<String>,
    /// Directory the included files' own references resolve against (`""`
    /// for the project directory itself). `None` when the main file is not
    /// a local file.
    pub project_directory: Option<String>,
    /// The item's `env_file` entries in load order, or else the optional
    /// `.env` in its project directory. Their values are visible to the
    /// included files only, and never override the including document's.
    pub env_files: Vec<ComposeLocalReference>,
}

/// Local files one Compose document pulls in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposeLocalReferences {
    /// `extends.file` documents, which resolve their references against
    /// their own directory and share the document's variables.
    pub extends: Vec<String>,
    pub includes: Vec<ComposeIncludeReference>,
}

/// Local files a Compose document pulls in: `extends.file`, `include` paths
/// and include `env_file` entries (or the implicit `.env` in the include's
/// project directory). `base` is the directory the document's references
/// resolve against, relative to the project directory (`""` for the
/// project directory itself).
///
/// `content` must already be interpolated, as resolution interpolates a
/// document before reading the paths it references; a path written as
/// `${DIR:-stack}/shared.yaml` is only known once that has happened.
/// Remote sources, absolute paths and paths that leave the project
/// directory are not listed; resolution rejects or fetches those itself,
/// exactly as it does for a checkout.
pub fn compose_local_references(base: &str, content: &str) -> ComposeLocalReferences {
    use yaml_serde::Value;
    let mut references = ComposeLocalReferences::default();
    let Ok(mut root) = yaml_serde::from_str::<Value>(content) else {
        return references;
    };
    let _ = root.apply_merge();
    if let Some(services) = root.get("services").and_then(Value::as_mapping) {
        for service in services.values() {
            if let Some(file) = service
                .get("extends")
                .and_then(|extends| extends.get("file"))
                .and_then(Value::as_str)
                .and_then(|file| join_file(base, file))
            {
                references.extends.push(file);
            }
        }
    }
    for include in root
        .get("include")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        let raw_paths: Vec<&str> = match include {
            Value::String(path) => vec![path.as_str()],
            _ => match include.get("path") {
                Some(Value::String(path)) => vec![path.as_str()],
                Some(Value::Sequence(paths)) => paths.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            },
        };
        let Some(main) = raw_paths.first() else {
            continue;
        };
        let main = join_file(base, main);
        let project_directory = match include.get("project_directory").and_then(Value::as_str) {
            Some(directory) => join_directory(base, directory),
            None => main.as_deref().map(parent_directory),
        };
        let explicit: Vec<&str> = match include.get("env_file") {
            Some(Value::String(file)) => vec![file.as_str()],
            Some(Value::Sequence(files)) => files.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let env_files = if explicit.is_empty() {
            project_directory
                .as_deref()
                .and_then(|directory| join_file(directory, ".env"))
                .map(|path| ComposeLocalReference {
                    path,
                    optional: true,
                })
                .into_iter()
                .collect()
        } else {
            explicit
                .into_iter()
                .filter(|file| *file != "/dev/null")
                .filter_map(|file| join_file(base, file))
                .map(|path| ComposeLocalReference {
                    path,
                    optional: false,
                })
                .collect()
        };
        references.includes.push(ComposeIncludeReference {
            paths: raw_paths
                .into_iter()
                .filter_map(|path| join_file(base, path))
                .collect(),
            project_directory,
            env_files,
        });
    }
    references
}

/// File `path` joined onto `base` and normalized, or `None` for a remote,
/// escaped (`$$`), absolute or empty path, or one that leaves the project
/// directory.
fn join_file(base: &str, path: &str) -> Option<String> {
    join_directory(base, path).filter(|path| !path.is_empty())
}

/// Directory `path` joined onto `base` and normalized; `""` is the project
/// directory itself.
fn join_directory(base: &str, path: &str) -> Option<String> {
    if path.contains("://")
        || path.starts_with("git@")
        || path.contains('$')
        || path.starts_with('/')
        || path.contains('\\')
    {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in base.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn parent_directory(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

/// Project configuration a check runs against, loaded from the database.
#[derive(Debug, Clone, Default)]
pub struct ComposeProjectContext {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    /// Compose project name the deployment uses (`temps-{project}-{environment}`).
    pub project_name: String,
    /// Project directory inside the repository; `.` for the root. Compose
    /// files and the repository `.env` are resolved relative to it.
    pub directory: String,
    pub policy: ComposeSecurityPolicy,
    pub environment: HashMap<String, String>,
    pub compose_path: Option<String>,
    pub compose_override: Option<String>,
    pub excluded_services: Vec<String>,
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ComposeContextError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },
    #[error("Project {project_id} is not a Docker Compose project")]
    NotComposeProject { project_id: i32 },
    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },
    #[error("Failed to load Compose check context for project {project_id}: {reason}")]
    Load { project_id: i32, reason: String },
}

#[async_trait]
pub trait ComposeConfigurationChecker: Send + Sync {
    /// Run the deploy job's resolution and policy validation on `request`.
    async fn check(&self, request: ComposeCheckRequest) -> ComposeCheckReport;

    /// Load the saved policy, environment variables and Compose settings the
    /// deployment of `project_id` would use. `environment_id` defaults to the
    /// project's production environment.
    async fn load_project_context(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<ComposeProjectContext, ComposeContextError>;
}

/// How a saved project override relates to the repository Compose file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ComposeOverrideRedundancy {
    /// The override is the repository file.
    Identical,
    /// The override contains the whole repository file plus changes.
    Superset,
}

impl ComposeOverrideRedundancy {
    pub fn warning(&self) -> &'static str {
        match self {
            Self::Identical => {
                "The project override is a copy of the repository Compose file. An override is \
                 merged on top of that file, so it only needs the fields you want to change; \
                 keeping a full copy means repository changes are silently shadowed. Clear the \
                 override, or keep only the changed fields."
            }
            Self::Superset => {
                "The project override contains the whole repository Compose file plus changes. \
                 An override is merged on top of that file, so it only needs the fields you want \
                 to change; keeping a full copy means repository changes are silently shadowed. \
                 Keep only the changed fields."
            }
        }
    }
}

/// Compare a saved override with the repository file it is merged onto.
/// Returns `None` when the override is a normal partial override or either
/// document does not parse.
pub fn override_redundancy(
    repository_yaml: &str,
    override_yaml: &str,
) -> Option<ComposeOverrideRedundancy> {
    if override_yaml.trim().is_empty() {
        return None;
    }
    let mut base: yaml_serde::Value = yaml_serde::from_str(repository_yaml).ok()?;
    let mut overlay: yaml_serde::Value = yaml_serde::from_str(override_yaml).ok()?;
    base.apply_merge().ok()?;
    overlay.apply_merge().ok()?;
    let services = base.get("services")?.as_mapping()?;
    if services.is_empty() {
        return None;
    }
    if base == overlay {
        return Some(ComposeOverrideRedundancy::Identical);
    }
    is_yaml_subset(&base, &overlay).then_some(ComposeOverrideRedundancy::Superset)
}

/// Whether every key, item and scalar of `small` is present in `large`.
fn is_yaml_subset(small: &yaml_serde::Value, large: &yaml_serde::Value) -> bool {
    use yaml_serde::Value;
    match (small, large) {
        (Value::Mapping(small), Value::Mapping(large)) => small.iter().all(|(key, value)| {
            large
                .get(key)
                .is_some_and(|other| is_yaml_subset(value, other))
        }),
        (Value::Sequence(small), Value::Sequence(large)) => small
            .iter()
            .all(|item| large.iter().any(|other| is_yaml_subset(item, other))),
        (Value::Tagged(small), Value::Tagged(large)) => {
            small.tag == large.tag && is_yaml_subset(&small.value, &large.value)
        }
        (small, large) => small == large,
    }
}

/// Service list fields whose items Compose appends when files are merged.
/// Repeated items are merged once where that cannot change the result.
pub const DEDUPLICATED_SERVICE_LISTS: &[&str] = &["env_file", "volumes", "environment", "ports"];

/// Longest list [`deduplicate_override_lists`] compares; a longer one is
/// left as written, which Compose merges as it always would.
const MAX_DEDUPLICATED_ITEMS: usize = 1024;

/// Remove the list items in `overlay` that only repeat what is already in
/// effect, for the fields in [`DEDUPLICATED_SERVICE_LISTS`], so a pasted copy
/// of the repository file does not append everything a second time. An
/// overlay list that becomes empty is removed, which leaves the base list
/// untouched.
///
/// Order decides which entry wins: a later env file overrides an earlier
/// one, and a later volume or variable for the same target or key replaces
/// it. Compose versions also differ in how they merge these lists (plain
/// append, or one entry per key). So an item is only dropped when it repeats
/// the entries directly before it, in order: the overlay starting with the
/// end of the base list, or an item repeating the one before it. Applying
/// the same run twice in a row leaves what applying it once does, under
/// every merge. Any other repeat moves an entry after ones it would
/// otherwise precede, so it stays: base `[common.env, production.env]` with
/// an overlay `[common.env]` keeps the overlay's `common.env` last.
///
/// `ports` is only de-duplicated within the overlay, where identical
/// mappings carry no order: Temps replaces a service's ports with the
/// override's list rather than appending, so removing an item there would
/// drop a port instead of a duplicate.
pub fn deduplicate_override_lists(base: &yaml_serde::Value, overlay: &mut yaml_serde::Value) {
    use yaml_serde::Value;
    let Some(overlay_services) = overlay
        .get_mut("services")
        .and_then(Value::as_mapping_mut)
    else {
        return;
    };
    let base_services = base.get("services").and_then(Value::as_mapping);
    for (name, overlay_service) in overlay_services.iter_mut() {
        let Some(overlay_service) = overlay_service.as_mapping_mut() else {
            continue;
        };
        let base_service = base_services.and_then(|services| services.get(name));
        for field in DEDUPLICATED_SERVICE_LISTS {
            let key = Value::String((*field).to_string());
            // `env_file: .env` is a one-item list in Compose's merge.
            if let Some(single @ Value::String(_)) = overlay_service.get(&key) {
                let single = single.clone();
                overlay_service.insert(key.clone(), Value::Sequence(vec![single]));
            }
            let Some(Value::Sequence(items)) = overlay_service.get_mut(&key) else {
                continue;
            };
            if *field == "ports" {
                let mut seen = Vec::new();
                items.retain(|item| {
                    let identity = list_identity(item);
                    let new = !seen.contains(&identity);
                    if new {
                        seen.push(identity);
                    }
                    new
                });
                continue;
            }
            let existing: Vec<Value> = match base_service.and_then(|service| service.get(*field)) {
                Some(Value::Sequence(items)) => items.iter().map(list_identity).collect(),
                Some(single @ Value::String(_)) => vec![list_identity(single)],
                _ => Vec::new(),
            };
            if items.len() > MAX_DEDUPLICATED_ITEMS || existing.len() > MAX_DEDUPLICATED_ITEMS {
                continue;
            }
            drop_repeated_runs(&existing, items);
            if items.is_empty() {
                overlay_service.remove(&key);
            }
        }
    }
}

/// Drop the leading overlay items that repeat the end of `base` in order,
/// until none do, then items that repeat the item directly before them.
/// See [`deduplicate_override_lists`] for why only these are safe.
fn drop_repeated_runs(base: &[yaml_serde::Value], items: &mut Vec<yaml_serde::Value>) {
    loop {
        let identities: Vec<yaml_serde::Value> = items.iter().map(list_identity).collect();
        let repeated = (1..=base.len().min(identities.len()))
            .rev()
            .find(|&run| identities[..run] == base[base.len() - run..])
            .unwrap_or(0);
        if repeated == 0 {
            break;
        }
        items.drain(..repeated);
    }
    items.dedup_by(|later, earlier| list_identity(later) == list_identity(earlier));
}

/// Comparison key for a list item: `env_file: .env` and
/// `env_file: [{path: .env}]` name the same file.
fn list_identity(item: &yaml_serde::Value) -> yaml_serde::Value {
    use yaml_serde::Value;
    if let Some(mapping) = item.as_mapping() {
        if mapping.len() == 1 {
            if let Some(path) = mapping.get(Value::String("path".to_string())) {
                return path.clone();
            }
        }
    }
    item.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "services:\n  app:\n    image: ghcr.io/o/a:1\n    env_file: .env\n    volumes:\n      - data:/data\n    ports:\n      - \"8080:80\"\nvolumes:\n  data: {}\n";

    #[test]
    fn identical_override_is_reported() {
        assert_eq!(
            override_redundancy(REPO, REPO),
            Some(ComposeOverrideRedundancy::Identical)
        );
    }

    #[test]
    fn superset_override_is_reported() {
        let overlay = "services:\n  app:\n    image: ghcr.io/o/a:1\n    env_file: .env\n    restart: always\n    volumes:\n      - data:/data\n    ports:\n      - \"8080:80\"\n  worker:\n    image: ghcr.io/o/w:1\nvolumes:\n  data: {}\n";
        assert_eq!(
            override_redundancy(REPO, overlay),
            Some(ComposeOverrideRedundancy::Superset)
        );
    }

    #[test]
    fn partial_override_is_not_reported() {
        let overlay = "services:\n  app:\n    restart: always\n";
        assert_eq!(override_redundancy(REPO, overlay), None);
        assert_eq!(override_redundancy(REPO, "  "), None);
        assert_eq!(override_redundancy("not: [valid", overlay), None);
    }

    #[test]
    fn duplicate_list_items_are_merged_once() {
        let base: yaml_serde::Value = yaml_serde::from_str(REPO).unwrap();
        let mut overlay: yaml_serde::Value = yaml_serde::from_str(
            "services:\n  app:\n    env_file:\n      - path: .env\n      - extra.env\n      - extra.env\n    volumes:\n      - data:/data\n      - ./conf:/conf\n    environment:\n      - A=1\n      - A=1\n    ports:\n      - \"8080:80\"\n      - \"8080:80\"\n",
        )
        .unwrap();
        deduplicate_override_lists(&base, &mut overlay);
        let app = &overlay["services"]["app"];
        assert_eq!(app["env_file"], yaml_serde::from_str::<yaml_serde::Value>("[extra.env]").unwrap());
        assert_eq!(app["volumes"], yaml_serde::from_str::<yaml_serde::Value>("[./conf:/conf]").unwrap());
        assert_eq!(app["environment"], yaml_serde::from_str::<yaml_serde::Value>("[A=1]").unwrap());
        // Ports keep replacement semantics: only internal duplicates go.
        assert_eq!(app["ports"], yaml_serde::from_str::<yaml_serde::Value>("[\"8080:80\"]").unwrap());
    }

    fn overlay_list(base: &str, overlay: &str, field: &str) -> Option<yaml_serde::Value> {
        let base: yaml_serde::Value =
            yaml_serde::from_str(&format!("services:\n  app:\n    {field}: {base}\n")).unwrap();
        let mut overlay: yaml_serde::Value =
            yaml_serde::from_str(&format!("services:\n  app:\n    {field}: {overlay}\n"))
                .unwrap();
        deduplicate_override_lists(&base, &mut overlay);
        overlay["services"]["app"].get(field).cloned()
    }

    fn list(text: &str) -> Option<yaml_serde::Value> {
        Some(yaml_serde::from_str(text).unwrap())
    }

    #[test]
    fn repeats_that_would_change_which_entry_wins_are_kept() {
        // The overlay's common.env loads after production.env.
        assert_eq!(
            overlay_list("[common.env, production.env]", "[common.env]", "env_file"),
            list("[common.env]")
        );
        assert_eq!(
            overlay_list("[a.env, b.env]", "[b.env, a.env]", "env_file"),
            list("[a.env]")
        );
        // The overlay's ./conf mount replaces ./other for the same target.
        assert_eq!(
            overlay_list("[./conf:/data]", "[./other:/data, ./conf:/data]", "volumes"),
            list("[./other:/data, ./conf:/data]")
        );
        assert_eq!(
            overlay_list("[A=1]", "[A=2, A=1]", "environment"),
            list("[A=2, A=1]")
        );
        // A repeat that is not adjacent stays inside the overlay too.
        assert_eq!(
            overlay_list("[]", "[x.env, y.env, x.env]", "env_file"),
            list("[x.env, y.env, x.env]")
        );
    }

    #[test]
    fn repeats_of_the_entries_just_before_are_dropped() {
        // The overlay continues the base list after repeating its end.
        assert_eq!(
            overlay_list("[a.env, b.env, c.env]", "[b.env, c.env, d.env]", "env_file"),
            list("[d.env]")
        );
        assert_eq!(
            overlay_list("[common.env, production.env]", "[common.env, production.env]", "env_file"),
            None
        );
        assert_eq!(
            overlay_list("[a.env]", "[a.env, a.env, b.env, b.env]", "env_file"),
            list("[b.env]")
        );
    }

    #[test]
    fn a_full_copy_override_merges_without_duplicates() {
        let base: yaml_serde::Value = yaml_serde::from_str(REPO).unwrap();
        let mut overlay: yaml_serde::Value = yaml_serde::from_str(REPO).unwrap();
        deduplicate_override_lists(&base, &mut overlay);
        let app = overlay["services"]["app"].as_mapping().unwrap();
        assert!(app.get("env_file").is_none());
        assert!(app.get("volumes").is_none());
    }

    #[test]
    fn local_references_follow_include_and_extends_paths() {
        let content = "include:\n  - shared.yaml\n  - path: [stack/a.yaml, stack/b.yaml]\n    env_file: [stack/first.env, stack/second.env]\n  - path: ../outside.yaml\n  - https://example.com/r.git\n  - $${DIR}/x.yaml\nservices:\n  app:\n    extends:\n      file: ./common/base.yaml\n      service: base\n";
        let references = compose_local_references("", content);
        assert_eq!(references.extends, vec!["common/base.yaml".to_string()]);
        let env = |path: &str, optional| ComposeLocalReference {
            path: path.to_string(),
            optional,
        };
        assert_eq!(
            references.includes[0],
            ComposeIncludeReference {
                paths: vec!["shared.yaml".to_string()],
                project_directory: Some(String::new()),
                env_files: vec![env(".env", true)],
            }
        );
        assert_eq!(
            references.includes[1],
            ComposeIncludeReference {
                paths: vec!["stack/a.yaml".to_string(), "stack/b.yaml".to_string()],
                project_directory: Some("stack".to_string()),
                env_files: vec![env("stack/first.env", false), env("stack/second.env", false)],
            }
        );
        // Out-of-project, remote and escaped (literal `$`) paths are left to
        // resolution, which rejects or fetches them itself.
        for include in &references.includes[2..] {
            assert!(include.paths.is_empty(), "{include:?}");
        }
        // Nested references resolve against the including document's base.
        let nested = compose_local_references("stack", "include: [../shared.yaml, inner/c.yaml]");
        assert_eq!(nested.includes[0].paths, vec!["shared.yaml".to_string()]);
        assert_eq!(nested.includes[1].paths, vec!["stack/inner/c.yaml".to_string()]);
        assert_eq!(
            compose_local_references("", "not: [valid"),
            ComposeLocalReferences::default()
        );
    }

    #[test]
    fn include_project_directories_may_be_the_project_root() {
        // `project_directory: .` (or `..` back to the root) names the
        // project directory itself; the include must still be fetched.
        let root = compose_local_references(
            "",
            "include:\n  - path: stack/shared.yaml\n    project_directory: .\n",
        );
        assert_eq!(
            root.includes[0],
            ComposeIncludeReference {
                paths: vec!["stack/shared.yaml".to_string()],
                project_directory: Some(String::new()),
                env_files: vec![ComposeLocalReference {
                    path: ".env".to_string(),
                    optional: true,
                }],
            }
        );
        let nested = compose_local_references(
            "stack",
            "include:\n  - path: inner.yaml\n    project_directory: ..\n",
        );
        assert_eq!(nested.includes[0].paths, vec!["stack/inner.yaml".to_string()]);
        assert_eq!(nested.includes[0].project_directory, Some(String::new()));
        // Leaving the project directory is still refused.
        let escaping = compose_local_references(
            "",
            "include:\n  - path: shared.yaml\n    project_directory: ..\n",
        );
        assert_eq!(escaping.includes[0].project_directory, None);
        assert!(escaping.includes[0].env_files.is_empty());
    }

    #[test]
    fn error_class_round_trips() {
        for class in [
            ComposeCheckErrorClass::Interpolation,
            ComposeCheckErrorClass::PolicyViolation,
            ComposeCheckErrorClass::InvalidPath,
            ComposeCheckErrorClass::InvalidYaml,
            ComposeCheckErrorClass::InvalidOverride,
            ComposeCheckErrorClass::UnsupportedComposeVersion,
            ComposeCheckErrorClass::ConfigResolution,
            ComposeCheckErrorClass::ComposeUnavailable,
            ComposeCheckErrorClass::Internal,
        ] {
            assert_eq!(ComposeCheckErrorClass::parse(class.as_str()), Some(class));
        }
    }
}
