// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps agent labels`: change a joined worker's scheduling labels
//! (ADR-091), such as `temps.sh/role=dedicated`.
//!
//! Labels live in the `labels` object of `agent.json`. The node asserts
//! them in every heartbeat; this command edits that object (validated, in
//! place, owner-only), then confirms the control plane accepted the change:
//! a current agent reloads it within seconds and records what was accepted
//! in `labels-status.json`; an older one is restarted through its managed
//! service.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use temps_core::node_labels::{self, NODE_ROLE_LABEL};
use temps_node_agent::labels::{LabelsOrigin, LabelsStatus};

use super::agent_ingress::Target;

/// How long a running agent gets to reload and report the change.
const RELOAD_WAIT: Duration = Duration::from_secs(15);
/// How long a restarted agent gets to start and report.
const RESTART_WAIT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const LABELS_ENV: &str = "TEMPS_NODE_LABELS";

#[derive(Subcommand, Debug, Clone)]
pub enum AgentLabelsCommand {
    /// Show this worker's labels, where they come from, and what the control
    /// plane last accepted
    List(Target),
    /// Add or change labels, e.g. `temps.sh/role=dedicated gpu=true`
    Set(SetArgs),
    /// Remove labels by key, e.g. `temps.sh/role`
    Unset(UnsetArgs),
}

#[derive(Args, Debug, Clone)]
pub struct SetArgs {
    /// Labels as key=value. `temps.sh/role` takes `builder` (build-only
    /// node) or `dedicated` (runs only environments that pin it)
    #[arg(required = true, value_name = "KEY=VALUE")]
    pub labels: Vec<String>,
    #[command(flatten)]
    pub apply: ApplyArgs,
}

#[derive(Args, Debug, Clone)]
pub struct UnsetArgs {
    /// Label keys to remove
    #[arg(required = true, value_name = "KEY")]
    pub keys: Vec<String>,
    #[command(flatten)]
    pub apply: ApplyArgs,
}

#[derive(Args, Debug, Clone)]
pub struct ApplyArgs {
    /// Do not restart the agent service if the running agent does not pick
    /// the change up by itself (agents older than this release)
    #[arg(long)]
    pub no_restart: bool,
    #[command(flatten)]
    pub target: Target,
}

impl AgentLabelsCommand {
    pub fn execute(&self) -> anyhow::Result<()> {
        match self {
            Self::List(target) => list(target),
            Self::Set(args) => {
                let wanted = node_labels::parse_labels(&args.labels)?;
                change(&args.apply, |labels| {
                    for (key, value) in &wanted {
                        labels.insert(key.clone(), serde_json::Value::String(value.clone()));
                    }
                    node_labels::check_count(labels.len())?;
                    Ok(Vec::new())
                })
            }
            Self::Unset(args) => change(&args.apply, |labels| {
                Ok(args
                    .keys
                    .iter()
                    .map(|key| key.trim())
                    .filter(|key| labels.remove(*key).is_none())
                    .map(str::to_string)
                    .collect())
            }),
        }
    }
}

type Labels = serde_json::Map<String, serde_json::Value>;

/// The worker `agent.json` belongs to, as far as this command needs it.
struct Worker {
    path: PathBuf,
    data_dir: PathBuf,
    config: serde_json::Value,
    identity: FileIdentity,
    node_id: Option<i64>,
    node_name: Option<String>,
}

impl Worker {
    fn load(target: &Target) -> anyhow::Result<Self> {
        refuse_wrong_user(target)?;
        let path = super::agent_ingress::config_path(target)
            .map_err(|error| anyhow::anyhow!("{error}{}", wrong_user_hint()))?;
        let AgentConfigFile {
            value: config,
            identity,
        } = read_agent_config(&path)?;
        let data_dir = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow::anyhow!("{} has no directory", path.display()))?;
        Ok(Self {
            node_id: config.get("node_id").and_then(serde_json::Value::as_i64),
            node_name: config
                .get("node_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            path,
            data_dir,
            config,
            identity,
        })
    }

