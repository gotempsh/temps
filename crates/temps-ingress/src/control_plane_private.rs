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
/// may overlap one (ADR-115 D8).
#[async_trait]
pub trait ContainerNetworks: Send + Sync {
    async fn networks(&self) -> Vec<(String, IpNet)>;
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
    containers: &[(String, IpNet)],
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
            let at = |port: u16| Some(std::net::SocketAddr::new(serving.address, port).to_string());
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

/// What the loop keeps between passes.
#[derive(Default)]
struct LoopState {
    last_report: Option<ListenerReport>,
    last_hosts: Vec<(String, PrivateAddress)>,
    last_checks: Vec<PrivateCheck>,
    checked_at: Option<Instant>,
    /// When the consent last written about was given: a new consent is
    /// always answered with a fresh report, even an identical one, since
    /// the status reads only reports made after the consent.
    reported_consent_at: Option<chrono::DateTime<chrono::Utc>>,
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
                // A consent change during the inside checks ends the pass
                // early; sync again at once rather than after the poll.
                if this.sync(&mut state).await == Pass::Interrupted {
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

    async fn sync(&self, state: &mut LoopState) -> Pass {
        // Read before the consent: a consent written after this, while the
        // pass awaits the machine's networks or its checks, outdates every
        // decision the pass makes, and nothing it decided is published.
        let generation = self.runtime.consent_generation();
        let consent = match self.consent.get().await {
            Ok(consent) => consent,
            Err(error) => {
                // Keep serving what was consented; the database may be
                // briefly unavailable.
                warn!(%error, "could not read the private ingress consent; keeping the last state");
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
        let own = self.addresses.addresses();
        let containers = all_container_networks(&own, self.containers.networks().await);
        self.runtime
            .set_networks(network_options(&own, &containers));
        let decision = decide(input, &own, &containers, &self.runtime);
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
            state.last_checks.clear();
            state.last_hosts.clear();
            if self.runtime.consent_generation() != generation {
                return Pass::Interrupted;
            }
            if let Err(error) = self.clear_report().await {
                warn!(%error, "could not clear node 0's private ingress report");
            }
            return Pass::Done;
        };

        // Inside checks (ADR 064 D7): after the addresses change, else
        // every CHECK_INTERVAL, and only while something is served.
        let mut hosts = self.route_table.control_plane_private_hosts();
        hosts.sort_by(|a, b| a.0.cmp(&b.0));
        let due = state
            .checked_at
            .is_none_or(|at| at.elapsed() >= CHECK_INTERVAL)
            || hosts != state.last_hosts;
        let mut ran_checks = false;
        if decision.serving.is_some() && due {
            ran_checks = true;
            state.last_checks = if hosts.is_empty() {
                Vec::new()
            } else {
                // Up to one DNS timeout per host: a withdrawn or narrowed
                // consent must not wait for all of them. The checks are of
                // a decision that no longer holds, so they are dropped.
                tokio::select! {
                    checks = self.checker.check(hosts.clone()) => checks,
                    _ = self.runtime.changed() => {
                        debug!("the private ingress consent changed during the inside checks; syncing again");
                        return Pass::Interrupted;
                    }
                }
            };
            state.last_hosts = hosts;
            state.checked_at = Some(Instant::now());
        } else if decision.serving.is_none() {
            state.last_checks.clear();
        }
        report.private_checks = state.last_checks.clone();

        // Written when it changes, with every fresh check, and once for
        // every new consent.
        let consent_at = consent.as_ref().map(|c| c.updated_at);
        if !must_write(state, &report, ran_checks, consent_at) {
            return Pass::Done;
        }
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
            }
            Err(error) => warn!(%error, "could not write node 0's private ingress report"),
        }
        Pass::Done
    }

    async fn clear_report(&self) -> Result<(), sea_orm::DbErr> {
        temps_entities::node_public_ingress::Entity::delete_by_id(CONTROL_PLANE_NODE_ID)
            .exec(self.db.as_ref())
            .await?;
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
        async fn networks(&self) -> Vec<(String, IpNet)> {
            if let Some(runtime) = &self.0 {
                runtime.consent_written(true);
            }
            Vec::new()
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
            &containers(),
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
    fn another_private_network_admits_its_own_subnet() {
        let decision = decide(
            consent(PrivateListen::Address("10.8.0.1".parse().unwrap()), &[]),
            &own(),
            &containers(),
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
            &containers(),
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
                &containers(),
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
            &containers(),
            &runtime(&["203.0.113.7:443"]),
        );
        assert_eq!(decision.serving, None);
        let report = decision.report.unwrap();
        assert_eq!(report.https, None);
        assert!(report.error.unwrap().contains("--tls-address=0.0.0.0:443"));
    }

    #[test]
    fn no_consent_serves_and_reports_nothing() {
        let decision = decide(None, &own(), &containers(), &wildcard());
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

    #[test]
    fn container_networks_include_bridge_interfaces() {
        let names: Vec<String> = containers().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["app-net", "docker0"]);
    }
}
