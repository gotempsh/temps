// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Private addresses the control plane serves itself (ADR-115 D2, D3, D6).
//!
//! A worker's agent binds its private listener where its owner said, checks
//! its private addresses from inside and reports both. The control plane
//! does the same in its own process: [`ControlPlanePrivateIngressLoop`]
//! reads the operator's consent (`settings.private_ingress`), resolves the
//! address it names on this machine, tells the proxy to serve node `0`'s
//! private routes there when the proxy's listener covers it, and writes
//! what it did to `node_public_ingress` row `0`, exactly as a worker's
//! report is written. The planner then reads row `0` like any worker's.
//!
//! The inside checks are the agent's own code, wired in by the binary
//! ([`InsideChecker`]), as ADR-107 wires the agent's connector supervisor.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use temps_config::private_ingress::{PrivateIngressService, PrivateListen};
use temps_core::private_ingress::{ControlPlanePrivateIngress, PrivateIngressServing};
use temps_core::private_network::{
    container_overlap, default_admit, is_container_interface, network_options, IpNet, LocalNetwork,
};
use temps_core::worker_ingress::{
    IngressMode, ListenerReport, PrivateCheck, ReportedInterface, PRIVATE_HTTPS_PORT,
    PRIVATE_HTTP_PORT,
};
use temps_core::CONTROL_PLANE_NODE_ID;
use temps_routes::public_ingress_store::MAX_PRIVATE_CHECKS;
use temps_routes::route_table::PrivateAddress;
use temps_routes::CachedPeerTable;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// How often the consent and the machine's addresses are read again when
/// nothing wakes the loop: the agent's interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How long the first pass waits for the proxy in this process to record
/// its listeners. Until then the loop cannot tell whether they cover the
/// private address, and a pass would report that no proxy runs. Past it, the
/// pass runs anyway and reports what it finds.
pub const PROXY_START_WAIT: Duration = Duration::from_secs(120);

/// Wait until the proxy has recorded its listeners, `deadline` passes or
/// `shutdown` is signalled. `false` only on shutdown.
async fn wait_for_proxy_listeners(
    runtime: &ControlPlanePrivateIngress,
    shutdown: &Notify,
    deadline: Duration,
) -> bool {
    let until = tokio::time::Instant::now() + deadline;
    while runtime.proxy_listeners().is_none() {
        tokio::select! {
            _ = shutdown.notified() => return false,
            _ = runtime.changed() => {}
            _ = tokio::time::sleep_until(until) => {
                warn!(
                    waited_secs = deadline.as_secs(),
                    "the proxy has not recorded its listeners; checking private ingress without them"
                );
                return true;
            }
        }
    }
    true
}

/// How often the inside checks run when the private addresses do not
/// change (ADR 064 D7).
pub const CHECK_INTERVAL: Duration = Duration::from_secs(300);

/// Every address of this machine, with its interface and prefix.
pub trait LocalAddresses: Send + Sync {
    fn addresses(&self) -> Vec<LocalNetwork>;
}

/// The machine's container networks (Docker's), by name: no admitted range
/// may overlap one (ADR-115 D8). `None` when they could not be listed (an
/// error or a timeout), which is not the same as there being none: a range
/// that overlaps a network nobody listed would admit its containers.
#[async_trait]
pub trait ContainerNetworks: Send + Sync {
    async fn networks(&self) -> Option<Vec<(String, IpNet)>>;
}

/// Why nothing is served while Docker's networks cannot be listed. Only the
/// listing of this pass counts: an older one would miss a network created
/// since, whose containers a range overlapping it would admit.
pub const CONTAINER_NETWORKS_UNKNOWN: &str = "Docker's networks could not be listed, so private addresses are not served until they can be: a range that overlaps a container network would admit its containers. Check that Docker answers (`docker network ls`).";

/// The networks to offer for a consent. While Docker's networks are unknown
/// (its listing failed this pass), no option can be served, and each says
/// why rather than look usable: a consent saved for it would serve nothing.
fn offered_networks(
    own: &[LocalNetwork],
    containers: Option<&[(String, IpNet)]>,
) -> Vec<temps_core::private_network::PrivateNetworkOption> {
    let mut options = network_options(own, containers.unwrap_or_default());
    if containers.is_none() {
        for option in &mut options {
            option
                .problem
                .get_or_insert_with(|| CONTAINER_NETWORKS_UNKNOWN.to_string());
        }
    }
    options
}

/// The consent as the loop applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentInput {
    pub listen: PrivateListen,
    /// The ranges named; empty for the listen address's own network.
    pub admit: Vec<IpNet>,
}

/// Checks private addresses from inside, as a worker's agent does.
#[async_trait]
pub trait InsideChecker: Send + Sync {
    async fn check(&self, addresses: Vec<(String, PrivateAddress)>) -> Vec<PrivateCheck>;
}

/// Where the consent says to serve, on this machine, or why it cannot.
pub fn resolve_listen(listen: PrivateListen, own: &[LocalNetwork]) -> Result<LocalNetwork, String> {
    match listen {
        PrivateListen::Tailscale => {
            let mut found: Vec<&LocalNetwork> = own.iter().filter(|n| n.tailscale).collect();
            found.sort_by_key(|n| !n.address.is_ipv4());
            found.first().map(|n| (*n).clone()).ok_or_else(|| {
                "Private addresses are served on Tailscale, but this machine has no Tailscale \
                 address. Start Tailscale (`tailscale up`), or serve on one of this machine's \
                 private addresses instead."
                    .to_string()
            })
        }
        PrivateListen::Address(ip) => {
            own.iter()
                .find(|n| n.address == ip)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "{ip} is not an address of this machine. Serve on one of this machine's \
                     private addresses, or on `tailscale`."
                    )
                })
        }
    }
}

/// Container networks: what Docker reports, and the subnets of container
/// bridge interfaces on this machine.
pub fn all_container_networks(
    own: &[LocalNetwork],
    docker: Vec<(String, IpNet)>,
) -> Vec<(String, IpNet)> {
    let mut out = docker;
    for network in own {
        if is_container_interface(&network.interface) {
            if let Some(subnet) = network.subnet() {
                if !out.iter().any(|(_, net)| *net == subnet) {
                    out.push((network.interface.clone(), subnet));
                }
            }
        }
    }
    out
}

/// The interfaces a report lists: private ones only, with Tailscale's
/// marked. Loopback and container bridges never are.
fn reported_interfaces(own: &[LocalNetwork]) -> Vec<ReportedInterface> {
    let mut out: Vec<ReportedInterface> = own
        .iter()
        .filter(|n| {
            !n.address.is_loopback()
                && temps_core::worker_ingress::private_block(n.address).is_some()
                && !is_container_interface(&n.interface)
        })
        .map(|n| ReportedInterface {
            name: n.interface.clone(),
            address: n.address.to_string(),
            default_route: false,
            tailscale: n.tailscale,
        })
        .collect();
    out.sort_by(|a, b| (&a.name, &a.address).cmp(&(&b.name, &b.address)));
    out
}

