// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a container *is*, compared with what its spec says it should be
//! (ADR-055 Phase 4 shadow mode).
//!
//! A [`ContainerShape`] is the part of a container that admission and the
//! driver decide: image, environment, command, labels, restart policy,
//! hardening, healthcheck, log rotation, storage, published ports and
//! limits. It is read from a create request ([`ContainerShape::from_create_body`],
//! what the spec would send) or from a running container
//! ([`ContainerShape::from_inspect`], what is there), and [`diff`] lists the
//! differences.
//!
//! Secret values never leave a shape: [`ContainerShape::mask`] replaces the
//! named variables' values, and command arguments that carry a secret are
//! written as `$(NAME)` placeholders in the spec, which match any value and
//! are never printed from the running container.
//!
//! Node-local attachments (the host interface a port is published on, the
//! resolvers) are not part of a shape: they differ by node on purpose.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, HealthConfig, HostConfig, HostConfigLogConfig,
};
use serde::{Deserialize, Serialize};

/// The value a masked secret variable is shown as.
pub const MASKED: &str = "***";

/// `/dev/shm` when none is set.
const DOCKER_DEFAULT_SHM_BYTES: i64 = 64 * 1024 * 1024;

/// Labels in these namespaces are Temps' own: an extra one on the running
/// container is a difference. Other labels (the image's) are ignored.
const TEMPS_LABEL_PREFIXES: &[&str] = &["temps.", "sh.temps."];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerShape {
    pub image: Option<String>,
    /// Variables by name. Masked variables hold [`MASKED`].
    pub env: BTreeMap<String, String>,
    /// `None`: the image's own command.
    pub command: Option<Vec<String>>,
    pub labels: BTreeMap<String, String>,
    pub restart_policy: Option<String>,
    pub pids_limit: Option<i64>,
    pub no_new_privileges: bool,
    pub cap_drop: Vec<String>,
    pub init: bool,
    /// Rendered `Test` plus timings; `None`: no healthcheck.
    pub healthcheck: Option<String>,
    /// `json-file max-size=20m max-file=3`; `None`: the daemon's default.
    pub log_config: Option<String>,
    /// `(volume or host path, container path)`.
    pub mounts: BTreeSet<(String, String)>,
    /// Published container ports, `5432/tcp`.
    pub ports: BTreeSet<String>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub shm_size_bytes: Option<i64>,
    pub cpu_shares: Option<i64>,
}

/// One part of a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeField {
    Image,
    Env,
    Command,
    Labels,
    RestartPolicy,
    PidsLimit,
    NoNewPrivileges,
    CapDrop,
    Init,
    Healthcheck,
    LogConfig,
    Mounts,
    Ports,
    MemoryLimit,
    CpuLimit,
    ShmSize,
    CpuShares,
}

impl ShapeField {
    pub fn as_str(self) -> &'static str {
        match self {
            ShapeField::Image => "image",
            ShapeField::Env => "env",
            ShapeField::Command => "command",
            ShapeField::Labels => "labels",
            ShapeField::RestartPolicy => "restart_policy",
            ShapeField::PidsLimit => "pids_limit",
            ShapeField::NoNewPrivileges => "no_new_privileges",
            ShapeField::CapDrop => "cap_drop",
            ShapeField::Init => "init",
            ShapeField::Healthcheck => "healthcheck",
            ShapeField::LogConfig => "log_config",
            ShapeField::Mounts => "mounts",
            ShapeField::Ports => "ports",
            ShapeField::MemoryLimit => "memory_limit",
            ShapeField::CpuLimit => "cpu_limit",
            ShapeField::ShmSize => "shm_size",
            ShapeField::CpuShares => "cpu_shares",
        }
    }
}

impl fmt::Display for ShapeField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a running container differs from its spec. Never holds a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapeDifference {
    pub field: ShapeField,
    /// What the spec says, rendered.
    pub expected: String,
    /// What the container has, rendered.
    pub actual: String,
}

fn positive(value: Option<i64>) -> Option<i64> {
    value.filter(|v| *v > 0)
}

fn env_map(env: Option<&Vec<String>>) -> BTreeMap<String, String> {
    env.into_iter()
        .flatten()
        .map(|entry| match entry.split_once('=') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None => (entry.clone(), String::new()),
        })
        .collect()
}

