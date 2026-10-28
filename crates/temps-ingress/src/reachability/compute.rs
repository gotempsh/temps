// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether each address of an environment is reachable, from facts the
//! service gathered (ADR 064 D3). Pure: no database, no network.
//!
//! Reachability is per address. Under ADR-020 any ingress serves any app and
//! forwards over the mesh, so the test is "does a way in serve this hostname,
//! and does DNS lead there", never "does the worker it runs on have one".

use std::net::IpAddr;

use temps_core::tailnet::{messages as tailnet_messages, TailnetCheckOutcome, TailscaleReport};
use temps_core::tailnet::{TailscaleState, TAILSCALE_SERVE_COMMAND};
use temps_core::worker_ingress::{address_scope, AddressScope, PrivateCheckOutcome};

use super::model::{
    ActionKind, AddressReachability, AddressSource, Audience, ReachAction, ReachState,
};
use super::run::unrecorded_message;

/// The ingress setup page, where tunnels and worker ingress are managed.
pub const INGRESS_SETUP_PATH: &str = "/settings/nodes/ingress";

/// One address to judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressInput {
    pub address: String,
    pub audience: Audience,
    pub source: AddressSource,
    pub custom_domain_id: Option<i32>,
    /// For a private address: what it was set up on.
    pub private: Option<PrivateBinding>,
}

/// What a private address records about the listener serving it (ADR 064
/// D5), next to its `audience`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrivateBinding {
    /// `custom:<id>` or `environment:<id>`: what the worker's inside check
    /// reports it under.
    pub address_id: String,
    pub range: Option<String>,
    pub node_id: Option<i32>,
    pub listener: Option<String>,
    /// ADR 065 D1: `tailscale_serve` or `tailscale_service`; `None` (or
    /// `listener`) for the phase 2 listener.
    pub via: Option<String>,
    /// For `tailscale_service`: the `svc:` name.
    pub service: Option<String>,
}

impl PrivateBinding {
    /// Served through tailscaled rather than the private listener.
    pub fn on_tailscale(&self) -> bool {
        self.via
            .as_deref()
            .is_some_and(|v| v != temps_entities::audience::via::LISTENER)
    }
}

/// The worker's last inside check of a private address (ADR 064 D7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsideCheck {
    pub outcome: PrivateCheckOutcome,
    pub message: String,
    /// When the control plane received it, in minutes before now.
    pub minutes_ago: i64,
}

/// What the control plane knows of the worker serving a private address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateFacts {
    pub worker: String,
    /// `Err` with why the worker's private listener does not serve the
    /// recorded listener address right now.
    pub bound: Result<(), String>,
    /// The listener is on the worker's Tailscale interface.
    pub tailscale: bool,
    pub inside: Option<InsideCheck>,
    /// For an address served through tailscaled (ADR 065 D5).
    pub tailnet: Option<TailnetFacts>,
    /// For `tailnet_users` (ADR 065 D6): why the worker cannot serve it to
    /// the people listed (nobody is, or its agent cannot check who they
    /// are). `None` when it can, and for every other audience.
    pub tailnet_users: Option<String>,
}

/// What the control plane knows of a tailnet name's worker and its Serve
/// entry (ADR 065 D4, D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetFacts {
    /// The worker's last tailscaled report, with its inside checks.
    pub report: TailscaleReport,
    /// How old that report is.
    pub minutes_ago: i64,
    /// The address's `tailnet_serve_entries` row on the worker.
    pub entry: Option<ServeEntryState>,
    /// For a Service: its hosts, read from the Tailscale API.
    pub service: Option<ServiceHostsState>,
}

/// A Service's hosts as the outside check read them (ADR 065 D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceHostsState {
    /// `svc:myapp`.
    pub name: String,
    /// The tag hosts are approved by, for the autoApprovers snippet.
    pub tag: String,
    /// Hosts none of the address's Serve entries accounts for, by name;
    /// `Err` when they could not be read.
    pub unrecorded: Result<Vec<String>, String>,
    /// Whether the admin approved the worker as a host; `None` when it
    /// does not advertise the Service (yet), or the answer did not say.
    pub approved: Option<bool>,
}

/// A Serve entry and what the worker last said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeEntryState {
    pub entry_id: i32,
    pub hostname: String,
    pub removing: bool,
    /// `applied`, `conflict` or `error`; `None` before the first report.
    pub report: Option<String>,
    pub detail: Option<String>,
}

/// An inside check older than this says the worker stopped reporting.
pub const INSIDE_CHECK_STALE_MINUTES: i64 = 20;

/// The command that turns a worker's private listener on.
pub const PRIVATE_COMMAND: &str = "temps agent ingress private --listen tailscale";

/// A way in that is a listener of Temps' own: the control plane's proxy or
/// a worker's direct ingress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectIngress {
    /// `control-plane`, or the worker's name.
    pub name: String,
    /// `None` for the control plane.
    pub node_id: Option<i32>,
    /// Addresses DNS should point at to reach it.
    pub ips: Vec<IpAddr>,
    /// Whether it answers plain HTTP on port 80, which HTTP-01 needs.
    pub takes_port_80: bool,
    /// The last reachability check from the control plane failed, and why.
    pub check_failed: Option<String>,
}

/// ADR 058 D9's per-hostname tunnel state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelState {
    /// `published`, `pending`, `error` or `not_published`.
    pub state: String,
    pub connector_name: Option<String>,
    pub detail: String,
    /// Whether some tunnel's domain covers it at all.
    pub has_candidates: bool,
}

/// What DNS answered for the address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Ips(Vec<IpAddr>),
    NoRecord,
    Failed(String),
    /// Not looked up: the address was settled without DNS.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressFacts {
    pub input: AddressInput,
    pub tunnel: Option<TunnelState>,
    pub resolution: Resolution,
    /// Temps manages a DNS record for it (outside tunnels).
    pub temps_managed_dns: bool,
    /// For a private address with a recorded worker.
    pub private: Option<PrivateFacts>,
}

/// Names that only ever resolve to the machine they are opened on.
pub fn is_local_only(address: &str) -> bool {
    let host = address.trim_end_matches('.').to_ascii_lowercase();
    ["localhost", "localho.st"]
        .iter()
        .any(|base| host == *base || host.ends_with(&format!(".{base}")))
}