/// What one pass decided, before it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateIngressDecision {
    pub serving: Option<PrivateIngressServing>,
    pub report: Option<ListenerReport>,
}

/// Decide what to serve and report from the consent and this machine.
/// `None` consent serves nothing and reports nothing.
pub fn decide(
    consent: Option<ConsentInput>,
    own: &[LocalNetwork],
    containers: Option<&[(String, IpNet)]>,
    runtime: &ControlPlanePrivateIngress,
) -> PrivateIngressDecision {
    let Some(consent) = consent else {
        return PrivateIngressDecision {
            serving: None,
            report: None,
        };
    };
    let bound = resolve_listen(consent.listen, own).and_then(|network| {
        let admit = if consent.admit.is_empty() {
            default_admit(&network)?
        } else {
            consent.admit.clone()
        };
        let Some(containers) = containers else {
            return Err(CONTAINER_NETWORKS_UNKNOWN.to_string());
        };
        if let Some(why) = container_overlap(&admit, containers) {
            return Err(why);
        }
        if let Some(why) = runtime.not_covered(network.address) {
            return Err(why);
        }
        PrivateIngressServing::new(network.address, admit)
    });
    let interfaces = reported_interfaces(own);
    match bound {
        Ok(serving) => {
            // Only a port the proxy takes on this address: HTTPS always is
            // (not_covered checked it), HTTP only when its listener covers
            // the address too, so a worker is never pointed at a port 80
            // nobody answers on.
            let at = |port: u16| {
                runtime
                    .listens_on(serving.address, port)
                    .then(|| std::net::SocketAddr::new(serving.address, port).to_string())
            };
            PrivateIngressDecision {
                report: Some(ListenerReport {
                    interfaces,
                    http: at(PRIVATE_HTTP_PORT),
                    https: at(PRIVATE_HTTPS_PORT),
                    mode: Some(IngressMode::Private),
                    error: None,
                    admit: serving.admit.iter().map(ToString::to_string).collect(),
                    ..ListenerReport::default()
                }),
                serving: Some(serving),
            }
        }
        Err(error) => PrivateIngressDecision {
            serving: None,
            report: Some(ListenerReport {
                interfaces,
                mode: Some(IngressMode::Private),
                error: Some(error),
                ..ListenerReport::default()
            }),
        },
    }
}

/// Forget node `0`'s private listener report. For a control plane whose
/// console and proxy run as separate processes, where no loop serves or
/// reports node `0`'s private addresses (ADR-115 D7): a report left by an
/// earlier single-process run would otherwise keep offering its listener
/// to the planner, which would point private names at a proxy that cannot
/// serve them.
pub async fn forget_control_plane_report(db: &DatabaseConnection) -> Result<u64, sea_orm::DbErr> {
    let deleted = temps_entities::node_public_ingress::Entity::delete_by_id(CONTROL_PLANE_NODE_ID)
        .exec(db)
        .await?;
    Ok(deleted.rows_affected)
}

pub struct ControlPlanePrivateIngressLoop {
    db: Arc<DatabaseConnection>,
    consent: PrivateIngressService,
    runtime: Arc<ControlPlanePrivateIngress>,
    route_table: Arc<CachedPeerTable>,
    addresses: Arc<dyn LocalAddresses>,
    containers: Arc<dyn ContainerNetworks>,
    checker: Arc<dyn InsideChecker>,
}

/// How a pass of the loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    Done,
    /// The consent changed while it ran; the next pass starts at once.
    Interrupted,
}

/// Most hostnames one pass checks, as the agent's `private_check::check_all`
/// checks at most.
const CHECKS_PER_PASS: usize = 64;

/// Inside-check results by hostname and when each was last checked. A pass
/// checks the hostnames checked longest ago, never-checked ones first, so
/// every one gets its turn however many there are.
#[derive(Default)]
struct CheckRotation {
    results: std::collections::BTreeMap<String, PrivateCheck>,
    checked_at: std::collections::BTreeMap<String, Instant>,
}

impl CheckRotation {
    /// Forget hostnames no longer served here.
    fn retain(&mut self, hosts: &[(String, PrivateAddress)]) {
        let live: std::collections::BTreeSet<&str> =
            hosts.iter().map(|(h, _)| h.as_str()).collect();
        self.results.retain(|host, _| live.contains(host.as_str()));
        self.checked_at
            .retain(|host, _| live.contains(host.as_str()));
    }

    /// Whether a hostname has never been checked.
    #[cfg(test)]
    fn has_unchecked(&self, hosts: &[(String, PrivateAddress)]) -> bool {
        hosts
            .iter()
            .any(|(host, _)| !self.checked_at.contains_key(host))
    }

    /// The hostnames due a check: never checked, or checked `interval` ago
    /// or longer. Each by its own result's age, so a batch checked late
    /// never makes the others wait a further interval.
    fn due(
        &self,
        hosts: &[(String, PrivateAddress)],
        interval: Duration,
    ) -> Vec<(String, PrivateAddress)> {
        hosts
            .iter()
            .filter(|(host, _)| {
                self.checked_at
                    .get(host)
                    .is_none_or(|at| at.elapsed() >= interval)
            })
            .cloned()
            .collect()
    }

    /// The next `limit` hostnames to check: never checked first, then the
    /// ones checked longest ago.
    fn next(
        &self,
        hosts: &[(String, PrivateAddress)],
        limit: usize,
    ) -> Vec<(String, PrivateAddress)> {
        let mut order: Vec<&(String, PrivateAddress)> = hosts.iter().collect();
        order.sort_by(|a, b| {
            self.checked_at
                .get(&a.0)
                .cmp(&self.checked_at.get(&b.0))
                .then_with(|| a.0.cmp(&b.0))
        });
        order.into_iter().take(limit).cloned().collect()
    }

    fn record(
        &mut self,
        checked: &[(String, PrivateAddress)],
        results: Vec<PrivateCheck>,
        at: Instant,
    ) {
        for (host, _) in checked {
            self.checked_at.insert(host.clone(), at);
            self.results.remove(host);
        }
        // Each result keeps when it was checked: one carried into a later
        // report, while other hosts take their turn, is not re-stamped as
        // just seen working. The checker stamps each one as it ends, so a
        // quick one in a slow batch keeps its own time; one it did not
        // stamp takes now.
        let now = chrono::Utc::now();
        for mut result in results {
            let at = result.checked_at.map_or(now, |at| at.min(now));
            result.checked_at = Some(at);
            self.results.insert(result.hostname.clone(), result);
        }
        // Only as many as a report carries are kept: past that, the ones
        // checked longest ago go (each host's turn is still remembered).
        if self.results.len() > MAX_PRIVATE_CHECKS {
            let mut by_age: Vec<(Option<Instant>, String)> = self
                .results
                .keys()
                .map(|host| (self.checked_at.get(host).copied(), host.clone()))
                .collect();
            by_age.sort();
            let excess = self.results.len() - MAX_PRIVATE_CHECKS;
            for (_, host) in by_age.into_iter().take(excess) {
                self.results.remove(&host);
            }
        }
    }