    fn labels(&self) -> anyhow::Result<Labels> {
        saved_labels(&self.config, &self.path)
    }

    /// The agent's record of what the control plane accepted, if it is about
    /// this node (a re-join gets a new node ID).
    fn status(&self) -> Option<LabelsStatus> {
        temps_node_agent::labels::read_status(&self.data_dir)
            .filter(|status| Some(i64::from(status.node_id)) == self.node_id)
    }

    fn display_name(&self) -> String {
        self.node_name
            .clone()
            .unwrap_or_else(|| "this worker".to_string())
    }
}

/// The `labels` object of `config`; absent or null is none.
fn saved_labels(config: &serde_json::Value, path: &Path) -> anyhow::Result<Labels> {
    match config.get("labels") {
        None | Some(serde_json::Value::Null) => Ok(Labels::new()),
        Some(serde_json::Value::Object(labels)) => Ok(labels.clone()),
        Some(_) => anyhow::bail!(
            "'labels' in {} is not an object. Fix it to {{\"key\": \"value\", ...}}, then run \
             this again.",
            path.display()
        ),
    }
}

/// `key=value` lines, sorted by key so the output is stable.
fn render(labels: &Labels) -> Vec<String> {
    let mut lines: Vec<String> = labels
        .iter()
        .map(|(key, value)| match value {
            serde_json::Value::String(value) => format!("{key}={value}"),
            other => format!("{key}={other}"),
        })
        .collect();
    lines.sort();
    lines
}

fn role_line(labels: &Labels) -> &'static str {
    match labels
        .get(NODE_ROLE_LABEL)
        .and_then(serde_json::Value::as_str)
    {
        Some(node_labels::DEDICATED_NODE_ROLE) => {
            "Role: dedicated. It runs only environments that select it under Target nodes; \
             unpinned deployments, label selectors and automatic sandbox placement skip it."
        }
        Some(node_labels::BUILDER_NODE_ROLE) => {
            "Role: builder. It builds images and never hosts application replicas."
        }
        _ => "Role: ordinary worker (no temps.sh/role label).",
    }
}

fn list(target: &Target) -> anyhow::Result<()> {
    let worker = Worker::load(target)?;
    let labels = worker.labels()?;
    println!("Labels saved in {}:", worker.path.display());
    if labels.is_empty() {
        println!("  (none)");
    }
    for line in render(&labels) {
        println!("  {line}");
    }
    println!("{}", role_line(&labels));
    let saved = serde_json::Value::Object(labels);
    match worker.status() {
        Some(status) if status.origin == LabelsOrigin::Override => println!(
            "The agent that reported at {} was started with --labels or {LABELS_ENV}, which \
             override agent.json; the control plane has: {}",
            status
                .accepted_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            status.labels
        ),
        Some(status) if status.labels == saved => println!(
            "The control plane accepted these labels at {}.",
            status
                .accepted_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        Some(status) => println!(
            "Not reported yet: the control plane last accepted {} at {}. A running agent \
             reports agent.json within seconds; an agent older than this release reports it \
             only after a restart.",
            status.labels,
            status
                .accepted_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        None => println!(
            "No record of what the control plane accepted: the agent is not running, has not \
             reached the control plane yet, or is older than this release. Check the node's \
             Labels in the console under Settings > Nodes."
        ),
    }
    for found in service_overrides() {
        println!("Warning: {found}");
    }
    if let Some(note) = shell_override_note() {
        println!("Note: {note}");
    }
    Ok(())
}

/// Edit the saved labels with `edit` (which returns keys it was asked to
/// remove but found absent), write them, and bring the agent onto them.
fn change(
    args: &ApplyArgs,
    edit: impl FnOnce(&mut Labels) -> anyhow::Result<Vec<String>>,
) -> anyhow::Result<()> {
    let worker = Worker::load(&args.target)?;
    if let Some(found) = service_overrides().into_iter().next() {
        anyhow::bail!(
            "{found}. The agent would ignore labels saved in agent.json, so nothing was changed. \
             Remove the override, restart the agent, then run this again."
        );
    }
    if let Some(status) = worker
        .status()
        .filter(|status| status.origin == LabelsOrigin::Override)
    {
        anyhow::bail!(
            "The agent that last reported (at {}) was started with --labels or {LABELS_ENV}, \
             which override agent.json, so it would ignore this change; nothing was changed. \
             Restart it without them, then run this again. If that agent no longer runs, remove \
             {} first.",
            status
                .accepted_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            worker
                .data_dir
                .join(temps_node_agent::labels::STATUS_FILE)
                .display()
        );
    }
    let before = worker.labels()?;
    let mut after = before.clone();
    let absent = edit(&mut after)?;
    for key in &absent {
        println!("'{key}' is not set on {}.", worker.display_name());
    }
    if after == before {
        println!("Nothing to change: the labels are already as asked.");
        return Ok(());
    }
    let mut config = worker.config.clone();
    let object = config.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("{} is not an agent config object", worker.path.display())
    })?;
    object.insert(
        "labels".to_string(),
        serde_json::Value::Object(after.clone()),
    );
    let written_at = chrono::Utc::now();
    write_config_atomically(&worker.path, &config, worker.identity)?;
    println!("Saved to {}:", worker.path.display());
    if after.is_empty() {
        println!("  (no labels)");
    }
    for line in render(&after) {
        println!("  {line}");
    }
    println!("{}", role_line(&after));
    if let Some(note) = shell_override_note() {
        println!("Note: {note}");
    }
    apply(
        &worker,
        &serde_json::Value::Object(after),
        written_at,
        args.no_restart,
    )
}

