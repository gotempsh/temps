// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Serving private addresses through tailscaled (ADR 065 D2–D5).
//!
//! With the worker owner's consent (`--tailscale-serve` in `agent.json`),
//! and only then, the agent:
//!
//! - opens one unix socket per declared Serve entry under
//!   [`SERVE_SOCKET_DIR`] (`0700`, owned by the agent's root user), each
//!   serving its one address ([`crate::internal_proxy::spawn_serve_socket`]);
//! - points the entry's name at that socket in tailscaled's Serve config,
//!   with a conditional write, touching only handlers whose target is one of
//!   its sockets. A slot something else holds is a conflict it reports; it
//!   never sets `AllowFunnel` and never resets the config;
//! - advertises the Services its entries use, keeping every other name;
//! - takes entries being removed off, and reports `removed` or `absent`;
//! - checks each name it serves from inside (D5) and reports the result.
//!
//! Without consent it never opens tailscaled's socket (D11).

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use temps_core::tailnet::{
    self, messages, ServeEntryReport, ServeOutcome, TailnetCheck, TailnetCheckOutcome,
    TailscaleReport, TailscaleState, SERVE_HTTPS_PORT, SERVE_SOCKET_DIR, TAILSCALED_SOCKET,
};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::internal_proxy::{ServeSocket, PRIVATE_REFUSAL_HEADER};
use crate::route_store::{ServeEntry, SharedRouteStore};
use crate::tailscale_api::{
    add_entry, entry_state, port_443_holder, remove_entry, service_has_handlers, EntrySpec,
    EntryState, LocalApi, LocalApiError, ServeConfig, Status, UnixLocalApi,
};

/// How often tailscaled is re-read when nothing changed.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
/// How often the names are checked from inside when nothing changed (D5).
const CHECK_INTERVAL: Duration = Duration::from_secs(300);
/// Conditional writes tried before giving up on a pass: each `412` means
/// someone else wrote in between.
const MAX_WRITE_ATTEMPTS: usize = 3;
/// Most entries applied on one worker.
const MAX_ENTRIES: usize = 256;

/// Every write this agent makes to `AdvertiseServices` holds this lock, so
/// two of its own writers can never lose each other's update. Prefs have no
/// `Etag`, so it cannot stop another tool's write landing in between.
static ADVERTISE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Asks MagicDNS for a name.
#[async_trait]
pub trait MagicDns: Send + Sync {
    async fn resolve(&self, name: &str) -> Result<Vec<IpAddr>, String>;
}

/// tailscaled's own resolver at `100.100.100.100`, asked directly so the
/// answer does not depend on the host's resolver configuration (D5 step 4).
pub struct TailscaleResolver {
    resolver: hickory_resolver::Resolver<hickory_resolver::net::runtime::TokioRuntimeProvider>,
}

impl TailscaleResolver {
    pub fn new() -> Result<Self, String> {
        use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
        let ip: IpAddr = tailnet::MAGIC_DNS_RESOLVER
            .parse()
            .map_err(|e| format!("MagicDNS resolver address: {e}"))?;
        let mut config = ResolverConfig::default();
        config.add_name_server(NameServerConfig::udp_and_tcp(ip));
        let mut opts = ResolverOpts::default();
        opts.timeout = Duration::from_secs(3);
        opts.attempts = 2;
        opts.cache_size = 0;
        let resolver = hickory_resolver::Resolver::builder_with_config(
            config,
            hickory_resolver::net::runtime::TokioRuntimeProvider::default(),
        )
        .with_options(opts)
        .build()
        .map_err(|e| format!("MagicDNS resolver: {e}"))?;
        Ok(Self { resolver })
    }
}