    /// The latest results the store keeps, the most recently checked first.
    /// The store keeps at most [`MAX_PRIVATE_CHECKS`] of them, so past that
    /// bound it is the oldest results that are left out, never one just
    /// checked, and only the kept ones are copied.
    fn report(&self) -> Vec<PrivateCheck> {
        let mut results: Vec<&PrivateCheck> = self.results.values().collect();
        let newest_first = |a: &&PrivateCheck, b: &&PrivateCheck| {
            self.checked_at
                .get(&b.hostname)
                .cmp(&self.checked_at.get(&a.hostname))
                .then_with(|| a.hostname.cmp(&b.hostname))
        };
        if results.len() > MAX_PRIVATE_CHECKS {
            results.select_nth_unstable_by(MAX_PRIVATE_CHECKS, newest_first);
            results.truncate(MAX_PRIVATE_CHECKS);
        }
        results.sort_by(newest_first);
        results.into_iter().cloned().collect()
    }

    fn clear(&mut self) {
        self.results.clear();
        self.checked_at.clear();
    }
}

/// What the loop keeps between passes.
#[derive(Default)]
struct LoopState {
    last_report: Option<ListenerReport>,
    last_hosts: Vec<(String, PrivateAddress)>,
    checks: CheckRotation,
    /// The consent as last read, to recheck what is served against while
    /// the database cannot be read.
    last_input: Option<ConsentInput>,
    /// When the consent last written about was given: a new consent is
    /// always answered with a fresh report, even an identical one, since
    /// the status reads only reports made after the consent.
    reported_consent_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether node 0's report is known to be gone: cleared once on start
    /// and on each change to serving nothing, and again only after a
    /// report was written or a clear failed, not every poll.
    cleared: bool,
}

impl ControlPlanePrivateIngressLoop {
    pub fn new(
        db: Arc<DatabaseConnection>,
        runtime: Arc<ControlPlanePrivateIngress>,
        route_table: Arc<CachedPeerTable>,
        addresses: Arc<dyn LocalAddresses>,
        containers: Arc<dyn ContainerNetworks>,
        checker: Arc<dyn InsideChecker>,
    ) -> Arc<Self> {
        Arc::new(Self {
            consent: PrivateIngressService::new(db.clone(), None),
            db,
            runtime,
            route_table,
            addresses,
            containers,
            checker,
        })
    }

    /// Sync once the proxy has recorded its listeners, then every
    /// [`POLL_INTERVAL`] and whenever the consent changes, until `shutdown`.
    pub fn start(self: &Arc<Self>, shutdown: Arc<Notify>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if !wait_for_proxy_listeners(&this.runtime, &shutdown, PROXY_START_WAIT).await {
                debug!("control-plane private ingress stopped before the proxy started");
                return;
            }
            let mut state = LoopState::default();
            loop {
                // A stop during a pass (a slow batch of inside checks) ends
                // it at once: nothing more is checked or reported.
                let pass = tokio::select! {
                    pass = this.sync(&mut state) => pass,
                    _ = shutdown.notified() => {
                        this.runtime.set_serving(None);
                        debug!("control-plane private ingress stopped during a pass");
                        return;
                    }
                };
                // A consent change during the inside checks ends the pass
                // early; sync again at once rather than after the poll.
                if pass == Pass::Interrupted {
                    continue;
                }
                tokio::select! {
                    _ = shutdown.notified() => {
                        this.runtime.set_serving(None);
                        debug!("control-plane private ingress stopped");
                        return;
                    }
                    _ = this.runtime.changed() => {}
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                }
            }
        })
    }

    /// Why what is served now is no longer what `input` decides on this
    /// machine as it is now (its addresses and their prefixes, Docker's
    /// networks), or `None` while it still is, or nothing is served.
    async fn serving_drifted(&self, input: Option<ConsentInput>) -> Option<String> {
        let serving = self.runtime.serving()?;
        let own = self.addresses.addresses();
        let containers = self
            .containers
            .networks()
            .await
            .map(|docker| all_container_networks(&own, docker));
        let decision = decide(input, &own, containers.as_deref(), &self.runtime);
        if decision.serving.as_ref() == Some(&serving) {
            return None;
        }
        Some(
            decision
                .report
                .and_then(|report| report.error)
                .unwrap_or_else(|| "this machine's private networks changed".to_string()),
        )
    }

    /// With the consent unreadable, stop serving once the last consent read
    /// no longer decides it on this machine (a narrowed subnet, an
    /// overlapping container network, Docker that cannot be listed, an
    /// address gone); never start or widen serving.
    async fn recheck_without_consent(&self, state: &LoopState, generation: u64) {
        if let Some(why) = self.serving_drifted(state.last_input.clone()).await {
            if self.runtime.set_serving_for(generation, None) {
                warn!(reason = %why, "stopped serving private addresses");
            }
        }
    }

    async fn sync(&self, state: &mut LoopState) -> Pass {
        // Read before the consent: a consent written after this, while the
        // pass awaits the machine's networks or its checks, outdates every
        // decision the pass makes, and nothing it decided is published.
        let generation = self.runtime.consent_generation();
        let consent = match self.consent.get().await {
            Ok(consent) => consent,
            Err(error) => {
                // Keep serving what was consented, since the database may be
                // briefly unavailable, but only while this machine still
                // allows it: a container network that now overlaps an
                // admitted range, Docker that cannot be listed, or an
                // address the machine no longer has stops serving.
                warn!(%error, "could not read the private ingress consent; keeping the last state while it is safe");
                self.recheck_without_consent(state, generation).await;
                return Pass::Done;
            }
        };
        let input = consent.as_ref().and_then(|c| {
            Some(ConsentInput {
                listen: c.listen()?,
                admit: c.admit()?,
            })
        });
        if consent.is_some() && input.is_none() {
            warn!("the stored private ingress consent names an address or range this version refuses; serving nothing");
        }
        state.last_input = input.clone();
        let own = self.addresses.addresses();
        let listed = self.containers.networks().await;
        if listed.is_none() {
            warn!("could not list Docker's networks; serving no private address until they can be");
        }
        let containers = listed.map(|docker| all_container_networks(&own, docker));
        self.runtime
            .set_networks(offered_networks(&own, containers.as_deref()));
        let decision = decide(input.clone(), &own, containers.as_deref(), &self.runtime);
        let before = self.runtime.serving();
        if !self
            .runtime
            .set_serving_for(generation, decision.serving.clone())
        {
            debug!("the private ingress consent changed during the pass; syncing again");
            return Pass::Interrupted;
        }
        if before != decision.serving {
            match &decision.serving {
                Some(serving) => {
                    info!(address = %serving.address, admit = %temps_core::private_network::admit_text(&serving.admit), "the control plane serves private addresses")
                }
                None => info!("the control plane serves no private address"),
            }
        }
        let Some(mut report) = decision.report else {
            state.last_report = None;
            state.reported_consent_at = None;
            state.checks.clear();
            state.last_hosts.clear();
            if self.runtime.consent_generation() != generation {
                return Pass::Interrupted;
            }
            if !state.cleared {
                match self.clear_report().await {
                    Ok(()) => state.cleared = true,
                    Err(error) => warn!(%error, "could not clear node 0's private ingress report"),
                }
            }
            return Pass::Done;
        };

        // Inside checks (ADR 064 D7), only while something is served: every
        // hostname again once its own result is CHECK_INTERVAL old, and all
        // of them after the addresses change. More due than one pass checks:
        // the rest go on the next passes, POLL_INTERVAL apart.
        let mut hosts = self.route_table.control_plane_private_hosts();
        hosts.sort_by(|a, b| a.0.cmp(&b.0));
        state.checks.retain(&hosts);
        let due = if hosts != state.last_hosts {
            hosts.clone()
        } else {
            state.checks.due(&hosts, CHECK_INTERVAL)
        };
        let consent_at = consent.as_ref().map(|c| c.updated_at);
        let mut ran_checks = false;
        if decision.serving.is_some() && !due.is_empty() {
            ran_checks = true;
            // A consent served for the first time is reported before the
            // checks, which may take minutes: until then the status reads
            // the address as not served under it.
            if state.reported_consent_at != consent_at {
                let mut applied = report.clone();
                applied.private_checks = state.checks.report();
                if self.publish(state, applied, consent_at, generation).await == Pass::Interrupted {
                    return Pass::Interrupted;
                }
            }
            let batch = state.checks.next(&due, CHECKS_PER_PASS);
            // Up to one DNS timeout per host: a withdrawn or narrowed
            // consent must not wait for all of them, and neither must a
            // change on this machine (a container network that now
            // overlaps, a narrowed subnet), which is looked for every
            // POLL_INTERVAL meanwhile. The checks are of a decision that no
            // longer holds, so they are dropped and the next pass decides
            // again at once.
            let checks = self.checker.check(batch.clone());
            tokio::pin!(checks);
            let results = loop {
                tokio::select! {
                    checks = &mut checks => break checks,
                    _ = self.runtime.changed() => {
                        debug!("the private ingress consent changed during the inside checks; syncing again");
                        return Pass::Interrupted;
                    }
                    _ = tokio::time::sleep(POLL_INTERVAL) => {
                        if let Some(why) = self.serving_drifted(input.clone()).await {
                            // Stop now: what is served is no longer safe,
                            // and the next pass waits on the database and
                            // Docker before it decides again.
                            if self.runtime.set_serving_for(generation, None) {
                                warn!(reason = %why, "this machine changed during the inside checks; stopped serving private addresses until the next pass decides again");
                            }
                            return Pass::Interrupted;
                        }
                    }
                }
            };
            state.checks.record(&batch, results, Instant::now());
            state.last_hosts = hosts;
        } else if decision.serving.is_none() {
            state.checks.clear();
        }
        report.private_checks = state.checks.report();

        // Written when it changes, with every fresh check, and once for
        // every new consent.
        if !must_write(state, &report, ran_checks, consent_at) {
            return Pass::Done;
        }
        self.publish(state, report, consent_at, generation).await
    }

    /// Write node 0's report, stamped with the consent it answers.
    async fn publish(
        &self,
        state: &mut LoopState,
        report: ListenerReport,
        consent_at: Option<chrono::DateTime<chrono::Utc>>,
        generation: u64,
    ) -> Pass {
        // Not a report of an outdated consent. One written in the instant
        // after this check is still stamped with the consent it answers,
        // which the planner and the status compare with the consent now.
        if self.runtime.consent_generation() != generation {
            debug!("the private ingress consent changed during the pass; syncing again");
            return Pass::Interrupted;
        }
        match temps_routes::public_ingress_store::PublicIngressStore::new(&self.db)
            .record_control_plane_report(report.clone(), consent_at)
            .await
        {
            Ok(()) => {
                state.last_report = Some(report);
                state.reported_consent_at = consent_at;
                state.cleared = false;
            }
            Err(error) => warn!(%error, "could not write node 0's private ingress report"),
        }
        Pass::Done
    }

    async fn clear_report(&self) -> Result<(), sea_orm::DbErr> {
        forget_control_plane_report(self.db.as_ref()).await?;
        Ok(())
    }
}