/// What the agent's record says about labels written at `since`.
#[derive(Debug, PartialEq)]
enum Confirmation {
    /// The control plane accepted exactly these labels at this time.
    Accepted(chrono::DateTime<chrono::Utc>),
    /// A newer agent reported, but its labels are pinned by an override.
    Overridden(serde_json::Value),
    /// Nothing yet.
    Pending,
}

fn confirmation(
    status: Option<&LabelsStatus>,
    wanted: &serde_json::Value,
    since: chrono::DateTime<chrono::Utc>,
) -> Confirmation {
    match status {
        Some(status) if status.accepted_at >= since => match status.origin {
            LabelsOrigin::Override => Confirmation::Overridden(status.labels.clone()),
            LabelsOrigin::AgentJson if status.labels == *wanted => {
                Confirmation::Accepted(status.accepted_at)
            }
            LabelsOrigin::AgentJson => Confirmation::Pending,
        },
        _ => Confirmation::Pending,
    }
}

fn wait_for(
    worker: &Worker,
    wanted: &serde_json::Value,
    since: chrono::DateTime<chrono::Utc>,
    within: Duration,
) -> Confirmation {
    let deadline = Instant::now() + within;
    loop {
        let found = confirmation(worker.status().as_ref(), wanted, since);
        if found != Confirmation::Pending || Instant::now() >= deadline {
            return found;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn apply(
    worker: &Worker,
    wanted: &serde_json::Value,
    since: chrono::DateTime<chrono::Utc>,
    no_restart: bool,
) -> anyhow::Result<()> {
    // Only an agent of this release or later reloads, and it has left a
    // record after its first heartbeat. Such an agent is never restarted:
    // a restart drops the traffic it serves, and when its report is late
    // (the control plane is unreachable, say) it reports the change as soon
    // as it can on its own.
    if worker.status().is_some() {
        println!("Waiting for the running agent to report them...");
        if let Some(done) = settled(worker, wait_for(worker, wanted, since, RELOAD_WAIT))? {
            return Ok(done);
        }
        println!(
            "Not reported yet. The agent applies agent.json by itself and reports it on its \
             next successful heartbeat: if the control plane is unreachable, that is when it \
             reconnects. If the agent is not running, start it (`temps agent service status`). \
             Check with `temps agent labels list`."
        );
        console_hint(worker, wanted);
        return Ok(());
    }
    if no_restart {
        println!(
            "The running agent has not reported the change (it may be older than this release, \
             or stopped). Restart it to apply it now; it reads agent.json at start."
        );
        console_hint(worker, wanted);
        return Ok(());
    }
    use super::agent_service::{restart_after_join, ServiceAfterJoin};
    match restart_after_join(&worker.data_dir) {
        ServiceAfterJoin::Restarted(service) => {
            println!("Restarted {} so it reads the new labels.", service.name);
            if let Some(done) = settled(worker, wait_for(worker, wanted, since, RESTART_WAIT))? {
                return Ok(done);
            }
            println!(
                "The agent restarted but has not reported to the control plane yet; check its \
                 logs: {}",
                service.logs
            );
        }
        ServiceAfterJoin::NotRestarted {
            service,
            reason,
            fix,
        } => {
            println!("Warning: {} was not restarted: {reason}", service.name);
            if let Some(fix) = fix {
                println!("Once that is dealt with, run:\n  {fix}");
            }
        }
        ServiceAfterJoin::Stopped(service) => println!(
            "{} is stopped; it reports the new labels when it starts:\n  {}",
            service.name, service.start
        ),
        ServiceAfterJoin::NotInstalled => {
            println!(
                "No Temps-managed agent service runs from {}. If `temps agent` runs under your \
                 own supervisor, restart it: an agent older than this release reads its labels \
                 only at start.",
                worker.data_dir.display()
            );
        }
    }
    console_hint(worker, wanted);
    Ok(())
}

/// `Some(())` when the wait settled the change one way or the other (an
/// override is an error), `None` to try the next step.
fn settled(worker: &Worker, found: Confirmation) -> anyhow::Result<Option<()>> {
    match found {
        Confirmation::Accepted(at) => {
            println!(
                "The control plane accepted the new labels at {}.",
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            );
            console_hint(worker, &serde_json::Value::Null);
            Ok(Some(()))
        }
        Confirmation::Overridden(labels) => anyhow::bail!(
            "agent.json was saved, but the running agent was started with --labels or \
             {LABELS_ENV}, which override it, and still reports {labels}. Restart it without \
             them to use agent.json."
        ),
        Confirmation::Pending => Ok(None),
    }
}

/// Where to see the result in the console. With `wanted` (not yet
/// confirmed), what the node should show there.
fn console_hint(worker: &Worker, wanted: &serde_json::Value) {
    let node = worker.display_name();
    match wanted.as_object() {
        Some(labels) => {
            println!(
                "To check: in the console, Settings > Nodes > {node} > Scheduling should list \
                 these labels within 30 seconds of the agent running on them:"
            );
            for line in render(labels) {
                println!("  {line}");
            }
            if labels
                .get(NODE_ROLE_LABEL)
                .and_then(serde_json::Value::as_str)
                == Some(node_labels::DEDICATED_NODE_ROLE)
            {
                println!("and the node should carry a Dedicated badge.");
            }
        }
        None => println!(
            "The console shows them under Settings > Nodes > {node} > Scheduling. Workloads \
             already on the node stay where they are; the labels decide future placements."
        ),
    }
}

/// Which file was read: a write replaces only that file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileIdentity {
    dev: u64,
    ino: u64,
}

impl FileIdentity {
    #[cfg(unix)]
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    #[cfg(not(unix))]
    fn of(_meta: &std::fs::Metadata) -> Self {
        Self { dev: 0, ino: 0 }
    }
}

/// `agent.json` as read, and which file that was.
pub(super) struct AgentConfigFile {
    pub(super) value: serde_json::Value,
    pub(super) identity: FileIdentity,
}

/// Read `path` as an agent config object: a regular file, never through a
/// symlink, bounded in size. Run as root, the directory holding it and every
/// directory above must be root's alone, and so must the file: otherwise
/// another user could swap files under this command, and turn a root read
/// of another worker's config (with its node token) into a file they own.
pub(super) fn read_agent_config(path: &Path) -> anyhow::Result<AgentConfigFile> {
    if running_as_root() {
        let dir = path
            .parent()
            .and_then(|dir| std::fs::canonicalize(dir).ok())
            .ok_or_else(|| {
                anyhow::anyhow!("Could not resolve the directory of {}", path.display())
            })?;
        if let Some(why) = super::agent_service::not_root_controlled(&dir)? {
            anyhow::bail!(
                "Refusing to edit {} as root: {why}, so another user could swap files under \
                 this command. Run it as the user who owns {}, without sudo.",
                path.display(),
                dir.display()
            );
        }
    }
    let (text, meta) = temps_node_agent::labels::read_regular_file(
        path,
        temps_node_agent::labels::MAX_CONFIG_BYTES,
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "Could not read {}: {e}.{}",
            path.display(),
            wrong_user_hint()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if running_as_root() && meta.uid() != 0 {
            anyhow::bail!(
                "Refusing to edit {} as root: it is owned by uid {}. Run this as that user, \
                 without sudo.",
                path.display(),
                meta.uid()
            );
        }
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", path.display()))?;
    if !value.is_object() {
        anyhow::bail!("{} is not an agent config object", path.display());
    }
    Ok(AgentConfigFile {
        value,
        identity: FileIdentity::of(&meta),
    })
}

/// Replace the file [`read_agent_config`] read at `path` with `config`: a
/// temporary file in the same directory, owner-only, synced, then renamed
/// over it, but only if `path` is still that file.
///
/// The temporary file is created exclusively, after removing any leftover:
/// `O_EXCL` never follows a symlink, so one planted in its place is never
/// written through.
pub(super) fn write_config_atomically(
    path: &Path,
    config: &serde_json::Value,
    read: FileIdentity,
) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| anyhow::anyhow!("Could not serialize the agent config: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The umask may have taken bits off; never more than 0600.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()
    };
    if let Err(error) = write() {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!(
            "Could not write {}: {error}.{}",
            tmp.display(),
            wrong_user_hint()
        );
    }
    let unchanged = std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_file() && FileIdentity::of(&meta) == read);
    if !unchanged {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!(
            "{} changed while it was being edited, so nothing was written. Run this again.",
            path.display()
        );
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("Could not replace {}: {e}", path.display())
    })
}