#[async_trait]
impl MagicDns for TailscaleResolver {
    async fn resolve(&self, name: &str) -> Result<Vec<IpAddr>, String> {
        let fqdn = format!("{}.", tailnet::normalize_name(name));
        match self.resolver.lookup_ip(fqdn.as_str()).await {
            Ok(lookup) => {
                let mut ips: Vec<IpAddr> = lookup.iter().collect();
                ips.sort();
                ips.dedup();
                Ok(ips)
            }
            Err(error) if error.is_no_records_found() => Ok(Vec::new()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Whether this process is root, and its user name.
fn effective_user() -> (bool, String) {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let name = std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| {
            passwd.lines().find_map(|line| {
                let mut fields = line.split(':');
                let name = fields.next()?;
                let id: u32 = fields.nth(1)?.parse().ok()?;
                (id == uid).then(|| name.to_string())
            })
        })
        .unwrap_or_else(|| format!("uid {uid}"));
    (uid == 0, name)
}

/// The `svc:` form of a Service name.
fn service_key(name: &str) -> String {
    if name.starts_with("svc:") {
        name.to_string()
    } else {
        format!("svc:{name}")
    }
}

/// `current` with `drop` taken out and `add` appended, every other name kept
/// in its place.
fn merge_advertised(
    current: &[String],
    add: &BTreeSet<String>,
    drop: &BTreeSet<String>,
) -> Vec<String> {
    let mut next: Vec<String> = current
        .iter()
        .filter(|name| !drop.contains(*name))
        .cloned()
        .collect();
    for service in add {
        if !next.contains(service) {
            next.push(service.clone());
        }
    }
    next
}

struct Bound {
    socket: Arc<ServeSocket>,
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
}

impl Drop for Bound {
    fn drop(&mut self) {
        use std::os::unix::fs::FileTypeExt;
        self.stop.notify_waiters();
        self.task.abort();
        // Only the socket this bound, never anything else at the path.
        if std::fs::symlink_metadata(&self.path).is_ok_and(|meta| meta.file_type().is_socket()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What [`ServeManager::pass`] is told about the rest of the worker.
#[derive(Debug, Clone, Default)]
pub struct PassInput {
    /// `--tailscale-serve` in `agent.json`.
    pub consent: bool,
    /// The worker's display name, for messages.
    pub node: String,
    /// What the phase 2 private listener is bound to, if anything: the
    /// machine name and that listener are exclusive (D1).
    pub private_listener: Option<SocketAddr>,
}

pub struct ServeManager {
    store: SharedRouteStore,
    /// Created on the first pass with consent, never before (D11).
    api: Option<Arc<dyn LocalApi>>,
    tailscaled_socket: PathBuf,
    serve_dir: PathBuf,
    resolver: Option<Arc<dyn MagicDns>>,
    /// Overrides [`effective_user`] in tests.
    user: Option<(bool, String)>,
    sockets: HashMap<String, Bound>,
    last_entries: Option<Vec<ServeEntry>>,
    reconciled_at: Option<tokio::time::Instant>,
    checked_at: Option<tokio::time::Instant>,
    report: TailscaleReport,
    /// Who tailnet visitors are (ADR 065 D6), asked through `api` only.
    identity: Arc<crate::tailnet_identity::TailnetIdentity>,
}

impl ServeManager {
    pub fn new(store: SharedRouteStore) -> Self {
        Self {
            store,
            api: None,
            tailscaled_socket: PathBuf::from(TAILSCALED_SOCKET),
            serve_dir: PathBuf::from(SERVE_SOCKET_DIR),
            resolver: None,
            user: None,
            sockets: HashMap::new(),
            last_entries: None,
            reconciled_at: None,
            checked_at: None,
            report: TailscaleReport::default(),
            identity: Arc::new(crate::tailnet_identity::TailnetIdentity::new()),
        }
    }

    /// Shared with the private listener, which asks it for `tailnet_users`
    /// addresses (ADR 065 D6).
    pub fn identity(&self) -> Arc<crate::tailnet_identity::TailnetIdentity> {
        Arc::clone(&self.identity)
    }

    #[cfg(test)]
    fn for_test(
        store: SharedRouteStore,
        api: Arc<dyn LocalApi>,
        serve_dir: PathBuf,
        resolver: Arc<dyn MagicDns>,
    ) -> Self {
        let mut manager = Self::new(store);
        manager.api = Some(api);
        manager.serve_dir = serve_dir;
        manager.resolver = Some(resolver);
        manager.user = Some((true, "root".to_string()));
        manager
    }

    /// The socket a Serve entry points at.
    fn socket_path(&self, address_id: &str) -> Option<PathBuf> {
        let file = tailnet::serve_socket_path(address_id)?;
        Some(self.serve_dir.join(file.file_name()?))
    }

    fn target(&self, address_id: &str) -> Option<String> {
        self.socket_path(address_id)
            .map(|path| format!("unix:{}", path.display()))
    }

    /// Run one pass when something changed or it is due, and return what
    /// to report.
    pub async fn pass(&mut self, input: &PassInput) -> TailscaleReport {
        let mut entries = self.store.serve_entries();
        entries.truncate(MAX_ENTRIES);
        let changed = self.last_entries.as_ref() != Some(&entries);
        let due = self
            .reconciled_at
            .is_none_or(|at| at.elapsed() >= RECONCILE_INTERVAL);
        if changed || due || self.report.consent != input.consent {
            self.report = self.reconcile(input, &entries).await;
            self.reconciled_at = Some(tokio::time::Instant::now());
            let check_due = changed
                || self
                    .checked_at
                    .is_none_or(|at| at.elapsed() >= CHECK_INTERVAL);
            if check_due && self.report.state == TailscaleState::Ready {
                self.report.checks = self.check(input, &entries).await;
                self.checked_at = Some(tokio::time::Instant::now());
            } else if self.report.state == TailscaleState::Ready {
                // Keep the last checks between check passes.
            } else {
                self.checked_at = None;
            }
            self.last_entries = Some(entries);
        }
        self.report.clone()
    }

    fn close_all(&mut self) {
        self.sockets.clear();
    }

    async fn reconcile(&mut self, input: &PassInput, entries: &[ServeEntry]) -> TailscaleReport {
        let mut report = TailscaleReport {
            consent: input.consent,
            checks: std::mem::take(&mut self.report.checks),
            ..TailscaleReport::default()
        };
        if !input.consent {
            // D11: without consent tailscaled's socket is never opened.
            self.close_all();
            self.api = None;
            self.identity.set_api(None);
            report.checks.clear();
            report.state = TailscaleState::NoConsent;
            report.error = Some(messages::no_consent(&input.node));
            return report;
        }
        let (root, user) = self.user.clone().unwrap_or_else(effective_user);
        report.root = root;
        report.user = Some(user.clone());
        if !root {
            self.identity.set_api(None);
            self.close_all();
            report.checks.clear();
            report.state = TailscaleState::NotRoot;
            report.error = Some(messages::not_root(&input.node, &user));
            return report;
        }
        let api = self
            .api
            .get_or_insert_with(|| Arc::new(UnixLocalApi::new(&self.tailscaled_socket)))
            .clone();
        self.identity.set_api(Some(Arc::clone(&api)));

        let status = match api.status().await {
            Ok(status) => status,
            Err(error) => {
                report.checks.clear();
                self.fail(&mut report, input, error, None);
                return report;
            }
        };
        // Who the worker itself is, so identity checks never admit it.
        self.identity
            .set_self(status.self_node.as_ref().map(|node| {
                crate::tailnet_identity::SelfNode {
                    stable_id: node.id.clone(),
                    ips: node
                        .tailscale_ips
                        .iter()
                        .flatten()
                        .filter_map(|ip| ip.parse().ok())
                        .collect(),
                }
            }));
        report.version = Some(status.version.clone()).filter(|v| !v.is_empty());
        report.backend_state = Some(status.backend_state.clone());
        if !tailnet::version_supported(&status.version) {
            report.checks.clear();
            report.state = TailscaleState::Unsupported;
            report.error = Some(messages::old_tailscaled(
                tailnet::parse_version(&status.version)
                    .map(|(major, minor)| format!("{major}.{minor}"))
                    .as_deref()
                    .unwrap_or(&status.version),
                &input.node,
            ));
            return report;
        }
        fill_facts(&mut report, &status);
        report.state = TailscaleState::Ready;
        if status.backend_state != "Running" {
            report.error = Some(messages::not_logged_in(&input.node, &status.backend_state));
            return report;
        }
        let Some(machine) = report.dns_name.clone() else {
            report.state = TailscaleState::NotUnderstood;
            report.error = Some(messages::not_understood(&status.version, &input.node));
            return report;
        };

        if let Err(error) = self.ensure_serve_dir() {
            report.error = Some(format!(
                "Could not prepare {} for the Serve sockets: {error}",
                self.serve_dir.display()
            ));
            return report;
        }
        // Sockets first, so tailscaled never points at one that is not there.
        self.bind_sockets(entries).await;

        let listener_on_tailscale = input
            .private_listener
            .filter(|bound| bound.port() == SERVE_HTTPS_PORT)
            .filter(|bound| {
                report
                    .addresses
                    .iter()
                    .any(|ip| ip.parse::<IpAddr>().ok() == Some(bound.ip()))
            });

        match self
            .apply(&*api, input, entries, &machine, listener_on_tailscale)
            .await
        {
            Ok((config, outcomes)) => {
                report.port_443_held_by =
                    port_443_holder(&config, &machine, &self.serve_dir.display().to_string());
                report.port_443_funnel = config
                    .allow_funnel
                    .get(&format!("{machine}:{SERVE_HTTPS_PORT}"))
                    .copied()
                    .unwrap_or(false);
                report.entries = outcomes;
            }
            Err(error) => {
                match error {
                    LocalApiError::NoSocket { .. } | LocalApiError::NotUnderstood { .. } => {
                        self.fail(&mut report, input, error, Some(&status.version));
                    }
                    // tailscaled runs; this pass's write did not land.
                    error @ (LocalApiError::Transport { .. } | LocalApiError::Status { .. }) => {
                        report.error = Some(format!(
                            "Could not update the Serve config on `{}`: {error}",
                            input.node
                        ));
                    }
                }
                report.entries = entries
                    .iter()
                    .map(|entry| ServeEntryReport {
                        entry_id: entry.entry_id,
                        address_id: entry.address_id.clone(),
                        outcome: ServeOutcome::Error,
                        etag: None,
                        detail: report.error.clone(),
                        funnel: false,
                    })
                    .collect();
            }
        }
        // Funnel on an entry: its socket refuses everything (D3).
        for outcome in &mut report.entries {
            if let Some(bound) = self.sockets.get(&outcome.address_id) {
                bound
                    .socket
                    .funnel_on
                    .store(outcome.funnel, Ordering::Release);
                if bound.socket.funnel_seen.swap(false, Ordering::AcqRel) {
                    outcome.funnel = true;
                }
            }
        }
        report
    }

    fn fail(
        &self,
        report: &mut TailscaleReport,
        input: &PassInput,
        error: LocalApiError,
        version: Option<&str>,
    ) {
        match error {
            LocalApiError::NoSocket { .. } => {
                report.state = TailscaleState::NoSocket;
                report.error = Some(messages::no_socket(&input.node));
            }
            LocalApiError::NotUnderstood { call, reason } => {
                warn!(call, %reason, "tailscaled answered in a way this agent does not understand");
                report.state = TailscaleState::NotUnderstood;
                report.error = Some(messages::not_understood(
                    version.unwrap_or("(unknown version)"),
                    &input.node,
                ));
            }
            error @ (LocalApiError::Transport { .. } | LocalApiError::Status { .. }) => {
                report.state = TailscaleState::NoSocket;
                report.error = Some(format!(
                    "Tailscale on `{}` did not answer: {error}",
                    input.node
                ));
            }
        }
    }

    /// The socket directory: created `0700`, and refused when another user
    /// owns it, since whoever owns it could connect to the sockets.
    fn ensure_serve_dir(&self) -> Result<(), String> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::create_dir_all(&self.serve_dir).map_err(|e| e.to_string())?;
        let meta = std::fs::symlink_metadata(&self.serve_dir).map_err(|e| e.to_string())?;
        if !meta.is_dir() {
            return Err("it is not a directory".to_string());
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        if meta.uid() != uid {
            return Err(format!("it is owned by uid {}, not {uid}", meta.uid()));
        }
        if meta.mode() & 0o777 != 0o700 {
            std::fs::set_permissions(&self.serve_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn bind_sockets(&mut self, entries: &[ServeEntry]) {
        let wanted: BTreeSet<&str> = entries
            .iter()
            .filter(|entry| !entry.removing)
            .map(|entry| entry.address_id.as_str())
            .collect();
        self.sockets
            .retain(|address_id, _| wanted.contains(address_id.as_str()));
        for address_id in wanted {
            if self
                .sockets
                .get(address_id)
                .is_some_and(|bound| !bound.task.is_finished())
            {
                continue;
            }
            let Some(path) = self.socket_path(address_id) else {
                continue;
            };
            // A finished task's `Bound` must go before the replacement binds
            // the same path: dropping it removes the socket file there.
            self.sockets.remove(address_id);
            let socket = Arc::new(ServeSocket::new(address_id));
            let stop = Arc::new(Notify::new());
            match crate::internal_proxy::spawn_serve_socket(
                &path,
                self.store.clone(),
                socket.clone(),
                self.identity(),
                stop.clone(),
            )
            .await
            {
                Ok(task) => {
                    info!(%address_id, path = %path.display(), "Serve socket listening");
                    self.sockets.insert(
                        address_id.to_string(),
                        Bound {
                            socket,
                            stop,
                            task,
                            path,
                        },
                    );
                }
                Err(error) => {
                    warn!(%address_id, path = %path.display(), %error, "could not open the Serve socket");
                }
            }
        }
    }

    /// Bring tailscaled's Serve config in line with the entries, with a
    /// conditional write, and say what happened to each.
    async fn apply(
        &self,
        api: &dyn LocalApi,
        input: &PassInput,
        entries: &[ServeEntry],
        machine: &str,
        listener_on_tailscale: Option<SocketAddr>,
    ) -> Result<(ServeConfig, Vec<ServeEntryReport>), LocalApiError> {
        for _ in 0..MAX_WRITE_ATTEMPTS {
            let (mut config, etag) = api.serve_config().await?;
            let mut outcomes = Vec::with_capacity(entries.len());
            let mut changed = false;
            for entry in entries {
                let mut outcome = ServeEntryReport {
                    entry_id: entry.entry_id,
                    address_id: entry.address_id.clone(),
                    outcome: ServeOutcome::Error,
                    etag: None,
                    detail: None,
                    funnel: false,
                };
                let Some(target) = self.target(&entry.address_id) else {
                    outcome.detail = Some(format!(
                        "`{}` is not an address id this agent can serve",
                        entry.address_id
                    ));
                    outcomes.push(outcome);
                    continue;
                };
                let service = entry.service.as_deref().map(service_key);
                let hostname = tailnet::normalize_name(&entry.hostname);
                // A machine-name entry is only ever the machine's own name.
                if service.is_none() && hostname != machine {
                    outcome.outcome = if entry.removing {
                        ServeOutcome::Absent
                    } else {
                        ServeOutcome::Conflict
                    };
                    outcome.detail = Some(format!(
                        "`{hostname}` is not this machine's name `{machine}`"
                    ));
                    outcomes.push(outcome);
                    continue;
                }
                let spec = EntrySpec {
                    hostname,
                    service,
                    target,
                    socket_dir: self.serve_dir.display().to_string(),
                };
                let state = entry_state(&config, &spec);
                match (entry.removing, state) {
                    (true, EntryState::Present { .. }) => {
                        remove_entry(&mut config, &spec);
                        changed = true;
                        outcome.outcome = ServeOutcome::Removed;
                    }
                    (true, EntryState::Free) => outcome.outcome = ServeOutcome::Absent,
                    (true, EntryState::Taken(what)) => {
                        // The agent wrote it, and now something else holds
                        // the slot: nothing is removed, and the user decides.
                        if entry.applied_etag.is_some() {
                            outcome.outcome = ServeOutcome::Conflict;
                            outcome.detail = Some(what);
                        } else {
                            outcome.outcome = ServeOutcome::Absent;
                        }
                    }
                    (false, _) if spec.service.is_none() && listener_on_tailscale.is_some() => {
                        outcome.outcome = ServeOutcome::Conflict;
                        outcome.detail = Some(messages::listener_exclusive(
                            &input.node,
                            &listener_on_tailscale
                                .map(|bound| bound.to_string())
                                .unwrap_or_default(),
                        ));
                    }
                    (false, EntryState::Present { funnel }) => {
                        outcome.outcome = ServeOutcome::Applied;
                        outcome.funnel = funnel;
                    }
                    (false, EntryState::Free) => {
                        add_entry(&mut config, &spec);
                        changed = true;
                        outcome.outcome = ServeOutcome::Applied;
                    }
                    (false, EntryState::Taken(what)) => {
                        outcome.outcome = ServeOutcome::Conflict;
                        outcome.detail = Some(what);
                    }
                }
                outcomes.push(outcome);
            }

            let etag = if changed {
                if !api.set_serve_config(&config, &etag).await? {
                    // Lost a race: read again and redo the pass.
                    continue;
                }
                let (reread, etag) = api.serve_config().await?;
                config = reread;
                etag
            } else {
                etag
            };
            for outcome in &mut outcomes {
                if outcome.outcome == ServeOutcome::Applied {
                    outcome.etag = Some(etag.clone());
                }
            }
            self.advertise(api, entries, &config, &mut outcomes).await;
            return Ok((config, outcomes));
        }
        Err(LocalApiError::Transport {
            call: "serve-config write",
            reason: format!(
                "the Serve config kept changing; {MAX_WRITE_ATTEMPTS} conditional writes lost the race"
            ),
        })
    }

    /// Add the Services this worker serves to `AdvertiseServices`, and drop
    /// the ones whose last handler it removed. Every other name is kept.
    ///
    /// Prefs have no `Etag`, so this is a read-merge-write under
    /// [`ADVERTISE_LOCK`], followed by a read back. When the read back lacks
    /// a name this pass added, another writer overwrote the list after this
    /// write; the list is merged again from what it now says, up to
    /// [`MAX_WRITE_ATTEMPTS`] times. A name another writer removed is never
    /// put back: the agent cannot tell that from a deliberate removal. What
    /// remains is a write landing between this read and this write, which
    /// can lose the other writer's change without leaving a trace here.
    async fn advertise(
        &self,
        api: &dyn LocalApi,
        entries: &[ServeEntry],
        config: &ServeConfig,
        outcomes: &mut [ServeEntryReport],
    ) {
        let mut add = BTreeSet::new();
        let mut drop = BTreeSet::new();
        for (entry, outcome) in entries.iter().zip(outcomes.iter()) {
            let Some(service) = entry.service.as_deref().map(service_key) else {
                continue;
            };
            match (entry.removing, outcome.outcome) {
                (false, ServeOutcome::Applied) => {
                    add.insert(service);
                }
                (true, ServeOutcome::Removed | ServeOutcome::Absent)
                    if !service_has_handlers(config, &service) =>
                {
                    drop.insert(service);
                }
                _ => {}
            }
        }
        drop.retain(|service| !add.contains(service));
        if add.is_empty() && drop.is_empty() {
            return;
        }
        let result = async {
            let _writer = ADVERTISE_LOCK.lock().await;
            let mut current = api.prefs().await?.advertise_services.unwrap_or_default();
            for _ in 0..MAX_WRITE_ATTEMPTS {
                let next = merge_advertised(&current, &add, &drop);
                if next == current {
                    return Ok(current);
                }
                api.set_advertise_services(&next).await?;
                current = api.prefs().await?.advertise_services.unwrap_or_default();
                if add.iter().all(|service| current.contains(service)) {
                    // A dropped name still there is reported below, not
                    // removed again: another writer may have put it back.
                    return Ok(current);
                }
                warn!(
                    advertised = ?current,
                    "another writer replaced AdvertiseServices after this write; merging again"
                );
            }
            Err(LocalApiError::Transport {
                call: "prefs write",
                reason: format!(
                    "AdvertiseServices kept changing; {MAX_WRITE_ATTEMPTS} writes were overwritten"
                ),
            })
        }
        .await;
        // An entry taken off is only reported removed once its name is no
        // longer advertised: the control plane deletes the row on `removed`,
        // and nothing would drop the name after that.
        let still_advertised = |service: &str| match &result {
            Ok(current) => current.iter().any(|name| name == service),
            Err(_) => true,
        };
        if let Err(error) = &result {
            warn!(%error, "could not update the advertised Tailscale Services");
        }
        for (entry, outcome) in entries.iter().zip(outcomes.iter_mut()) {
            let Some(service) = entry.service.as_deref().map(service_key) else {
                continue;
            };
            match &result {
                Err(error) if !entry.removing => {
                    outcome.outcome = ServeOutcome::Error;
                    outcome.detail = Some(format!("advertising the Service failed: {error}"));
                }
                _ if entry.removing && drop.contains(&service) && still_advertised(&service) => {
                    outcome.outcome = ServeOutcome::Error;
                    outcome.detail = Some(match &result {
                        Err(error) => {
                            format!("removing {service} from AdvertiseServices failed: {error}")
                        }
                        Ok(_) => format!(
                            "{service} is still in AdvertiseServices after this worker removed it; \
                             another writer may have added it back"
                        ),
                    });
                }
                _ => {}
            }
        }
    }

    /// The inside checks (D5) of every name applied here.
    async fn check(&self, input: &PassInput, entries: &[ServeEntry]) -> Vec<TailnetCheck> {
        let resolver = match &self.resolver {
            Some(resolver) => Some(resolver.clone()),
            None => TailscaleResolver::new()
                .map(|r| Arc::new(r) as Arc<dyn MagicDns>)
                .map_err(|error| warn!(%error, "MagicDNS resolver unavailable"))
                .ok(),
        };
        let mut checks = Vec::new();
        for entry in entries.iter().filter(|entry| !entry.removing) {
            let Some(outcome) = self
                .report
                .entries
                .iter()
                .find(|outcome| outcome.entry_id == entry.entry_id)
                .cloned()
            else {
                continue;
            };
            checks.push(
                self.check_one(input, entry, &outcome, resolver.as_deref())
                    .await,
            );
        }
        checks
    }

    async fn check_one(
        &self,
        input: &PassInput,
        entry: &ServeEntry,
        outcome: &ServeEntryReport,
        resolver: Option<&dyn MagicDns>,
    ) -> TailnetCheck {
        let report = &self.report;
        let hostname = tailnet::normalize_name(&entry.hostname);
        let result = |outcome, message: String| TailnetCheck {
            address_id: entry.address_id.clone(),
            hostname: hostname.clone(),
            outcome,
            message,
        };
        let tailnet_name = report.tailnet.clone().unwrap_or_default();
        // 1. tailscaled runs and is supported.
        if report.backend_state.as_deref() != Some("Running") {
            return result(
                TailnetCheckOutcome::Tailscale,
                messages::not_logged_in(
                    &input.node,
                    report.backend_state.as_deref().unwrap_or("unknown"),
                ),
            );
        }
        // 2. MagicDNS and HTTPS, and the name is covered.
        if !report.magic_dns {
            return result(
                TailnetCheckOutcome::Certificates,
                messages::magic_dns_off(&tailnet_name),
            );
        }
        let covered = if entry.service.is_some() {
            report.https()
                && !tailnet_name.is_empty()
                && hostname.ends_with(&format!(".{tailnet_name}"))
        } else {
            report
                .cert_domains
                .iter()
                .any(|domain| tailnet::normalize_name(domain) == hostname)
        };
        if !covered {
            return result(
                TailnetCheckOutcome::Certificates,
                messages::https_off(&tailnet_name, &hostname),
            );
        }
        // 3. The entry is the agent's, on its socket, without Funnel.
        match outcome.outcome {
            ServeOutcome::Applied if outcome.funnel => {
                return result(TailnetCheckOutcome::Funnel, messages::funnel(&input.node))
            }
            ServeOutcome::Applied => {}
            ServeOutcome::Conflict => {
                return result(
                    TailnetCheckOutcome::Entry,
                    messages::slot_taken(
                        &hostname,
                        outcome.detail.as_deref().unwrap_or("another handler"),
                    ),
                )
            }
            ServeOutcome::Error | ServeOutcome::Removed | ServeOutcome::Absent => {
                return result(
                    TailnetCheckOutcome::Entry,
                    format!(
                        "The Serve entry for `{hostname}` is not in place on `{}`: {}",
                        input.node,
                        outcome.detail.as_deref().unwrap_or("no reason given")
                    ),
                )
            }
        }
        // 4. MagicDNS answers with this machine (or the Service).
        let expected: Vec<IpAddr> = if entry.service.is_some() {
            entry
                .addrs
                .iter()
                .filter_map(|ip| ip.parse().ok())
                .collect()
        } else {
            report
                .addresses
                .iter()
                .filter_map(|ip| ip.parse().ok())
                .collect()
        };
        let Some(resolver) = resolver else {
            return result(
                TailnetCheckOutcome::NotResolved,
                format!(
                    "MagicDNS could not be asked about `{hostname}` from `{}`.",
                    input.node
                ),
            );
        };
        match resolver.resolve(&hostname).await {
            Ok(answers) if answers.is_empty() => {
                return result(
                    TailnetCheckOutcome::NotResolved,
                    format!(
                        "MagicDNS does not resolve `{hostname}`. Check MagicDNS is on for \
                         tailnet `{tailnet_name}`."
                    ),
                )
            }
            Ok(answers) => {
                let wrong: Vec<String> = answers
                    .iter()
                    .filter(|ip| !tailnet::is_tailscale_ip(**ip) || !expected.contains(ip))
                    .map(ToString::to_string)
                    .collect();
                if !wrong.is_empty() {
                    let shown: Vec<String> = expected.iter().map(ToString::to_string).collect();
                    return result(
                        TailnetCheckOutcome::ResolvesElsewhere,
                        format!(
                            "MagicDNS resolves `{hostname}` to {}, not {}.",
                            wrong.join(", "),
                            if shown.is_empty() {
                                "an address Temps recorded".to_string()
                            } else {
                                shown.join(", ")
                            }
                        ),
                    );
                }
            }
            Err(error) => {
                return result(
                    TailnetCheckOutcome::NotResolved,
                    format!("MagicDNS did not answer for `{hostname}`: {error}"),
                )
            }
        }
        // 5. The socket is bound and routes the address.
        let Some(path) = self.socket_path(&entry.address_id) else {
            return result(
                TailnetCheckOutcome::NoRoute,
                format!("`{}` has no Serve socket", entry.address_id),
            );
        };
        match probe_socket(&path, &hostname).await {
            Ok(answered) if answered == entry.address_id => result(
                TailnetCheckOutcome::Reachable,
                format!(
                    "`{hostname}` is served on `{}` through Tailscale.",
                    input.node
                ),
            ),
            Ok(other) => result(
                TailnetCheckOutcome::NoRoute,
                format!(
                    "The Serve socket for `{hostname}` answered for `{other}`, not `{}`.",
                    entry.address_id
                ),
            ),
            Err(error) => result(
                TailnetCheckOutcome::NoRoute,
                format!("The Serve socket for `{hostname}` does not route it: {error}"),
            ),
        }
    }
}

fn fill_facts(report: &mut TailscaleReport, status: &Status) {
    let me = status.self_node.clone().unwrap_or_default();
    report.dns_name = Some(tailnet::normalize_name(&me.dns_name)).filter(|n| !n.is_empty());
    report.node_id = Some(me.id.trim().to_string()).filter(|id| !id.is_empty());
    let current = status.current_tailnet.clone().unwrap_or_default();
    report.tailnet =
        Some(tailnet::normalize_name(&current.magic_dns_suffix)).filter(|n| !n.is_empty());
    report.magic_dns = current.magic_dns_enabled;
    report.cert_domains = status
        .cert_domains
        .clone()
        .unwrap_or_default()
        .iter()
        .map(|d| tailnet::normalize_name(d))
        .collect();
    report.tags = me.tags.clone().unwrap_or_default();
    report.login = status.login();
    report.addresses = me
        .tailscale_ips
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|ip| ip.parse::<IpAddr>().is_ok_and(tailnet::is_tailscale_ip))
        .collect();
}

/// Ask a Serve socket for its own answer (D5 step 5): which address it
/// routes, if it routes one.
async fn probe_socket(path: &Path, hostname: &str) -> Result<String, String> {
    let (status, headers, body) = crate::tailscale_api::unix_call(
        path,
        "serve probe",
        http::Method::GET,
        tailnet::SERVE_PROBE_PATH,
        hostname,
        &[],
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    let marked = headers
        .get(PRIVATE_REFUSAL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    match (status.as_u16(), marked.as_deref()) {
        (200, Some("probe")) => Ok(String::from_utf8_lossy(&body).trim().to_string()),
        (_, Some(reason)) => Err(format!("it refused with `{reason}` (HTTP {status})")),
        _ => Err(format!("HTTP {status} without the agent's answer")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_store::{RouteBackend, RouteEntry, RouteStore};
    use crate::tailscale_api::fake;

    struct FixedDns(Vec<IpAddr>);

    #[async_trait]
    impl MagicDns for FixedDns {
        async fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, String> {
            Ok(self.0.clone())
        }
    }

    fn entry(id: i32, address_id: &str, hostname: &str) -> ServeEntry {
        ServeEntry {
            entry_id: id,
            address_id: address_id.into(),
            hostname: hostname.into(),
            via: "tailscale_serve".into(),
            service: None,
            removing: false,
            applied_etag: None,
            route: Some(RouteEntry {
                host: hostname.into(),
                backends: vec![RouteBackend {
                    address: "127.0.0.1:9".into(),
                    container_id: None,
                    container_name: None,
                }],
                deployment_id: Some(1),
                project_id: Some(1),
                environment_id: Some(1),
            }),
            addrs: Vec::new(),
            principals: None,
        }
    }

    fn store_with(entries: Vec<ServeEntry>) -> SharedRouteStore {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(RouteStore::new(dir.path().join("routes.json")));
        store.apply_public_snapshot(crate::route_store::PublicIngressSnapshot {
            enabled: true,
            serve_entries: entries,
            ..Default::default()
        });
        std::mem::forget(dir);
        store
    }

    fn input() -> PassInput {
        PassInput {
            consent: true,
            node: "master".into(),
            private_listener: None,
        }
    }

    async fn manager(
        state: fake::FakeState,
        entries: Vec<ServeEntry>,
    ) -> (ServeManager, fake::Shared, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (socket, shared) = fake::start(dir.path(), state).await;
        let serve_dir = dir.path().join("serve");
        let manager = ServeManager::for_test(
            store_with(entries),
            Arc::new(UnixLocalApi::new(socket)),
            serve_dir,
            Arc::new(FixedDns(vec!["100.101.102.103".parse().unwrap()])),
        );
        (manager, shared, dir)
    }

    #[tokio::test]
    async fn without_consent_tailscaled_is_never_asked() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = ServeManager::new(store_with(vec![entry(
            1,
            "custom:12",
            "master.example.ts.net",
        )]));
        // A socket path that would fail loudly if it were opened.
        manager.tailscaled_socket = dir.path().join("must-not-open.sock");
        let report = manager
            .pass(&PassInput {
                consent: false,
                ..input()
            })
            .await;
        assert_eq!(report.state, TailscaleState::NoConsent);
        assert!(manager.api.is_none());
        assert!(report.entries.is_empty());
        assert_eq!(
            report.error.as_deref(),
            Some(messages::no_consent("master").as_str())
        );
    }

    #[tokio::test]
    async fn a_non_root_agent_says_so_and_touches_nothing() {
        let (mut manager, shared, _dir) = manager(
            fake::running(),
            vec![entry(1, "custom:12", "master.example.ts.net")],
        )
        .await;
        manager.user = Some((false, "temps".into()));
        let report = manager.pass(&input()).await;
        assert_eq!(report.state, TailscaleState::NotRoot);
        assert_eq!(
            report.error.as_deref(),
            Some(messages::not_root("master", "temps").as_str())
        );
        assert_eq!(shared.lock().writes, 0);
    }

    #[tokio::test]
    async fn an_old_tailscaled_is_unsupported() {
        let mut state = fake::running();
        state.status["Version"] = "1.90.4-tabc".into();
        let (mut manager, shared, _dir) =
            manager(state, vec![entry(1, "custom:12", "master.example.ts.net")]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.state, TailscaleState::Unsupported);
        assert_eq!(
            report.error.as_deref(),
            Some(messages::old_tailscaled("1.90", "master").as_str())
        );
        assert_eq!(shared.lock().writes, 0);
    }

    #[tokio::test]
    async fn an_unparseable_answer_stops_rather_than_guesses() {
        let mut state = fake::running();
        state.garbage = Some("{\"BackendState\": 7}".into());
        let (mut manager, shared, _dir) =
            manager(state, vec![entry(1, "custom:12", "master.example.ts.net")]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.state, TailscaleState::NotUnderstood);
        assert!(report.entries.is_empty());
        assert_eq!(shared.lock().writes, 0);
    }

    #[tokio::test]
    async fn the_machine_name_is_applied_checked_and_removed() {
        let (mut manager, shared, dir) = manager(
            fake::running(),
            vec![entry(1, "custom:12", "master.example.ts.net")],
        )
        .await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.state, TailscaleState::Ready, "{report:?}");
        assert!(report.serve_ready());
        assert_eq!(report.entries.len(), 1);
        let applied = &report.entries[0];
        assert_eq!(applied.outcome, ServeOutcome::Applied, "{applied:?}");
        assert_eq!(applied.etag.as_deref(), Some("\"v1\""));
        let target = format!(
            "unix:{}",
            dir.path().join("serve").join("custom-12.sock").display()
        );
        assert_eq!(
            shared.lock().config,
            serde_json::json!({
                "TCP": {"443": {"HTTPS": true}},
                "Web": {"master.example.ts.net:443": {"Handlers": {"/": {"Proxy": target}}}}
            })
        );
        // The directory is the agent's alone.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("serve"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // Inside check: MagicDNS answers with this machine; the socket routes it.
        assert_eq!(report.checks.len(), 1);
        assert_eq!(
            report.checks[0].outcome,
            TailnetCheckOutcome::Reachable,
            "{:?}",
            report.checks[0]
        );

        // A second pass with nothing changed writes nothing.
        manager.reconciled_at = None;
        let again = manager.pass(&input()).await;
        assert_eq!(again.entries[0].outcome, ServeOutcome::Applied);
        assert_eq!(shared.lock().writes, 1);

        // Removal takes only the agent's handler off.
        let mut removing = entry(1, "custom:12", "master.example.ts.net");
        removing.removing = true;
        removing.applied_etag = Some("\"v1\"".into());
        removing.route = None;
        manager
            .store
            .apply_public_snapshot(crate::route_store::PublicIngressSnapshot {
                enabled: true,
                serve_entries: vec![removing.clone()],
                ..Default::default()
            });
        let removed = manager.pass(&input()).await;
        assert_eq!(removed.entries[0].outcome, ServeOutcome::Removed);
        assert_eq!(shared.lock().config, serde_json::json!({}));
        assert!(!dir.path().join("serve").join("custom-12.sock").exists());
        // Already gone: absent.
        manager.reconciled_at = None;
        let absent = manager.pass(&input()).await;
        assert_eq!(absent.entries[0].outcome, ServeOutcome::Absent);
    }

    #[tokio::test]
    async fn a_slot_someone_else_holds_is_a_conflict_and_is_never_written() {
        let mut state = fake::running();
        let foreign = serde_json::json!({
            "TCP": {"443": {"HTTPS": true}},
            "Web": {"master.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000"}}}}
        });
        state.config = foreign.clone();
        let (mut manager, shared, _dir) =
            manager(state, vec![entry(1, "custom:12", "master.example.ts.net")]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Conflict);
        assert_eq!(
            report.entries[0].detail.as_deref(),
            Some("http://127.0.0.1:3000")
        );
        assert_eq!(
            report.port_443_held_by.as_deref(),
            Some("http://127.0.0.1:3000")
        );
        assert_eq!(shared.lock().writes, 0);
        assert_eq!(shared.lock().config, foreign);

        // Removing an entry it never wrote touches nothing either.
        let mut removing = entry(1, "custom:12", "master.example.ts.net");
        removing.removing = true;
        manager
            .store
            .apply_public_snapshot(crate::route_store::PublicIngressSnapshot {
                enabled: true,
                serve_entries: vec![removing.clone()],
                ..Default::default()
            });
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Absent);
        // ...and one it wrote, now held by someone else, is a conflict.
        removing.applied_etag = Some("\"v1\"".into());
        manager
            .store
            .apply_public_snapshot(crate::route_store::PublicIngressSnapshot {
                enabled: true,
                serve_entries: vec![removing],
                ..Default::default()
            });
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Conflict);
        assert_eq!(shared.lock().writes, 0);
        assert_eq!(shared.lock().config, foreign);
    }

    #[tokio::test]
    async fn a_lost_race_is_read_again_and_reapplied() {
        let mut state = fake::running();
        state.lose_races = 2;
        let (mut winning, shared, _dir) =
            manager(state, vec![entry(1, "custom:12", "master.example.ts.net")]).await;
        let report = winning.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Applied);
        assert_eq!(shared.lock().writes, 1);

        let mut state = fake::running();
        state.lose_races = 10;
        let (mut losing, shared, _dir) =
            manager(state, vec![entry(1, "custom:12", "master.example.ts.net")]).await;
        let report = losing.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Error);
        assert_eq!(shared.lock().writes, 0);
    }

    #[tokio::test]
    async fn funnel_on_the_entry_is_reported_and_its_socket_refuses() {
        let (mut manager, shared, dir) = manager(
            fake::running(),
            vec![entry(1, "custom:12", "master.example.ts.net")],
        )
        .await;
        manager.pass(&input()).await;
        shared.lock().config["AllowFunnel"] =
            serde_json::json!({"master.example.ts.net:443": true});
        shared.lock().version += 1;
        manager.reconciled_at = None;
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Applied);
        assert!(report.entries[0].funnel);
        assert!(report.port_443_funnel);
        // The agent never turns Funnel off, nor on.
        assert_eq!(
            shared.lock().config["AllowFunnel"]["master.example.ts.net:443"],
            serde_json::json!(true)
        );
        let probe = probe_socket(
            &dir.path().join("serve").join("custom-12.sock"),
            "master.example.ts.net",
        )
        .await;
        assert!(
            probe.as_ref().is_err_and(|e| e.contains("funnel")),
            "{probe:?}"
        );
    }

    #[tokio::test]
    async fn the_machine_name_is_refused_while_the_listener_holds_443() {
        let (mut manager, shared, _dir) = manager(
            fake::running(),
            vec![entry(1, "custom:12", "master.example.ts.net")],
        )
        .await;
        let report = manager
            .pass(&PassInput {
                private_listener: Some("100.101.102.103:443".parse().unwrap()),
                ..input()
            })
            .await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Conflict);
        assert_eq!(
            report.entries[0].detail.as_deref(),
            Some(messages::listener_exclusive("master", "100.101.102.103:443").as_str())
        );
        assert_eq!(shared.lock().writes, 0);
    }

    #[tokio::test]
    async fn a_name_resolving_elsewhere_fails_the_inside_check() {
        let (mut manager, _shared, _dir) = manager(
            fake::running(),
            vec![entry(1, "custom:12", "master.example.ts.net")],
        )
        .await;
        manager.resolver = Some(Arc::new(FixedDns(vec!["100.64.9.9".parse().unwrap()])));
        let report = manager.pass(&input()).await;
        assert_eq!(
            report.checks[0].outcome,
            TailnetCheckOutcome::ResolvesElsewhere
        );
        assert!(
            report.checks[0].message.contains("100.64.9.9"),
            "{}",
            report.checks[0].message
        );
    }

    #[tokio::test]
    async fn https_off_fails_the_inside_check_with_the_adr_message() {
        let mut state = fake::running();
        state.status["CertDomains"] = serde_json::Value::Null;
        let (mut manager, _shared, _dir) =
            manager(state, vec![entry(1, "custom:12", "master.example.ts.net")]).await;
        let report = manager.pass(&input()).await;
        assert!(!report.serve_ready());
        assert_eq!(report.checks[0].outcome, TailnetCheckOutcome::Certificates);
        assert_eq!(
            report.checks[0].message,
            messages::https_off("example.ts.net", "master.example.ts.net")
        );
    }

    #[tokio::test]
    async fn a_finished_socket_task_is_replaced_and_its_socket_file_kept() {
        use std::os::unix::fs::FileTypeExt;
        let entries = vec![entry(1, "custom:12", "master.example.ts.net")];
        let (mut manager, _shared, _dir) = manager(fake::running(), entries.clone()).await;
        std::fs::create_dir_all(&manager.serve_dir).unwrap();
        manager.bind_sockets(&entries).await;
        let path = manager.socket_path("custom:12").expect("a socket path");
        manager.sockets["custom:12"].task.abort();
        while !manager.sockets["custom:12"].task.is_finished() {
            tokio::task::yield_now().await;
        }

        manager.bind_sockets(&entries).await;

        let bound = &manager.sockets["custom:12"];
        assert!(!bound.task.is_finished());
        assert!(
            std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_socket()),
            "the replacement socket file is gone"
        );
        tokio::net::UnixStream::connect(&path)
            .await
            .expect("the replacement socket accepts connections");
    }

