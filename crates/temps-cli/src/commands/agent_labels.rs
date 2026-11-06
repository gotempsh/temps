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
    /// Held from the read to the write, by commands that change labels.
    _lock: Option<ConfigLock>,
    node_id: Option<i64>,
    control_plane_url: Option<String>,
    node_name: Option<String>,
}

impl Worker {
    /// `agent.json` for `target`; with `lock`, locked until dropped, for a
    /// read-edit-write.
    fn load(target: &Target, lock: bool) -> anyhow::Result<Self> {
        refuse_wrong_user(target)?;
        let path = super::agent_ingress::config_path(target)
            .map_err(|error| anyhow::anyhow!("{error}{}", wrong_user_hint()))?;
        let lock = if lock {
            Some(lock_agent_config(&path)?)
        } else {
            None
        };
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
            control_plane_url: config
                .get("control_plane_url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            node_name: config
                .get("node_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            path,
            data_dir,
            config,
            identity,
            _lock: lock,
        })
    }

    fn labels(&self) -> anyhow::Result<Labels> {
        saved_labels(&self.config, &self.path)
    }

    /// The agent's record of what the control plane accepted, if it is about
    /// this node on this control plane (a re-join gets a new node ID, and
    /// node IDs repeat across control planes).
    fn status(&self) -> Option<LabelsStatus> {
        let (status, meta) = temps_node_agent::labels::read_status_file(&self.data_dir)?;
        // Run as root, only a record root wrote counts.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if running_as_root() && meta.uid() != 0 {
                return None;
            }
        }
        #[cfg(not(unix))]
        let _ = meta;
        // A record from before it named its control plane counts by node
        // ID alone, as it always did.
        let url = |url: &str| url.trim_end_matches('/').to_string();
        Some(status).filter(|status| {
            Some(i64::from(status.node_id)) == self.node_id
                && status.control_plane_url.as_deref().is_none_or(|recorded| {
                    self.control_plane_url.as_deref().map(url) == Some(url(recorded))
                })
        })
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
    let worker = Worker::load(target, false)?;
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
    match service_overrides(&worker.data_dir) {
        Ok(found) => {
            for found in found {
                println!("Warning: {found}");
            }
        }
        Err(error) => println!("Warning: {error}."),
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
    let mut worker = Worker::load(&args.target, true)?;
    let overrides = service_overrides(&worker.data_dir).map_err(|error| {
        anyhow::anyhow!(
            "{error}, so it is not known whether the agent would use labels saved in \
             agent.json; nothing was changed. Fix that, then run this again."
        )
    })?;
    if let Some(found) = overrides.into_iter().next() {
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
    // Written: another edit may go ahead while this one waits for the agent.
    worker._lock = None;
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
             reconnects. If the agent is not running, start it (`temps agent service status`); \
             if it was downgraded to a release older than this command, restart it, since such \
             an agent reads agent.json only at start. Check with `temps agent labels list`."
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

/// Which file was read, and its size and modification time then: a write
/// replaces only that file, and only if nobody wrote to it in place since
/// (`temps join`, an editor saving in place).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct FileIdentity {
    dev: u64,
    ino: u64,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl FileIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let (dev, ino) = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let (dev, ino) = (0, 0);
        Self {
            dev,
            ino,
            len: meta.len(),
            modified: meta.modified().ok(),
        }
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
    refuse_shared_dir_as_root(path)?;
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        anyhow::bail!(
            "{} is a symbolic link, which this command does not follow or replace. Pass \
             --data-dir with the directory holding the real agent.json.",
            path.display()
        );
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

/// Run as root, refuse unless the directory holding `path` and every one
/// above it are root's alone (see [`read_agent_config`]).
fn refuse_shared_dir_as_root(path: &Path) -> anyhow::Result<()> {
    if !running_as_root() {
        return Ok(());
    }
    let dir = path
        .parent()
        .and_then(|dir| std::fs::canonicalize(dir).ok())
        .ok_or_else(|| anyhow::anyhow!("Could not resolve the directory of {}", path.display()))?;
    if let Some(why) = super::agent_service::not_root_controlled(&dir)? {
        anyhow::bail!(
            "Refusing to edit {} as root: {why}, so another user could swap files under this \
             command. Run it as the user who owns {}, without sudo.",
            path.display(),
            dir.display()
        );
    }
    Ok(())
}

/// How long a command waits for another one editing the same `agent.json`.
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// Held while a command reads, edits and replaces `agent.json`: two
/// overlapping `temps agent labels` / `temps agent ingress` runs take turns,
/// so neither loses the other's change. Released on drop.
pub(super) struct ConfigLock {
    _file: std::fs::File,
}

/// Lock `path` for a read-edit-write (an exclusive lock on
/// `agent.json.lock` beside it), waiting up to [`LOCK_WAIT`].
pub(super) fn lock_agent_config(path: &Path) -> anyhow::Result<ConfigLock> {
    lock_agent_config_within(path, LOCK_WAIT)
}

fn lock_agent_config_within(path: &Path, wait: Duration) -> anyhow::Result<ConfigLock> {
    refuse_shared_dir_as_root(path)?;
    open_config_lock(path, Some(wait))
}

/// Lock `path`, giving up after `wait`; with `None`, waiting as long as the
/// holder runs (the lock goes with the process holding it), saying so once.
fn open_config_lock(path: &Path, wait: Option<Duration>) -> anyhow::Result<ConfigLock> {
    let lock_path = path.with_extension("json.lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&lock_path).map_err(|e| {
        anyhow::anyhow!(
            "Could not open {}: {e}.{}",
            lock_path.display(),
            wrong_user_hint()
        )
    })?;
    let started = Instant::now();
    let mut told = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(ConfigLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock)
                if wait.is_none_or(|wait| started.elapsed() < wait) =>
            {
                if wait.is_none() && !told && started.elapsed() >= LOCK_WAIT {
                    told = true;
                    println!(
                        "Waiting for another `temps agent labels` or `temps agent ingress` \
                         editing {} (it holds {}): let it finish, or stop it, to continue.",
                        path.display(),
                        lock_path.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
                "Another `temps agent labels` or `temps agent ingress` is editing {} (it holds \
                 {}). Run this again when it has finished.",
                path.display(),
                lock_path.display()
            ),
            Err(std::fs::TryLockError::Error(e)) => {
                anyhow::bail!("Could not lock {}: {e}", lock_path.display())
            }
        }
    }
}

/// Replace the file [`read_agent_config`] read at `path` with `config`: a
/// temporary file in the same directory, owner-only, synced, then renamed
/// over it, but only if `path` is still that file.
///
/// The temporary file has a name of this process's own and is created
/// exclusively: `O_EXCL` never follows a symlink, so one planted in its place
/// is never written through, and another writer's file is never touched.
/// Callers hold [`lock_agent_config`] across the read and this write.
pub(super) fn write_config_atomically(
    path: &Path,
    config: &serde_json::Value,
    read: FileIdentity,
) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| anyhow::anyhow!("Could not serialize the agent config: {e}"))?;
    remove_stale_temp_files(path);
    let tmp = write_temp_file(path, &text)?;
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