#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn running_as_root() -> bool {
    false
}

/// On a Mac the agent runs as the user who joined (ADR-090): under `sudo`,
/// `~` is root's and the LaunchAgent is out of reach.
fn refuse_wrong_user(target: &Target) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") && running_as_root() && target.data_dir.is_none() {
        anyhow::bail!(
            "On a Mac the agent runs as the user who joined, with its config in that user's \
             ~/.temps. Run `temps agent labels` without sudo."
        );
    }
    Ok(())
}

/// Appended to a read or write failure: who usually owns `agent.json`.
fn wrong_user_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        " Run this as the user who joined this Mac, without sudo."
    } else if running_as_root() {
        ""
    } else {
        " The agent service usually runs as root: try again with sudo."
    }
}

/// A `TEMPS_NODE_LABELS` in this shell matters only to a `temps agent`
/// started from it, never to the service.
fn shell_override_note() -> Option<String> {
    #[allow(
        clippy::disallowed_methods,
        reason = "diagnostic only: reports an override a foreground agent would read"
    )]
    let value = std::env::var(LABELS_ENV).ok()?;
    Some(format!(
        "{LABELS_ENV}={value} is set in this shell. A `temps agent` started from here would \
         use it instead of agent.json; the installed service does not see it."
    ))
}

/// Overrides of `agent.json` labels in the installed agent service.
fn service_overrides() -> Vec<String> {
    let mut found = Vec::new();
    if cfg!(target_os = "macos") {
        if let Some(home) = dirs::home_dir() {
            let plist = super::agent_launchd::plist_path(&home);
            if let Ok(text) = std::fs::read_to_string(&plist) {
                found.extend(
                    plist_override(&text).map(|what| format!("{} {what}", plist.display())),
                );
            }
        }
    } else if cfg!(target_os = "linux") && Path::new("/run/systemd/system").is_dir() {
        found.extend(systemd_override(|args| {
            super::agent_service::systemctl_query(args)
        }));
    }
    found
}

