// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compose → workload plane translation (ADR-055 §14).
//!
//! Turns a checked Compose model (the output of
//! [`crate::compose::ComposeExecutor::check_configuration`], so the security
//! policy, filesystem confinement and `docker compose config` resolution have
//! already run) into one [`WorkloadSpec`] per service, in dependency order.
//!
//! The translation mirrors what the legacy CLI path makes Compose run, not
//! what the file alone says: the generated overrides the CLI path writes
//! (`.env.temps` appended to every service's `env_file`, project variables
//! winning over inline values, every service joining `default` plus the
//! Temps network, the `sh.temps.*` labels) are folded in here, so a
//! translated service and the container `docker compose up` creates for it
//! can be compared field by field.
//!
//! It is allow-list based. A service field is either translated, consumed
//! before the plane is involved (`build`, `pull_policy`, `platform`), already
//! enforced by the `Trusted` profile every deployment gets (`cap_drop`,
//! `init: true`), or refused with [`ComposeTranslationError`] naming the
//! service and the field. Nothing is dropped silently.
//!
//! Environment values never enter the [`WorkloadSpec`] (it is stored and sent
//! to nodes): they are returned beside it in [`ServiceEnvironment`], whose
//! `Debug` output is redacted, for the caller to stage with the plane.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use temps_workload::{
    AppProtocol, Artifact, EnvSpec, Isolation, Lifetime, NetworkSpec, PlacementSpec, PortProtocol,
    PortSpec, Probe, ProbeKind, Probes, Process, ResourceAmounts, Resources, RestartPolicy,
    VolumeMount, WorkloadRef, WorkloadSpec,
};
use thiserror::Error;
use yaml_serde::{Mapping, Value};

use crate::compose::ComposeExecutor;
use crate::compose_interpolation;

/// Label Temps adds to every Compose service so logs can be attributed.
pub const SERVICE_LABEL: &str = "sh.temps.service";

/// Service fields the translation expresses in the [`WorkloadSpec`] or the
/// surrounding [`TranslatedService`].
pub const TRANSLATED_FIELDS: &[&str] = &[
    "image",
    "command",
    "entrypoint",
    "environment",
    "env_file",
    "ports",
    "expose",
    "networks",
    "volumes",
    "restart",
    "deploy",
    "mem_limit",
    "mem_reservation",
    "cpus",
    "pids_limit",
    "scale",
    "healthcheck",
    "user",
    "working_dir",
    "labels",
    "depends_on",
];

/// Service fields the legacy path consumes before any container exists. The
/// plane receives their outcome (a built or pulled image), not the field.
pub const PRE_PLANE_FIELDS: &[&str] = &["build", "pull_policy", "platform"];

/// Service fields whose only allowed values are already what the `Trusted`
/// profile applies to every deployment.
pub const SUBSUMED_FIELDS: &[&str] = &["cap_drop", "init"];

/// Why a variable reached the container, so a shadow diff can say which
/// source a mismatch came from without ever showing a value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ServiceEnvironment {
    vars: BTreeMap<String, String>,
}

impl ServiceEnvironment {
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    pub fn into_vars(self) -> BTreeMap<String, String> {
        self.vars
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.vars.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.vars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

impl std::fmt::Debug for ServiceEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Names only: values are project variables and may be credentials.
        f.debug_struct("ServiceEnvironment")
            .field("names", &self.vars.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The condition a `depends_on` entry waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyCondition {
    /// `service_started` (and the short list form).
    Started,
    /// `service_healthy`.
    Healthy,
    /// `service_completed_successfully`.
    CompletedSuccessfully,
}

impl DependencyCondition {
    pub fn as_compose(self) -> &'static str {
        match self {
            DependencyCondition::Started => "service_started",
            DependencyCondition::Healthy => "service_healthy",
            DependencyCondition::CompletedSuccessfully => "service_completed_successfully",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDependency {
    pub service: String,
    pub condition: DependencyCondition,
    /// `required: false`: Compose ignores the dependency when the service is
    /// not in the project.
    pub required: bool,
}

/// A network the service joins, by its Docker name, with the DNS aliases
/// Compose gives it there (the service name first, then any declared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceNetwork {
    pub name: String,
    pub aliases: Vec<String>,
}

/// A port binding as Compose would publish it. The plane allocates host
/// ports itself, so a fixed `host_port` is recorded here, never in the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedPort {
    pub host_ip: Option<String>,
    /// `None`: Docker picks a free port (`127.0.0.1::8080`).
    pub host_port: Option<u16>,
    pub container_port: u16,
    pub protocol: PortProtocol,
}

/// A project-level network or volume, resolved to its Docker name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectResource {
    /// The key in the Compose file (`default`, `cache`).
    pub key: String,
    /// The Docker object name (`<project>_<key>` unless named or external).
    pub name: String,
    /// Created outside the project; never created or removed by it.
    pub external: bool,
}

/// One Compose service as a workload.
#[derive(Debug, Clone, PartialEq)]
pub struct TranslatedService {
    pub name: String,
    /// Never holds an environment value; see [`Self::environment`].
    pub spec: WorkloadSpec,
    /// The container environment exactly as the legacy path assembles it.
    pub environment: ServiceEnvironment,
    pub depends_on: Vec<ServiceDependency>,
    /// Every network the service joins, the Temps network included.
    pub networks: Vec<ServiceNetwork>,
    pub published_ports: Vec<PublishedPort>,
    /// `expose:` entries. They set no binding and no reachability: on a
    /// user-defined network every port is reachable from its peers.
    pub exposed_ports: Vec<String>,
    /// Mount paths of anonymous volumes (`- /app/node_modules`).
    pub anonymous_volumes: Vec<String>,
    /// The image is built from the repository before it runs.
    pub built: bool,
    pub platform: Option<String>,
    /// Why the service must keep running, or `None` when exiting 0 is a
    /// completed one-shot task (a migration). The legacy readiness rule.
    pub must_stay_running: Option<&'static str>,
}

/// A whole Compose project as workloads.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposeTranslation {
    pub project: String,
    pub networks: Vec<ProjectResource>,
    pub volumes: Vec<ProjectResource>,
    /// In dependency order: every service follows the ones it depends on.
    pub services: Vec<TranslatedService>,
}

impl ComposeTranslation {
    pub fn service(&self, name: &str) -> Option<&TranslatedService> {
        self.services.iter().find(|service| service.name == name)
    }

    pub fn service_names(&self) -> Vec<&str> {
        self.services.iter().map(|s| s.name.as_str()).collect()
    }
}

/// What an `env_file` entry reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvFileContent {
    /// The repository ships the file; this is its text.
    Dotenv(String),
    /// Temps synthesized it from the project's variables.
    ProjectEnvironment,
}

/// Inputs for one translation.
#[derive(Debug, Clone, Copy)]
pub struct ComposeTranslationInput<'a> {
    /// The Compose project name (`temps-<project>-<environment>`).
    pub project_name: &'a str,
    /// The compose file, stack-root relative (`deploy/compose.yaml`).
    pub compose_file: &'a str,
    /// The checked, resolved model (`CheckedComposeConfiguration::content`).
    pub content: &'a str,
    /// Must be `None`: an override is merged into `content` by resolution.
    pub override_content: Option<&'a str>,
    /// The project's variables (what the legacy path writes to `.env.temps`).
    pub environment_vars: &'a HashMap<String, String>,
    /// The repository `.env` (interpolation only, as `--env-file .env`).
    pub repository_env: Option<&'a str>,
    /// Every `env_file` the model references, keyed by its stack-root
    /// relative path (`ComposeExecutor::plan_env_files_for_compose_file`).
    pub env_files: &'a BTreeMap<String, EnvFileContent>,
    /// Temps labels for every service (the legacy labels override).
    pub labels: &'a HashMap<String, String>,
    /// Services that receive public traffic.
    pub routed_services: &'a [String],
    pub owner: WorkloadRef,
    /// The shared Temps network every service joins.
    pub temps_network: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ComposeTranslationError {
    #[error(
        "Compose project '{project}': service '{service}' sets '{field}', which the workload \
         plane cannot express: {reason}"
    )]
    Unsupported {
        project: String,
        service: String,
        field: String,
        reason: String,
    },
    #[error("Compose project '{project}': service '{service}' has an invalid '{field}': {reason}")]
    InvalidValue {
        project: String,
        service: String,
        field: String,
        reason: String,
    },
    #[error(
        "Compose project '{project}': top-level '{section}' entry '{name}' sets '{field}', which \
         the workload plane cannot express: {reason}"
    )]
    UnsupportedTopLevel {
        project: String,
        section: String,
        name: String,
        field: String,
        reason: String,
    },
    #[error("Compose project '{project}': services {services:?} depend on each other in a cycle")]
    DependencyCycle {
        project: String,
        services: Vec<String>,
    },
    #[error(
        "Compose project '{project}': service '{service}' depends on '{dependency}', which is \
         not a service of the project"
    )]
    UnknownDependency {
        project: String,
        service: String,
        dependency: String,
    },
    #[error("Compose project '{project}': the checked model is not valid YAML: {reason}")]
    InvalidYaml { project: String, reason: String },
    #[error("Compose project '{project}' defines no services")]
    NoServices { project: String },
    #[error(
        "Compose project '{project}': an override file was not merged into the model; the \
         translation needs the single resolved model `docker compose config` produces"
    )]
    OverrideNotMerged { project: String },
}

/// Translate a checked Compose model. See the module documentation.
pub fn translate(
    input: &ComposeTranslationInput<'_>,
) -> Result<ComposeTranslation, ComposeTranslationError> {
    let project = input.project_name.to_string();
    if input
        .override_content
        .is_some_and(|content| !content.trim().is_empty())
    {
        return Err(ComposeTranslationError::OverrideNotMerged { project });
    }
    let mut root: Value = yaml_serde::from_str(input.content).map_err(|error| {
        ComposeTranslationError::InvalidYaml {
            project: project.clone(),
            reason: error.to_string(),
        }
    })?;
    root.apply_merge()
        .map_err(|error| ComposeTranslationError::InvalidYaml {
            project: project.clone(),
            reason: format!("merge keys could not be applied: {error}"),
        })?;
    let Some(root) = root.as_mapping() else {
        return Err(ComposeTranslationError::InvalidYaml {
            project,
            reason: "the document is not a mapping".to_string(),
        });
    };

    let ctx = Context::new(input);
    for (key, _) in root {
        let key = key.as_str().unwrap_or_default();
        match key {
            "services" | "networks" | "volumes" | "name" | "version" => {}
            other if other.starts_with("x-") => {}
            other => {
                return Err(ComposeTranslationError::UnsupportedTopLevel {
                    project,
                    section: other.to_string(),
                    name: "*".to_string(),
                    field: other.to_string(),
                    reason: top_level_reason(other).to_string(),
                })
            }
        }
    }

    let services = root
        .get("services")
        .and_then(Value::as_mapping)
        .filter(|services| !services.is_empty())
        .ok_or_else(|| ComposeTranslationError::NoServices {
            project: project.clone(),
        })?;

    let networks = ctx.project_resources(root.get("networks"), "networks")?;
    let volumes = ctx.project_resources(root.get("volumes"), "volumes")?;
    let expectations =
        ComposeExecutor::readiness_expectations(input.content, None, input.routed_services);

    let mut translated = Vec::with_capacity(services.len());
    for (name, definition) in services {
        let name = name
            .as_str()
            .ok_or_else(|| ComposeTranslationError::InvalidYaml {
                project: project.clone(),
                reason: "a service name is not a string".to_string(),
            })?;
        let definition = definition.as_mapping().ok_or_else(|| {
            ctx.invalid(name, "services", "the service definition is not a mapping")
        })?;
        let mut service = ctx.service(name, definition, &networks, &volumes)?;
        service.must_stay_running = expectations.why_long_running(name);
        translated.push(service);
    }

    let services = order_by_dependencies(&project, translated)?;
    Ok(ComposeTranslation {
        project,
        networks,
        volumes,
        services,
    })
}

fn top_level_reason(key: &str) -> &'static str {
    match key {
        "secrets" | "configs" => {
            "file-backed Compose secrets and configs are host files; Temps delivers secrets \
             itself under /run/secrets"
        }
        "include" => "includes must be resolved by `docker compose config` before translation",
        _ => "this top-level key has no workload equivalent",
    }
}

struct Context<'a> {
    input: &'a ComposeTranslationInput<'a>,
    /// What a valueless `environment` entry resolves from: the repository
    /// `.env`, then the project variables (`--env-file .env --env-file
    /// .env.temps`, later wins).
    interpolation: HashMap<String, String>,
}