    fn service_entry() -> ServeEntry {
        let mut service = entry(1, "custom:12", "myapp.example.ts.net");
        service.via = "tailscale_service".into();
        service.service = Some("svc:myapp".into());
        service.addrs = vec!["100.100.50.1".into()];
        service
    }

    /// Another writer that read the list before this write and wrote after
    /// it drops `svc:myapp`; the read back sees that and merges again,
    /// keeping the other writer's names.
    #[tokio::test]
    async fn an_advertise_overwritten_after_the_write_is_merged_again() {
        let mut state = fake::running();
        state.prefs["AdvertiseServices"] = serde_json::json!(["svc:someone-else"]);
        state
            .advertise_after_write
            .push_back(serde_json::json!(["svc:someone-else", "svc:operator"]));
        let (mut manager, shared, _dir) = manager(state, vec![service_entry()]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Applied);
        let shared = shared.lock();
        assert_eq!(
            shared.prefs["AdvertiseServices"],
            serde_json::json!(["svc:someone-else", "svc:operator", "svc:myapp"])
        );
        assert_eq!(shared.prefs_writes, 2);
    }

    /// A list that keeps being overwritten is given up on after a bound,
    /// reported as an error, and no name the agent did not add is removed.
    #[tokio::test]
    async fn an_advertise_that_keeps_being_overwritten_is_an_error() {
        let mut state = fake::running();
        state.prefs["AdvertiseServices"] = serde_json::json!(["svc:someone-else"]);
        for _ in 0..MAX_WRITE_ATTEMPTS {
            state
                .advertise_after_write
                .push_back(serde_json::json!(["svc:someone-else"]));
        }
        let (mut manager, shared, _dir) = manager(state, vec![service_entry()]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Error);
        let detail = report.entries[0].detail.as_deref().unwrap_or_default();
        assert!(detail.contains("kept changing"), "{detail}");
        let shared = shared.lock();
        assert_eq!(shared.prefs_writes, MAX_WRITE_ATTEMPTS as u32);
        assert_eq!(
            shared.prefs["AdvertiseServices"],
            serde_json::json!(["svc:someone-else"])
        );
    }