/// What in a LaunchAgent plist overrides `agent.json` labels.
fn plist_override(plist: &str) -> Option<&'static str> {
    if plist.contains(&format!("<key>{LABELS_ENV}</key>")) {
        Some("sets TEMPS_NODE_LABELS for the agent, which overrides agent.json")
    } else if plist.contains("<string>--labels") {
        Some("starts the agent with --labels, which overrides agent.json")
    } else {
        None
    }
}

/// What in the loaded systemd unit (drop-ins and environment files
/// included) overrides `agent.json` labels. `query` runs `systemctl`.
fn systemd_override(query: impl Fn(&[&str]) -> Result<String, String>) -> Option<String> {
    let service = "temps-agent.service";
    let show = query(&[
        "show",
        service,
        "-p",
        "Environment",
        "-p",
        "ExecStart",
        "-p",
        "EnvironmentFiles",
    ])
    .ok()?;
    for line in show.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Environment"
                if value
                    .split_whitespace()
                    .any(|v| v.starts_with("TEMPS_NODE_LABELS=")) =>
            {
                return Some(format!(
                    "{service} sets {LABELS_ENV} (see `systemctl cat {service}`), which \
                     overrides agent.json"
                ));
            }
            "ExecStart"
                if value
                    .split_whitespace()
                    .any(|arg| arg == "--labels" || arg.starts_with("--labels=")) =>
            {
                return Some(format!(
                    "{service} starts the agent with --labels (see `systemctl cat {service}`), \
                     which overrides agent.json"
                ));
            }
            "EnvironmentFiles" => {
                // `/path (ignore_errors=no)` entries.
                for file in value
                    .split_whitespace()
                    .filter(|part| part.starts_with('/'))
                {
                    let sets = std::fs::read_to_string(file).is_ok_and(|text| {
                        text.lines().any(|line| {
                            line.trim_start()
                                .trim_start_matches("export ")
                                .starts_with("TEMPS_NODE_LABELS=")
                        })
                    });
                    if sets {
                        return Some(format!(
                            "{file}, an environment file of {service}, sets {LABELS_ENV}, which \
                             overrides agent.json"
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};

    fn status(labels: serde_json::Value, origin: LabelsOrigin, age_secs: i64) -> LabelsStatus {
        LabelsStatus {
            node_id: 3,
            labels,
            origin,
            accepted_at: Utc::now() - ChronoDuration::seconds(age_secs),
        }
    }

    #[test]
    fn only_a_record_of_the_new_labels_written_after_the_change_confirms_it() {
        let wanted = serde_json::json!({"temps.sh/role": "dedicated"});
        let since = Utc::now() - ChronoDuration::seconds(5);
        assert_eq!(confirmation(None, &wanted, since), Confirmation::Pending);
        // An old record of the same labels proves nothing about this write.
        let old = status(wanted.clone(), LabelsOrigin::AgentJson, 60);
        assert_eq!(
            confirmation(Some(&old), &wanted, since),
            Confirmation::Pending
        );
        let other = status(serde_json::json!({}), LabelsOrigin::AgentJson, 0);
        assert_eq!(
            confirmation(Some(&other), &wanted, since),
            Confirmation::Pending
        );
        let fresh = status(wanted.clone(), LabelsOrigin::AgentJson, 0);
        assert_eq!(
            confirmation(Some(&fresh), &wanted, since),
            Confirmation::Accepted(fresh.accepted_at)
        );
    }

    #[test]
    fn a_fresh_report_from_a_pinned_agent_is_an_override() {
        let wanted = serde_json::json!({"gpu": "true"});
        let since = Utc::now() - ChronoDuration::seconds(5);
        let pinned = status(serde_json::json!({"zone": "a"}), LabelsOrigin::Override, 0);
        assert_eq!(
            confirmation(Some(&pinned), &wanted, since),
            Confirmation::Overridden(serde_json::json!({"zone": "a"}))
        );
        let worker = Worker {
            path: PathBuf::from("/x/agent.json"),
            data_dir: PathBuf::from("/x"),
            config: serde_json::json!({}),
            identity: FileIdentity { dev: 0, ino: 0 },
            node_id: Some(3),
            node_name: None,
        };
        let error = settled(&worker, Confirmation::Overridden(serde_json::json!({})))
            .unwrap_err()
            .to_string();
        assert!(error.contains("TEMPS_NODE_LABELS"), "{error}");
    }

    /// ADR-091 D6: an agent known to be pinned refuses the change before
    /// agent.json is touched.
    #[test]
    fn a_pinned_agents_record_refuses_the_change_before_writing() {
        if running_as_root() {
            // A root run refuses a temporary directory others can write.
            eprintln!("skipped: runs as root");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        let original = r#"{"node_id":3,"labels":{"zone":"a"}}"#;
        std::fs::write(&path, original).unwrap();
        temps_node_agent::labels::record_accepted(
            dir.path(),
            &status(serde_json::json!({"zone": "a"}), LabelsOrigin::Override, 60),
        )
        .unwrap();
        let args = ApplyArgs {
            no_restart: true,
            target: Target {
                data_dir: Some(dir.path().to_path_buf()),
            },
        };
        let error = change(&args, |labels| {
            labels.insert("gpu".to_string(), serde_json::json!("true"));
            Ok(Vec::new())
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("nothing was changed"), "{error}");
        assert!(error.contains("labels-status.json"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn overrides_in_the_systemd_unit_are_found() {
        let show = |text: &'static str| move |_: &[&str]| Ok::<_, String>(text.to_string());
        assert_eq!(
            systemd_override(show(
                "LoadState=loaded\nEnvironment=TEMPS_DATA_DIR=/root/.temps\nExecStart={ path=/usr/local/bin/temps ; argv[]=/usr/local/bin/temps agent ; }\nEnvironmentFiles=\n"
            )),
            None
        );
        let env = systemd_override(show(
            "Environment=TEMPS_DATA_DIR=/root/.temps TEMPS_NODE_LABELS=zone=a\n",
        ))
        .unwrap();
        assert!(env.contains("sets TEMPS_NODE_LABELS"), "{env}");
        let flag = systemd_override(show(
            "ExecStart={ path=/usr/local/bin/temps ; argv[]=/usr/local/bin/temps agent --labels zone=a ; }\n",
        ))
        .unwrap();
        assert!(flag.contains("--labels"), "{flag}");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent.env");
        std::fs::write(&file, "RUST_LOG=info\nexport TEMPS_NODE_LABELS=zone=a\n").unwrap();
        let text = format!("EnvironmentFiles={} (ignore_errors=no)\n", file.display());
        let from_file = systemd_override(move |_: &[&str]| Ok(text.clone())).unwrap();
        assert!(from_file.contains("environment file"), "{from_file}");
        // systemctl unavailable or the unit absent: nothing to report.
        assert_eq!(systemd_override(|_: &[&str]| Err("no".to_string())), None);
    }

    #[test]
    fn overrides_in_the_launch_agent_are_found() {
        let plist = super::super::agent_launchd::render_plist(
            Path::new("/opt/temps/bin/temps"),
            Path::new("/Users/me/.temps"),
            Path::new("/usr/local/bin/docker"),
            Path::new("/Users/me/Library/Logs/Temps/agent.log"),
        )
        .unwrap();
        assert_eq!(
            plist_override(&plist),
            None,
            "the plist Temps writes pins nothing"
        );
        let with_env = plist.replace(
            "<key>PATH</key>",
            "<key>TEMPS_NODE_LABELS</key>\n    <string>zone=a</string>\n    <key>PATH</key>",
        );
        assert!(plist_override(&with_env)
            .unwrap()
            .contains("TEMPS_NODE_LABELS"));
        let with_flag = plist.replace(
            "<string>agent</string>",
            "<string>agent</string>\n    <string>--labels</string>",
        );
        assert!(plist_override(&with_flag).unwrap().contains("--labels"));
    }

    #[test]
    fn saved_labels_are_read_as_an_object() {
        let path = Path::new("/x/agent.json");
        assert!(saved_labels(&serde_json::json!({}), path)
            .unwrap()
            .is_empty());
        assert_eq!(
            saved_labels(&serde_json::json!({"labels": {"gpu": "true"}}), path)
                .unwrap()
                .len(),
            1
        );
        let error = saved_labels(&serde_json::json!({"labels": "gpu"}), path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not an object"), "{error}");
        assert_eq!(
            render(
                &saved_labels(&serde_json::json!({"labels": {"n": 1, "a": "b"}}), path).unwrap()
            ),
            vec!["a=b".to_string(), "n=1".to_string()]
        );
    }

    #[test]
    fn the_config_is_replaced_whole_owner_only_and_keeps_other_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, r#"{"token":"t","labels":{}}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            // A leftover temp file from an interrupted run is replaced.
            std::fs::write(path.with_extension("json.tmp"), "junk").unwrap();
            std::fs::set_permissions(
                path.with_extension("json.tmp"),
                std::fs::Permissions::from_mode(0o666),
            )
            .unwrap();
        }
        let file = read_agent_config(&path).unwrap();
        let mut config = file.value.clone();
        config["labels"] = serde_json::json!({"temps.sh/role": "dedicated"});
        write_config_atomically(&path, &config, file.identity).unwrap();
        let back = read_agent_config(&path).unwrap().value;
        assert_eq!(back["token"], "t");
        assert_eq!(back["labels"]["temps.sh/role"], "dedicated");
        assert!(!path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_is_never_read_or_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.json");
        std::fs::write(&victim, r#"{"token":"secret of another node"}"#).unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, "{}").unwrap();
        // A symlink where the temporary file goes is replaced, not followed.
        std::os::unix::fs::symlink(&victim, path.with_extension("json.tmp")).unwrap();
        let file = read_agent_config(&path).unwrap();
        write_config_atomically(&path, &serde_json::json!({"labels": {}}), file.identity).unwrap();
        assert!(std::fs::read_to_string(&victim)
            .unwrap()
            .contains("another node"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("labels"));
        // agent.json itself a symlink: not even read.
        let linked = dir.path().join("linked").join("agent.json");
        std::fs::create_dir(linked.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, &linked).unwrap();
        let error = read_agent_config(&linked).err().unwrap().to_string();
        assert!(error.contains("Could not read"), "{error}");
        assert!(!error.contains("another node"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_config_swapped_after_it_was_read_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, r#"{"labels":{}}"#).unwrap();
        let file = read_agent_config(&path).unwrap();
        // Someone renames another file over agent.json in between.
        let other = dir.path().join("other.json");
        std::fs::write(&other, r#"{"mine":true}"#).unwrap();
        std::fs::rename(&other, &path).unwrap();
        let error = write_config_atomically(&path, &file.value, file.identity)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("changed while it was being edited"),
            "{error}"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("mine"));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn a_failed_write_leaves_the_config_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        // The parent of the temp file does not exist: open fails.
        let path = dir.path().join("missing").join("agent.json");
        let identity = FileIdentity { dev: 0, ino: 0 };
        let error = write_config_atomically(&path, &serde_json::json!({}), identity)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Could not write"), "{error}");
    }
}