fn render_health(health: Option<&HealthConfig>) -> Option<String> {
    let health = health?;
    let test = health.test.as_ref().filter(|t| !t.is_empty())?;
    if test.first().map(String::as_str) == Some("NONE") {
        return None;
    }
    let nanos = |v: Option<i64>| v.filter(|n| *n > 0).map(|n| n as f64 / 1e9);
    let mut rendered = test.join(" ");
    for (name, value) in [
        ("interval", nanos(health.interval)),
        ("timeout", nanos(health.timeout)),
        ("start_period", nanos(health.start_period)),
        ("start_interval", nanos(health.start_interval)),
    ] {
        if let Some(value) = value {
            rendered.push_str(&format!(" {name}={value}s"));
        }
    }
    if let Some(retries) = health.retries.filter(|r| *r > 0) {
        rendered.push_str(&format!(" retries={retries}"));
    }
    Some(rendered)
}

fn render_log(log: Option<&HostConfigLogConfig>) -> Option<String> {
    let log = log?;
    let config = log.config.clone().unwrap_or_default();
    if config.is_empty() {
        // `json-file` with no options is what the daemon does by default.
        return match log.typ.as_deref() {
            None | Some("") | Some("json-file") => None,
            Some(other) => Some(other.to_string()),
        };
    }
    let options: BTreeMap<_, _> = config.into_iter().collect();
    let mut rendered = log.typ.clone().unwrap_or_else(|| "json-file".to_string());
    for (key, value) in options {
        rendered.push_str(&format!(" {key}={value}"));
    }
    Some(rendered)
}

fn bind_mount(bind: &str) -> Option<(String, String)> {
    let mut parts = bind.splitn(3, ':');
    let source = parts.next()?;
    let target = parts.next()?;
    Some((source.to_string(), target.to_string()))
}

impl ContainerShape {
    fn host(mut self, host: Option<&HostConfig>) -> Self {
        let Some(host) = host else {
            return self;
        };
        self.restart_policy = host
            .restart_policy
            .as_ref()
            .and_then(|p| p.name)
            .map(|name| name.to_string())
            .filter(|name| !name.is_empty() && name != "no");
        self.pids_limit = positive(host.pids_limit);
        self.no_new_privileges = host.security_opt.iter().flatten().any(|opt| {
            opt == "no-new-privileges"
                || opt == "no-new-privileges:true"
                || opt == "no-new-privileges=true"
        });
        self.cap_drop = host.cap_drop.clone().unwrap_or_default();
        self.cap_drop.sort();
        self.init = host.init.unwrap_or(false);
        self.log_config = render_log(host.log_config.as_ref());
        self.ports = host
            .port_bindings
            .iter()
            .flat_map(|bindings| bindings.keys().cloned())
            .collect();
        self.memory_bytes = positive(host.memory);
        self.nano_cpus = positive(host.nano_cpus);
        // Inspect reports Docker's 64 MiB default whether or not it was set.
        self.shm_size_bytes = positive(host.shm_size).filter(|v| *v != DOCKER_DEFAULT_SHM_BYTES);
        self.cpu_shares = positive(host.cpu_shares);
        self
    }

    /// The shape a create request would give the container.
    pub fn from_create_body(body: &ContainerCreateBody) -> Self {
        let host = body.host_config.as_ref();
        let mut mounts: BTreeSet<(String, String)> = host
            .and_then(|h| h.binds.as_ref())
            .into_iter()
            .flatten()
            .filter_map(|bind| bind_mount(bind))
            .collect();
        for mount in host.and_then(|h| h.mounts.as_ref()).into_iter().flatten() {
            if let (Some(source), Some(target)) = (&mount.source, &mount.target) {
                mounts.insert((source.clone(), target.clone()));
            }
        }
        Self {
            image: body.image.clone(),
            env: env_map(body.env.as_ref()),
            command: body.cmd.clone(),
            labels: body
                .labels
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect(),
            healthcheck: render_health(body.healthcheck.as_ref()),
            mounts,
            ..Default::default()
        }
        .host(host)
    }

    /// The shape of a container as Docker reports it.
    pub fn from_inspect(inspect: &ContainerInspectResponse) -> Self {
        let config = inspect.config.as_ref();
        let mounts = inspect
            .mounts
            .iter()
            .flatten()
            .filter_map(|mount| {
                let source = mount.name.clone().or_else(|| mount.source.clone())?;
                Some((source, mount.destination.clone()?))
            })
            .collect();
        Self {
            image: config.and_then(|c| c.image.clone()),
            env: env_map(config.and_then(|c| c.env.as_ref())),
            command: config.and_then(|c| c.cmd.clone()),
            labels: config
                .and_then(|c| c.labels.clone())
                .unwrap_or_default()
                .into_iter()
                .collect(),
            healthcheck: render_health(config.and_then(|c| c.healthcheck.as_ref())),
            mounts,
            ..Default::default()
        }
        .host(inspect.host_config.as_ref())
    }