    /// A Service still advertised after this worker dropped it is not
    /// reported removed (the row would be deleted and the name left behind),
    /// and its removal is not retried within the pass.
    #[tokio::test]
    async fn a_dropped_service_still_advertised_is_an_error_not_removed() {
        let mut state = fake::running();
        state.prefs["AdvertiseServices"] = serde_json::json!(["svc:someone-else", "svc:myapp"]);
        state
            .advertise_after_write
            .push_back(serde_json::json!(["svc:someone-else", "svc:myapp"]));
        let mut removing = service_entry();
        removing.removing = true;
        removing.route = None;
        let (mut manager, shared, _dir) = manager(state, vec![removing]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Error);
        let detail = report.entries[0].detail.as_deref().unwrap_or_default();
        assert!(detail.contains("still in AdvertiseServices"), "{detail}");
        assert_eq!(shared.lock().prefs_writes, 1);
    }

    /// A name another writer removed after this write is not put back.
    #[tokio::test]
    async fn a_name_another_writer_removed_is_not_put_back() {
        let mut state = fake::running();
        state.prefs["AdvertiseServices"] = serde_json::json!(["svc:someone-else"]);
        state
            .advertise_after_write
            .push_back(serde_json::json!(["svc:myapp"]));
        let (mut manager, shared, _dir) = manager(state, vec![service_entry()]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Applied);
        let shared = shared.lock();
        assert_eq!(
            shared.prefs["AdvertiseServices"],
            serde_json::json!(["svc:myapp"])
        );
        assert_eq!(shared.prefs_writes, 1);
    }

    #[tokio::test]
    async fn a_service_entry_is_advertised_and_other_names_are_kept() {
        let mut state = fake::running();
        state.prefs["AdvertiseServices"] = serde_json::json!(["svc:someone-else"]);
        let mut service = entry(1, "custom:12", "myapp.example.ts.net");
        service.via = "tailscale_service".into();
        service.service = Some("svc:myapp".into());
        service.addrs = vec!["100.100.50.1".into()];
        let (mut manager, shared, _dir) = manager(state, vec![service.clone()]).await;
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Applied);
        assert_eq!(
            shared.lock().prefs["AdvertiseServices"],
            serde_json::json!(["svc:someone-else", "svc:myapp"])
        );
        service.removing = true;
        service.route = None;
        manager
            .store
            .apply_public_snapshot(crate::route_store::PublicIngressSnapshot {
                enabled: true,
                serve_entries: vec![service],
                ..Default::default()
            });
        let report = manager.pass(&input()).await;
        assert_eq!(report.entries[0].outcome, ServeOutcome::Removed);
        assert_eq!(
            shared.lock().prefs["AdvertiseServices"],
            serde_json::json!(["svc:someone-else"])
        );
    }
}