impl<'a> Context<'a> {
    fn new(input: &'a ComposeTranslationInput<'a>) -> Self {
        let mut interpolation = input
            .repository_env
            .and_then(|content| compose_interpolation::parse_dotenv(content).ok())
            .unwrap_or_default();
        interpolation.extend(
            input
                .environment_vars
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        Self {
            input,
            interpolation,
        }
    }

    fn project(&self) -> String {
        self.input.project_name.to_string()
    }

    fn unsupported(
        &self,
        service: &str,
        field: &str,
        reason: impl Into<String>,
    ) -> ComposeTranslationError {
        ComposeTranslationError::Unsupported {
            project: self.project(),
            service: service.to_string(),
            field: field.to_string(),
            reason: reason.into(),
        }
    }

    fn invalid(
        &self,
        service: &str,
        field: &str,
        reason: impl Into<String>,
    ) -> ComposeTranslationError {
        ComposeTranslationError::InvalidValue {
            project: self.project(),
            service: service.to_string(),
            field: field.to_string(),
            reason: reason.into(),
        }
    }

    /// A scalar from the checked model as the literal the container sees:
    /// `$$` is a `$` (the checked model escapes every literal dollar), and a
    /// remaining `${VAR}` / `$VAR` would be a reference nothing resolved.
    fn literal(
        &self,
        service: &str,
        field: &str,
        raw: &str,
    ) -> Result<String, ComposeTranslationError> {
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '$' {
                out.push(c);
                continue;
            }
            match chars.peek().copied() {
                Some('$') => {
                    chars.next();
                    out.push('$');
                }
                Some(next) if next == '{' || next == '_' || next.is_ascii_alphabetic() => {
                    return Err(self.invalid(
                        service,
                        field,
                        "contains a variable reference the configuration check did not \
                         resolve; write $$ for a literal dollar sign",
                    ));
                }
                _ => out.push('$'),
            }
        }
        Ok(out)
    }

    /// A string, number or bool scalar rendered as Compose renders it.
    fn scalar(
        &self,
        service: &str,
        field: &str,
        value: &Value,
    ) -> Result<String, ComposeTranslationError> {
        match value {
            Value::String(text) => self.literal(service, field, text),
            Value::Number(number) => Ok(number.to_string()),
            Value::Bool(flag) => Ok(flag.to_string()),
            _ => Err(self.invalid(service, field, "expected a string, number or boolean")),
        }
    }

    fn string_or_list(
        &self,
        service: &str,
        field: &str,
        value: &Value,
    ) -> Result<Vec<String>, ComposeTranslationError> {
        match value {
            Value::Null => Ok(Vec::new()),
            Value::String(text) => {
                let text = self.literal(service, field, text)?;
                shlex::split(&text).ok_or_else(|| {
                    self.invalid(service, field, "the command line has unbalanced quotes")
                })
            }
            Value::Sequence(items) => items
                .iter()
                .map(|item| self.scalar(service, field, item))
                .collect(),
            _ => Err(self.invalid(service, field, "expected a string or a list of strings")),
        }
    }

    fn project_resources(
        &self,
        section: Option<&Value>,
        kind: &str,
    ) -> Result<Vec<ProjectResource>, ComposeTranslationError> {
        let mut resources = Vec::new();
        let declared = match section {
            None | Some(Value::Null) => Mapping::new(),
            Some(Value::Mapping(map)) => map.clone(),
            Some(_) => {
                return Err(ComposeTranslationError::InvalidYaml {
                    project: self.project(),
                    reason: format!("top-level '{kind}' is not a mapping"),
                })
            }
        };
        let refuse =
            |name: &str, field: &str, reason: &str| ComposeTranslationError::UnsupportedTopLevel {
                project: self.project(),
                section: kind.to_string(),
                name: name.to_string(),
                field: field.to_string(),
                reason: reason.to_string(),
            };
        let mut has_default = false;
        for (key, definition) in &declared {
            let key = key.as_str().unwrap_or_default().to_string();
            has_default |= key == "default";
            let mut external = false;
            let mut explicit_name: Option<String> = None;
            if let Some(definition) = definition.as_mapping() {
                for (field, value) in definition {
                    let field = field.as_str().unwrap_or_default();
                    match field {
                        "name" => {
                            explicit_name = value
                                .as_str()
                                .map(|name| self.literal(&key, "name", name))
                                .transpose()?;
                        }
                        "external" => {
                            external = match value {
                                Value::Bool(flag) => *flag,
                                // Legacy `external: {name: x}`.
                                Value::Mapping(map) => {
                                    if let Some(name) = map.get("name").and_then(Value::as_str) {
                                        explicit_name =
                                            Some(self.literal(&key, "external.name", name)?);
                                    }
                                    true
                                }
                                _ => return Err(refuse(&key, "external", "must be a boolean")),
                            };
                        }
                        "driver" => {
                            let expected = if kind == "networks" {
                                "bridge"
                            } else {
                                "local"
                            };
                            if value.as_str() != Some(expected) {
                                return Err(refuse(
                                    &key,
                                    "driver",
                                    &format!("only the '{expected}' driver runs on a Temps node"),
                                ));
                            }
                        }
                        // Docker object labels; no runtime effect.
                        "labels" => {}
                        other if other.starts_with("x-") => {}
                        other => {
                            return Err(refuse(
                                &key,
                                other,
                                "driver options, IPAM and network modes are not translated",
                            ))
                        }
                    }
                }
            } else if !definition.is_null() {
                return Err(refuse(&key, kind, "the definition is not a mapping"));
            }
            let name = match (explicit_name, external) {
                (Some(name), _) => name,
                (None, true) => key.clone(),
                (None, false) => format!("{}_{key}", self.input.project_name),
            };
            resources.push(ProjectResource {
                key,
                name,
                external,
            });
        }
        // Every service joins `default` (the legacy network override); it
        // exists even when the file never declares it.
        if kind == "networks" && !has_default {
            resources.push(ProjectResource {
                key: "default".to_string(),
                name: format!("{}_default", self.input.project_name),
                external: false,
            });
        }
        Ok(resources)
    }

    fn service(
        &self,
        name: &str,
        definition: &Mapping,
        networks: &[ProjectResource],
        volumes: &[ProjectResource],
    ) -> Result<TranslatedService, ComposeTranslationError> {
        for (field, _) in definition {
            let field = field.as_str().unwrap_or_default();
            let known = TRANSLATED_FIELDS.contains(&field)
                || PRE_PLANE_FIELDS.contains(&field)
                || SUBSUMED_FIELDS.contains(&field)
                || field.starts_with("x-");
            if !known {
                return Err(self.unsupported(name, field, unsupported_field_reason(field)));
            }
        }
        let field = |key: &str| definition.get(key).filter(|value| !value.is_null());

        // Subsumed fields: accept only what the Trusted profile already does.
        if let Some(init) = field("init") {
            if init.as_bool() != Some(true) {
                return Err(self.unsupported(
                    name,
                    "init",
                    "every plane container runs with an init process; an image that owns PID 1 \
                     (init: false) needs the legacy Compose path",
                ));
            }
        }
        if let Some(cap_drop) = field("cap_drop") {
            // Any subset of ALL is already dropped; the list itself must be
            // well formed.
            if !cap_drop.is_sequence() {
                return Err(self.invalid(name, "cap_drop", "expected a list of capabilities"));
            }
        }

        let built = field("build").is_some();
        let image = match field("image") {
            Some(image) => self.scalar(name, "image", image)?,
            // Compose v2 names a built image `<project>-<service>`.
            None if built => format!("{}-{name}", self.input.project_name),
            None => {
                return Err(self.invalid(
                    name,
                    "image",
                    "a service needs an image or a build section",
                ))
            }
        };
        let platform = field("platform")
            .map(|platform| self.scalar(name, "platform", platform))
            .transpose()?;

        let process = Process {
            command: field("command")
                .map(|value| self.string_or_list(name, "command", value))
                .transpose()?,
            entrypoint: field("entrypoint")
                .map(|value| self.string_or_list(name, "entrypoint", value))
                .transpose()?,
            workdir: field("working_dir")
                .map(|value| self.scalar(name, "working_dir", value))
                .transpose()?,
            user: field("user")
                .map(|value| self.scalar(name, "user", value))
                .transpose()?,
        };

        let environment = self.environment(name, definition)?;
        let (ports, published_ports) = self.ports(name, field("ports"))?;
        let exposed_ports = match field("expose") {
            None => Vec::new(),
            Some(Value::Sequence(items)) => items
                .iter()
                .map(|item| self.scalar(name, "expose", item))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(self.invalid(name, "expose", "expected a list of ports")),
        };
        let service_networks = self.networks(name, field("networks"), networks)?;
        let (volume_mounts, anonymous_volumes) = self.volumes(name, field("volumes"), volumes)?;
        let restart = self.restart(name, field("restart"))?;
        let resources = self.resources(name, definition)?;
        let readiness = self.healthcheck(name, field("healthcheck"))?;
        let labels = self.labels(name, field("labels"))?;
        let depends_on = self.depends_on(name, field("depends_on"))?;

        let extra_networks = service_networks
            .iter()
            .map(|network| network.name.clone())
            .filter(|network| network != self.input.temps_network)
            .collect();

        let spec = WorkloadSpec {
            owner: self.input.owner,
            artifact: Artifact::oci(image),
            isolation: Isolation::Process,
            process,
            env: EnvSpec::default(),
            resources,
            volumes: volume_mounts,
            ports,
            network: NetworkSpec {
                egress: None,
                extra_networks,
            },
            probes: Probes {
                startup: None,
                readiness,
                liveness: None,
            },
            // Plane-run deployments live on the control plane (ADR-055 §8).
            placement: PlacementSpec {
                nodes: Some(vec![0]),
                labels: None,
                spread: false,
                replicas: 1,
            },
            lifetime: Lifetime::LongRunning { restart },
            security: None,
            labels,
        };

        Ok(TranslatedService {
            name: name.to_string(),
            spec,
            environment,
            depends_on,
            networks: service_networks,
            published_ports,
            exposed_ports,
            anonymous_volumes,
            built,
            platform,
            must_stay_running: None,
        })
    }