    /// Replace the values of `secret_names` with [`MASKED`], so the shape can
    /// be logged, stored and sent between nodes.
    pub fn mask<'a>(mut self, secret_names: impl IntoIterator<Item = &'a str>) -> Self {
        for name in secret_names {
            if let Some(value) = self.env.get_mut(name) {
                *value = MASKED.to_string();
            }
        }
        self
    }
}

/// `$(NAME)` anywhere in an argument: a value only the node knows.
fn is_placeholder(arg: &str) -> bool {
    arg.find("$(")
        .is_some_and(|start| arg[start..].contains(')'))
}

fn args_match(expected: &[String], actual: &[String]) -> bool {
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .all(|(e, a)| is_placeholder(e) || e == a)
}

/// The running command as it may be shown next to `expected`.
///
/// When the spec carries a secret as a `$(NAME)` placeholder, its value is
/// somewhere in the running command, and not necessarily at the
/// placeholder's position (an argument may have been added before it). So
/// every argument that is not one of the spec's literal arguments is
/// masked; a spec without placeholders shows the command as it is.
fn mask_args(expected: &[String], actual: &[String]) -> Vec<String> {
    if !expected.iter().any(|e| is_placeholder(e)) {
        return actual.to_vec();
    }
    let literals: BTreeSet<&str> = expected
        .iter()
        .filter(|e| !is_placeholder(e))
        .map(String::as_str)
        .collect();
    actual
        .iter()
        .map(|arg| {
            if literals.contains(arg.as_str()) {
                arg.clone()
            } else {
                MASKED.to_string()
            }
        })
        .collect()
}

fn opt<T: fmt::Display>(value: &Option<T>) -> String {
    value
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "none".to_string())
}

fn render_map(map: &BTreeMap<String, String>) -> String {
    if map.is_empty() {
        return "none".to_string();
    }
    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_set(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        return "none".to_string();
    }
    set.iter().cloned().collect::<Vec<_>>().join(", ")
}