fn ip_list(ips: &[IpAddr]) -> String {
    ips.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn make_reachable() -> ReachAction {
    ReachAction::new(ActionKind::MakeReachable, "Make it reachable")
}

/// Whether DNS has to be looked up to judge this address: not for local
/// names, undeployed environments or tunnel-settled ones.
pub fn needs_lookup(address: &str, deployed: bool, tunnel: Option<&TunnelState>) -> bool {
    !is_local_only(address)
        && deployed
        && !matches!(
            tunnel.map(|t| t.state.as_str()),
            Some("published" | "pending" | "error")
        )
}

/// Judge one address.
pub fn address_reachability(
    facts: &AddressFacts,
    directs: &[DirectIngress],
    deployed: bool,
    environment_name: &str,
) -> AddressReachability {
    let input = &facts.input;
    let address = input.address.as_str();
    let verdict = |state: ReachState,
                   way_in: Option<String>,
                   reason: String,
                   action: Option<ReachAction>| AddressReachability {
        address: input.address.clone(),
        audience: input.audience,
        source: input.source,
        custom_domain_id: input.custom_domain_id,
        way_in,
        state,
        reason,
        action,
        network: None,
        nothing_set_up: false,
        findings: Vec::new(),
    };
    // ADR 068 D4: only these verdicts may read as "no address"; every other
    // not-reachable one is a way in that broke.
    let never_set_up = |mut verdict: AddressReachability| {
        verdict.nothing_set_up = true;
        verdict
    };

    if !input.audience.is_served() {
        // Routing serves these to nobody (`temps-routes`); say so rather
        // than judge the public ways in.
        let reason = match input.audience {
            Audience::SignedIn => format!(
                "{address} is for people who sign in, which this version of Temps cannot \
                 serve yet, so nobody can open it."
            ),
            _ => format!(
                "{address} was set up by a newer version of Temps for an audience this one \
                 does not know, so nobody can open it until Temps is upgraded again."
            ),
        };
        return never_set_up(verdict(ReachState::NotReachable, None, reason, None));
    }
    if is_local_only(address) {
        return never_set_up(verdict(
            ReachState::NotReachable,
            None,
            format!(
                "{address} only resolves to the computer it is opened on, so nobody else can \
                 reach it."
            ),
            Some(make_reachable()),
        ));
    }
    if !deployed {
        return never_set_up(verdict(
            ReachState::NotReachable,
            None,
            format!("Nothing is deployed to {environment_name} yet."),
            Some(ReachAction::new(ActionKind::Deploy, "Deploy it")),
        ));
    }
    if input.audience.is_private() {
        let judged = private_reachability(facts, environment_name);
        // ADR 065 D6: a worker that cannot tell who a visitor is is never
        // sent the address, so nobody can open it there.
        if input.audience == Audience::TailnetUsers && judged.state != ReachState::NotReachable {
            if let Some(why) = facts.private.as_ref().and_then(|p| p.tailnet_users.clone()) {
                return AddressReachability {
                    state: ReachState::NotReachable,
                    reason: why,
                    action: Some(
                        ReachAction::new(ActionKind::OpenIngress, "Open the worker")
                            .with_href("/settings/nodes"),
                    ),
                    ..judged
                };
            }
        }
        return judged;
    }

    let mut tunnel_reason = None;
    if let Some(tunnel) = &facts.tunnel {
        let way_in = tunnel
            .connector_name
            .as_ref()
            .map(|name| format!("tunnel:{name}"));
        match tunnel.state.as_str() {
            "published" => {
                return verdict(ReachState::Reachable, way_in, tunnel.detail.clone(), None)
            }
            "pending" => {
                return verdict(
                    ReachState::Checking,
                    way_in,
                    tunnel.detail.clone(),
                    Some(ReachAction::new(ActionKind::Wait, "Wait a moment")),
                )
            }
            "error" => {
                return verdict(
                    ReachState::NotReachable,
                    way_in,
                    tunnel.detail.clone(),
                    Some(
                        ReachAction::new(ActionKind::OpenIngress, "Open the tunnel")
                            .with_href(INGRESS_SETUP_PATH),
                    ),
                )
            }
            // A tunnel covers it but does not publish it: that is the reason
            // if nothing else serves it either.
            _ if tunnel.has_candidates => tunnel_reason = Some(tunnel.detail.clone()),
            _ => {}
        }
    }

    let ips = match &facts.resolution {
        Resolution::Ips(ips) if !ips.is_empty() => ips,
        Resolution::Failed(error) => {
            return verdict(
                ReachState::Unknown,
                None,
                format!("Could not look up {address}: {error}"),
                None,
            )
        }
        Resolution::Skipped => {
            return verdict(
                ReachState::Unknown,
                None,
                format!("{address} was not checked."),
                None,
            )
        }
        Resolution::Ips(_) | Resolution::NoRecord => {
            // The generated host with no record and no tunnel covering it
            // was never given a way in. A custom or environment domain
            // without a record was set up by someone, so it is an outage.
            let never = input.source == AddressSource::Generated && tunnel_reason.is_none();
            let reason =
                tunnel_reason.unwrap_or_else(|| format!("No DNS record for {address} exists yet."));
            let target = directs
                .iter()
                .find(|d| d.check_failed.is_none() && !d.ips.is_empty());
            let action = match target {
                Some(direct) => Some(ReachAction::new(
                    ActionKind::SetDnsRecord,
                    format!(
                        "Add an A record for {address} pointing at {} ({})",
                        direct.ips[0], direct.name
                    ),
                )),
                None => Some(make_reachable()),
            };
            let verdict = verdict(ReachState::NotReachable, None, reason, action);
            return if never {
                never_set_up(verdict)
            } else {
                verdict
            };
        }
    };

    if let Some(direct) = directs
        .iter()
        .find(|d| d.ips.iter().any(|ip| ips.contains(ip)))
    {
        let way_in = Some(format!("direct:{}", direct.name));
        return match &direct.check_failed {
            Some(why) => verdict(
                ReachState::NotReachable,
                way_in,
                format!(
                    "DNS points at {} ({}), but it failed its last reachability check: {why}",
                    direct.name,
                    ip_list(ips)
                ),
                Some(
                    ReachAction::new(ActionKind::OpenWorker, format!("Check {}", direct.name))
                        .with_href(INGRESS_SETUP_PATH),
                ),
            ),
            None => verdict(
                ReachState::Reachable,
                way_in,
                format!("DNS points at {} ({}).", direct.name, ip_list(ips)),
                None,
            ),
        };
    }

    if ips
        .iter()
        .all(|ip| address_scope(*ip) != AddressScope::Public)
    {
        return verdict(
            ReachState::NotReachable,
            None,
            format!(
                "{address} points at {}, a private address nobody on the internet can reach.",
                ip_list(ips)
            ),
            Some(make_reachable()),
        );
    }

    if facts.temps_managed_dns {
        return verdict(
            ReachState::Reachable,
            Some("direct".to_string()),
            format!(
                "Its DNS record is managed by Temps and points at {}.",
                ip_list(ips)
            ),
            None,
        );
    }

    verdict(
        ReachState::Unknown,
        None,
        format!(
            "{address} points at {}, which is not this control plane or a worker Temps knows. \
             It works if what answers there forwards to Temps.",
            ip_list(ips)
        ),
        None,
    )
}

/// The outside check of a private address, from the control plane (ADR
/// 064 D7): its name resolves only to private addresses, among them the
/// recorded listener, and no tunnel publishes it. `Err` with the verdict.
pub fn outside_check(
    address: &str,
    listener: IpAddr,
    resolution: &Resolution,
    tunnel: Option<&TunnelState>,
) -> Result<(), (ReachState, String, Option<ReachAction>)> {
    if let Some(tunnel) = tunnel {
        if matches!(tunnel.state.as_str(), "published" | "pending" | "error") {
            let name = tunnel
                .connector_name
                .clone()
                .unwrap_or_else(|| "a tunnel".to_string());
            return Err((
                ReachState::NotReachable,
                format!(
                    "{address} is private, but tunnel {name} publishes it, so anyone on the \
                     internet can open it through Cloudflare. Keep it off tunnels under Expose \
                     your apps."
                ),
                Some(
                    ReachAction::new(ActionKind::OpenIngress, "Open the tunnel")
                        .with_href(INGRESS_SETUP_PATH),
                ),
            ));
        }
    }
    let ips = match resolution {
        Resolution::Ips(ips) if !ips.is_empty() => ips,
        Resolution::Failed(error) => {
            return Err((
                ReachState::Unknown,
                format!("Could not look up {address}: {error}"),
                None,
            ))
        }
        Resolution::Skipped => {
            return Err((
                ReachState::Unknown,
                format!("{address} was not checked."),
                None,
            ))
        }
        Resolution::Ips(_) | Resolution::NoRecord => {
            return Err((
                ReachState::NotReachable,
                format!("No DNS record for {address} exists yet; it should point at {listener}."),
                Some(make_reachable()),
            ))
        }
    };
    let public: Vec<IpAddr> = ips
        .iter()
        .copied()
        .filter(|ip| temps_core::worker_ingress::private_block(*ip).is_none())
        .collect();
    if !public.is_empty() {
        return Err((
            ReachState::NotReachable,
            format!(
                "{address} is private, but it resolves to {}, which is not a private address. \
                 Point it only at {listener}, or something else answers for it.",
                ip_list(&public)
            ),
            Some(ReachAction::new(
                ActionKind::SetDnsRecord,
                format!("Point {address} only at {listener}"),
            )),
        ));
    }
    if !ips.contains(&listener) {
        return Err((
            ReachState::NotReachable,
            format!(
                "{address} resolves to {}, not to the private listener it was set up on \
                 ({listener}).",
                ip_list(ips)
            ),
            Some(ReachAction::new(
                ActionKind::SetDnsRecord,
                format!("Point {address} at {listener}"),
            )),
        ));
    }
    Ok(())
}

/// Judge a private address: the outside check, the worker's listener, then
/// the worker's own inside check (ADR 064 D7).
fn private_reachability(facts: &AddressFacts, environment_name: &str) -> AddressReachability {
    let input = &facts.input;
    let address = input.address.as_str();
    let binding = input.private.clone().unwrap_or_default();
    if binding.on_tailscale() {
        return tailnet_reachability(facts, environment_name);
    }
    let verdict = |state: ReachState,
                   way_in: Option<String>,
                   reason: String,
                   action: Option<ReachAction>,
                   network: Option<String>| AddressReachability {
        address: input.address.clone(),
        audience: input.audience,
        source: input.source,
        custom_domain_id: input.custom_domain_id,
        way_in,
        state,
        reason,
        action,
        network,
        nothing_set_up: false,
        findings: Vec::new(),
    };
    let listener = binding
        .listener
        .as_deref()
        .and_then(|l| l.parse::<IpAddr>().ok());
    let (Some(private), Some(listener)) = (&facts.private, listener) else {
        return verdict(
            ReachState::NotReachable,
            None,
            format!(
                "{address} is private, but no worker's private listener serves it yet, so \
                 nobody can open it. Make it reachable on your private network."
            ),
            Some(make_reachable()),
            None,
        );
    };
    let way_in = Some(format!("private:{}", private.worker));
    let network = Some(if private.tailscale {
        "Tailscale".to_string()
    } else {
        binding
            .range
            .clone()
            .unwrap_or_else(|| "your private network".to_string())
    });
    if let Err((state, reason, action)) =
        outside_check(address, listener, &facts.resolution, facts.tunnel.as_ref())
    {
        return verdict(state, way_in, reason, action, network);
    }
    if let Err(why) = &private.bound {
        return verdict(
            ReachState::NotReachable,
            way_in,
            format!(
                "{address} points at {}'s private listener ({listener}), but it is not \
                 serving: {why}",
                private.worker
            ),
            Some(
                ReachAction::new(
                    ActionKind::RunCommand,
                    format!("On {}, as root, run", private.worker),
                )
                .with_command(PRIVATE_COMMAND),
            ),
            network,
        );
    }
    let shown_network = network.clone().unwrap_or_default();
    match &private.inside {
        None => verdict(
            ReachState::Checking,
            way_in,
            format!(
                "Waiting for {} to check {address} from inside {shown_network}; it does after \
                 each change and every 5 minutes.",
                private.worker
            ),
            Some(ReachAction::new(ActionKind::Wait, "Wait a moment")),
            network,
        ),
        Some(check) if check.minutes_ago > INSIDE_CHECK_STALE_MINUTES => verdict(
            ReachState::Unknown,
            way_in,
            format!(
                "{} has not reported on {address} for {} minutes, so Temps cannot tell \
                 whether it still answers. Check that the worker is online.",
                private.worker, check.minutes_ago
            ),
            Some(
                ReachAction::new(ActionKind::OpenWorker, format!("Check {}", private.worker))
                    .with_href(INGRESS_SETUP_PATH),
            ),
            network,
        ),
        Some(check) if check.outcome == PrivateCheckOutcome::Reachable => verdict(
            ReachState::Reachable,
            way_in,
            format!(
                "Private at {address} ({shown_network}): {} answers over HTTPS at {listener} \
                 with a valid certificate, and only {} can connect.",
                private.worker,
                binding.range.as_deref().unwrap_or("its private network")
            ),
            None,
            network,
        ),
        Some(check) => verdict(
            ReachState::NotReachable,
            way_in,
            format!(
                "{} checked {address} from inside {shown_network}: {}",
                private.worker, check.message
            ),
            match check.outcome {
                PrivateCheckOutcome::NoRoute => Some(ReachAction::new(
                    ActionKind::Deploy,
                    format!("Redeploy {environment_name}"),
                )),
                _ => Some(make_reachable()),
            },
            network,
        ),
    }
}

/// A report older than this says the worker stopped reporting.
pub const TAILNET_REPORT_STALE_MINUTES: i64 = 20;

/// Why a worker's tailscaled cannot serve through Serve now, with what
/// fixes it (ADR 065 D5's messages), in the order the agent decides them:
/// what the plan refuses and what an address served there shows.
pub fn tailnet_worker_problem(
    node: &str,
    report: &TailscaleReport,
    minutes_ago: i64,
) -> Option<(String, Option<ReachAction>)> {
    let run = |command: &str| {
        Some(
            ReachAction::new(ActionKind::RunCommand, format!("On {node}, as root, run"))
                .with_command(command),
        )
    };
    if minutes_ago > TAILNET_REPORT_STALE_MINUTES {
        return Some((
            format!(
                "{node} has not reported for {minutes_ago} minutes, so Temps cannot tell whether \
                 Tailscale Serve works there. Check that the worker is online."
            ),
            Some(
                ReachAction::new(ActionKind::OpenWorker, format!("Check {node}"))
                    .with_href(INGRESS_SETUP_PATH),
            ),
        ));
    }
    let tailnet = report
        .tailnet
        .clone()
        .unwrap_or_else(|| "your tailnet".into());
    let name = report
        .dns_name
        .clone()
        .unwrap_or_else(|| format!("{node}.{tailnet}"));
    let version = report.version.clone().unwrap_or_default();
    let problem = |message: String| Some((message, None));
    if !report.consent || report.state == TailscaleState::NoConsent {
        return Some((
            tailnet_messages::no_consent(node),
            run(TAILSCALE_SERVE_COMMAND),
        ));
    }
    if !report.root || report.state == TailscaleState::NotRoot {
        return problem(tailnet_messages::not_root(
            node,
            report.user.as_deref().unwrap_or("another user"),
        ));
    }
    match report.state {
        TailscaleState::NoSocket => return problem(tailnet_messages::no_socket(node)),
        TailscaleState::Unsupported => {
            return problem(tailnet_messages::old_tailscaled(&version, node))
        }
        TailscaleState::NotUnderstood => {
            return problem(report.error.clone().unwrap_or_else(|| {
                format!("{}.", tailnet_messages::not_understood(&version, node))
            }))
        }
        TailscaleState::NoConsent | TailscaleState::NotRoot | TailscaleState::Ready => {}
    }
    match report.backend_state.as_deref() {
        Some("Running") => {}
        other => {
            return Some((
                tailnet_messages::not_logged_in(node, other.unwrap_or("not running")),
                run("sudo tailscale up"),
            ))
        }
    }
    if !report.magic_dns {
        return problem(tailnet_messages::magic_dns_off(&tailnet));
    }
    if !report.https() {
        return problem(tailnet_messages::https_off(&tailnet, &name));
    }
    if report.dns_name.is_none() {
        return problem(format!(
            "{}.",
            tailnet_messages::not_understood(&version, node)
        ));
    }
    None
}

/// The outside check of a tailnet name (ADR 065 D5): no tunnel publishes
/// it, and public DNS returns nothing for it or only Tailscale addresses.
pub fn tailnet_outside_check(
    address: &str,
    resolution: &Resolution,
    tunnel: Option<&TunnelState>,
) -> Result<(), (ReachState, String, Option<ReachAction>)> {
    if let Some(tunnel) = tunnel {
        if matches!(tunnel.state.as_str(), "published" | "pending" | "error") {
            let name = tunnel
                .connector_name
                .clone()
                .unwrap_or_else(|| "a tunnel".to_string());
            return Err((
                ReachState::NotReachable,
                format!(
                    "{address} is private to your tailnet, but tunnel {name} publishes it, so \
                     anyone on the internet can open it through Cloudflare. Keep it off tunnels \
                     under Expose your apps."
                ),
                Some(
                    ReachAction::new(ActionKind::OpenIngress, "Open the tunnel")
                        .with_href(INGRESS_SETUP_PATH),
                ),
            ));
        }
    }
    match resolution {
        Resolution::Failed(error) => Err((
            ReachState::Unknown,
            format!("Could not look up {address}: {error}"),
            None,
        )),
        Resolution::Ips(ips) => {
            let outside: Vec<IpAddr> = ips
                .iter()
                .copied()
                .filter(|ip| !temps_core::tailnet::is_tailscale_ip(*ip))
                .collect();
            if outside.is_empty() {
                Ok(())
            } else {
                Err((
                    ReachState::NotReachable,
                    format!(
                        "{address} is private to your tailnet, but outside it resolves to {}, \
                         which is not a Tailscale address, so something else answers for it.",
                        ip_list(&outside)
                    ),
                    None,
                ))
            }
        }
        Resolution::NoRecord | Resolution::Skipped => Ok(()),
    }
}

/// Judge a tailnet name: the outside check, the worker's tailscaled, its
/// Serve entry, then the worker's inside check (ADR 065 D5).
fn tailnet_reachability(facts: &AddressFacts, environment_name: &str) -> AddressReachability {
    let input = &facts.input;
    let address = input.address.as_str();
    let network = Some("Tailscale".to_string());
    let verdict = |state: ReachState,
                   way_in: Option<String>,
                   reason: String,
                   action: Option<ReachAction>| AddressReachability {
        address: input.address.clone(),
        audience: input.audience,
        source: input.source,
        custom_domain_id: input.custom_domain_id,
        way_in,
        state,
        reason,
        action,
        network: network.clone(),
        // A tailnet name exists once a run made it: set up, so never
        // `no_address` (ADR 068 D4), like a private listener.
        nothing_set_up: false,
        findings: Vec::new(),
    };
    let (Some(private), Some(tailnet)) = (
        &facts.private,
        facts.private.as_ref().and_then(|p| p.tailnet.as_ref()),
    ) else {
        return verdict(
            ReachState::NotReachable,
            None,
            format!(
                "{address} is private to your tailnet, but no worker serves it yet, so nobody \
                 can open it. Make it reachable on your tailnet."
            ),
            Some(make_reachable()),
        );
    };
    let node = private.worker.as_str();
    let way_in = Some(format!("private:{node}"));
    if let Err((state, reason, action)) =
        tailnet_outside_check(address, &facts.resolution, facts.tunnel.as_ref())
    {
        return verdict(state, way_in, reason, action);
    }
    let entry_id = tailnet.entry.as_ref().map(|e| e.entry_id);
    let funnel = tailnet
        .report
        .entries
        .iter()
        .any(|e| Some(e.entry_id) == entry_id && e.funnel)
        || tailnet.report.checks.iter().any(|c| {
            c.hostname.eq_ignore_ascii_case(address) && c.outcome == TailnetCheckOutcome::Funnel
        });
    if funnel {
        return verdict(
            ReachState::NotReachable,
            way_in,
            tailnet_messages::funnel(node),
            Some(
                ReachAction::new(ActionKind::RunCommand, format!("On {node}, as root, run"))
                    .with_command("tailscale funnel --https=443 off"),
            ),
        );
    }
    if let Some(service) = &tailnet.service {
        match &service.unrecorded {
            Err(why) => {
                return verdict(
                    ReachState::Unknown,
                    way_in,
                    format!(
                        "Temps could not read which machines host `{}`, so it cannot tell \
                         whether only {node} serves {address}: {why}",
                        service.name
                    ),
                    Some(
                        ReachAction::new(
                            ActionKind::OpenIngress,
                            "Check the Tailscale OAuth client",
                        )
                        .with_href(INGRESS_SETUP_PATH),
                    ),
                )
            }
            Ok(foreign) if !foreign.is_empty() => {
                return verdict(
                    ReachState::NotReachable,
                    way_in,
                    unrecorded_message(&service.name, foreign),
                    None,
                )
            }
            Ok(_) => {}
        }
    }
    if let Err(why) = &private.bound {
        return verdict(
            ReachState::NotReachable,
            way_in,
            format!("{address} is served by {node}, but {why}"),
            Some(
                ReachAction::new(ActionKind::OpenWorker, format!("Check {node}"))
                    .with_href(INGRESS_SETUP_PATH),
            ),
        );
    }
    if let Some((reason, action)) =
        tailnet_worker_problem(node, &tailnet.report, tailnet.minutes_ago)
    {
        let state = if tailnet.minutes_ago > TAILNET_REPORT_STALE_MINUTES {
            ReachState::Unknown
        } else {
            ReachState::NotReachable
        };
        return verdict(state, way_in, reason, action);
    }
    let Some(entry) = &tailnet.entry else {
        return verdict(
            ReachState::NotReachable,
            way_in,
            format!(
                "{address} is private to your tailnet, but Temps no longer asks {node} to serve \
                 it. Make it reachable again."
            ),
            Some(make_reachable()),
        );
    };
    if entry.removing {
        return verdict(
            ReachState::NotReachable,
            way_in,
            format!("{node} is taking {address} off its Serve config."),
            Some(make_reachable()),
        );
    }
    match entry.report.as_deref() {
        Some(temps_entities::tailnet_serve_entries::REPORT_APPLIED) => {}
        Some(temps_entities::tailnet_serve_entries::REPORT_CONFLICT) => {
            return verdict(
                ReachState::NotReachable,
                way_in,
                tailnet_messages::slot_taken(
                    &entry.hostname,
                    entry.detail.as_deref().unwrap_or("another handler"),
                ),
                None,
            )
        }
        Some(temps_entities::tailnet_serve_entries::REPORT_ERROR) => {
            return verdict(
                ReachState::NotReachable,
                way_in,
                format!(
                    "{node} could not write {address} to its Serve config: {}",
                    entry.detail.as_deref().unwrap_or("it did not say why.")
                ),
                Some(
                    ReachAction::new(ActionKind::OpenWorker, format!("Check {node}"))
                        .with_href(INGRESS_SETUP_PATH),
                ),
            )
        }
        _ => {
            return verdict(
                ReachState::Checking,
                way_in,
                format!("Waiting for {node} to serve {address} with Tailscale Serve."),
                Some(ReachAction::new(ActionKind::Wait, "Wait a moment")),
            )
        }
    }
    if let Some(service) = &tailnet.service {
        match service.approved {
            Some(true) => {}
            Some(false) => {
                return verdict(
                    ReachState::NotReachable,
                    way_in,
                    tailnet_messages::not_approved(&service.name, node, &service.tag),
                    // The message names both ways to approve it.
                    None,
                );
            }
            None => {
                return verdict(
                    ReachState::Checking,
                    way_in,
                    format!(
                        "Waiting for {node} to advertise `{}`; its agent does on its next pass, \
                         every 30 seconds.",
                        service.name
                    ),
                    Some(ReachAction::new(ActionKind::Wait, "Wait a moment")),
                )
            }
        }
    }
    let check = tailnet.report.checks.iter().find(|c| {
        Some(c.address_id.as_str()) == input.private.as_ref().map(|b| b.address_id.as_str())
            && c.hostname.eq_ignore_ascii_case(address)
    });
    match check {
        None => verdict(
            ReachState::Checking,
            way_in,
            format!(
                "Waiting for {node} to check {address} from inside your tailnet; it does after \
                 each change and every 5 minutes."
            ),
            Some(ReachAction::new(ActionKind::Wait, "Wait a moment")),
        ),
        Some(check) if check.outcome == TailnetCheckOutcome::Reachable => verdict(
            ReachState::Reachable,
            way_in,
            format!(
                "Private at {address} (Tailscale): {node} serves it with Tailscale Serve, and \
                 only devices on your tailnet can open it."
            ),
            None,
        ),
        Some(check) => verdict(
            ReachState::NotReachable,
            way_in,
            format!(
                "{node} checked {address} from inside your tailnet: {}",
                check.message
            ),
            match check.outcome {
                TailnetCheckOutcome::NoRoute => Some(ReachAction::new(
                    ActionKind::Deploy,
                    format!("Redeploy {environment_name}"),
                )),
                _ => Some(make_reachable()),
            },
        ),
    }
}

/// The environment's overall state: reachable when any address is.
pub fn overall_state(addresses: &[AddressReachability]) -> ReachState {
    let any = |state: ReachState| addresses.iter().any(|a| a.state == state);
    if any(ReachState::Reachable) {
        ReachState::Reachable
    } else if any(ReachState::Checking) {
        ReachState::Checking
    } else if any(ReachState::Unknown) {
        ReachState::Unknown
    } else {
        ReachState::NotReachable
    }
}

/// The header's line. It names the address D4 would pick (ADR 068), so the
/// header and the primary address never disagree: custom and environment
/// domains first, then a tunnel host, then the generated hostname, which is
/// usually the less memorable one.
pub fn summary(
    addresses: &[AddressReachability],
    deployed: bool,
    environment_name: &str,
    running_on: &[String],
) -> String {
    let rank = super::addresses::rank;
    let best = |state: ReachState| {
        addresses
            .iter()
            .filter(|a| a.state == state)
            .min_by_key(|a| rank(a))
    };
    if let Some(address) = best(ReachState::Reachable) {
        return match (address.audience, &address.network) {
            (Audience::Private, Some(network)) => {
                format!("Private at {} ({network})", address.address)
            }
            (Audience::Private, None) => format!("Private at {}", address.address),
            (Audience::SignedIn, _) => format!("Behind sign-in at {}", address.address),
            (Audience::TailnetUsers, _) => {
                format!("Only for listed tailnet users at {}", address.address)
            }
            _ => format!("Public at {}", address.address),
        };
    }
    if let Some(address) = best(ReachState::Checking) {
        return format!("Being set up at {}: {}", address.address, address.reason);
    }
    if !deployed {
        return format!("Nothing is deployed to {environment_name} yet.");
    }
    let place = match running_on {
        [] => String::new(),
        [one] => format!(" on {one}"),
        many => format!(" on {}", many.join(", ")),
    };
    if let Some(address) = best(ReachState::Unknown) {
        return format!(
            "Deployed{place}; Temps cannot tell whether {} is reachable: {}",
            address.address, address.reason
        );
    }
    match addresses.iter().min_by_key(|a| rank(a)) {
        Some(address) => format!(
            "Deployed{place}, but nobody can reach it yet: {}",
            address.reason
        ),
        None => format!("Deployed{place}, but it has no address yet."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(address: &str, source: AddressSource) -> AddressInput {
        AddressInput {
            address: address.to_string(),
            audience: Audience::Public,
            source,
            custom_domain_id: None,
            private: None,
        }
    }

    fn facts(address: &str, resolution: Resolution) -> AddressFacts {
        AddressFacts {
            input: input(address, AddressSource::CustomDomain),
            tunnel: None,
            resolution,
            temps_managed_dns: false,
            private: None,
        }
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test IP")
    }

    fn control_plane() -> DirectIngress {
        DirectIngress {
            name: "control-plane".into(),
            node_id: None,
            ips: vec![ip("203.0.113.10")],
            takes_port_80: true,
            check_failed: None,
        }
    }

    /// ADR 065 D6: an audience this version cannot serve is served to
    /// nobody, and the verdict says so instead of judging a public way in.
    #[test]
    fn an_audience_this_version_cannot_serve_is_not_reachable() {
        // Unknown to this version: served to nobody, and said so.
        let mut unserved = facts("app.example.com", Resolution::Ips(vec![ip("203.0.113.10")]));
        unserved.input.audience = Audience::parse("a_future_audience");
        let verdict = address_reachability(&unserved, &[control_plane()], true, "production");
        assert_eq!(verdict.state, ReachState::NotReachable);
        assert!(
            verdict.reason.contains("newer version"),
            "{}",
            verdict.reason
        );
        // Served since phase 4a: judged like any address, behind its gate.
        let mut gated = facts("app.example.com", Resolution::Ips(vec![ip("203.0.113.10")]));
        gated.input.audience = Audience::parse("signed_in");
        let verdict = address_reachability(&gated, &[control_plane()], true, "production");
        assert!(
            !verdict.reason.contains("newer version"),
            "{}",
            verdict.reason
        );
        assert!(Audience::SignedIn.is_served());
        assert_eq!(Audience::parse("tailnet_users"), Audience::TailnetUsers);
        assert!(Audience::TailnetUsers.is_served());
        assert_eq!(Audience::parse("tailnet-users"), Audience::Unsupported);
        assert_eq!(Audience::parse("private"), Audience::Private);
        assert!(!Audience::Unsupported.is_served());
    }

    #[test]
    fn a_localho_st_preview_is_never_reachable() {
        // Even when something resolved it.
        let verdict = address_reachability(
            &AddressFacts {
                input: input("app-production.localho.st", AddressSource::Generated),
                tunnel: None,
                resolution: Resolution::Ips(vec![ip("127.0.0.1")]),
                temps_managed_dns: false,
                private: None,
            },
            &[control_plane()],
            true,
            "production",
        );
        assert_eq!(verdict.state, ReachState::NotReachable);
        assert_eq!(
            verdict.action.map(|a| a.kind),
            Some(ActionKind::MakeReachable)
        );
        assert!(is_local_only("localhost"));
        assert!(is_local_only("x.localhost."));
        assert!(!is_local_only("localho.st.example.com"));
    }

    #[test]
    fn nothing_deployed_is_not_reachable_with_deploy_as_the_fix() {
        let verdict = address_reachability(
            &facts("app.example.com", Resolution::Skipped),
            &[],
            false,
            "production",
        );
        assert_eq!(verdict.state, ReachState::NotReachable);
        assert_eq!(verdict.reason, "Nothing is deployed to production yet.");
        assert_eq!(verdict.action.map(|a| a.kind), Some(ActionKind::Deploy));
    }

    #[test]
    fn the_tunnel_state_decides_a_tunneled_address() {
        let tunneled = |state: &str| AddressFacts {
            tunnel: Some(TunnelState {
                state: state.into(),
                connector_name: Some("homelab".into()),
                detail: format!("{state} detail"),
                has_candidates: true,
            }),
            ..facts("app.example.com", Resolution::Skipped)
        };
        let published = address_reachability(&tunneled("published"), &[], true, "production");
        assert_eq!(published.state, ReachState::Reachable);
        assert_eq!(published.way_in.as_deref(), Some("tunnel:homelab"));
        assert_eq!(
            address_reachability(&tunneled("pending"), &[], true, "production").state,
            ReachState::Checking
        );
        let error = address_reachability(&tunneled("error"), &[], true, "production");
        assert_eq!(error.state, ReachState::NotReachable);
        assert_eq!(error.reason, "error detail");
        assert!(!needs_lookup(
            "app.example.com",
            true,
            tunneled("pending").tunnel.as_ref()
        ));
        assert!(needs_lookup(
            "app.example.com",
            true,
            tunneled("not_published").tunnel.as_ref()
        ));
    }

    #[test]
    fn a_covering_tunnel_that_does_not_publish_explains_a_missing_record() {
        let verdict = address_reachability(
            &AddressFacts {
                tunnel: Some(TunnelState {
                    state: "not_published".into(),
                    connector_name: None,
                    detail:
                        "Tunnel 'homelab' covers it but only publishes hostnames chosen for it."
                            .into(),
                    has_candidates: true,
                }),
                ..facts("app.example.com", Resolution::NoRecord)
            },
            &[],
            true,
            "production",
        );
        assert_eq!(verdict.state, ReachState::NotReachable);
        assert!(verdict.reason.starts_with("Tunnel 'homelab' covers it"));
    }

    #[test]
    fn a_single_box_install_is_reachable_through_its_own_proxy() {
        let verdict = address_reachability(
            &facts("app.example.com", Resolution::Ips(vec![ip("203.0.113.10")])),
            &[control_plane()],
            true,
            "production",
        );
        assert_eq!(verdict.state, ReachState::Reachable);
        assert_eq!(verdict.way_in.as_deref(), Some("direct:control-plane"));
    }

    #[test]
    fn a_worker_that_failed_its_check_is_not_reachable() {
        let worker = DirectIngress {
            name: "edge-1".into(),
            node_id: Some(4),
            ips: vec![ip("198.51.100.7")],
            takes_port_80: true,
            check_failed: Some("port 443 timed out".into()),
        };
        let verdict = address_reachability(
            &facts("app.example.com", Resolution::Ips(vec![ip("198.51.100.7")])),
            &[worker],
            true,
            "production",
        );
        assert_eq!(verdict.state, ReachState::NotReachable);
        assert_eq!(verdict.way_in.as_deref(), Some("direct:edge-1"));
        assert!(verdict.reason.contains("port 443 timed out"));
    }

    #[test]
    fn dns_answers_without_a_known_ingress() {
        let private = address_reachability(
            &facts("app.example.com", Resolution::Ips(vec![ip("192.168.1.20")])),
            &[control_plane()],
            true,
            "production",
        );
        assert_eq!(private.state, ReachState::NotReachable);
        assert!(private.reason.contains("private address"));

        let elsewhere = address_reachability(
            &facts(
                "app.example.com",
                Resolution::Ips(vec![ip("93.184.215.14")]),
            ),
            &[control_plane()],
            true,
            "production",
        );
        assert_eq!(elsewhere.state, ReachState::Unknown);

        let managed = address_reachability(
            &AddressFacts {
                temps_managed_dns: true,
                private: None,
                ..facts("app.example.com", Resolution::Ips(vec![ip("104.16.1.1")]))
            },
            &[control_plane()],
            true,
            "production",
        );
        assert_eq!(managed.state, ReachState::Reachable);

        let missing = address_reachability(
            &facts("app.example.com", Resolution::NoRecord),
            &[control_plane()],
            true,
            "production",
        );
        assert_eq!(missing.state, ReachState::NotReachable);
        let action = missing.action.expect("an action");
        assert_eq!(action.kind, ActionKind::SetDnsRecord);
        assert!(action.label.contains("203.0.113.10"));
    }

    #[test]
    fn the_summary_names_the_best_address_or_the_reason() {
        let reachable = |address: &str, source| AddressReachability {
            address: address.into(),
            audience: Audience::Public,
            source,
            custom_domain_id: None,
            way_in: None,
            state: ReachState::Reachable,
            reason: String::new(),
            action: None,
            network: None,
            nothing_set_up: false,
            findings: Vec::new(),
        };
        let both = vec![
            reachable("app-production.example.com", AddressSource::Generated),
            reachable("myapp.example.com", AddressSource::CustomDomain),
        ];
        assert_eq!(overall_state(&both), ReachState::Reachable);
        assert_eq!(
            summary(&both, true, "production", &[]),
            "Public at myapp.example.com"
        );

        // The same address D4 picks: an environment domain ahead of a tunnel
        // host, whichever was gathered first.
        let tunnel_first = vec![
            reachable(
                "app-production.tunnel.example.com",
                AddressSource::TunnelHost,
            ),
            reachable("app.example.com", AddressSource::EnvironmentDomain),
        ];
        assert_eq!(
            summary(&tunnel_first, true, "production", &[]),
            "Public at app.example.com"
        );
        assert_eq!(
            super::super::addresses::primary_address(&tunnel_first).map(|p| p.address),
            Some("app.example.com".to_string())
        );

        let local = vec![AddressReachability {
            state: ReachState::NotReachable,
            reason: "x only resolves to the computer it is opened on.".into(),
            ..reachable("x.localho.st", AddressSource::Generated)
        }];
        assert_eq!(overall_state(&local), ReachState::NotReachable);
        assert_eq!(
            summary(&local, true, "production", &["master".to_string()]),
            "Deployed on master, but nobody can reach it yet: x only resolves to the computer it is opened on."
        );
        assert_eq!(
            summary(&[], true, "production", &[]),
            "Deployed, but it has no address yet."
        );
    }

    fn private_facts(
        resolution: Resolution,
        tunnel: Option<TunnelState>,
        bound: Result<(), String>,
        inside: Option<InsideCheck>,
    ) -> AddressFacts {
        AddressFacts {
            input: AddressInput {
                audience: Audience::Private,
                private: Some(PrivateBinding {
                    address_id: "custom:4".into(),
                    range: Some("100.64.0.0/10".into()),
                    node_id: Some(2),
                    listener: Some("100.101.102.103".into()),
                    via: None,
                    service: None,
                }),
                ..input("internal.example.com", AddressSource::CustomDomain)
            },
            tunnel,
            resolution,
            temps_managed_dns: true,
            private: Some(PrivateFacts {
                tailnet_users: None,
                worker: "homelab".into(),
                bound,
                tailscale: true,
                inside,
                tailnet: None,
            }),
        }
    }

    fn inside(outcome: PrivateCheckOutcome, minutes_ago: i64) -> Option<InsideCheck> {
        Some(InsideCheck {
            outcome,
            message: "It resolves to 10.0.0.9; nothing was dialled.".into(),
            minutes_ago,
        })
    }

    #[test]
    fn the_outside_check_wants_only_private_addresses_and_no_tunnel() {
        let listener = ip("100.101.102.103");
        assert_eq!(
            outside_check(
                "internal.example.com",
                listener,
                &Resolution::Ips(vec![listener]),
                None
            ),
            Ok(())
        );
        // A public address next to the private one gives the name away to
        // whatever answers there.
        let (state, reason, _) = outside_check(
            "internal.example.com",
            listener,
            &Resolution::Ips(vec![listener, ip("203.0.113.7")]),
            None,
        )
        .expect_err("public address");
        assert_eq!(state, ReachState::NotReachable);
        assert!(reason.contains("203.0.113.7"), "{reason}");
        let (_, reason, _) = outside_check(
            "internal.example.com",
            listener,
            &Resolution::Ips(vec![ip("10.0.0.9")]),
            None,
        )
        .expect_err("elsewhere");
        assert!(reason.contains("not to the private listener"), "{reason}");
        let tunnel = TunnelState {
            state: "published".into(),
            connector_name: Some("homelab".into()),
            detail: String::new(),
            has_candidates: true,
        };
        let (_, reason, action) = outside_check(
            "internal.example.com",
            listener,
            &Resolution::Ips(vec![listener]),
            Some(&tunnel),
        )
        .expect_err("tunnel");
        assert!(reason.contains("tunnel homelab publishes it"), "{reason}");
        assert_eq!(action.expect("action").kind, ActionKind::OpenIngress);
        let (_, reason, _) = outside_check(
            "internal.example.com",
            listener,
            &Resolution::NoRecord,
            None,
        )
        .expect_err("no record");
        assert!(reason.contains("100.101.102.103"), "{reason}");
    }

    #[test]
    fn a_private_address_is_judged_by_both_checks() {
        let ok = Resolution::Ips(vec![ip("100.101.102.103")]);
        let judged = |facts: AddressFacts| address_reachability(&facts, &[], true, "production");

        let reachable = judged(private_facts(
            ok.clone(),
            None,
            Ok(()),
            inside(PrivateCheckOutcome::Reachable, 1),
        ));
        assert_eq!(reachable.state, ReachState::Reachable);
        assert_eq!(reachable.way_in.as_deref(), Some("private:homelab"));
        assert_eq!(reachable.network.as_deref(), Some("Tailscale"));
        assert_eq!(
            summary(&[reachable], true, "production", &[]),
            "Private at internal.example.com (Tailscale)"
        );

        let waiting = judged(private_facts(ok.clone(), None, Ok(()), None));
        assert_eq!(waiting.state, ReachState::Checking);

        let stale = judged(private_facts(
            ok.clone(),
            None,
            Ok(()),
            inside(PrivateCheckOutcome::Reachable, 90),
        ));
        assert_eq!(stale.state, ReachState::Unknown);

        let failed = judged(private_facts(
            ok.clone(),
            None,
            Ok(()),
            inside(PrivateCheckOutcome::ResolvesElsewhere, 1),
        ));
        assert_eq!(failed.state, ReachState::NotReachable);
        assert!(
            failed.reason.contains("nothing was dialled"),
            "{}",
            failed.reason
        );

        let unbound = judged(private_facts(
            ok.clone(),
            None,
            Err("tailscale0 has no address".into()),
            inside(PrivateCheckOutcome::Reachable, 1),
        ));
        assert_eq!(unbound.state, ReachState::NotReachable);
        assert_eq!(
            unbound.action.and_then(|a| a.command).as_deref(),
            Some(PRIVATE_COMMAND)
        );

        // A good inside check never outweighs a failed outside one.
        let public = judged(private_facts(
            Resolution::Ips(vec![ip("203.0.113.7")]),
            None,
            Ok(()),
            inside(PrivateCheckOutcome::Reachable, 1),
        ));
        assert_eq!(public.state, ReachState::NotReachable);

        // No worker recorded: never "reachable" through the public paths.
        let mut nothing = private_facts(ok, None, Ok(()), None);
        nothing.private = None;
        let nothing = address_reachability(&nothing, &[control_plane()], true, "production");
        assert_eq!(nothing.state, ReachState::NotReachable);
        assert_eq!(nothing.way_in, None);
    }

    fn tailnet_facts(resolution: Resolution, report: Option<&str>) -> AddressFacts {
        use temps_core::tailnet::{ServeEntryReport, ServeOutcome, TailnetCheck};
        AddressFacts {
            input: AddressInput {
                audience: Audience::Private,
                private: Some(PrivateBinding {
                    address_id: "custom:4".into(),
                    range: None,
                    node_id: Some(2),
                    listener: None,
                    via: Some("tailscale_serve".into()),
                    service: None,
                }),
                ..input("homelab.example.ts.net", AddressSource::CustomDomain)
            },
            tunnel: None,
            resolution,
            temps_managed_dns: false,
            private: Some(PrivateFacts {
                tailnet_users: None,
                worker: "homelab".into(),
                bound: Ok(()),
                tailscale: true,
                inside: None,
                tailnet: Some(TailnetFacts {
                    report: TailscaleReport {
                        consent: true,
                        root: true,
                        state: TailscaleState::Ready,
                        version: Some("1.94.1".into()),
                        backend_state: Some("Running".into()),
                        dns_name: Some("homelab.example.ts.net".into()),
                        tailnet: Some("example.ts.net".into()),
                        magic_dns: true,
                        cert_domains: vec!["homelab.example.ts.net".into()],
                        entries: vec![ServeEntryReport {
                            entry_id: 9,
                            address_id: "custom:4".into(),
                            outcome: ServeOutcome::Applied,
                            etag: Some("\"e1\"".into()),
                            detail: None,
                            funnel: false,
                        }],
                        checks: vec![TailnetCheck {
                            address_id: "custom:4".into(),
                            hostname: "homelab.example.ts.net".into(),
                            outcome: TailnetCheckOutcome::Reachable,
                            message: "Reachable.".into(),
                        }],
                        ..TailscaleReport::default()
                    },
                    minutes_ago: 1,
                    entry: Some(ServeEntryState {
                        entry_id: 9,
                        hostname: "homelab.example.ts.net".into(),
                        removing: false,
                        report: report.map(str::to_string),
                        detail: Some("http://127.0.0.1:3000".into()),
                    }),
                    service: None,
                }),
            }),
        }
    }

    fn tailnet_of(facts: &mut AddressFacts) -> &mut TailnetFacts {
        facts
            .private
            .as_mut()
            .and_then(|p| p.tailnet.as_mut())
            .expect("tailnet facts")
    }

    /// ADR 065 D5: a tailnet name is reachable only when outside DNS says
    /// nothing else answers, the agent applied the entry and its inside
    /// check passed.
    #[test]
    fn a_tailnet_name_is_judged_by_outside_dns_its_entry_and_the_inside_check() {
        let judged = |facts: AddressFacts| {
            address_reachability(&facts, &[control_plane()], true, "production")
        };
        let ok = judged(tailnet_facts(Resolution::NoRecord, Some("applied")));
        assert_eq!(ok.state, ReachState::Reachable, "{}", ok.reason);
        assert_eq!(ok.network.as_deref(), Some("Tailscale"));
        assert_eq!(ok.way_in.as_deref(), Some("private:homelab"));
        // The control plane on the tailnet sees MagicDNS answers: fine.
        assert_eq!(
            judged(tailnet_facts(
                Resolution::Ips(vec![ip("100.101.102.103")]),
                Some("applied")
            ))
            .state,
            ReachState::Reachable
        );
        // Anything else answering for it outside fails.
        let public = judged(tailnet_facts(
            Resolution::Ips(vec![ip("203.0.113.7")]),
            Some("applied"),
        ));
        assert_eq!(public.state, ReachState::NotReachable);
        assert!(public.reason.contains("203.0.113.7"), "{}", public.reason);
        // A tunnel publishing it fails, whatever the worker says.
        let mut tunneled = tailnet_facts(Resolution::Skipped, Some("applied"));
        tunneled.tunnel = Some(TunnelState {
            state: "published".into(),
            connector_name: Some("edge".into()),
            detail: String::new(),
            has_candidates: true,
        });
        assert!(judged(tunneled).reason.contains("tunnel edge publishes it"));

        // Funnel, from the entry report or the inside check: red, D5's words.
        let mut funnel = tailnet_facts(Resolution::NoRecord, Some("applied"));
        tailnet_of(&mut funnel).report.entries[0].funnel = true;
        let funnel = judged(funnel);
        assert_eq!(funnel.state, ReachState::NotReachable);
        assert_eq!(
            funnel.reason,
            "This private address is also on Tailscale Funnel, which makes it public. Run \
             `tailscale funnel --https=443 off` on `homelab`."
        );

        // The agent's report on the entry.
        let conflict = judged(tailnet_facts(Resolution::NoRecord, Some("conflict")));
        assert_eq!(conflict.state, ReachState::NotReachable);
        assert!(conflict
            .reason
            .starts_with("`homelab.example.ts.net` already serves `http://127.0.0.1:3000`"));
        assert_eq!(
            judged(tailnet_facts(Resolution::NoRecord, None)).state,
            ReachState::Checking
        );
        let mut removing = tailnet_facts(Resolution::NoRecord, Some("applied"));
        if let Some(entry) = tailnet_of(&mut removing).entry.as_mut() {
            entry.removing = true;
        }
        assert_eq!(judged(removing).state, ReachState::NotReachable);
        let mut no_entry = tailnet_facts(Resolution::NoRecord, Some("applied"));
        tailnet_of(&mut no_entry).entry = None;
        assert_eq!(
            judged(no_entry).action.map(|a| a.kind),
            Some(ActionKind::MakeReachable)
        );

        // The worker's tailscaled, with D5's message.
        let mut logged_out = tailnet_facts(Resolution::NoRecord, Some("applied"));
        tailnet_of(&mut logged_out).report.backend_state = Some("NeedsLogin".into());
        assert_eq!(
            judged(logged_out).reason,
            "Tailscale on `homelab` is `NeedsLogin`. Run `sudo tailscale up`."
        );
        let mut stale = tailnet_facts(Resolution::NoRecord, Some("applied"));
        tailnet_of(&mut stale).minutes_ago = 40;
        assert_eq!(judged(stale).state, ReachState::Unknown);

        // The inside check.
        let mut failed = tailnet_facts(Resolution::NoRecord, Some("applied"));
        tailnet_of(&mut failed).report.checks[0].outcome = TailnetCheckOutcome::NoRoute;
        let failed = judged(failed);
        assert_eq!(failed.state, ReachState::NotReachable);
        assert_eq!(failed.action.map(|a| a.kind), Some(ActionKind::Deploy));
        let mut unchecked = tailnet_facts(Resolution::NoRecord, Some("applied"));
        tailnet_of(&mut unchecked).report.checks.clear();
        assert_eq!(judged(unchecked).state, ReachState::Checking);
        // Another address's check is not this one's.
        let mut other = tailnet_facts(Resolution::NoRecord, Some("applied"));
        tailnet_of(&mut other).report.checks[0].address_id = "custom:5".into();
        assert_eq!(judged(other).state, ReachState::Checking);

        // No worker recorded: never reachable.
        let mut nothing = tailnet_facts(Resolution::NoRecord, Some("applied"));
        nothing.private = None;
        assert_eq!(judged(nothing).state, ReachState::NotReachable);
    }

    /// ADR 065 D5: for a Service, every host advertising it matches one
    /// of its Serve entries, and the worker is approved; a foreign host
    /// is named and fails, whatever the inside check says.
    #[test]
    fn a_service_is_judged_by_its_hosts_and_approval() {
        let judged = |facts: AddressFacts| {
            address_reachability(&facts, &[control_plane()], true, "production")
        };
        let service = |unrecorded: Result<Vec<String>, String>, approved: Option<bool>| {
            let mut facts = tailnet_facts(Resolution::NoRecord, Some("applied"));
            if let Some(binding) = facts.input.private.as_mut() {
                binding.via = Some("tailscale_service".into());
                binding.service = Some("svc:myapp".into());
            }
            tailnet_of(&mut facts).service = Some(ServiceHostsState {
                name: "svc:myapp".into(),
                tag: "tag:temps".into(),
                unrecorded,
                approved,
            });
            facts
        };

        assert_eq!(
            judged(service(Ok(vec![]), Some(true))).state,
            ReachState::Reachable
        );

        let foreign = judged(service(Ok(vec!["nas".into()]), Some(true)));
        assert_eq!(foreign.state, ReachState::NotReachable);
        assert!(
            foreign.reason.contains("advertised by `nas`"),
            "{}",
            foreign.reason
        );

        let unapproved = judged(service(Ok(vec![]), Some(false)));
        assert_eq!(unapproved.state, ReachState::NotReachable);
        assert_eq!(
            unapproved.reason,
            tailnet_messages::not_approved("svc:myapp", "homelab", "tag:temps")
        );

        assert_eq!(
            judged(service(Ok(vec![]), None)).state,
            ReachState::Checking
        );

        let unreadable = judged(service(Err("401 Unauthorized".into()), Some(true)));
        assert_eq!(unreadable.state, ReachState::Unknown);
        assert!(unreadable.reason.contains("401 Unauthorized"));
    }
}