    /// The legacy assembly: each `env_file` in order (later wins), then
    /// `.env.temps` (every project variable, appended by the env override),
    /// then the inline `environment`, then project variables again for the
    /// inline keys they also define (the override's `environment`).
    fn environment(
        &self,
        name: &str,
        definition: &Mapping,
    ) -> Result<ServiceEnvironment, ComposeTranslationError> {
        let mut vars: BTreeMap<String, String> = BTreeMap::new();
        for path in self.env_file_paths(name, definition.get("env_file"))? {
            let Some((path, required)) = path else {
                continue;
            };
            match self.input.env_files.get(&path) {
                Some(EnvFileContent::Dotenv(content)) => {
                    let earlier: HashMap<String, String> = vars.clone().into_iter().collect();
                    let parsed = compose_interpolation::parse_dotenv_layered(
                        content,
                        &self.interpolation,
                        earlier,
                    )
                    .map_err(|error| {
                        self.invalid(
                            name,
                            "env_file",
                            format!("'{path}' could not be read: {error}"),
                        )
                    })?;
                    vars = parsed.into_iter().collect();
                }
                Some(EnvFileContent::ProjectEnvironment) => {
                    vars.extend(
                        self.input
                            .environment_vars
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone())),
                    );
                }
                None if !required => {}
                None => {
                    return Err(self.invalid(
                        name,
                        "env_file",
                        format!("'{path}' was not provided to the translation"),
                    ))
                }
            }
        }
        vars.extend(
            self.input
                .environment_vars
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );

        let mut inline: Vec<(String, Option<String>)> = Vec::new();
        match definition.get("environment") {
            None | Some(Value::Null) => {}
            Some(Value::Mapping(map)) => {
                for (key, value) in map {
                    let key = key.as_str().ok_or_else(|| {
                        self.invalid(name, "environment", "a key is not a string")
                    })?;
                    let value = match value {
                        Value::Null => None,
                        other => Some(self.scalar(name, &format!("environment.{key}"), other)?),
                    };
                    inline.push((key.to_string(), value));
                }
            }
            Some(Value::Sequence(items)) => {
                for item in items {
                    let entry = item.as_str().ok_or_else(|| {
                        self.invalid(name, "environment", "a list entry is not a string")
                    })?;
                    match entry.split_once('=') {
                        Some((key, value)) => inline.push((
                            key.to_string(),
                            Some(self.literal(name, &format!("environment.{key}"), value)?),
                        )),
                        None => inline.push((entry.to_string(), None)),
                    }
                }
            }
            Some(_) => {
                return Err(self.invalid(name, "environment", "expected a mapping or a list"))
            }
        }
        for (key, value) in inline {
            if let Some(project_value) = self.input.environment_vars.get(&key) {
                vars.insert(key, project_value.clone());
                continue;
            }
            match value.or_else(|| self.interpolation.get(&key).cloned()) {
                Some(value) => {
                    vars.insert(key, value);
                }
                // Unresolved and valueless: the inline key still wins over
                // the env_file, so Compose drops the env_file value too
                // (`docker compose config` shows the key with no value and
                // the container does not get it).
                None => {
                    vars.remove(&key);
                }
            }
        }
        Ok(ServiceEnvironment { vars })
    }

    /// `env_file` paths, stack-root relative (Compose resolves them against
    /// the compose file's directory), with their `required` flag. `None`
    /// marks an entry the legacy path also leaves to Compose.
    #[allow(clippy::type_complexity)]
    fn env_file_paths(
        &self,
        name: &str,
        value: Option<&Value>,
    ) -> Result<Vec<Option<(String, bool)>>, ComposeTranslationError> {
        let mut entries: Vec<(String, bool)> = Vec::new();
        match value {
            None | Some(Value::Null) => {}
            Some(Value::String(path)) => {
                entries.push((self.literal(name, "env_file", path)?, true))
            }
            Some(Value::Sequence(items)) => {
                for item in items {
                    match item {
                        Value::String(path) => {
                            entries.push((self.literal(name, "env_file", path)?, true))
                        }
                        Value::Mapping(map) => {
                            for (key, _) in map {
                                match key.as_str().unwrap_or_default() {
                                    "path" | "required" => {}
                                    "format"
                                        if map
                                            .get("format")
                                            .and_then(Value::as_str)
                                            .is_none_or(str::is_empty) => {}
                                    other => {
                                        return Err(self.unsupported(
                                            name,
                                            &format!("env_file.{other}"),
                                            "only 'path' and 'required' are translated",
                                        ))
                                    }
                                }
                            }
                            let path =
                                map.get("path").and_then(Value::as_str).ok_or_else(|| {
                                    self.invalid(name, "env_file", "a long-form entry has no path")
                                })?;
                            let required =
                                map.get("required").and_then(Value::as_bool).unwrap_or(true);
                            entries.push((self.literal(name, "env_file", path)?, required));
                        }
                        _ => {
                            return Err(self.invalid(
                                name,
                                "env_file",
                                "expected a path or a {path, required} mapping",
                            ))
                        }
                    }
                }
            }
            Some(_) => return Err(self.invalid(name, "env_file", "expected a path or a list")),
        }
        let compose_dir = std::path::Path::new(self.input.compose_file)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        entries
            .into_iter()
            .map(|(path, required)| {
                if path.starts_with('/') || path.starts_with('~') {
                    return Err(self.unsupported(
                        name,
                        "env_file",
                        format!("'{path}' is outside the stack directory"),
                    ));
                }
                let joined = if compose_dir.is_empty() {
                    path.clone()
                } else {
                    format!("{compose_dir}/{path}")
                };
                let resolved = ComposeExecutor::lexically_normalize(&joined);
                if resolved == "." || resolved.starts_with("..") {
                    return Err(self.unsupported(
                        name,
                        "env_file",
                        format!("'{path}' is outside the stack directory"),
                    ));
                }
                Ok(Some((resolved, required)))
            })
            .collect()
    }

    fn ports(
        &self,
        name: &str,
        value: Option<&Value>,
    ) -> Result<(Vec<PortSpec>, Vec<PublishedPort>), ComposeTranslationError> {
        let Some(value) = value else {
            return Ok((Vec::new(), Vec::new()));
        };
        let items = value
            .as_sequence()
            .ok_or_else(|| self.invalid(name, "ports", "expected a list"))?;
        let mut published = Vec::with_capacity(items.len());
        let mut specs: Vec<PortSpec> = Vec::new();
        for item in items {
            let (port, app_protocol) = match item {
                Value::Number(_) | Value::String(_) => {
                    let text = self.scalar(name, "ports", item)?;
                    (self.short_port(name, &text)?, None)
                }
                Value::Mapping(map) => self.long_port(name, map)?,
                _ => return Err(self.invalid(name, "ports", "expected a port string or mapping")),
            };
            let app_protocol = app_protocol.unwrap_or(match port.protocol {
                PortProtocol::Tcp => AppProtocol::Tcp,
                PortProtocol::Udp => AppProtocol::Udp,
            });
            if !specs.iter().any(|spec| {
                spec.container_port == port.container_port && spec.protocol == port.protocol
            }) {
                specs.push(PortSpec {
                    name: format!("{}-{}", port.protocol.as_str(), port.container_port),
                    container_port: port.container_port,
                    protocol: port.protocol,
                    app_protocol,
                });
            }
            published.push(port);
        }
        Ok((specs, published))
    }

    fn port_number(
        &self,
        name: &str,
        field: &str,
        text: &str,
    ) -> Result<u16, ComposeTranslationError> {
        if text.contains('-') {
            return Err(self.unsupported(
                name,
                field,
                format!("port range '{text}' cannot be expressed; list each port"),
            ));
        }
        match text.parse::<u16>() {
            Ok(port) if port > 0 => Ok(port),
            _ => Err(self.invalid(name, field, format!("'{text}' is not a port number"))),
        }
    }

    fn protocol(&self, name: &str, text: &str) -> Result<PortProtocol, ComposeTranslationError> {
        match text.to_ascii_lowercase().as_str() {
            "" | "tcp" => Ok(PortProtocol::Tcp),
            "udp" => Ok(PortProtocol::Udp),
            other => Err(self.unsupported(
                name,
                "ports",
                format!("protocol '{other}' is not supported (tcp or udp)"),
            )),
        }
    }

    /// `[[ip:]host:]container[/protocol]`, IPv6 addresses in brackets.
    fn short_port(&self, name: &str, text: &str) -> Result<PublishedPort, ComposeTranslationError> {
        let (address, protocol) = match text.rsplit_once('/') {
            Some((address, protocol)) => (address, self.protocol(name, protocol)?),
            None => (text, PortProtocol::Tcp),
        };
        let (host_ip, rest) = if let Some(stripped) = address.strip_prefix('[') {
            let (ip, rest) = stripped.split_once("]:").ok_or_else(|| {
                self.invalid(
                    name,
                    "ports",
                    format!("'{text}' has an unterminated IPv6 address"),
                )
            })?;
            (Some(ip.to_string()), rest.to_string())
        } else {
            let parts: Vec<&str> = address.split(':').collect();
            match parts.len() {
                1 | 2 => (None, address.to_string()),
                3 => (
                    Some(parts[0].to_string()),
                    format!("{}:{}", parts[1], parts[2]),
                ),
                _ => {
                    return Err(self.invalid(
                        name,
                        "ports",
                        format!("'{text}' is not [[ip:]host:]container[/protocol]"),
                    ))
                }
            }
        };
        let (host_port, container) = match rest.split_once(':') {
            Some((host, container)) => (
                (!host.is_empty())
                    .then(|| self.port_number(name, "ports", host))
                    .transpose()?,
                container.to_string(),
            ),
            None => (None, rest),
        };
        let container_port = self.port_number(name, "ports", &container)?;
        Ok(PublishedPort {
            host_ip: host_ip.filter(|ip| !ip.is_empty()),
            host_port,
            container_port,
            protocol,
        })
    }

    fn long_port(
        &self,
        name: &str,
        map: &Mapping,
    ) -> Result<(PublishedPort, Option<AppProtocol>), ComposeTranslationError> {
        let mut target = None;
        let mut host_port = None;
        let mut host_ip = None;
        let mut protocol = PortProtocol::Tcp;
        let mut app_protocol = None;
        for (key, value) in map {
            let key = key.as_str().unwrap_or_default();
            match key {
                "target" => {
                    target = Some(self.port_number(
                        name,
                        "ports.target",
                        &self.scalar(name, "ports.target", value)?,
                    )?)
                }
                "published" => {
                    let text = self.scalar(name, "ports.published", value)?;
                    host_port = (!text.is_empty())
                        .then(|| self.port_number(name, "ports.published", &text))
                        .transpose()?;
                }
                "host_ip" => {
                    host_ip =
                        Some(self.scalar(name, "ports.host_ip", value)?).filter(|ip| !ip.is_empty())
                }
                "protocol" => {
                    protocol = self.protocol(name, &self.scalar(name, "ports.protocol", value)?)?
                }
                // `ingress` is the only mode outside Swarm; `host` is the same
                // binding on a single engine.
                "mode" => {
                    let mode = self.scalar(name, "ports.mode", value)?;
                    if mode != "ingress" && mode != "host" {
                        return Err(self.unsupported(name, "ports.mode", format!("mode '{mode}'")));
                    }
                }
                "app_protocol" => {
                    let text = self.scalar(name, "ports.app_protocol", value)?;
                    app_protocol = Some(match text.to_ascii_lowercase().as_str() {
                        "http" | "https" | "http2" | "h2c" | "grpc" => AppProtocol::Http,
                        "tcp" => AppProtocol::Tcp,
                        "udp" => AppProtocol::Udp,
                        other => {
                            return Err(self.unsupported(
                                name,
                                "ports.app_protocol",
                                format!("'{other}' is not http, tcp or udp"),
                            ))
                        }
                    });
                }
                // A display name; the plane names ports itself.
                "name" => {}
                other => {
                    return Err(self.unsupported(
                        name,
                        &format!("ports.{other}"),
                        "not a translated port field",
                    ))
                }
            }
        }
        let container_port =
            target.ok_or_else(|| self.invalid(name, "ports", "a long-form port has no target"))?;
        Ok((
            PublishedPort {
                host_ip,
                host_port,
                container_port,
                protocol,
            },
            app_protocol,
        ))
    }

    /// The service's networks: the ones it lists (or `default`), plus
    /// `default` and the Temps network the legacy network override adds.
    fn networks(
        &self,
        name: &str,
        value: Option<&Value>,
        declared: &[ProjectResource],
    ) -> Result<Vec<ServiceNetwork>, ComposeTranslationError> {
        let mut listed: Vec<(String, Vec<String>)> = Vec::new();
        match value {
            None => {}
            Some(Value::Sequence(items)) => {
                for item in items {
                    let key = item.as_str().ok_or_else(|| {
                        self.invalid(name, "networks", "a list entry is not a string")
                    })?;
                    listed.push((key.to_string(), Vec::new()));
                }
            }
            Some(Value::Mapping(map)) => {
                for (key, settings) in map {
                    let key = key
                        .as_str()
                        .ok_or_else(|| self.invalid(name, "networks", "a key is not a string"))?;
                    let mut aliases = Vec::new();
                    if let Some(settings) = settings.as_mapping() {
                        for (field, value) in settings {
                            match field.as_str().unwrap_or_default() {
                                "aliases" => {
                                    for alias in value.as_sequence().into_iter().flatten() {
                                        aliases.push(self.scalar(
                                            name,
                                            "networks.aliases",
                                            alias,
                                        )?);
                                    }
                                }
                                other => {
                                    return Err(self.unsupported(
                                        name,
                                        &format!("networks.{key}.{other}"),
                                        "only network aliases are translated; fixed addresses, \
                                         priorities and driver options are not",
                                    ))
                                }
                            }
                        }
                    } else if !settings.is_null() {
                        return Err(self.invalid(
                            name,
                            "networks",
                            "a network entry is not a mapping",
                        ));
                    }
                    listed.push((key.to_string(), aliases));
                }
            }
            Some(_) => return Err(self.invalid(name, "networks", "expected a list or mapping")),
        }
        if !listed.iter().any(|(key, _)| key == "default") {
            listed.push(("default".to_string(), Vec::new()));
        }
        let mut networks = Vec::with_capacity(listed.len() + 1);
        for (key, extra_aliases) in listed {
            let resource = declared
                .iter()
                .find(|resource| resource.key == key)
                .ok_or_else(|| {
                    self.invalid(name, "networks", format!("network '{key}' is not declared"))
                })?;
            let mut aliases = vec![name.to_string()];
            aliases.extend(extra_aliases.into_iter().filter(|alias| alias != name));
            networks.push(ServiceNetwork {
                name: resource.name.clone(),
                aliases,
            });
        }
        if !networks
            .iter()
            .any(|network| network.name == self.input.temps_network)
        {
            networks.push(ServiceNetwork {
                name: self.input.temps_network.to_string(),
                aliases: vec![name.to_string()],
            });
        }
        Ok(networks)
    }

    fn volumes(
        &self,
        name: &str,
        value: Option<&Value>,
        declared: &[ProjectResource],
    ) -> Result<(Vec<VolumeMount>, Vec<String>), ComposeTranslationError> {
        let Some(value) = value else {
            return Ok((Vec::new(), Vec::new()));
        };
        let items = value
            .as_sequence()
            .ok_or_else(|| self.invalid(name, "volumes", "expected a list"))?;
        let mut mounts = Vec::new();
        let mut anonymous = Vec::new();
        let named = |source: &str,
                     target: String,
                     read_only: bool|
         -> Result<VolumeMount, ComposeTranslationError> {
            let resource = declared
                .iter()
                .find(|resource| resource.key == source)
                .ok_or_else(|| {
                    self.invalid(
                        name,
                        "volumes",
                        format!("volume '{source}' is not declared"),
                    )
                })?;
            Ok(VolumeMount {
                volume: resource.name.clone(),
                mount_path: target,
                read_only,
                source: None,
            })
        };
        let bind_refusal = |source: &str| {
            self.unsupported(
                name,
                "volumes",
                format!(
                    "'{source}' is a host bind mount; plane workloads mount only Temps volumes"
                ),
            )
        };
        for item in items {
            match item {
                Value::String(text) => {
                    let text = self.literal(name, "volumes", text)?;
                    let parts: Vec<&str> = text.split(':').collect();
                    match parts.as_slice() {
                        [target] => anonymous.push((*target).to_string()),
                        [source, target, rest @ ..] => {
                            let read_only = match rest {
                                [] => false,
                                [mode] => match *mode {
                                    "ro" => true,
                                    "rw" => false,
                                    other => {
                                        return Err(self.unsupported(
                                            name,
                                            "volumes",
                                            format!("mount option '{other}' (only ro or rw)"),
                                        ))
                                    }
                                },
                                _ => {
                                    return Err(self.invalid(name, "volumes", format!("'{text}'")))
                                }
                            };
                            if source.starts_with(['.', '/', '~']) {
                                return Err(bind_refusal(source));
                            }
                            mounts.push(named(source, (*target).to_string(), read_only)?);
                        }
                        [] => return Err(self.invalid(name, "volumes", "an empty entry")),
                    }
                }
                Value::Mapping(map) => {
                    let mut kind = "volume".to_string();
                    let mut source = None;
                    let mut target = None;
                    let mut read_only = false;
                    for (key, value) in map {
                        match key.as_str().unwrap_or_default() {
                            "type" => kind = self.scalar(name, "volumes.type", value)?,
                            "source" => {
                                source = Some(self.scalar(name, "volumes.source", value)?)
                            }
                            "target" => {
                                target = Some(self.scalar(name, "volumes.target", value)?)
                            }
                            "read_only" => read_only = value.as_bool().unwrap_or(false),
                            other => {
                                return Err(self.unsupported(
                                    name,
                                    &format!("volumes.{other}"),
                                    "only type, source, target and read_only are translated",
                                ))
                            }
                        }
                    }
                    let target = target.ok_or_else(|| {
                        self.invalid(name, "volumes", "a long-form volume has no target")
                    })?;
                    match (kind.as_str(), source.filter(|source| !source.is_empty())) {
                        ("volume", None) => anonymous.push(target),
                        ("volume", Some(source)) => mounts.push(named(&source, target, read_only)?),
                        ("bind", source) => {
                            return Err(bind_refusal(source.as_deref().unwrap_or("?")))
                        }
                        (other, _) => {
                            return Err(self.unsupported(
                                name,
                                "volumes.type",
                                format!("'{other}' mounts are not translated"),
                            ))
                        }
                    }
                }
                _ => return Err(self.invalid(name, "volumes", "expected a string or mapping")),
            }
        }
        Ok((mounts, anonymous))
    }

    fn restart(
        &self,
        name: &str,
        value: Option<&Value>,
    ) -> Result<RestartPolicy, ComposeTranslationError> {
        let Some(value) = value else {
            // Compose's default.
            return Ok(RestartPolicy::Never);
        };
        let text = self.scalar(name, "restart", value)?;
        match text.as_str() {
            "no" | "false" => Ok(RestartPolicy::Never),
            "always" => Ok(RestartPolicy::Always),
            "on-failure" => Ok(RestartPolicy::OnFailure),
            "unless-stopped" => Ok(RestartPolicy::UnlessStopped),
            other if other.starts_with("on-failure:") => Err(self.unsupported(
                name,
                "restart",
                format!("'{other}': a retry count cannot be expressed; use on-failure"),
            )),
            other => Err(self.invalid(
                name,
                "restart",
                format!("'{other}' is not a restart policy"),
            )),
        }
    }

    fn resources(
        &self,
        name: &str,
        definition: &Mapping,
    ) -> Result<Resources, ComposeTranslationError> {
        let mut limits = ResourceAmounts::default();
        let mut requests = ResourceAmounts::default();
        let set = |slot: &mut Option<u32>,
                   value: u32,
                   field: &str|
         -> Result<(), ComposeTranslationError> {
            match *slot {
                Some(existing) if existing != value => Err(self.invalid(
                    name,
                    field,
                    format!(
                        "conflicts with another setting of the same limit ({existing} vs {value})"
                    ),
                )),
                _ => {
                    *slot = Some(value);
                    Ok(())
                }
            }
        };

        if let Some(scale) = definition.get("scale").filter(|value| !value.is_null()) {
            if scale.as_u64() != Some(1) {
                return Err(self.unsupported(name, "scale", "Compose services run one replica"));
            }
        }
        if let Some(deploy) = definition.get("deploy").filter(|value| !value.is_null()) {
            let deploy = deploy
                .as_mapping()
                .ok_or_else(|| self.invalid(name, "deploy", "expected a mapping"))?;
            for (key, value) in deploy {
                match key.as_str().unwrap_or_default() {
                    "replicas" => {
                        if value.as_u64() != Some(1) {
                            return Err(self.unsupported(
                                name,
                                "deploy.replicas",
                                "Compose services run one replica",
                            ));
                        }
                    }
                    "mode" => {
                        if value.as_str() != Some("replicated") {
                            return Err(self.unsupported(name, "deploy.mode", "only 'replicated'"));
                        }
                    }
                    "resources" => {
                        let resources = value.as_mapping().ok_or_else(|| {
                            self.invalid(name, "deploy.resources", "expected a mapping")
                        })?;
                        for (section, amounts) in resources {
                            let section = section.as_str().unwrap_or_default();
                            let target = match section {
                                "limits" => &mut limits,
                                "reservations" => &mut requests,
                                other => {
                                    return Err(self.unsupported(
                                        name,
                                        &format!("deploy.resources.{other}"),
                                        "only limits and reservations",
                                    ))
                                }
                            };
                            let Some(amounts) = amounts.as_mapping() else {
                                continue;
                            };
                            for (key, value) in amounts {
                                let key = key.as_str().unwrap_or_default();
                                let field = format!("deploy.resources.{section}.{key}");
                                match key {
                                    "cpus" => set(
                                        &mut target.cpu_millis,
                                        self.cpu_millis(name, &field, value)?,
                                        &field,
                                    )?,
                                    "memory" => set(
                                        &mut target.memory_mb,
                                        self.memory_mb(name, &field, value)?,
                                        &field,
                                    )?,
                                    "pids" if section == "limits" => set(
                                        &mut target.pids,
                                        self.count(name, &field, value)?,
                                        &field,
                                    )?,
                                    _ => {
                                        return Err(self.unsupported(
                                            name,
                                            &field,
                                            "only cpus, memory and pids are translated",
                                        ))
                                    }
                                }
                            }
                        }
                    }
                    other => {
                        return Err(self.unsupported(
                            name,
                            &format!("deploy.{other}"),
                            "only deploy.resources, deploy.replicas (1) and deploy.mode are \
                             translated",
                        ))
                    }
                }
            }
        }
        if let Some(value) = definition.get("mem_limit").filter(|value| !value.is_null()) {
            set(
                &mut limits.memory_mb,
                self.memory_mb(name, "mem_limit", value)?,
                "mem_limit",
            )?;
        }
        if let Some(value) = definition
            .get("mem_reservation")
            .filter(|value| !value.is_null())
        {
            set(
                &mut requests.memory_mb,
                self.memory_mb(name, "mem_reservation", value)?,
                "mem_reservation",
            )?;
        }
        if let Some(value) = definition.get("cpus").filter(|value| !value.is_null()) {
            set(
                &mut limits.cpu_millis,
                self.cpu_millis(name, "cpus", value)?,
                "cpus",
            )?;
        }
        if let Some(value) = definition
            .get("pids_limit")
            .filter(|value| !value.is_null())
        {
            set(
                &mut limits.pids,
                self.count(name, "pids_limit", value)?,
                "pids_limit",
            )?;
        }
        Ok(Resources {
            requests: (!requests.is_empty()).then_some(requests),
            limits: (!limits.is_empty()).then_some(limits),
        })
    }

    fn cpu_millis(
        &self,
        name: &str,
        field: &str,
        value: &Value,
    ) -> Result<u32, ComposeTranslationError> {
        let text = self.scalar(name, field, value)?;
        let cpus: f64 = text
            .trim()
            .parse()
            .map_err(|_| self.invalid(name, field, format!("'{text}' is not a CPU count")))?;
        let millis = cpus * 1000.0;
        if !(millis >= 1.0 && millis <= f64::from(u32::MAX))
            || (millis - millis.round()).abs() > 1e-6
        {
            return Err(self.invalid(
                name,
                field,
                format!("'{text}' is not a positive CPU count in thousandths of a core"),
            ));
        }
        Ok(millis.round() as u32)
    }

    fn memory_mb(
        &self,
        name: &str,
        field: &str,
        value: &Value,
    ) -> Result<u32, ComposeTranslationError> {
        const MIB: u64 = 1024 * 1024;
        let bytes = ComposeExecutor::parse_compose_byte_size(value)
            .ok_or_else(|| self.invalid(name, field, "expected a size such as 512m or 2g"))?;
        if bytes % MIB != 0 {
            return Err(self.invalid(
                name,
                field,
                format!("{bytes} bytes is not a whole number of MiB"),
            ));
        }
        u32::try_from(bytes / MIB).map_err(|_| self.invalid(name, field, "too large"))
    }

    fn count(
        &self,
        name: &str,
        field: &str,
        value: &Value,
    ) -> Result<u32, ComposeTranslationError> {
        value
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .filter(|count| *count > 0)
            .ok_or_else(|| self.invalid(name, field, "expected a positive integer"))
    }

    /// A Compose healthcheck as a readiness probe (an `Exec` probe; the
    /// command the Docker healthcheck would run).
    fn healthcheck(
        &self,
        name: &str,
        value: Option<&Value>,
    ) -> Result<Option<Probe>, ComposeTranslationError> {
        let Some(value) = value else { return Ok(None) };
        let map = value
            .as_mapping()
            .ok_or_else(|| self.invalid(name, "healthcheck", "expected a mapping"))?;
        if map.get("disable").and_then(Value::as_bool) == Some(true) {
            return Ok(None);
        }
        let mut probe: Option<Probe> = None;
        let mut interval = None;
        let mut timeout = None;
        let mut retries = None;
        let mut start_period = None;
        for (key, value) in map {
            match key.as_str().unwrap_or_default() {
                "test" => {
                    // The legacy security override pins `localhost` in HTTP
                    // probes to 127.0.0.1; the translation does the same.
                    let mut test = value.clone();
                    ComposeExecutor::rewrite_healthcheck_http_localhost(&mut test);
                    probe = self
                        .healthcheck_test(name, &test)?
                        .map(|command| Probe::new(ProbeKind::Exec { command }));
                    if probe.is_none() {
                        return Ok(None);
                    }
                }
                "interval" => {
                    interval = Some(self.duration_secs(name, "healthcheck.interval", value)?)
                }
                "timeout" => {
                    timeout = Some(self.duration_secs(name, "healthcheck.timeout", value)?)
                }
                "start_period" => {
                    start_period =
                        Some(self.duration_secs(name, "healthcheck.start_period", value)?)
                }
                "retries" => retries = Some(self.count(name, "healthcheck.retries", value)?),
                "disable" => {}
                other => {
                    return Err(self.unsupported(
                        name,
                        &format!("healthcheck.{other}"),
                        "only test, interval, timeout, retries and start_period are translated",
                    ))
                }
            }
        }
        let Some(mut probe) = probe else {
            // No test: the image's own HEALTHCHECK with these timings.
            return Err(self.unsupported(
                name,
                "healthcheck",
                "a healthcheck without a test reuses the image's HEALTHCHECK, which a probe \
                 cannot name",
            ));
        };
        // Docker's defaults: 30s interval and timeout, 3 retries, 0s start.
        probe.period_secs = interval.unwrap_or(30);
        probe.timeout_secs = timeout.unwrap_or(30);
        probe.failure_threshold = retries.unwrap_or(3);
        probe.initial_delay_secs = start_period.unwrap_or(0);
        Ok(Some(probe))
    }

    /// `["CMD", ...]`, `["CMD-SHELL", "..."]`, a shell string, or `NONE`.
    fn healthcheck_test(
        &self,
        name: &str,
        value: &Value,
    ) -> Result<Option<Vec<String>>, ComposeTranslationError> {
        let field = "healthcheck.test";
        match value {
            Value::String(text) => {
                let text = self.literal(name, field, text)?;
                Ok(Some(vec!["/bin/sh".to_string(), "-c".to_string(), text]))
            }
            Value::Sequence(items) => {
                let items: Vec<String> = items
                    .iter()
                    .map(|item| self.scalar(name, field, item))
                    .collect::<Result<_, _>>()?;
                match items.split_first() {
                    Some((kind, rest)) if kind == "CMD" && !rest.is_empty() => {
                        Ok(Some(rest.to_vec()))
                    }
                    Some((kind, rest)) if kind == "CMD-SHELL" && rest.len() == 1 => Ok(Some(vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        rest[0].clone(),
                    ])),
                    Some((kind, _)) if kind == "NONE" => Ok(None),
                    _ => Err(self.invalid(name, field, "expected CMD, CMD-SHELL or NONE")),
                }
            }
            _ => Err(self.invalid(name, field, "expected a string or list")),
        }
    }

    /// A Go duration (`1m30s`) in whole seconds.
    fn duration_secs(
        &self,
        name: &str,
        field: &str,
        value: &Value,
    ) -> Result<u32, ComposeTranslationError> {
        let text = self.scalar(name, field, value)?;
        parse_go_duration_nanos(&text)
            .and_then(|nanos| {
                (nanos % 1_000_000_000 == 0)
                    .then(|| u32::try_from(nanos / 1_000_000_000).ok())
                    .flatten()
            })
            .ok_or_else(|| {
                self.invalid(
                    name,
                    field,
                    format!("'{text}' is not a duration in whole seconds (such as 30s or 1m30s)"),
                )
            })
    }

    fn labels(
        &self,
        name: &str,
        value: Option<&Value>,
    ) -> Result<BTreeMap<String, String>, ComposeTranslationError> {
        let mut labels = BTreeMap::new();
        match value {
            None => {}
            Some(Value::Mapping(map)) => {
                for (key, value) in map {
                    let key = key
                        .as_str()
                        .ok_or_else(|| self.invalid(name, "labels", "a key is not a string"))?;
                    let value = match value {
                        Value::Null => String::new(),
                        other => self.scalar(name, "labels", other)?,
                    };
                    labels.insert(key.to_string(), value);
                }
            }
            Some(Value::Sequence(items)) => {
                for item in items {
                    let entry = self.scalar(name, "labels", item)?;
                    let (key, value) = entry.split_once('=').unwrap_or((entry.as_str(), ""));
                    labels.insert(key.to_string(), value.to_string());
                }
            }
            Some(_) => return Err(self.invalid(name, "labels", "expected a mapping or a list")),
        }
        // The legacy labels override: Temps' labels win, plus the service.
        if !self.input.labels.is_empty() {
            labels.extend(
                self.input
                    .labels
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            labels.insert(SERVICE_LABEL.to_string(), name.to_string());
        }
        Ok(labels)
    }

    fn depends_on(
        &self,
        name: &str,
        value: Option<&Value>,
    ) -> Result<Vec<ServiceDependency>, ComposeTranslationError> {
        let mut dependencies = Vec::new();
        match value {
            None => {}
            Some(Value::Sequence(items)) => {
                for item in items {
                    let service = item.as_str().ok_or_else(|| {
                        self.invalid(name, "depends_on", "a list entry is not a string")
                    })?;
                    dependencies.push(ServiceDependency {
                        service: service.to_string(),
                        condition: DependencyCondition::Started,
                        required: true,
                    });
                }
            }
            Some(Value::Mapping(map)) => {
                for (service, settings) in map {
                    let service = service
                        .as_str()
                        .ok_or_else(|| self.invalid(name, "depends_on", "a key is not a string"))?;
                    let mut condition = DependencyCondition::Started;
                    let mut required = true;
                    for (key, value) in settings.as_mapping().into_iter().flatten() {
                        let field =
                            format!("depends_on.{service}.{}", key.as_str().unwrap_or_default());
                        match key.as_str().unwrap_or_default() {
                            "condition" => {
                                condition = match value.as_str() {
                                    Some("service_started") => DependencyCondition::Started,
                                    Some("service_healthy") => DependencyCondition::Healthy,
                                    Some("service_completed_successfully") => {
                                        DependencyCondition::CompletedSuccessfully
                                    }
                                    _ => {
                                        return Err(self.invalid(name, &field, "unknown condition"))
                                    }
                                }
                            }
                            "required" => required = value.as_bool().unwrap_or(true),
                            "restart" if value.as_bool() != Some(true) => {}
                            _ => {
                                return Err(self.unsupported(
                                    name,
                                    &field,
                                    "restarting a service when its dependency restarts is not \
                                     translated",
                                ))
                            }
                        }
                    }
                    dependencies.push(ServiceDependency {
                        service: service.to_string(),
                        condition,
                        required,
                    });
                }
            }
            Some(_) => return Err(self.invalid(name, "depends_on", "expected a list or mapping")),
        }
        Ok(dependencies)
    }
}