/// Write `text` to a new temporary file beside `path`: a name of this
/// process's own, created exclusively (`O_EXCL` never follows a symlink
/// planted in its place), owner-only and synced. Removed again on failure.
fn write_temp_file(path: &Path, text: &str) -> anyhow::Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let tmp = path.with_extension(format!("json.{}.{unique}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        use std::io::Write;
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
    Ok(tmp)
}

/// Whether `name` is a temporary file [`write_temp_file`] makes for
/// `agent.json`: `agent.json.<pid>.<nanos>.tmp`.
fn is_config_temp_name(name: &str) -> bool {
    name.strip_prefix("agent.json.")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .and_then(|middle| middle.split_once('.'))
        .is_some_and(|(pid, nanos)| {
            [pid, nanos]
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Remove temporary files a crashed earlier run left beside `path`. Only
/// called under the config lock, so no other writer's file is in flight;
/// only regular files are removed, never what a symlink points at.
fn remove_stale_temp_files(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let stale = name.to_str().is_some_and(is_config_temp_name)
            && entry.file_type().is_ok_and(|kind| kind.is_file());
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// For `temps join`, which replaces `agent.json` with a new enrollment's
/// config (or writes the first one): under the same lock as `temps agent
/// labels` and `temps agent ingress`, through a temporary file renamed over
/// it. An overlapping edit then either finishes first or reads the new
/// file; it can never write the old enrollment back over this one.
pub(super) fn replace_agent_config(path: &Path, text: &str) -> anyhow::Result<()> {
    // The lock beside the canonical directory, where `temps agent labels`
    // and `temps agent ingress` take it.
    let dir = path
        .parent()
        .and_then(|dir| std::fs::canonicalize(dir).ok())
        .ok_or_else(|| anyhow::anyhow!("Could not resolve the directory of {}", path.display()))?;
    let canonical = dir.join("agent.json");
    // A symlinked agent.json (a config kept elsewhere) stays a link: the
    // file it points at gets the new enrollment, as before, so the agent
    // and whatever manages that file keep reading the same one.
    let target =
        if std::fs::symlink_metadata(&canonical).is_ok_and(|meta| meta.file_type().is_symlink()) {
            std::fs::canonicalize(&canonical).map_err(|e| {
                anyhow::anyhow!(
                    "{} is a symbolic link whose target cannot be resolved ({e}). Point it at the \
                 config file, or remove it, then run `temps join` again.",
                    canonical.display()
                )
            })?
        } else {
            canonical.clone()
        };
    // Never without the lock: an edit that already checked the old file
    // would rename its copy over this one. Join has registered the node, so
    // it waits for such an edit rather than give up.
    let _lock = open_config_lock(&canonical, None)?;
    remove_stale_temp_files(&canonical);
    let tmp = write_temp_file(&target, text)?;
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("Could not replace {}: {e}", target.display())
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

/// Overrides of `agent.json` labels in the installed agent service, if that
/// service runs the agent from `data_dir` (canonical): one for another
/// directory does not read this `agent.json`. `Err` when the service could
/// not be checked: a change is then refused rather than written blind.
fn service_overrides(data_dir: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    if cfg!(target_os = "macos") {
        if let Some(home) = dirs::home_dir() {
            let plist = super::agent_launchd::plist_path(&home);
            if let Ok(text) = std::fs::read_to_string(&plist) {
                let elsewhere = super::agent_launchd::installed_data_dir(&text)
                    .is_some_and(|dir| !same_dir(&dir, data_dir));
                if !elsewhere {
                    found.extend(
                        plist_override(&text).map(|what| format!("{} {what}", plist.display())),
                    );
                }
            }
        }
    } else if cfg!(target_os = "linux") && Path::new("/run/systemd/system").is_dir() {
        found.extend(systemd_override(
            super::agent_service::systemctl_query,
            data_dir,
        )?);
    }
    Ok(found)
}

/// Whether `dir` is `data_dir` (canonical), resolving `dir` when it exists.
fn same_dir(dir: &Path, data_dir: &Path) -> bool {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()) == data_dir
}

/// The words of a `systemctl show` value. systemd quotes a word holding
/// whitespace or quotes as a whole, in double quotes with `\` escapes
/// (`"TEMPS_NODE_LABELS=zone=west coast"`); single quotes are honoured too.
fn show_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => word.extend(chars.next()),
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                word.extend(chars.next());
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// The files an `EnvironmentFile=` entry names: systemd accepts a wildcard
/// pattern as well as a path.
fn environment_files(pattern: &str) -> Result<Vec<PathBuf>, String> {
    if !pattern.contains(['*', '?', '[']) {
        return Ok(vec![PathBuf::from(pattern)]);
    }
    let matches = glob::glob(pattern).map_err(|error| {
        format!("Could not expand the environment file pattern {pattern}: {error}")
    })?;
    matches
        .map(|entry| {
            entry.map_err(|error| {
                format!("Could not check a file matching the environment file pattern {pattern}: {error}")
            })
        })
        .collect()
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
/// included) overrides `agent.json` labels. `query` runs `systemctl`; a
/// failed query, or an environment file that exists but cannot be read, is
/// an `Err`: the override cannot be ruled out.
fn systemd_override(
    query: impl Fn(&[&str]) -> Result<String, String>,
    data_dir: &Path,
) -> Result<Option<String>, String> {
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
    .map_err(|error| {
        format!(
            "Could not check {service} for a {LABELS_ENV} or --labels override \
             (`systemctl show {service}` failed: {error})"
        )
    })?;
    // A unit running the agent from another directory does not read this
    // agent.json, so its labels do not override it. Without TEMPS_DATA_DIR
    // the directory is not known, and the unit is checked.
    let unit_dir = show
        .lines()
        .filter_map(|line| line.strip_prefix("Environment="))
        .flat_map(show_words)
        .find_map(|part| part.strip_prefix("TEMPS_DATA_DIR=").map(PathBuf::from));
    if unit_dir.is_some_and(|dir| !same_dir(&dir, data_dir)) {
        return Ok(None);
    }
    for line in show.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Environment"
                if show_words(value)
                    .iter()
                    .any(|part| part.starts_with("TEMPS_NODE_LABELS=")) =>
            {
                return Ok(Some(format!(
                    "{service} sets {LABELS_ENV} (see `systemctl cat {service}`), which \
                     overrides agent.json"
                )));
            }
            "ExecStart"
                if show_words(value)
                    .iter()
                    .any(|arg| arg == "--labels" || arg.starts_with("--labels=")) =>
            {
                return Ok(Some(format!(
                    "{service} starts the agent with --labels (see `systemctl cat {service}`), \
                     which overrides agent.json"
                )));
            }
            // One `EnvironmentFiles=/path (ignore_errors=no)` line per file;
            // the path may hold spaces.
            "EnvironmentFiles" => {
                let file = value
                    .rsplit_once(" (ignore_errors=")
                    .map_or(value, |(path, _)| path);
                if !file.starts_with('/') {
                    continue;
                }
                for file in environment_files(file)? {
                    let text = match std::fs::read_to_string(&file) {
                        Ok(text) => text,
                        // A missing optional file sets nothing.
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(format!(
                                "Could not read {}, an environment file of {service}, to check \
                                 it for {LABELS_ENV}: {error}.{}",
                                file.display(),
                                wrong_user_hint()
                            ))
                        }
                    };
                    let sets = text.lines().any(|line| {
                        line.trim_start()
                            .trim_start_matches("export ")
                            .trim_start()
                            .starts_with("TEMPS_NODE_LABELS=")
                    });
                    if sets {
                        return Ok(Some(format!(
                            "{}, an environment file of {service}, sets {LABELS_ENV}, which \
                             overrides agent.json",
                            file.display()
                        )));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};

    const PLANE: &str = "https://cp.example.test";

    fn status(labels: serde_json::Value, origin: LabelsOrigin, age_secs: i64) -> LabelsStatus {
        LabelsStatus {
            node_id: 3,
            control_plane_url: Some(PLANE.to_string()),
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
            identity: FileIdentity::default(),
            _lock: None,
            node_id: Some(3),
            control_plane_url: Some(PLANE.to_string()),
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
        if skip_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        let original =
            format!(r#"{{"node_id":3,"control_plane_url":"{PLANE}","labels":{{"zone":"a"}}}}"#);
        std::fs::write(&path, &original).unwrap();
        // A record from the same node ID on another control plane is not
        // this worker's.
        let mut elsewhere = status(serde_json::json!({"zone": "b"}), LabelsOrigin::AgentJson, 1);
        elsewhere.control_plane_url = Some("https://other.example.test".to_string());
        temps_node_agent::labels::record_accepted(dir.path(), &elsewhere).unwrap();
        assert!(Worker::load(
            &Target {
                data_dir: Some(dir.path().to_path_buf())
            },
            false
        )
        .unwrap()
        .status()
        .is_none());
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
    fn systemctl_show_values_split_into_words_as_systemd_quotes_them() {
        assert_eq!(
            show_words(r#"A=1 "B=two words" "C=say \"hi\"" 'D=x y'  E="#),
            ["A=1", "B=two words", r#"C=say "hi""#, "D=x y", "E="]
        );
        assert!(show_words("  ").is_empty());
    }

    #[test]
    fn overrides_in_the_systemd_unit_are_found() {
        let show = |text: &'static str| move |_: &[&str]| Ok::<_, String>(text.to_string());
        let root = Path::new("/root/.temps");
        let spaced_dir = Path::new("/srv/my temps");
        assert_eq!(
            systemd_override(show(
                "LoadState=loaded\nEnvironment=TEMPS_DATA_DIR=/root/.temps\nExecStart={ path=/usr/local/bin/temps ; argv[]=/usr/local/bin/temps agent ; }\nEnvironmentFiles=\n"
            ), root),
            Ok(None)
        );
        let env = systemd_override(
            show("Environment=TEMPS_DATA_DIR=/root/.temps TEMPS_NODE_LABELS=zone=a\n"),
            root,
        )
        .unwrap()
        .unwrap();
        assert!(env.contains("sets TEMPS_NODE_LABELS"), "{env}");
        // A unit running the agent from another directory does not read
        // this agent.json: its override is not this worker's.
        assert_eq!(
            systemd_override(
                show("Environment=TEMPS_DATA_DIR=/root/.temps TEMPS_NODE_LABELS=zone=a\n"),
                Path::new("/srv/second-worker")
            ),
            Ok(None)
        );
        // systemd quotes an assignment whose value holds a space.
        let quoted = systemd_override(show(
            "Environment=\"TEMPS_DATA_DIR=/srv/my temps\" \"TEMPS_NODE_LABELS=zone=west coast\"\n",
        ), spaced_dir)
        .unwrap()
        .unwrap();
        assert!(quoted.contains("sets TEMPS_NODE_LABELS"), "{quoted}");
        assert_eq!(
            systemd_override(
                show("Environment=\"TEMPS_DATA_DIR=/srv/my temps\"\n"),
                spaced_dir
            ),
            Ok(None)
        );
        let flag = systemd_override(show(
            "ExecStart={ path=/usr/local/bin/temps ; argv[]=/usr/local/bin/temps agent --labels zone=a ; }\n",
        ), root)
        .unwrap()
        .unwrap();
        assert!(flag.contains("--labels"), "{flag}");
        let quoted_flag = systemd_override(show(
            "ExecStart={ path=/usr/local/bin/temps ; argv[]=/usr/local/bin/temps agent \"--labels=zone=west coast\" ; }\n",
        ), root)
        .unwrap()
        .unwrap();
        assert!(quoted_flag.contains("--labels"), "{quoted_flag}");
        // An environment file, in a directory whose name holds a space.
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("unit env");
        std::fs::create_dir(&spaced).unwrap();
        let file = spaced.join("agent.env");
        std::fs::write(&file, "RUST_LOG=info\nexport TEMPS_NODE_LABELS=zone=a\n").unwrap();
        let text = format!("EnvironmentFiles={} (ignore_errors=no)\n", file.display());
        let from_file = systemd_override(move |_: &[&str]| Ok(text.clone()), root)
            .unwrap()
            .unwrap();
        assert!(from_file.contains("environment file"), "{from_file}");
        // systemd also takes a wildcard pattern: the files it matches count.
        let pattern = format!(
            "EnvironmentFiles={} (ignore_errors=yes)\n",
            spaced.join("*.env").display()
        );
        let from_pattern = systemd_override(move |_: &[&str]| Ok(pattern.clone()), root)
            .unwrap()
            .unwrap();
        assert!(from_pattern.contains("agent.env"), "{from_pattern}");
        // An optional file that does not exist sets nothing.
        let missing = format!(
            "EnvironmentFiles={} (ignore_errors=yes)\n",
            spaced.join("absent.env").display()
        );
        assert_eq!(
            systemd_override(move |_: &[&str]| Ok(missing.clone()), root),
            Ok(None)
        );
        // systemctl failing is not "no override": the change is refused.
        let error = systemd_override(|_: &[&str]| Err("timed out".to_string()), root).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
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
        if skip_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, r#"{"token":"t","labels":{}}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        // Another writer's temporary file is not this run's to touch.
        std::fs::write(path.with_extension("json.tmp"), "junk").unwrap();
        let file = read_agent_config(&path).unwrap();
        let mut config = file.value.clone();
        config["labels"] = serde_json::json!({"temps.sh/role": "dedicated"});
        write_config_atomically(&path, &config, file.identity).unwrap();
        let back = read_agent_config(&path).unwrap().value;
        assert_eq!(back["token"], "t");
        assert_eq!(back["labels"]["temps.sh/role"], "dedicated");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.tmp")).unwrap(),
            "junk"
        );
        assert_eq!(
            own_temp_files(dir.path()),
            0,
            "nothing of this run's is left"
        );
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
        if skip_as_root() {
            return;
        }
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
        assert!(error.contains("symbolic link"), "{error}");
        assert!(!error.contains("another node"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_config_swapped_after_it_was_read_is_not_replaced() {
        if skip_as_root() {
            return;
        }
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
        assert_eq!(own_temp_files(dir.path()), 0);
    }

    #[test]
    fn a_config_written_in_place_after_it_was_read_is_not_replaced() {
        if skip_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, r#"{"labels":{}}"#).unwrap();
        let file = read_agent_config(&path).unwrap();
        // Rewritten in place (same inode), as `std::fs::write` does.
        std::fs::write(&path, r#"{"labels":{},"token":"new"}"#).unwrap();
        let error = write_config_atomically(&path, &file.value, file.identity)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("changed while it was being edited"),
            "{error}"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
    }

    #[test]
    fn leftover_temp_files_are_removed_and_a_join_replaces_the_config_under_the_lock() {
        if skip_as_root() {
            return;
        }
        assert!(is_config_temp_name("agent.json.42.1700000000000000000.tmp"));
        for name in [
            "agent.json.tmp",
            "agent.json.x.1.tmp",
            "agent.json.1.2",
            "other.1.2.tmp",
        ] {
            assert!(!is_config_temp_name(name), "{name}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, r#"{"labels":{},"token":"old"}"#).unwrap();
        let stale = dir.path().join("agent.json.42.17.tmp");
        std::fs::write(&stale, "{").unwrap();
        let unrelated = dir.path().join("agent.json.notes.tmp");
        std::fs::write(&unrelated, "keep").unwrap();
        let file = read_agent_config(&path).unwrap();
        write_config_atomically(&path, &file.value, file.identity).unwrap();
        assert!(!stale.exists(), "a crashed run's temp file is removed");
        assert!(
            unrelated.exists(),
            "only this command's temp files are removed"
        );

        // An edit that read the old enrollment, then a join: the edit can no
        // longer write the old config back.
        let edit = read_agent_config(&path).unwrap();
        replace_agent_config(&path, r#"{"labels":{},"token":"new"}"#).unwrap();
        assert!(write_config_atomically(&path, &edit.value, edit.identity).is_err());
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A join waits for an edit holding the lock, past the time an edit
        // would give up, and never writes without it.
        let held = lock_agent_config(&path).unwrap();
        let writer = {
            let path = path.clone();
            std::thread::spawn(move || replace_agent_config(&path, r#"{"token":"third"}"#))
        };
        std::thread::sleep(LOCK_WAIT + Duration::from_millis(500));
        assert!(!writer.is_finished(), "join is still waiting for the lock");
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
        drop(held);
        writer.join().unwrap().unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("third"));
    }

    #[cfg(unix)]
    #[test]
    fn a_join_keeps_a_symlinked_config_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("managed");
        std::fs::create_dir(&managed).unwrap();
        let real = managed.join("worker.json");
        std::fs::write(&real, r#"{"token":"old"}"#).unwrap();
        let link = dir.path().join("agent.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        replace_agent_config(&link, r#"{"token":"new"}"#).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::read_to_string(&real).unwrap().contains("new"));
        // A dangling link is an error naming it, not a silent new file.
        std::fs::remove_file(&real).unwrap();
        let error = replace_agent_config(&link, "{}").unwrap_err().to_string();
        assert!(error.contains("symbolic link"), "{error}");
    }

    /// A root run refuses the world-writable temporary directory these tests
    /// use, by design (see `read_agent_config`).
    fn skip_as_root() -> bool {
        let root = running_as_root();
        if root {
            eprintln!("skipped: runs as root");
        }
        root
    }

    /// Temporary files [`write_config_atomically`] names after its process.
    fn own_temp_files(dir: &Path) -> usize {
        let prefix = format!("agent.json.{}.", std::process::id());
        std::fs::read_dir(dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&prefix)
            })
            .count()
    }

    #[test]
    fn overlapping_edits_take_turns_on_the_lock() {
        if skip_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, r#"{"labels":{}}"#).unwrap();
        let held = lock_agent_config(&path).unwrap();
        let error = lock_agent_config_within(&path, Duration::from_millis(300))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("is editing"), "{error}");
        drop(held);
        assert!(lock_agent_config_within(&path, Duration::from_millis(300)).is_ok());
    }

    #[test]
    fn a_failed_write_leaves_the_config_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        // The parent of the temp file does not exist: open fails.
        let path = dir.path().join("missing").join("agent.json");
        let identity = FileIdentity::default();
        let error = write_config_atomically(&path, &serde_json::json!({}), identity)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Could not write"), "{error}");
    }
}