/// Every difference between the spec's shape and the running container's,
/// in [`ShapeField`] order.
///
/// Only what the spec decides is compared: variables, labels and a command
/// the image supplies and the spec does not set are the image's business.
pub fn diff(expected: &ContainerShape, actual: &ContainerShape) -> Vec<ShapeDifference> {
    let mut out = Vec::new();
    let mut push = |field, expected: String, actual: String| {
        out.push(ShapeDifference {
            field,
            expected,
            actual,
        })
    };

    if expected.image != actual.image {
        push(ShapeField::Image, opt(&expected.image), opt(&actual.image));
    }

    let mut env_expected = BTreeMap::new();
    let mut env_actual = BTreeMap::new();
    for (name, value) in &expected.env {
        let observed = actual.env.get(name);
        let same = match observed {
            Some(_) if value == MASKED => true,
            Some(observed) => observed == value,
            None => false,
        };
        if !same {
            env_expected.insert(name.clone(), value.clone());
            env_actual.insert(
                name.clone(),
                observed.cloned().unwrap_or_else(|| "<unset>".to_string()),
            );
        }
    }
    if !env_expected.is_empty() {
        push(
            ShapeField::Env,
            render_map(&env_expected),
            render_map(&env_actual),
        );
    }

    if let Some(command) = &expected.command {
        let observed = actual.command.clone().unwrap_or_default();
        if !args_match(command, &observed) {
            push(
                ShapeField::Command,
                command.join(" "),
                mask_args(command, &observed).join(" "),
            );
        }
    }

    let mut missing = BTreeMap::new();
    let mut found = BTreeMap::new();
    for (key, value) in &expected.labels {
        match actual.labels.get(key) {
            Some(observed) if observed == value => {}
            observed => {
                missing.insert(key.clone(), value.clone());
                if let Some(observed) = observed {
                    found.insert(key.clone(), observed.clone());
                }
            }
        }
    }
    for (key, value) in &actual.labels {
        let ours = TEMPS_LABEL_PREFIXES.iter().any(|p| key.starts_with(p));
        if ours && !expected.labels.contains_key(key) {
            found.insert(key.clone(), value.clone());
        }
    }
    if !missing.is_empty() || !found.is_empty() {
        push(ShapeField::Labels, render_map(&missing), render_map(&found));
    }

    if expected.restart_policy != actual.restart_policy {
        push(
            ShapeField::RestartPolicy,
            opt(&expected.restart_policy),
            opt(&actual.restart_policy),
        );
    }
    if expected.pids_limit != actual.pids_limit {
        push(
            ShapeField::PidsLimit,
            opt(&expected.pids_limit),
            opt(&actual.pids_limit),
        );
    }
    if expected.no_new_privileges != actual.no_new_privileges {
        push(
            ShapeField::NoNewPrivileges,
            expected.no_new_privileges.to_string(),
            actual.no_new_privileges.to_string(),
        );
    }
    if expected.cap_drop != actual.cap_drop {
        push(
            ShapeField::CapDrop,
            render_set(&expected.cap_drop.iter().cloned().collect()),
            render_set(&actual.cap_drop.iter().cloned().collect()),
        );
    }
    if expected.init != actual.init {
        push(
            ShapeField::Init,
            expected.init.to_string(),
            actual.init.to_string(),
        );
    }
    if expected.healthcheck != actual.healthcheck {
        push(
            ShapeField::Healthcheck,
            opt(&expected.healthcheck),
            opt(&actual.healthcheck),
        );
    }
    if expected.log_config != actual.log_config {
        push(
            ShapeField::LogConfig,
            opt(&expected.log_config),
            opt(&actual.log_config),
        );
    }
    if expected.mounts != actual.mounts {
        let render = |set: &BTreeSet<(String, String)>| {
            render_set(&set.iter().map(|(s, t)| format!("{s}:{t}")).collect())
        };
        push(
            ShapeField::Mounts,
            render(&expected.mounts),
            render(&actual.mounts),
        );
    }
    if !expected.ports.is_subset(&actual.ports) {
        push(
            ShapeField::Ports,
            render_set(&expected.ports),
            render_set(&actual.ports),
        );
    }
    for (field, e, a) in [
        (
            ShapeField::MemoryLimit,
            expected.memory_bytes,
            actual.memory_bytes,
        ),
        (ShapeField::CpuLimit, expected.nano_cpus, actual.nano_cpus),
        (
            ShapeField::ShmSize,
            expected.shm_size_bytes,
            actual.shm_size_bytes,
        ),
        (
            ShapeField::CpuShares,
            expected.cpu_shares,
            actual.cpu_shares,
        ),
    ] {
        if e != a {
            push(field, opt(&e), opt(&a));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerConfig, MountPoint, RestartPolicy, RestartPolicyNameEnum};
    use std::collections::HashMap;

    fn body() -> ContainerCreateBody {
        ContainerCreateBody {
            image: Some("gotempsh/redis-walg:8-bookworm".into()),
            env: Some(vec!["REDIS_PASSWORD=seeded-secret".into()]),
            cmd: Some(vec![
                "redis-server".into(),
                "--requirepass".into(),
                "$(REDIS_PASSWORD)".into(),
            ]),
            labels: Some(HashMap::from([
                ("temps.service_type".into(), "redis".into()),
                ("sh.temps.kind".into(), "service".into()),
            ])),
            healthcheck: Some(HealthConfig {
                test: Some(vec!["CMD-SHELL".into(), "redis-cli ping".into()]),
                interval: Some(1_000_000_000),
                ..Default::default()
            }),
            host_config: Some(HostConfig {
                binds: Some(vec!["redis-cache_data:/data".into()]),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                pids_limit: Some(512),
                security_opt: Some(vec!["no-new-privileges:true".into()]),
                port_bindings: Some(HashMap::from([("6379/tcp".into(), None)])),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn running_as(body: &ContainerCreateBody) -> ContainerInspectResponse {
        let host = body.host_config.clone().unwrap_or_default();
        let mounts = host
            .binds
            .iter()
            .flatten()
            .filter_map(|b| bind_mount(b))
            .map(|(name, destination)| MountPoint {
                name: Some(name),
                destination: Some(destination),
                ..Default::default()
            })
            .collect();
        let mut env = body.env.clone().unwrap_or_default();
        // The image's own variables are not the spec's business.
        env.push("PATH=/usr/local/bin".into());
        let mut labels = body.labels.clone().unwrap_or_default();
        labels.insert("org.opencontainers.image.version".into(), "8".into());
        ContainerInspectResponse {
            config: Some(ContainerConfig {
                image: body.image.clone(),
                env: Some(env),
                cmd: body.cmd.clone().map(|cmd| {
                    cmd.into_iter()
                        .map(|a| a.replace("$(REDIS_PASSWORD)", "seeded-secret"))
                        .collect()
                }),
                labels: Some(labels),
                healthcheck: body.healthcheck.clone(),
                ..Default::default()
            }),
            host_config: Some(HostConfig {
                binds: None,
                ..host
            }),
            mounts: Some(mounts),
            ..Default::default()
        }
    }

    #[test]
    fn a_container_created_from_its_spec_has_no_differences() {
        let expected = ContainerShape::from_create_body(&body()).mask(["REDIS_PASSWORD"]);
        let actual = ContainerShape::from_inspect(&running_as(&body())).mask(["REDIS_PASSWORD"]);
        assert_eq!(diff(&expected, &actual), vec![]);
    }

    #[test]
    fn hardening_labels_and_healthcheck_differences_are_listed_in_order() {
        let expected = ContainerShape::from_create_body(&body()).mask(["REDIS_PASSWORD"]);
        let mut legacy = body();
        legacy.labels = Some(HashMap::from([("sh.temps.service".into(), "true".into())]));
        legacy.healthcheck = None;
        let host = legacy.host_config.as_mut().unwrap();
        host.pids_limit = None;
        host.security_opt = None;
        host.restart_policy = Some(RestartPolicy {
            name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
            maximum_retry_count: None,
        });
        let actual = ContainerShape::from_inspect(&running_as(&legacy)).mask(["REDIS_PASSWORD"]);
        let fields: Vec<_> = diff(&expected, &actual).iter().map(|d| d.field).collect();
        assert_eq!(
            fields,
            vec![
                ShapeField::Labels,
                ShapeField::RestartPolicy,
                ShapeField::PidsLimit,
                ShapeField::NoNewPrivileges,
                ShapeField::Healthcheck,
            ]
        );
        let labels = &diff(&expected, &actual)[0];
        assert!(
            labels.expected.contains("temps.service_type=redis"),
            "{labels:?}"
        );
        assert!(
            labels.actual.contains("sh.temps.service=true"),
            "{labels:?}"
        );
    }

    #[test]
    fn secret_values_never_appear_in_a_difference() {
        let expected = ContainerShape::from_create_body(&body()).mask(["REDIS_PASSWORD"]);
        let mut changed = body();
        changed.cmd = Some(vec![
            "redis-server".into(),
            "--appendonly".into(),
            "--requirepass".into(),
            "$(REDIS_PASSWORD)".into(),
        ]);
        let actual = ContainerShape::from_inspect(&running_as(&changed)).mask(["REDIS_PASSWORD"]);
        let differences = diff(&expected, &actual);
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].field, ShapeField::Command);
        let rendered = format!("{differences:?}");
        assert!(!rendered.contains("seeded-secret"), "{rendered}");
    }

    #[test]
    fn a_changed_variable_is_listed_and_a_masked_one_compares_equal() {
        let mut spec = body();
        spec.env = Some(vec![
            "REDIS_PASSWORD=x".into(),
            "PGDATA=/var/lib/postgresql/18/docker".into(),
        ]);
        let expected = ContainerShape::from_create_body(&spec).mask(["REDIS_PASSWORD"]);
        let mut running = body();
        running.env = Some(vec!["REDIS_PASSWORD=other".into()]);
        let actual = ContainerShape::from_inspect(&running_as(&running)).mask(["REDIS_PASSWORD"]);
        let differences = diff(&expected, &actual);
        assert_eq!(differences.len(), 1, "{differences:?}");
        assert_eq!(differences[0].field, ShapeField::Env);
        assert_eq!(
            differences[0].expected,
            "PGDATA=/var/lib/postgresql/18/docker"
        );
        assert_eq!(differences[0].actual, "PGDATA=<unset>");
    }

    #[test]
    fn the_daemon_default_log_config_reads_as_none() {
        let default = HostConfigLogConfig {
            typ: Some("json-file".into()),
            config: Some(HashMap::new()),
        };
        assert_eq!(render_log(Some(&default)), None);
        let rotated = HostConfigLogConfig {
            typ: Some("json-file".into()),
            config: Some(HashMap::from([
                ("max-size".into(), "20m".into()),
                ("max-file".into(), "3".into()),
            ])),
        };
        assert_eq!(
            render_log(Some(&rotated)).as_deref(),
            Some("json-file max-file=3 max-size=20m")
        );
    }
}