fn unsupported_field_reason(field: &str) -> &'static str {
    match field {
        "secrets" | "configs" => {
            "file-backed Compose secrets and configs are not translated; Temps delivers project \
             secrets under /run/secrets"
        }
        "network_mode" | "pid" | "ipc" | "uts" | "userns_mode" | "cgroup" => {
            "host and shared namespaces are not available to plane workloads"
        }
        "extends" => "extends must be resolved by `docker compose config` before translation",
        "stop_signal" | "stop_grace_period" => {
            "the plane stops instances with its own signal and grace period"
        }
        "hostname" | "domainname" | "extra_hosts" | "dns" | "dns_search" | "dns_opt"
        | "mac_address" => "per-container DNS and host settings are not translated",
        "logging" => "the plane applies its own log rotation",
        "shm_size" | "read_only" | "tty" | "stdin_open" | "stop_signal_timeout" => {
            "this container setting has no field in a workload spec"
        }
        "profiles" => "profile selection is not translated; exclude the service instead",
        _ => "this Compose field has no workload equivalent",
    }
}

/// Order services so each follows its dependencies (Kahn's algorithm, ties
/// broken by name so the order is stable).
fn order_by_dependencies(
    project: &str,
    services: Vec<TranslatedService>,
) -> Result<Vec<TranslatedService>, ComposeTranslationError> {
    let names: BTreeSet<String> = services.iter().map(|s| s.name.clone()).collect();
    let mut pending: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for service in &services {
        let mut deps = BTreeSet::new();
        for dependency in &service.depends_on {
            if names.contains(&dependency.service) {
                deps.insert(dependency.service.clone());
            } else if dependency.required {
                return Err(ComposeTranslationError::UnknownDependency {
                    project: project.to_string(),
                    service: service.name.clone(),
                    dependency: dependency.service.clone(),
                });
            }
        }
        pending.insert(service.name.clone(), deps);
    }
    let mut by_name: HashMap<String, TranslatedService> =
        services.into_iter().map(|s| (s.name.clone(), s)).collect();
    let mut ordered = Vec::with_capacity(by_name.len());
    while !pending.is_empty() {
        let ready: Vec<String> = pending
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(name, _)| name.clone())
            .collect();
        if ready.is_empty() {
            return Err(ComposeTranslationError::DependencyCycle {
                project: project.to_string(),
                services: pending.keys().cloned().collect(),
            });
        }
        for name in ready {
            pending.remove(&name);
            for deps in pending.values_mut() {
                deps.remove(&name);
            }
            if let Some(service) = by_name.remove(&name) {
                ordered.push(service);
            }
        }
    }
    Ok(ordered)
}