/// Whether a pass writes its report: when it differs from the last one
/// written, when checks ran, or when the consent is newer than the one the
/// last report answered.
fn must_write(
    state: &LoopState,
    report: &ListenerReport,
    ran_checks: bool,
    consent_at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    ran_checks
        || state.last_report.as_ref() != Some(report)
        || state.reported_consent_at != consent_at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(interface: &str, address: &str, prefix: u8, tailscale: bool) -> LocalNetwork {
        LocalNetwork {
            interface: interface.into(),
            address: address.parse().unwrap(),
            prefix,
            tailscale,
        }
    }

    fn own() -> Vec<LocalNetwork> {
        vec![
            net("lo", "127.0.0.1", 8, false),
            net("eth0", "192.168.1.20", 24, false),
            net("docker0", "172.17.0.1", 16, false),
            net("wg0", "10.8.0.1", 24, false),
            net("tailscale0", "fd7a:115c:a1e0::1", 128, true),
            net("tailscale0", "100.101.102.103", 32, true),
        ]
    }

    fn containers() -> Vec<(String, IpNet)> {
        all_container_networks(
            &own(),
            vec![("app-net".into(), "10.20.0.0/16".parse().unwrap())],
        )
    }

    fn consent(listen: PrivateListen, admit: &[&str]) -> Option<ConsentInput> {
        Some(ConsentInput {
            listen,
            admit: admit.iter().map(|r| r.parse().unwrap()).collect(),
        })
    }

    fn runtime(listeners: &[&str]) -> Arc<ControlPlanePrivateIngress> {
        let runtime = ControlPlanePrivateIngress::new();
        runtime.set_proxy_listeners(listeners.iter().map(|l| l.parse().unwrap()).collect());
        runtime
    }

    fn wildcard() -> Arc<ControlPlanePrivateIngress> {
        runtime(&["0.0.0.0:80", "0.0.0.0:443"])
    }

    struct Machine;
    impl LocalAddresses for Machine {
        fn addresses(&self) -> Vec<LocalNetwork> {
            own()
        }
    }

    /// No container networks, or, given a runtime, a lookup during which the
    /// consent is withdrawn.
    struct Containers(Option<Arc<ControlPlanePrivateIngress>>);
    #[async_trait]
    impl ContainerNetworks for Containers {
        async fn networks(&self) -> Option<Vec<(String, IpNet)>> {
            if let Some(runtime) = &self.0 {
                runtime.consent_written();
            }
            Some(Vec::new())
        }
    }

    /// Inside checks that never finish on their own (a DNS timeout per
    /// host), saying when they start.
    struct Hanging(Arc<Notify>);
    #[async_trait]
    impl InsideChecker for Hanging {
        async fn check(&self, _: Vec<(String, PrivateAddress)>) -> Vec<PrivateCheck> {
            self.0.notify_one();
            std::future::pending().await
        }
    }

    /// A loop whose one consent read finds `listen` 10.8.0.1, with node 0
    /// serving one private address.
    fn pass_loop(
        runtime: &Arc<ControlPlanePrivateIngress>,
        containers: Containers,
        checks_started: Arc<Notify>,
    ) -> Arc<ControlPlanePrivateIngressLoop> {
        let settings = temps_entities::settings::Model {
            id: 1,
            data: serde_json::json!({
                "private_ingress": {"listen": "10.8.0.1", "updated_at": "2026-11-26T10:00:00Z"}
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![settings]])
            .into_connection();
        let route_table = Arc::new(CachedPeerTable::new(Arc::new(
            DatabaseConnection::Disconnected,
        )));
        route_table.insert_private_route_for_test(
            "app.internal.example.com",
            temps_routes::route_table::PrivateRoute {
                route: temps_routes::route_table::RouteInfo {
                    backend: temps_routes::route_table::BackendType::StaticDir {
                        path: "/srv/app".into(),
                    },
                    redirect_to: None,
                    status_code: None,
                    project: None,
                    environment: None,
                    deployment: None,
                    cert_eligible: false,
                },
                address: PrivateAddress {
                    address_id: "env:1".into(),
                    range: Some("10.8.0.0/24".into()),
                    node_id: Some(CONTROL_PLANE_NODE_ID),
                    listener: Some("10.8.0.1".into()),
                    via: Some("listener".into()),
                    principals: None,
                },
            },
        );
        ControlPlanePrivateIngressLoop::new(
            Arc::new(db),
            Arc::clone(runtime),
            route_table,
            Arc::new(Machine),
            Arc::new(containers),
            Arc::new(Hanging(checks_started)),
        )
    }

    /// Docker's networks as given: `None` when it cannot be listed.
    struct Listed(Option<Vec<(String, IpNet)>>);
    #[async_trait]
    impl ContainerNetworks for Listed {
        async fn networks(&self) -> Option<Vec<(String, IpNet)>> {
            self.0.clone()
        }
    }

    /// This machine's networks as given.
    struct Own(Vec<LocalNetwork>);
    impl LocalAddresses for Own {
        fn addresses(&self) -> Vec<LocalNetwork> {
            self.0.clone()
        }
    }

    fn unreadable_db() -> DatabaseConnection {
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("database unavailable".into())])
            .into_connection()
    }

    /// A consent the database cannot read keeps the last serving only while
    /// the last consent read still decides it on this machine, and never
    /// starts or widens serving.
    #[tokio::test]
    async fn an_unreadable_consent_keeps_serving_only_what_is_still_safe() {
        let overlapping = vec![("new-net".to_string(), "10.8.0.0/16".parse().unwrap())];
        let apart = vec![("app-net".to_string(), "10.20.0.0/16".parse().unwrap())];
        // wg0 narrowed from /24 to /28 while the database was down.
        let narrowed: Vec<LocalNetwork> = own()
            .into_iter()
            .map(|n| {
                if n.interface == "wg0" {
                    LocalNetwork { prefix: 28, ..n }
                } else {
                    n
                }
            })
            .collect();
        let at = |address: &str| PrivateListen::Address(address.parse().unwrap());
        for (machine, listed, listen, admit, kept) in [
            (
                own(),
                Some(apart.clone()),
                at("10.8.0.1"),
                vec!["10.8.0.0/24"],
                true,
            ),
            // The default admit, from wg0's prefix, still /24.
            (own(), Some(apart.clone()), at("10.8.0.1"), vec![], true),
            (
                own(),
                Some(overlapping),
                at("10.8.0.1"),
                vec!["10.8.0.0/24"],
                false,
            ),
            (own(), None, at("10.8.0.1"), vec!["10.8.0.0/24"], false),
            // The default admit is now the /28: the /24 is no longer consented.
            (narrowed, Some(apart.clone()), at("10.8.0.1"), vec![], false),
            // An address this machine no longer has.
            (
                own(),
                Some(apart.clone()),
                at("10.9.0.1"),
                vec!["10.8.0.0/24"],
                false,
            ),
        ] {
            let runtime = wildcard();
            let serving = PrivateIngressServing::new(
                "10.8.0.1".parse().unwrap(),
                vec!["10.8.0.0/24".parse().unwrap()],
            )
            .unwrap();
            runtime.set_serving(Some(serving.clone()));
            let pass_loop = ControlPlanePrivateIngressLoop::new(
                Arc::new(unreadable_db()),
                Arc::clone(&runtime),
                Arc::new(CachedPeerTable::new(Arc::new(
                    DatabaseConnection::Disconnected,
                ))),
                Arc::new(Own(machine)),
                Arc::new(Listed(listed.clone())),
                Arc::new(Hanging(Arc::new(Notify::new()))),
            );
            let mut state = LoopState {
                last_input: consent(listen, &admit),
                ..LoopState::default()
            };
            assert_eq!(pass_loop.sync(&mut state).await, Pass::Done);
            assert_eq!(
                runtime.serving(),
                kept.then_some(serving),
                "{listed:?} {listen:?} {admit:?}"
            );
        }

        // Nothing served before, or no consent ever read: nothing starts.
        for last_input in [
            None,
            consent(PrivateListen::Address("10.8.0.1".parse().unwrap()), &[]),
        ] {
            let runtime = wildcard();
            let pass_loop = ControlPlanePrivateIngressLoop::new(
                Arc::new(unreadable_db()),
                Arc::clone(&runtime),
                Arc::new(CachedPeerTable::new(Arc::new(
                    DatabaseConnection::Disconnected,
                ))),
                Arc::new(Machine),
                Arc::new(Listed(Some(Vec::new()))),
                Arc::new(Hanging(Arc::new(Notify::new()))),
            );
            pass_loop
                .sync(&mut LoopState {
                    last_input,
                    ..LoopState::default()
                })
                .await;
            assert_eq!(runtime.serving(), None);
        }
    }

    /// With no consent, node 0's report is cleared on the first pass, again
    /// after a clear failed, and not on every poll after that.
    #[tokio::test]
    async fn with_no_consent_the_report_is_cleared_once_and_a_failed_clear_again() {
        let no_consent = Vec::<temps_entities::settings::Model>::new;
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([no_consent(), no_consent(), no_consent(), no_consent()])
                .append_exec_errors([sea_orm::DbErr::Custom("database unavailable".into())])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let runtime = wildcard();
        let pass_loop = ControlPlanePrivateIngressLoop::new(
            Arc::clone(&db),
            Arc::clone(&runtime),
            Arc::new(CachedPeerTable::new(Arc::new(
                DatabaseConnection::Disconnected,
            ))),
            Arc::new(Machine),
            Arc::new(Listed(Some(Vec::new()))),
            Arc::new(Hanging(Arc::new(Notify::new()))),
        );
        let mut state = LoopState::default();
        for _ in 0..4 {
            assert_eq!(pass_loop.sync(&mut state).await, Pass::Done);
        }
        assert!(state.cleared);
        drop(pass_loop);
        let log = Arc::try_unwrap(db)
            .expect("only the test holds the database")
            .into_transaction_log();
        let deletes = log
            .iter()
            .filter(|t| format!("{t:?}").contains("DELETE"))
            .count();
        assert_eq!(deletes, 2, "the failed clear once more, then none: {log:?}");
    }

    /// Docker's networks, which change after the first look.
    struct Changing {
        calls: std::sync::atomic::AtomicUsize,
        later: Vec<(String, IpNet)>,
    }
    #[async_trait]
    impl ContainerNetworks for Changing {
        async fn networks(&self) -> Option<Vec<(String, IpNet)>> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(if call == 0 {
                Vec::new()
            } else {
                self.later.clone()
            })
        }
    }

    /// A container network that starts overlapping the admitted range while
    /// slow inside checks run is noticed within a poll interval: the pass
    /// ends without waiting for the checks, and the next one decides again.
    #[tokio::test(start_paused = true)]
    async fn a_network_change_during_slow_inside_checks_ends_the_pass() {
        let runtime = wildcard();
        let started = Arc::new(Notify::new());
        let mut pass_loop = pass_loop(&runtime, Containers(None), Arc::clone(&started));
        Arc::get_mut(&mut pass_loop)
            .expect("only this test holds it")
            .containers = Arc::new(Changing {
            calls: std::sync::atomic::AtomicUsize::new(0),
            later: vec![("new-net".into(), "10.8.0.0/16".parse().unwrap())],
        });
        tokio::time::timeout(Duration::from_secs(1), runtime.changed())
            .await
            .expect("the listeners woke the loop");
        let mut state = LoopState::default();
        let pass = tokio::time::timeout(POLL_INTERVAL * 3, pass_loop.sync(&mut state))
            .await
            .expect("the pass ends within a poll interval, not after the checks");
        assert_eq!(pass, Pass::Interrupted);
        // Stopped at once, not once the next pass has read the database
        // and Docker again.
        assert_eq!(runtime.serving(), None);
        // The next pass serves nothing either: the range overlaps a
        // container network.
        pass_loop.sync(&mut state).await;
        assert_eq!(runtime.serving(), None);
    }

    /// A stop while the inside checks run ends the loop at once: it does not
    /// wait for the batch, nor serve or report anything after it.
    #[tokio::test]
    async fn a_stop_during_the_inside_checks_ends_the_loop_at_once() {
        let runtime = wildcard();
        let started = Arc::new(Notify::new());
        let settings = temps_entities::settings::Model {
            id: 1,
            data: serde_json::json!({
                "private_ingress": {"listen": "10.8.0.1", "updated_at": "2026-11-26T10:00:00Z"}
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![settings]])
            .append_query_results([Vec::<temps_entities::node_public_ingress::Model>::new()])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let db = Arc::new(db);
        let mut pass_loop = pass_loop(&runtime, Containers(None), Arc::clone(&started));
        let only = Arc::get_mut(&mut pass_loop).expect("only this test holds it");
        only.db = Arc::clone(&db);
        only.consent = PrivateIngressService::new(db, None);
        // The wake-up the proxy's listeners left, which would otherwise end
        // the first pass before its checks.
        tokio::time::timeout(Duration::from_secs(1), runtime.changed())
            .await
            .expect("the listeners woke the loop");
        let shutdown = Arc::new(Notify::new());
        let task = pass_loop.start(Arc::clone(&shutdown));
        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .expect("the inside checks started");
        shutdown.notify_one();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the loop ends without waiting for the checks")
            .expect("the loop did not panic");
        assert_eq!(runtime.serving(), None);
    }

    /// A new consent is reported as applied before the inside checks, which
    /// can take minutes; the status otherwise says it is not served.
    #[tokio::test]
    async fn a_new_consent_is_reported_before_its_inside_checks() {
        let runtime = wildcard();
        let started = Arc::new(Notify::new());
        let settings = temps_entities::settings::Model {
            id: 1,
            data: serde_json::json!({
                "private_ingress": {"listen": "10.8.0.1", "updated_at": "2026-11-26T10:00:00Z"}
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![settings]])
            .append_query_results([Vec::<temps_entities::node_public_ingress::Model>::new()])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let db = Arc::new(db);
        let mut pass_loop = pass_loop(&runtime, Containers(None), Arc::clone(&started));
        let only = Arc::get_mut(&mut pass_loop).expect("only this test holds it");
        only.db = Arc::clone(&db);
        only.consent = PrivateIngressService::new(db, None);
        tokio::time::timeout(Duration::from_secs(1), runtime.changed())
            .await
            .expect("the listeners woke the loop");
        let mut state = LoopState::default();
        let end_checks = async {
            started.notified().await;
            runtime.notify_changed();
        };
        let (pass, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(pass_loop.sync(&mut state), end_checks)
        })
        .await
        .expect("the pass ends without waiting for the checks");
        assert_eq!(pass, Pass::Interrupted);
        let at: chrono::DateTime<chrono::Utc> = "2026-11-26T10:00:00Z".parse().unwrap();
        assert_eq!(
            state.reported_consent_at,
            Some(at),
            "reported before the checks"
        );
        let report = state.last_report.expect("a report was written");
        assert!(report.https.is_some());
        assert!(report.private_checks.is_empty(), "no check result yet");
    }

    /// A consent change while the inside checks are pending ends the pass
    /// at once, without waiting for them, so the next pass applies it.
    #[tokio::test]
    async fn a_consent_change_during_the_inside_checks_ends_the_pass() {
        let runtime = wildcard();
        let started = Arc::new(Notify::new());
        let pass_loop = pass_loop(&runtime, Containers(None), Arc::clone(&started));
        let mut state = LoopState::default();
        let change = async {
            started.notified().await;
            runtime.notify_changed();
        };
        // The wake-up the proxy's listeners left, which the loop consumes
        // while it waits for them before its first pass.
        tokio::time::timeout(Duration::from_secs(1), runtime.changed())
            .await
            .expect("the listeners woke the loop");
        let (pass, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(pass_loop.sync(&mut state), change)
        })
        .await
        .expect("the pass ends without waiting for the checks");
        assert_eq!(pass, Pass::Interrupted);
        // Nothing of the checks is reported (and this mock database takes
        // no write of the consent before them).
        assert!(state.last_report.is_none(), "nothing is reported of it");
    }

    /// A withdrawal that commits while a pass awaits the machine's networks
    /// is not undone by that pass: what it decided from the consent it read
    /// is not published, and the pass ends to read the consent again.
    #[tokio::test]
    async fn a_pass_does_not_serve_a_consent_withdrawn_while_it_ran() {
        let runtime = wildcard();
        let pass_loop = pass_loop(
            &runtime,
            Containers(Some(Arc::clone(&runtime))),
            Arc::new(Notify::new()),
        );
        let mut state = LoopState::default();
        let pass = tokio::time::timeout(Duration::from_secs(5), pass_loop.sync(&mut state))
            .await
            .expect("the pass ends");
        assert_eq!(pass, Pass::Interrupted);
        assert_eq!(runtime.serving(), None);
        assert!(state.last_report.is_none());
    }

    #[tokio::test]
    async fn the_first_pass_waits_for_the_proxy_to_record_its_listeners() {
        let runtime = ControlPlanePrivateIngress::new();
        let shutdown = Notify::new();
        let later = Arc::clone(&runtime);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            later.set_proxy_listeners(vec!["0.0.0.0:443".parse().unwrap()]);
        });
        let started = tokio::time::Instant::now();
        assert!(wait_for_proxy_listeners(&runtime, &shutdown, Duration::from_secs(30)).await);
        assert!(runtime.proxy_listeners().is_some());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "woken, not timed out"
        );
    }

    #[tokio::test]
    async fn the_first_pass_runs_anyway_when_no_proxy_records_listeners() {
        let runtime = ControlPlanePrivateIngress::new();
        let shutdown = Notify::new();
        assert!(wait_for_proxy_listeners(&runtime, &shutdown, Duration::from_millis(20)).await);
        assert!(runtime.proxy_listeners().is_none());
    }

    #[tokio::test]
    async fn shutdown_ends_the_wait_for_the_proxy() {
        let runtime = ControlPlanePrivateIngress::new();
        let shutdown = Notify::new();
        shutdown.notify_one();
        assert!(!wait_for_proxy_listeners(&runtime, &shutdown, Duration::from_secs(30)).await);
    }

    #[test]
    fn tailscale_resolves_to_its_ipv4_address_and_says_when_there_is_none() {
        assert_eq!(
            resolve_listen(PrivateListen::Tailscale, &own())
                .unwrap()
                .address,
            "100.101.102.103".parse::<std::net::IpAddr>().unwrap()
        );
        let no_tailscale = &own()[..4];
        assert!(resolve_listen(PrivateListen::Tailscale, no_tailscale)
            .unwrap_err()
            .contains("tailscale up"));
        assert!(
            resolve_listen(PrivateListen::Address("10.9.9.9".parse().unwrap()), &own())
                .unwrap_err()
                .contains("not an address of this machine")
        );
    }

    #[test]
    fn consent_on_a_covered_address_serves_it_and_reports_it_like_a_worker() {
        let decision = decide(
            consent(PrivateListen::Tailscale, &[]),
            &own(),
            Some(&containers()),
            &wildcard(),
        );
        let serving = decision.serving.unwrap();
        assert_eq!(serving.address.to_string(), "100.101.102.103");
        let report = decision.report.unwrap();
        assert_eq!(report.mode, Some(IngressMode::Private));
        assert_eq!(report.https.as_deref(), Some("100.101.102.103:443"));
        assert_eq!(report.error, None);
        assert_eq!(report.admit, ["100.64.0.0/10", "fd7a:115c:a1e0::/48"]);
        // The Tailscale interface is marked; loopback and bridges are left
        // out.
        assert!(report
            .interfaces
            .iter()
            .any(|i| i.tailscale && i.address == "100.101.102.103"));
        assert!(!report
            .interfaces
            .iter()
            .any(|i| i.name == "docker0" || i.name == "lo"));
    }

    #[test]
    fn http_is_reported_only_where_its_listener_takes_the_address() {
        let tailscale = consent(PrivateListen::Tailscale, &[]);
        // HTTPS on every address, HTTP on loopback only: workers are told
        // of the HTTPS port alone.
        let decision = decide(
            tailscale.clone(),
            &own(),
            Some(&containers()),
            &runtime(&["127.0.0.1:80", "0.0.0.0:443"]),
        );
        let report = decision.report.unwrap();
        assert_eq!(report.https.as_deref(), Some("100.101.102.103:443"));
        assert_eq!(report.http, None);
        // Both on every address: both are reported.
        let report = decide(tailscale, &own(), Some(&containers()), &wildcard())
            .report
            .unwrap();
        assert_eq!(report.http.as_deref(), Some("100.101.102.103:80"));
    }

    #[test]
    fn another_private_network_admits_its_own_subnet() {
        let decision = decide(
            consent(PrivateListen::Address("10.8.0.1".parse().unwrap()), &[]),
            &own(),
            Some(&containers()),
            &wildcard(),
        );
        let serving = decision.serving.unwrap();
        assert_eq!(serving.admit, vec!["10.8.0.0/24".parse::<IpNet>().unwrap()]);
        assert_eq!(decision.report.unwrap().admit, ["10.8.0.0/24"]);
    }

    #[test]
    fn named_ranges_replace_the_default() {
        let decision = decide(
            consent(
                PrivateListen::Address("10.8.0.1".parse().unwrap()),
                &["10.8.0.0/25", "192.168.50.0/24"],
            ),
            &own(),
            Some(&containers()),
            &wildcard(),
        );
        assert_eq!(
            decision.report.unwrap().admit,
            ["10.8.0.0/25", "192.168.50.0/24"]
        );
    }

    #[test]
    fn a_range_over_a_container_network_serves_nothing() {
        for admit in [&["172.16.0.0/12"][..], &["10.0.0.0/8"][..]] {
            let decision = decide(
                consent(PrivateListen::Address("10.8.0.1".parse().unwrap()), admit),
                &own(),
                Some(&containers()),
                &wildcard(),
            );
            assert_eq!(decision.serving, None, "{admit:?}");
            let error = decision.report.unwrap().error.unwrap();
            assert!(error.contains("container network"), "{error}");
        }
    }

    #[test]
    fn a_proxy_bound_elsewhere_serves_nothing_and_says_how_to_fix_it() {
        let decision = decide(
            consent(PrivateListen::Tailscale, &[]),
            &own(),
            Some(&containers()),
            &runtime(&["203.0.113.7:443"]),
        );
        assert_eq!(decision.serving, None);
        let report = decision.report.unwrap();
        assert_eq!(report.https, None);
        assert!(report.error.unwrap().contains("--tls-address=0.0.0.0:443"));
    }

    /// A Docker listing that fails or times out is unknown, not empty, and
    /// an earlier listing is no stand-in: a network created since (here a
    /// custom-named bridge, which no interface prefix reveals) would be
    /// missed. Unknown serves nothing, and no network is offered as usable.
    #[test]
    fn a_failed_docker_listing_serves_nothing_even_after_an_empty_one() {
        let consent = || {
            consent(
                PrivateListen::Address("10.8.0.1".parse().unwrap()),
                &["10.8.0.0/24"],
            )
        };
        // Listed, empty: served.
        assert!(decide(consent(), &own(), Some(&[]), &wildcard())
            .serving
            .is_some());
        assert!(offered_networks(&own(), Some(&[]))
            .iter()
            .any(|o| o.problem.is_none()));

        // The next listing fails: refused, the report says why, and every
        // option says it cannot be served now.
        let decision = decide(consent(), &own(), None, &wildcard());
        assert_eq!(decision.serving, None);
        assert_eq!(
            decision.report.unwrap().error.as_deref(),
            Some(CONTAINER_NETWORKS_UNKNOWN)
        );
        let offered = offered_networks(&own(), None);
        assert!(!offered.is_empty());
        assert!(offered.iter().all(|o| o.problem.is_some()));

        // Listed again with a custom-named bridge in the range: refused.
        let custom: Vec<(String, IpNet)> =
            vec![("br-apps".to_string(), "10.8.0.0/24".parse().unwrap())];
        assert_eq!(
            decide(consent(), &own(), Some(&custom), &wildcard()).serving,
            None
        );
    }

    /// A console running without the proxy forgets node 0's report, row 0
    /// and nothing else, so no plan offers a listener nobody serves.
    #[tokio::test]
    async fn a_split_console_forgets_node_zeros_listener() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        assert_eq!(forget_control_plane_report(&db).await.unwrap(), 1);
        let statements: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(|s| s.to_string())
            .collect();
        assert_eq!(statements.len(), 1);
        assert!(
            statements[0].starts_with("DELETE FROM \"node_public_ingress\"")
                && statements[0].contains("\"node_id\" = 0"),
            "{}",
            statements[0]
        );
    }

    #[test]
    fn no_consent_serves_and_reports_nothing() {
        let decision = decide(None, &own(), Some(&containers()), &wildcard());
        assert_eq!(
            decision,
            PrivateIngressDecision {
                serving: None,
                report: None
            }
        );
    }

    #[test]
    fn a_new_consent_is_reported_even_when_the_outcome_is_the_same() {
        let report = ListenerReport {
            error: Some("the proxy listens on 127.0.0.1:443 only".into()),
            ..ListenerReport::default()
        };
        let first = chrono::Utc::now();
        let state = LoopState {
            last_report: Some(report.clone()),
            reported_consent_at: Some(first),
            ..LoopState::default()
        };
        assert!(!must_write(&state, &report, false, Some(first)));
        let changed = first + chrono::Duration::seconds(1);
        assert!(must_write(&state, &report, false, Some(changed)));
        assert!(must_write(&state, &report, true, Some(first)));
    }

    fn address(n: usize) -> (String, PrivateAddress) {
        (
            format!("app{n:03}.internal.example.test"),
            PrivateAddress {
                address_id: format!("environment:{n}"),
                range: Some("100.64.0.0/10".into()),
                node_id: Some(CONTROL_PLANE_NODE_ID),
                listener: Some("100.101.102.103".into()),
                via: Some("listener".into()),
                principals: None,
            },
        )
    }

    fn passed(host: &str) -> PrivateCheck {
        PrivateCheck {
            address_id: String::new(),
            hostname: host.to_string(),
            listener: "100.101.102.103".into(),
            outcome: temps_core::worker_ingress::PrivateCheckOutcome::Reachable,
            message: "answered".into(),
            status: Some(200),
            checked_at: None,
        }
    }

    /// The checker checks at most 64 hostnames a call; the 65th and later
    /// ones get their turn on the following passes instead of never.
    #[test]
    fn every_hostname_gets_its_turn_past_one_batch() {
        let hosts: Vec<_> = (0..65).map(address).collect();
        let mut rotation = CheckRotation::default();
        let start = Instant::now();

        let first = rotation.next(&hosts, CHECKS_PER_PASS);
        assert_eq!(first.len(), CHECKS_PER_PASS);
        let results = first.iter().map(|(h, _)| passed(h)).collect();
        rotation.record(&first, results, start);
        assert!(rotation.has_unchecked(&hosts), "the 65th is still waiting");

        // The next pass starts with the one never checked, then the oldest.
        let second = rotation.next(&hosts, CHECKS_PER_PASS);
        assert_eq!(second[0].0, "app064.internal.example.test");
        let results = second.iter().map(|(h, _)| passed(h)).collect();
        rotation.record(&second, results, start + Duration::from_secs(1));
        assert!(!rotation.has_unchecked(&hosts));
        assert_eq!(rotation.report().len(), 65);

        // Past the store's bound the oldest results go: the last batch
        // checked leads the report.
        let report = rotation.report();
        let freshest: Vec<&str> = report[..second.len()]
            .iter()
            .map(|c| c.hostname.as_str())
            .collect();
        assert!(freshest.contains(&"app064.internal.example.test"));
        assert_eq!(
            report.last().map(|c| c.hostname.as_str()),
            Some("app063.internal.example.test"),
            "the one result not checked again is the oldest"
        );

        // Each result says when it ran. The one carried from the first
        // batch keeps its time: the report does not make it look checked
        // again with the second.
        let carried = report.last().and_then(|c| c.checked_at).unwrap();
        assert!(report.iter().all(|c| c.checked_at.is_some()));
        assert!(report[..second.len()]
            .iter()
            .all(|c| c.checked_at.unwrap() >= carried));

        // A result the checker stamped when it ran keeps that time, not
        // the end of its batch.
        let third = rotation.next(&hosts, 1);
        let early = chrono::Utc::now() - chrono::Duration::minutes(2);
        let mut stamped = passed(&third[0].0);
        stamped.checked_at = Some(early);
        rotation.record(&third, vec![stamped], start + Duration::from_secs(2));
        let kept = rotation
            .report()
            .into_iter()
            .find(|c| c.hostname == third[0].0)
            .and_then(|c| c.checked_at);
        assert_eq!(kept, Some(early));

        // A hostname no longer served drops out of the report.
        rotation.retain(&hosts[..10]);
        assert_eq!(rotation.report().len(), 10);
    }

    /// Each hostname is due by its own result's age: a batch checked a
    /// whole interval ago is due, one just checked is not, and one never
    /// checked always is.
    #[test]
    fn each_hostname_is_due_by_the_age_of_its_own_result() {
        let hosts: Vec<_> = (0..3).map(address).collect();
        let mut rotation = CheckRotation::default();
        let now = Instant::now();
        let Some(long_ago) = now.checked_sub(CHECK_INTERVAL + Duration::from_secs(1)) else {
            return;
        };
        rotation.record(&hosts[..1], vec![passed(&hosts[0].0)], long_ago);
        rotation.record(&hosts[1..2], vec![passed(&hosts[1].0)], now);
        let due: Vec<String> = rotation
            .due(&hosts, CHECK_INTERVAL)
            .into_iter()
            .map(|(host, _)| host)
            .collect();
        assert_eq!(due, vec![hosts[0].0.clone(), hosts[2].0.clone()]);
    }

    #[test]
    fn the_report_holds_only_as_many_results_as_the_store_keeps() {
        let hosts: Vec<(String, PrivateAddress)> =
            (0..MAX_PRIVATE_CHECKS + 40).map(address).collect();
        let mut rotation = CheckRotation::default();
        let start = Instant::now();
        // The first 40 are checked first, so they are the oldest.
        let (old, new) = hosts.split_at(40);
        rotation.record(old, old.iter().map(|(h, _)| passed(h)).collect(), start);
        rotation.record(
            new,
            new.iter().map(|(h, _)| passed(h)).collect(),
            start + Duration::from_secs(1),
        );
        // The store itself keeps no more than a report carries.
        assert_eq!(rotation.results.len(), MAX_PRIVATE_CHECKS);
        let report = rotation.report();
        assert_eq!(report.len(), MAX_PRIVATE_CHECKS);
        assert!(report
            .iter()
            .all(|c| new.iter().any(|(h, _)| *h == c.hostname)));
        // Still sorted, newest first then by name.
        assert_eq!(report[0].hostname, new[0].0);
    }

    #[test]
    fn container_networks_include_bridge_interfaces() {
        let names: Vec<String> = containers().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["app-net", "docker0"]);
    }
}