/// Go's `time.ParseDuration`, in nanoseconds; `None` when malformed or
/// negative.
fn parse_go_duration_nanos(text: &str) -> Option<u64> {
    let text = text.trim();
    if text == "0" {
        return Some(0);
    }
    let mut total: f64 = 0.0;
    let mut rest = text;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let number_len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if number_len == 0 {
            return None;
        }
        let number: f64 = rest[..number_len].parse().ok()?;
        rest = &rest[number_len..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        rest = &rest[unit_len..];
        let scale = match unit {
            "ns" => 1.0,
            "us" | "µs" => 1e3,
            "ms" => 1e6,
            "s" => 1e9,
            "m" => 60e9,
            "h" => 3600e9,
            _ => return None,
        };
        total += number * scale;
    }
    (total.is_finite() && total >= 0.0 && total < u64::MAX as f64).then(|| total.round() as u64)
}

// ── Shadow comparison (ADR-055 §14, `workload_mode = shadow`) ───────────────

/// What one service's container looks like after the legacy CLI path
/// started it. Only names, never environment values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedService {
    pub service: String,
    /// The image reference the container was created from.
    pub image: String,
    /// The container's environment variable names (the image's included).
    pub env_names: BTreeSet<String>,
    /// Docker's restart policy name (`no`, `always`, `on-failure`,
    /// `unless-stopped`); `None` when Docker reports none.
    pub restart: Option<String>,
    /// Docker networks the container is attached to.
    pub networks: BTreeSet<String>,
    /// Container ports published on the host.
    pub published_ports: BTreeSet<u16>,
}

/// One field where the translation and the legacy container disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowDifference {
    pub service: String,
    pub field: &'static str,
    pub legacy: String,
    pub translated: String,
}

impl std::fmt::Display for ShadowDifference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "service '{}': {} differs (legacy: {}; translated: {})",
            self.service, self.field, self.legacy, self.translated
        )
    }
}

/// Docker's name for a restart policy.
pub fn docker_restart_name(restart: RestartPolicy) -> &'static str {
    match restart {
        RestartPolicy::Never => "no",
        RestartPolicy::Always => "always",
        RestartPolicy::OnFailure => "on-failure",
        RestartPolicy::UnlessStopped => "unless-stopped",
    }
}

fn joined<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<&str> = items.into_iter().collect();
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

/// Compare the translation with what the legacy path runs. Environment
/// variables are compared by name only, and only in one direction: a
/// container also carries its image's `ENV`, which the translation does not
/// know, so only a translated name the container lacks is a difference.
pub fn shadow_diff(
    translation: &ComposeTranslation,
    observed: &[ObservedService],
) -> Vec<ShadowDifference> {
    let mut differences = Vec::new();
    let observed_by_name: BTreeMap<&str, &ObservedService> = observed
        .iter()
        .map(|service| (service.service.as_str(), service))
        .collect();
    for service in &translation.services {
        let Some(container) = observed_by_name.get(service.name.as_str()) else {
            differences.push(ShadowDifference {
                service: service.name.clone(),
                field: "service",
                legacy: "no container".to_string(),
                translated: "one workload".to_string(),
            });
            continue;
        };
        if !service.built {
            if let Artifact::OciImage { reference, .. } = &service.spec.artifact {
                if reference != &container.image {
                    differences.push(ShadowDifference {
                        service: service.name.clone(),
                        field: "image",
                        legacy: container.image.clone(),
                        translated: reference.clone(),
                    });
                }
            }
        }
        let missing: Vec<&str> = service
            .environment
            .names()
            .filter(|name| !container.env_names.contains(*name))
            .collect();
        if !missing.is_empty() {
            differences.push(ShadowDifference {
                service: service.name.clone(),
                field: "environment",
                legacy: format!("lacks {}", joined(missing.iter().copied())),
                translated: format!("{} variable(s)", service.environment.len()),
            });
        }
        let translated_restart = match &service.spec.lifetime {
            Lifetime::LongRunning { restart } => Some(docker_restart_name(*restart)),
            _ => None,
        };
        let legacy_restart = container.restart.as_deref().filter(|name| !name.is_empty());
        let same_restart = match (legacy_restart, translated_restart) {
            (None, Some("no")) | (Some("no"), None) | (None, None) => true,
            (legacy, translated) => legacy == translated,
        };
        if !same_restart {
            differences.push(ShadowDifference {
                service: service.name.clone(),
                field: "restart",
                legacy: legacy_restart.unwrap_or("(none)").to_string(),
                translated: translated_restart.unwrap_or("(none)").to_string(),
            });
        }
        let translated_networks: BTreeSet<&str> =
            service.networks.iter().map(|n| n.name.as_str()).collect();
        let legacy_networks: BTreeSet<&str> =
            container.networks.iter().map(String::as_str).collect();
        if translated_networks != legacy_networks {
            differences.push(ShadowDifference {
                service: service.name.clone(),
                field: "networks",
                legacy: joined(legacy_networks.iter().copied()),
                translated: joined(translated_networks.iter().copied()),
            });
        }
        let translated_ports: BTreeSet<u16> = service
            .published_ports
            .iter()
            .map(|port| port.container_port)
            .collect();
        if translated_ports != container.published_ports {
            let render = |ports: &BTreeSet<u16>| {
                if ports.is_empty() {
                    "(none)".to_string()
                } else {
                    ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            };
            differences.push(ShadowDifference {
                service: service.name.clone(),
                field: "published ports",
                legacy: render(&container.published_ports),
                translated: render(&translated_ports),
            });
        }
    }
    for container in observed {
        if translation.service(&container.service).is_none() {
            differences.push(ShadowDifference {
                service: container.service.clone(),
                field: "service",
                legacy: "a running container".to_string(),
                translated: "no workload".to_string(),
            });
        }
    }
    differences
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::WorkloadKind;

    const PROJECT: &str = "temps-7-9";
    const NETWORK: &str = "temps-app-network";

    struct Case<'a> {
        compose: &'a str,
        compose_file: &'a str,
        env: HashMap<String, String>,
        repository_env: Option<&'a str>,
        env_files: BTreeMap<String, EnvFileContent>,
        labels: HashMap<String, String>,
        routed: Vec<String>,
    }

    impl<'a> Case<'a> {
        fn new(compose: &'a str) -> Self {
            Self {
                compose,
                compose_file: "docker-compose.yml",
                env: HashMap::new(),
                repository_env: None,
                env_files: BTreeMap::new(),
                labels: HashMap::new(),
                routed: Vec::new(),
            }
        }

        fn translate(&self) -> Result<ComposeTranslation, ComposeTranslationError> {
            translate(&ComposeTranslationInput {
                project_name: PROJECT,
                compose_file: self.compose_file,
                content: self.compose,
                override_content: None,
                environment_vars: &self.env,
                repository_env: self.repository_env,
                env_files: &self.env_files,
                labels: &self.labels,
                routed_services: &self.routed,
                owner: WorkloadRef::new(WorkloadKind::Deployment, 42),
                temps_network: NETWORK,
            })
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn sample_fixture() -> (String, String) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sample-compose-app/deploy");
        (
            std::fs::read_to_string(dir.join("compose.yaml")).unwrap(),
            std::fs::read_to_string(dir.join("app.env")).unwrap(),
        )
    }

    #[test]
    fn sample_compose_app_translates_in_dependency_order() {
        let (compose, app_env) = sample_fixture();
        // The checked model has `${GREETING:?}` resolved; mirror that.
        let compose = compose.replace("${GREETING:?GREETING must be set}", "from-temps");
        let mut case = Case::new(&compose);
        case.compose_file = "deploy/compose.yaml";
        case.env = vars(&[("GREETING", "from-temps")]);
        case.env_files
            .insert("deploy/app.env".into(), EnvFileContent::Dotenv(app_env));
        case.routed = vec!["web".into()];
        let translation = case.translate().unwrap();

        assert_eq!(translation.service_names(), ["migrate", "web"]);
        let web = translation.service("web").unwrap();
        assert_eq!(web.spec.artifact, Artifact::oci("busybox:1.36"),);
        assert_eq!(web.spec.process.command.as_ref().unwrap()[0], "sh");
        assert_eq!(web.environment.vars()["GREETING"], "from-temps");
        assert_eq!(web.environment.vars()["APP_NAME"], "sample-compose-app");
        assert_eq!(web.anonymous_volumes, ["/app/node_modules"]);
        assert_eq!(
            web.published_ports,
            [PublishedPort {
                host_ip: Some("127.0.0.1".into()),
                host_port: None,
                container_port: 8080,
                protocol: PortProtocol::Tcp,
            }]
        );
        assert_eq!(web.spec.ports[0].container_port, 8080);
        assert_eq!(
            web.depends_on,
            [ServiceDependency {
                service: "migrate".into(),
                condition: DependencyCondition::CompletedSuccessfully,
                required: true,
            }]
        );
        assert!(web.must_stay_running.is_some());
        let migrate = translation.service("migrate").unwrap();
        assert_eq!(migrate.must_stay_running, None);
        assert_eq!(
            migrate.spec.lifetime,
            Lifetime::LongRunning {
                restart: RestartPolicy::Never
            }
        );
        // Spec never stores values.
        assert!(web.spec.env.vars.is_empty());
        // Both join the project default network and the Temps network.
        assert_eq!(
            web.networks
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            [format!("{PROJECT}_default").as_str(), NETWORK]
        );
        assert_eq!(
            web.spec.network.extra_networks,
            [format!("{PROJECT}_default")]
        );
        assert_eq!(web.networks[0].aliases, ["web"]);
    }

    /// What the legacy container for each translated service would report
    /// when it matches the translation exactly.
    fn observed_from(translation: &ComposeTranslation) -> Vec<ObservedService> {
        translation
            .services
            .iter()
            .map(|service| ObservedService {
                service: service.name.clone(),
                image: match &service.spec.artifact {
                    Artifact::OciImage { reference, .. } => reference.clone(),
                    other => format!("{other:?}"),
                },
                env_names: service
                    .environment
                    .names()
                    .map(str::to_string)
                    // The image's own ENV is on the container too.
                    .chain(std::iter::once("PATH".to_string()))
                    .collect(),
                restart: match &service.spec.lifetime {
                    Lifetime::LongRunning { restart } => {
                        Some(docker_restart_name(*restart).to_string())
                    }
                    _ => None,
                },
                networks: service.networks.iter().map(|n| n.name.clone()).collect(),
                published_ports: service
                    .published_ports
                    .iter()
                    .map(|p| p.container_port)
                    .collect(),
            })
            .collect()
    }

    fn sample_translation() -> ComposeTranslation {
        let (compose, app_env) = sample_fixture();
        let compose = compose.replace("${GREETING:?GREETING must be set}", "from-temps");
        let mut case = Case::new(&compose);
        case.compose_file = "deploy/compose.yaml";
        case.env = vars(&[("GREETING", "from-temps")]);
        case.env_files
            .insert("deploy/app.env".into(), EnvFileContent::Dotenv(app_env));
        case.routed = vec!["web".into()];
        case.translate().unwrap()
    }

    #[test]
    fn shadow_diff_is_empty_when_legacy_runs_what_was_translated() {
        let translation = sample_translation();
        let observed = observed_from(&translation);
        assert_eq!(shadow_diff(&translation, &observed), Vec::new());
        // Docker reporting no policy is the same as "no".
        let mut silent = observed.clone();
        for service in &mut silent {
            if service.restart.as_deref() == Some("no") {
                service.restart = None;
            }
        }
        assert_eq!(shadow_diff(&translation, &silent), Vec::new());
    }

    #[test]
    fn shadow_diff_names_each_difference_without_values() {
        let translation = sample_translation();
        type Case = (
            &'static str,
            Box<dyn Fn(&mut ObservedService)>,
            &'static str,
        );
        let table: Vec<Case> = vec![
            (
                "image",
                Box::new(|s: &mut ObservedService| s.image = "busybox:1.35".into()),
                "image",
            ),
            (
                "env",
                Box::new(|s: &mut ObservedService| {
                    s.env_names.remove("GREETING");
                }),
                "environment",
            ),
            (
                "restart",
                Box::new(|s: &mut ObservedService| s.restart = Some("always".into())),
                "restart",
            ),
            (
                "networks",
                Box::new(|s: &mut ObservedService| {
                    s.networks.insert("bridge".into());
                }),
                "networks",
            ),
            (
                "ports",
                Box::new(|s: &mut ObservedService| {
                    s.published_ports.insert(9999);
                }),
                "published ports",
            ),
        ];
        for (case, mutate, field) in table {
            let mut observed = observed_from(&translation);
            let web = observed.iter_mut().find(|s| s.service == "web").unwrap();
            mutate(web);
            let differences = shadow_diff(&translation, &observed);
            assert_eq!(differences.len(), 1, "{case}: {differences:?}");
            assert_eq!(differences[0].service, "web", "{case}");
            assert_eq!(differences[0].field, field, "{case}");
            let rendered = differences[0].to_string();
            assert!(!rendered.contains("from-temps"), "{case}: {rendered}");
        }

        let mut missing = observed_from(&translation);
        missing.retain(|s| s.service != "migrate");
        missing.push(ObservedService {
            service: "sidecar".into(),
            ..Default::default()
        });
        let differences = shadow_diff(&translation, &missing);
        let fields: Vec<(&str, &str)> = differences
            .iter()
            .map(|d| (d.service.as_str(), d.field))
            .collect();
        assert_eq!(fields, [("migrate", "service"), ("sidecar", "service")]);
    }

    #[test]
    fn environment_debug_never_shows_values() {
        let case = {
            let mut case = Case::new(
                "services:\n  app:\n    image: a\n    environment:\n      TOKEN: hunter2\n",
            );
            case.env = vars(&[("OTHER", "s3cr3t")]);
            case
        };
        let translation = case.translate().unwrap();
        let rendered = format!("{translation:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(rendered.contains("TOKEN"));
    }

    /// Env precedence the legacy overrides produce: env_file < .env.temps <
    /// inline < project variables for inline keys.
    #[test]
    fn environment_precedence_matches_the_legacy_overrides() {
        let compose = "services:\n  app:\n    image: a\n    env_file: [one.env, two.env]\n    environment:\n      INLINE: inline\n      SHARED: inline\n      FROM_REPO_ENV:\n      UNRESOLVED:\n      - ignored\n";
        // The trailing `- ignored` makes the YAML a sequence under a key; use a valid doc:
        let compose = compose.replace("      - ignored\n", "");
        let mut case = Case::new(&compose);
        case.env = vars(&[("SHARED", "project"), ("PROJECT_ONLY", "p")]);
        case.repository_env = Some("FROM_REPO_ENV=repo\n");
        case.env_files.insert(
            "one.env".into(),
            EnvFileContent::Dotenv("FILE=one\nSHARED=file\nUNRESOLVED=from-file\n".into()),
        );
        case.env_files.insert(
            "two.env".into(),
            EnvFileContent::Dotenv("FILE=two\n".into()),
        );
        let env = case.translate().unwrap().services[0].environment.clone();
        let env = env.vars();
        assert_eq!(env["FILE"], "two");
        assert_eq!(env["INLINE"], "inline");
        assert_eq!(env["SHARED"], "project");
        assert_eq!(env["PROJECT_ONLY"], "p");
        assert_eq!(env["FROM_REPO_ENV"], "repo");
        // A valueless inline key Compose cannot resolve masks the env_file
        // value (the legacy-parity test checks this against `docker compose
        // config`).
        assert!(!env.contains_key("UNRESOLVED"), "{env:?}");
    }

    #[test]
    fn literal_dollars_are_unescaped_and_unresolved_references_refused() {
        let ok = Case::new("services:\n  app:\n    image: a\n    command: echo $$HOME\n")
            .translate()
            .unwrap();
        assert_eq!(
            ok.services[0].spec.process.command,
            Some(vec!["echo".to_string(), "$HOME".to_string()])
        );
        let err = Case::new("services:\n  app:\n    image: a\n    command: echo ${HOME}\n")
            .translate()
            .unwrap_err();
        assert!(
            matches!(
                err,
                ComposeTranslationError::InvalidValue { ref service, ref field, .. }
                    if service == "app" && field == "command"
            ),
            "{err}"
        );
    }

    /// Every refusal names the service and the field.
    #[test]
    fn untranslatable_fields_are_refused_with_service_and_field() {
        let cases: &[(&str, &str)] = &[
            ("services:\n  app:\n    image: a\n    volumes:\n      - ./data:/data\n", "volumes"),
            ("services:\n  app:\n    image: a\n    volumes:\n      - type: tmpfs\n        target: /t\n", "volumes.type"),
            ("services:\n  app:\n    image: a\n    stop_grace_period: 1m\n", "stop_grace_period"),
            ("services:\n  app:\n    image: a\n    hostname: h\n", "hostname"),
            ("services:\n  app:\n    image: a\n    init: false\n", "init"),
            ("services:\n  app:\n    image: a\n    restart: on-failure:3\n", "restart"),
            ("services:\n  app:\n    image: a\n    ports:\n      - 127.0.0.1:8000-8001:80-81\n", "ports"),
            ("services:\n  app:\n    image: a\n    deploy:\n      replicas: 2\n", "deploy.replicas"),
            ("services:\n  app:\n    image: a\n    deploy:\n      update_config: {}\n", "deploy.update_config"),
            ("services:\n  app:\n    image: a\n    networks:\n      default:\n        ipv4_address: 10.0.0.2\n", "networks.default.ipv4_address"),
            ("services:\n  app:\n    image: a\n    healthcheck:\n      test: [CMD, true]\n      start_interval: 1s\n", "healthcheck.start_interval"),
            ("services:\n  app:\n    image: a\n    healthcheck:\n      test: [CMD, true]\n      interval: 500ms\n", "healthcheck.interval"),
            ("services:\n  app:\n    image: a\n    mem_limit: 1.5g\n", "mem_limit"),
            ("services:\n  app:\n    image: a\n    mem_limit: 512m\n    deploy:\n      resources:\n        limits:\n          memory: 256m\n", "mem_limit"),
            ("services:\n  app:\n    image: a\n    secrets: [token]\n", "secrets"),
            ("services:\n  app:\n    image: a\n    logging:\n      driver: none\n", "logging"),
            ("services:\n  app:\n    image: a\n    depends_on:\n      db:\n        condition: service_started\n        restart: true\n  db:\n    image: b\n", "depends_on.db.restart"),
        ];
        for (compose, expected_field) in cases {
            let err = Case::new(compose).translate().unwrap_err();
            match &err {
                ComposeTranslationError::Unsupported { service, field, .. }
                | ComposeTranslationError::InvalidValue { service, field, .. } => {
                    assert_eq!(service, "app", "{compose}: {err}");
                    assert_eq!(field, expected_field, "{compose}: {err}");
                    assert!(err.to_string().contains(PROJECT));
                }
                other => panic!("{compose}: unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn top_level_secrets_and_drivers_are_refused() {
        let err = Case::new("services:\n  app:\n    image: a\nsecrets:\n  t:\n    file: ./t\n")
            .translate()
            .unwrap_err();
        assert!(
            matches!(err, ComposeTranslationError::UnsupportedTopLevel { ref section, .. } if section == "secrets")
        );
        let err =
            Case::new("services:\n  app:\n    image: a\nnetworks:\n  back:\n    driver: overlay\n")
                .translate()
                .unwrap_err();
        assert!(
            matches!(err, ComposeTranslationError::UnsupportedTopLevel { ref name, ref field, .. } if name == "back" && field == "driver")
        );
    }

    #[test]
    fn dependency_errors_are_typed() {
        let cycle = Case::new("services:\n  a:\n    image: x\n    depends_on: [b]\n  b:\n    image: x\n    depends_on: [a]\n")
            .translate()
            .unwrap_err();
        assert_eq!(
            cycle,
            ComposeTranslationError::DependencyCycle {
                project: PROJECT.into(),
                services: vec!["a".into(), "b".into()],
            }
        );
        let unknown = Case::new("services:\n  a:\n    image: x\n    depends_on: [ghost]\n")
            .translate()
            .unwrap_err();
        assert!(
            matches!(unknown, ComposeTranslationError::UnknownDependency { ref dependency, .. } if dependency == "ghost")
        );
        // `required: false` on an absent service is Compose's "ignore it".
        let optional = Case::new("services:\n  a:\n    image: x\n    depends_on:\n      ghost:\n        condition: service_started\n        required: false\n")
            .translate()
            .unwrap();
        assert_eq!(optional.service_names(), ["a"]);
    }

    #[test]
    fn order_is_topological_and_stable() {
        let translation = Case::new("services:\n  web:\n    image: x\n    depends_on: [api]\n  api:\n    image: x\n    depends_on:\n      db:\n        condition: service_healthy\n  db:\n    image: x\n    healthcheck:\n      test: [CMD-SHELL, pg_isready]\n  worker:\n    image: x\n")
            .translate()
            .unwrap();
        assert_eq!(translation.service_names(), ["db", "worker", "api", "web"]);
        let db = translation.service("db").unwrap();
        assert_eq!(
            db.spec.probes.readiness.as_ref().unwrap().kind,
            ProbeKind::Exec {
                command: vec!["/bin/sh".into(), "-c".into(), "pg_isready".into()]
            }
        );
        assert!(db.must_stay_running.is_some());
    }

    #[test]
    fn resources_and_named_volumes_translate() {
        let translation = Case::new("services:\n  app:\n    image: a\n    cpus: 0.5\n    mem_reservation: 128m\n    deploy:\n      resources:\n        limits:\n          memory: 1g\n          pids: 100\n    volumes:\n      - data:/var/lib/data:ro\n      - type: volume\n        source: cache\n        target: /cache\nvolumes:\n  data: {}\n  cache:\n    name: shared-cache\n    external: true\n")
            .translate()
            .unwrap();
        let app = &translation.services[0];
        assert_eq!(
            app.spec.resources,
            Resources {
                requests: Some(ResourceAmounts {
                    memory_mb: Some(128),
                    ..Default::default()
                }),
                limits: Some(ResourceAmounts {
                    cpu_millis: Some(500),
                    memory_mb: Some(1024),
                    pids: Some(100),
                    ..Default::default()
                }),
            }
        );
        assert_eq!(
            app.spec.volumes,
            [
                VolumeMount {
                    volume: format!("{PROJECT}_data"),
                    mount_path: "/var/lib/data".into(),
                    read_only: true,
                    source: None,
                },
                VolumeMount {
                    volume: "shared-cache".into(),
                    mount_path: "/cache".into(),
                    read_only: false,
                    source: None,
                },
            ]
        );
        assert!(translation
            .volumes
            .iter()
            .any(|v| v.external && v.name == "shared-cache"));
    }

    #[test]
    fn built_service_gets_compose_image_name_and_labels() {
        let mut case =
            Case::new("services:\n  api:\n    build: .\n    labels:\n      - team=core\n");
        case.labels = HashMap::from([("sh.temps.project_id".to_string(), "7".to_string())]);
        let translation = case.translate().unwrap();
        let api = &translation.services[0];
        assert!(api.built);
        assert_eq!(api.spec.artifact, Artifact::oci(format!("{PROJECT}-api")));
        assert_eq!(api.spec.labels["team"], "core");
        assert_eq!(api.spec.labels["sh.temps.project_id"], "7");
        assert_eq!(api.spec.labels[SERVICE_LABEL], "api");
    }

    #[test]
    fn go_durations_parse() {
        assert_eq!(parse_go_duration_nanos("1m30s"), Some(90_000_000_000));
        assert_eq!(parse_go_duration_nanos("1.5s"), Some(1_500_000_000));
        assert_eq!(parse_go_duration_nanos("0"), Some(0));
        assert_eq!(parse_go_duration_nanos("10"), None);
        assert_eq!(parse_go_duration_nanos(""), None);
    }

    #[test]
    fn override_must_be_merged_first() {
        let env = HashMap::new();
        let files = BTreeMap::new();
        let labels = HashMap::new();
        let err = translate(&ComposeTranslationInput {
            project_name: PROJECT,
            compose_file: "docker-compose.yml",
            content: "services:\n  a:\n    image: x\n",
            override_content: Some("services:\n  a:\n    image: y\n"),
            environment_vars: &env,
            repository_env: None,
            env_files: &files,
            labels: &labels,
            routed_services: &[],
            owner: WorkloadRef::new(WorkloadKind::Deployment, 1),
            temps_network: NETWORK,
        })
        .unwrap_err();
        assert!(matches!(
            err,
            ComposeTranslationError::OverrideNotMerged { .. }
        ));
    }

    /// Legacy parity: the translation of a checked model against what the
    /// legacy CLI path makes Compose run. Each fixture goes through the real
    /// pipeline -- `check_configuration`, the job's env-file plan, then the
    /// legacy `write_compose_files` and `docker compose config` over every
    /// generated override -- and every translated field is compared with
    /// Compose's own resolved model. Without the Compose CLI the translation
    /// half still runs and the comparison is skipped.
    mod legacy_parity {
        use super::*;
        use crate::compose::{ComposeCheckInput, ComposeDeployRequest, EnvFileSource};
        use serde_json::Value as Json;
        use std::path::Path;
        use std::sync::Arc;
        use temps_entities::compose_security::{ComposeSecurityCheck, ComposeSecurityPolicy};

        struct Fixture {
            name: &'static str,
            compose_file: &'static str,
            /// Files written into the checkout, the compose file included.
            files: Vec<(&'static str, String)>,
            env: HashMap<String, String>,
            labels: HashMap<String, String>,
            routed: Vec<String>,
        }

        fn fixtures() -> Vec<Fixture> {
            let (sample, app_env) = sample_fixture();
            vec![
                Fixture {
                    name: "sample-compose-app",
                    compose_file: "deploy/compose.yaml",
                    files: vec![("deploy/compose.yaml", sample), ("deploy/app.env", app_env)],
                    env: vars(&[("GREETING", "from-temps")]),
                    labels: HashMap::new(),
                    routed: vec!["web".into()],
                },
                Fixture {
                    // `IDIOMATIC_COMPOSE` in compose.rs: defaults everywhere and an
                    // `.env` the repository does not ship (synthesized).
                    name: "idiomatic",
                    compose_file: "docker-compose.yml",
                    files: vec![(
                        "docker-compose.yml",
                        "services:\n  app:\n    image: ${IMG:-ghcr.io/example/app:1}\n    env_file: .env\n    environment:\n      GREETING: ${GREETING:-x}\n    ports:\n      - \"127.0.0.1:${APP_PORT:-8080}:80\"\n".to_string(),
                    )],
                    env: vars(&[("APP_PORT", "9000"), ("EXTRA", "e")]),
                    labels: HashMap::new(),
                    routed: vec!["app".into()],
                },
                Fixture {
                    name: "multi-service",
                    compose_file: "docker-compose.yml",
                    files: vec![
                        ("docker-compose.yml", MULTI_SERVICE.to_string()),
                        ("db.env", "POSTGRES_DB=app\nFROM_FILE=file\nMASKED=from-file\n".to_string()),
                        (".env", "REPO_ONLY=repo\nTAG=16\n".to_string()),
                    ],
                    env: vars(&[("POSTGRES_PASSWORD", "pw"), ("API_KEY", "k$y")]),
                    labels: HashMap::from([
                        ("sh.temps.project_id".to_string(), "7".to_string()),
                        ("sh.temps.environment_id".to_string(), "9".to_string()),
                    ]),
                    routed: vec!["api".into()],
                },
            ]
        }

        const MULTI_SERVICE: &str = r#"services:
  db:
    image: postgres:${TAG}
    restart: unless-stopped
    env_file:
      - path: db.env
        required: true
      - path: missing.env
        required: false
    environment:
      - POSTGRES_PASSWORD
      - REPO_ONLY
      - MASKED
      - LITERAL=a$$b
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 5s
      timeout: 3s
      retries: 10
      start_period: 1m
    labels:
      - tier=data
    networks:
      - backend
  api:
    image: ghcr.io/example/api:2
    entrypoint: /usr/local/bin/serve --flag
    command: ["--port", "3000"]
    user: "1000:1000"
    working_dir: /srv
    environment:
      API_KEY: placeholder
      MODE: production
      WORKERS: 4
    ports:
      - target: 3000
        host_ip: 127.0.0.1
        published: ""
        protocol: tcp
    expose:
      - "9090"
    depends_on:
      db:
        condition: service_healthy
    networks:
      backend:
        aliases: [api-internal]
      default: {}
    deploy:
      resources:
        limits:
          cpus: "0.5"
          memory: 256M
        reservations:
          memory: 64M
    healthcheck:
      test: ["CMD", "wget", "-q", "-O-", "http://localhost:3000/health"]
  worker:
    image: ghcr.io/example/api:2
    command: sh -c 'echo "worker ready" && sleep 1'
    restart: on-failure
    depends_on: [db, api]
    volumes:
      - /tmp/scratch
      - type: volume
        source: shared
        target: /shared
        read_only: true
networks:
  backend: {}
volumes:
  pgdata: {}
  shared: {}
"#;

        async fn compose_cli_available() -> bool {
            tokio::process::Command::new("docker")
                .args(["compose", "version"])
                .output()
                .await
                .is_ok_and(|output| output.status.success())
        }

        fn executor(data_dir: &Path) -> ComposeExecutor {
            // The plane caps memory and pids itself (admission), so the
            // legacy injected caps are left out of the comparison.
            ComposeExecutor::new_with_handle(
                Arc::new(temps_core::DockerHandle::disabled(
                    temps_core::PROFILE_CONTROL_PLANE,
                    temps_core::CONTROL_PLANE_DOCKER_REASON,
                )),
                data_dir.to_path_buf(),
            )
            .with_security_policy(ComposeSecurityPolicy {
                disabled_checks: [ComposeSecurityCheck::Memory, ComposeSecurityCheck::Pids]
                    .into_iter()
                    .collect(),
            })
        }

        async fn run(fixture: &Fixture, cli: bool) {
            let checkout = tempfile::tempdir().unwrap();
            let data = tempfile::tempdir().unwrap();
            for (path, content) in &fixture.files {
                let target = checkout.path().join(path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, content).unwrap();
            }
            let content =
                std::fs::read_to_string(checkout.path().join(fixture.compose_file)).unwrap();
            let repository_env = std::fs::read_to_string(checkout.path().join(".env")).ok();
            let executor = executor(data.path());
            let checked = executor
                .check_configuration(&ComposeCheckInput {
                    project_name: PROJECT,
                    project_dir: Some(checkout.path()),
                    compose_file: fixture.compose_file,
                    content: &content,
                    override_content: None,
                    excluded_services: &[],
                    environment: &fixture.env,
                    repository_env: None,
                    filesystem_checks: true,
                    allow_cli: false,
                    defer_unknown_required: false,
                })
                .await
                .unwrap_or_else(|error| panic!("{}: check failed: {error}", fixture.name));

            // The env files exactly as the deploy job plans them.
            let env_files: BTreeMap<String, EnvFileContent> =
                ComposeExecutor::plan_env_files_for_compose_file(
                    &checked.content,
                    fixture.compose_file,
                    Some(checkout.path()),
                )
                .into_iter()
                .map(|plan| {
                    let content = match plan.source {
                        EnvFileSource::Repository(path) => {
                            EnvFileContent::Dotenv(std::fs::read_to_string(path).unwrap())
                        }
                        EnvFileSource::ProjectEnvironment => EnvFileContent::ProjectEnvironment,
                    };
                    (plan.path, content)
                })
                .collect();
            let translation = translate(&ComposeTranslationInput {
                project_name: PROJECT,
                compose_file: fixture.compose_file,
                content: &checked.content,
                override_content: checked.override_content.as_deref(),
                environment_vars: &fixture.env,
                repository_env: repository_env.as_deref(),
                env_files: &env_files,
                labels: &fixture.labels,
                routed_services: &fixture.routed,
                owner: WorkloadRef::new(WorkloadKind::Deployment, 42),
                temps_network: temps_core::NETWORK_NAME.as_str(),
            })
            .unwrap_or_else(|error| panic!("{}: translation failed: {error}", fixture.name));

            if !cli {
                println!(
                    "{}: Docker Compose CLI not available; skipping parity",
                    fixture.name
                );
                return;
            }
            let request = ComposeDeployRequest {
                project_name: PROJECT.to_string(),
                compose_content: checked.content.clone(),
                env_content: repository_env,
                work_dir: checkout.path().to_path_buf(),
                compose_path: Some(fixture.compose_file.to_string()),
                environment_vars: fixture.env.clone(),
                secrets: HashMap::new(),
                secret_compose_services: HashMap::new(),
                build_args: HashMap::new(),
                labels: fixture.labels.clone(),
                repo_dir: Some(checkout.path().to_path_buf()),
                compose_override: None,
                relaxed_capability_services: Vec::new(),
                unsandboxed_services: Vec::new(),
                ready_timeout: None,
                routed_services: fixture.routed.clone(),
                output_sink: None,
            };
            executor
                .write_compose_files(checkout.path(), &request, "gen-1")
                .await
                .unwrap();
            let args = ComposeExecutor::compose_input_args(checkout.path(), fixture.compose_file);
            let output = tokio::process::Command::new("docker")
                .args(["compose", "-p", PROJECT])
                .args(&args)
                .args(["config", "--format", "json"])
                .current_dir(checkout.path())
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}: docker compose config failed: {}",
                fixture.name,
                String::from_utf8_lossy(&output.stderr)
            );
            let model: Json = serde_json::from_slice(&output.stdout).unwrap();
            assert_parity(fixture.name, &translation, &model);
        }

        fn strings(value: &Json) -> Option<Vec<String>> {
            value.as_array().map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| item.to_string())
                    })
                    .collect()
            })
        }

        fn text(value: &Json) -> Option<String> {
            match value {
                Json::Null => None,
                Json::String(text) => Some(text.clone()),
                other => Some(other.to_string()),
            }
        }

        fn bytes(value: &Json) -> Option<u64> {
            let yaml = match value {
                Json::Number(number) => Value::Number(number.as_u64()?.into()),
                Json::String(text) => Value::String(text.clone()),
                _ => return None,
            };
            ComposeExecutor::parse_compose_byte_size(&yaml)
        }

        /// Where the translation deliberately differs from the legacy path.
        /// The legacy env override (`generate_env_override`) writes a project
        /// variable for an inline key without escaping `$`, so Compose
        /// interpolates it (`k$y` reaches the container as `k`), while the same
        /// variable through `.env.temps` stays literal. The translation keeps
        /// project values literal everywhere.
        const KNOWN_LEGACY_DIVERGENCES: &[(&str, &str)] = &[("multi-service/api", "API_KEY")];

        fn assert_parity(name: &str, translation: &ComposeTranslation, model: &Json) {
            let services = model["services"].as_object().unwrap();
            assert_eq!(
                services.keys().collect::<BTreeSet<_>>(),
                translation
                    .services
                    .iter()
                    .map(|s| &s.name)
                    .collect::<BTreeSet<_>>(),
                "{name}: service set"
            );
            for service in &translation.services {
                let m = &model["services"][&service.name];
                let at = format!("{name}/{}", service.name);
                let spec = &service.spec;

                if let Some(image) = m["image"].as_str() {
                    assert_eq!(spec.artifact, Artifact::oci(image), "{at}: image");
                }
                assert_eq!(
                    strings(&m["command"]),
                    spec.process.command,
                    "{at}: command"
                );
                assert_eq!(
                    strings(&m["entrypoint"]),
                    spec.process.entrypoint,
                    "{at}: entrypoint"
                );
                assert_eq!(text(&m["user"]), spec.process.user, "{at}: user");
                assert_eq!(
                    text(&m["working_dir"]),
                    spec.process.workdir,
                    "{at}: working_dir"
                );

                // Environment: every value Compose resolved, nothing else.
                let mut legacy_env = BTreeMap::new();
                for (key, value) in m["environment"].as_object().into_iter().flatten() {
                    // `config` re-escapes `$` as `$$` so its output stays
                    // loadable; the container gets the single `$`.
                    if let Some(value) = text(value) {
                        legacy_env.insert(key.clone(), value.replace("$$", "$"));
                    }
                }
                let mut translated_env = service.environment.vars().clone();
                for (where_, key) in KNOWN_LEGACY_DIVERGENCES {
                    if *where_ == at {
                        // Still divergent? Once legacy is fixed, drop the entry.
                        assert_ne!(
                            legacy_env.get(*key),
                            translated_env.get(*key),
                            "{at}: {key} now matches legacy; remove it from KNOWN_LEGACY_DIVERGENCES"
                        );
                        legacy_env.remove(*key);
                        translated_env.remove(*key);
                    }
                }
                assert_eq!(legacy_env, translated_env, "{at}: environment");

                // Published ports.
                let mut legacy_ports: Vec<PublishedPort> = m["ports"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|port| PublishedPort {
                        host_ip: text(&port["host_ip"]),
                        host_port: text(&port["published"])
                            .filter(|p| !p.is_empty())
                            .map(|p| p.parse().unwrap()),
                        container_port: port["target"].as_u64().unwrap() as u16,
                        protocol: if port["protocol"] == "udp" {
                            PortProtocol::Udp
                        } else {
                            PortProtocol::Tcp
                        },
                    })
                    .collect();
                let mut translated_ports = service.published_ports.clone();
                let key = |p: &PublishedPort| {
                    (
                        p.container_port,
                        p.host_port,
                        p.host_ip.clone(),
                        p.protocol.as_str(),
                    )
                };
                legacy_ports.sort_by_key(key);
                translated_ports.sort_by_key(key);
                assert_eq!(legacy_ports, translated_ports, "{at}: ports");

                // Networks by Docker name; declared aliases are kept.
                let mut legacy_networks = BTreeSet::new();
                for (key, settings) in m["networks"].as_object().into_iter().flatten() {
                    let network_name = model["networks"][key]["name"].as_str().unwrap().to_string();
                    let translated = service
                        .networks
                        .iter()
                        .find(|network| network.name == network_name)
                        .unwrap_or_else(|| panic!("{at}: network {network_name} not translated"));
                    for alias in strings(&settings["aliases"]).unwrap_or_default() {
                        assert!(translated.aliases.contains(&alias), "{at}: alias {alias}");
                    }
                    legacy_networks.insert(network_name);
                }
                assert_eq!(
                    legacy_networks,
                    service
                        .networks
                        .iter()
                        .map(|n| n.name.clone())
                        .collect::<BTreeSet<_>>(),
                    "{at}: networks"
                );

                // Volumes: named by Docker name, anonymous by target.
                let mut legacy_named = Vec::new();
                let mut legacy_anonymous = Vec::new();
                for volume in m["volumes"].as_array().into_iter().flatten() {
                    assert_eq!(
                        volume["type"], "volume",
                        "{at}: only volume mounts translate"
                    );
                    let target = volume["target"].as_str().unwrap().to_string();
                    match volume["source"].as_str().filter(|s| !s.is_empty()) {
                        Some(source) => legacy_named.push(VolumeMount {
                            volume: model["volumes"][source]["name"]
                                .as_str()
                                .unwrap()
                                .to_string(),
                            mount_path: target,
                            read_only: volume["read_only"].as_bool().unwrap_or(false),
                            source: None,
                        }),
                        None => legacy_anonymous.push(target),
                    }
                }
                assert_eq!(legacy_named, spec.volumes, "{at}: named volumes");
                assert_eq!(
                    legacy_anonymous, service.anonymous_volumes,
                    "{at}: anonymous volumes"
                );

                let restart = match m["restart"].as_str() {
                    None | Some("no") => RestartPolicy::Never,
                    Some("always") => RestartPolicy::Always,
                    Some("on-failure") => RestartPolicy::OnFailure,
                    Some("unless-stopped") => RestartPolicy::UnlessStopped,
                    Some(other) => panic!("{at}: restart {other}"),
                };
                assert_eq!(
                    spec.lifetime,
                    Lifetime::LongRunning { restart },
                    "{at}: restart"
                );

                let mut legacy_deps: Vec<(String, String, bool)> = m["depends_on"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(dep, settings)| {
                        (
                            dep.clone(),
                            settings["condition"].as_str().unwrap().to_string(),
                            settings["required"].as_bool().unwrap_or(true),
                        )
                    })
                    .collect();
                let mut translated_deps: Vec<(String, String, bool)> = service
                    .depends_on
                    .iter()
                    .map(|d| {
                        (
                            d.service.clone(),
                            d.condition.as_compose().to_string(),
                            d.required,
                        )
                    })
                    .collect();
                legacy_deps.sort();
                translated_deps.sort();
                assert_eq!(legacy_deps, translated_deps, "{at}: depends_on");

                let legacy_labels: BTreeMap<String, String> = m["labels"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), text(v).unwrap_or_default()))
                    .collect();
                assert_eq!(legacy_labels, spec.labels, "{at}: labels");

                // Healthcheck -> readiness probe.
                let legacy_probe = m["healthcheck"]
                    .as_object()
                    .filter(|check| check.get("disable") != Some(&Json::Bool(true)))
                    .map(|check| {
                        let test = strings(&check["test"]).unwrap();
                        let command = match test.split_first() {
                            Some((kind, rest)) if kind == "CMD" => rest.to_vec(),
                            Some((kind, rest)) if kind == "CMD-SHELL" => {
                                vec!["/bin/sh".into(), "-c".into(), rest[0].clone()]
                            }
                            other => panic!("{at}: healthcheck {other:?}"),
                        };
                        let secs = |key: &str, default: u32| {
                            check
                                .get(key)
                                .and_then(Json::as_str)
                                .map(|d| {
                                    (parse_go_duration_nanos(d).unwrap() / 1_000_000_000) as u32
                                })
                                .unwrap_or(default)
                        };
                        let mut probe = Probe::new(ProbeKind::Exec { command });
                        probe.period_secs = secs("interval", 30);
                        probe.timeout_secs = secs("timeout", 30);
                        probe.initial_delay_secs = secs("start_period", 0);
                        probe.failure_threshold = check
                            .get("retries")
                            .and_then(Json::as_u64)
                            .map(|r| r as u32)
                            .unwrap_or(3);
                        probe
                    });
                assert_eq!(legacy_probe, spec.probes.readiness, "{at}: healthcheck");

                // Resources: Compose normalises to bytes and CPU fractions.
                let limits = &m["deploy"]["resources"]["limits"];
                let reservations = &m["deploy"]["resources"]["reservations"];
                let memory_limit = bytes(&m["mem_limit"]).or_else(|| bytes(&limits["memory"]));
                let memory_request =
                    bytes(&m["mem_reservation"]).or_else(|| bytes(&reservations["memory"]));
                let cpu_limit = text(&m["cpus"])
                    .or_else(|| text(&limits["cpus"]))
                    .map(|c| (c.parse::<f64>().unwrap() * 1000.0).round() as u32)
                    .filter(|millis| *millis > 0);
                let translated_limits = spec.resources.limits.unwrap_or_default();
                let translated_requests = spec.resources.requests.unwrap_or_default();
                assert_eq!(
                    memory_limit,
                    translated_limits
                        .memory_mb
                        .map(|mb| u64::from(mb) * 1024 * 1024),
                    "{at}: memory limit"
                );
                assert_eq!(
                    memory_request,
                    translated_requests
                        .memory_mb
                        .map(|mb| u64::from(mb) * 1024 * 1024),
                    "{at}: memory reservation"
                );
                assert_eq!(cpu_limit, translated_limits.cpu_millis, "{at}: cpus");
            }
        }

        #[tokio::test]
        async fn translation_matches_the_legacy_compose_model() {
            let cli = compose_cli_available().await;
            for fixture in fixtures() {
                run(&fixture, cli).await;
            }
        }
    }
}
