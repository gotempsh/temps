// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tailscale's part of planning (ADR-116 D3, ADR 065 D1, D6): it plans
//! private and tailnet-users addresses whole. A name under a tailnet is a
//! worker's machine name, served with Tailscale Serve, or a Service; any
//! other name falls to the worker's own private listener, which the engine
//! plans.

use temps_core::tailnet::messages as tailnet_messages;

use super::{self as tailscale, Ownership, TailscaleFactory};
use crate::reachability::compute::tailnet_worker_problem;
use crate::reachability::model::{
    ActionKind, AddressAudience, Audience, PlanBlocked, PlanOption, PlanOptionKind, ReachAction,
    ReachabilityPlan, StepKind,
};
use crate::reachability::plan::*;

impl ProviderPlan for TailscaleFactory {
    fn plans(&self, audience: Audience) -> bool {
        matches!(audience, Audience::Private | Audience::TailnetUsers)
    }

    fn address(&self, facts: &PlanFacts) -> Result<Option<String>, String> {
        planned_private_address(facts)
    }

    fn plan(&self, facts: &PlanFacts) -> ReachabilityPlan {
        match facts.audience {
            Audience::TailnetUsers => plan_tailnet_users(facts),
            _ => plan_private(facts),
        }
    }

    /// A `tailnet_users` audience is judged before anyone is listed: on
    /// whether any way in can tell who a visitor is.
    fn choice_plan(&self, facts: &PlanFacts) -> ReachabilityPlan {
        if facts.audience == Audience::TailnetUsers && facts.principals.is_empty() {
            tailnet_users_ways(facts)
        } else {
            self.plan(facts)
        }
    }
}

/// What a private address is as a tailnet name (ADR 065 D1).
enum TailnetName<'a> {
    /// A worker's own name.
    Machine(&'a TailnetWorkerFact),
    /// Under a tailnet, and no machine's name: only a Service gives it.
    UnderTailnet,
    Other,
}

fn tailnet_name<'a>(facts: &'a PlanFacts, host: &str) -> TailnetName<'a> {
    let host = temps_core::tailnet::normalize_name(host);
    if let Some(worker) = facts
        .tailnet_workers
        .iter()
        .find(|w| w.report.dns_name.as_deref() == Some(host.as_str()))
    {
        return TailnetName::Machine(worker);
    }
    let under = |suffix: &str| host.ends_with(&format!(".{suffix}"));
    if under("ts.net")
        || facts
            .tailnet_workers
            .iter()
            .filter_map(|w| w.report.tailnet.as_deref())
            .any(under)
    {
        TailnetName::UnderTailnet
    } else {
        TailnetName::Other
    }
}

/// Workers whose owner allowed Tailscale Serve, the one the environment
/// runs on first.
fn serve_candidates(facts: &PlanFacts) -> Vec<&TailnetWorkerFact> {
    let mut workers: Vec<&TailnetWorkerFact> = facts
        .tailnet_workers
        .iter()
        .filter(|w| w.report.consent)
        .collect();
    workers.sort_by_key(|w| Some(&w.name) != facts.suggested_private_worker.as_ref());
    workers
}

/// Why `worker` cannot serve its machine name for this app now, with what
/// fixes it (ADR 065 D5's messages), in the order the agent decides them.
pub fn machine_name_problem(
    facts: &PlanFacts,
    worker: &TailnetWorkerFact,
) -> Option<(String, Option<ReachAction>)> {
    let node = worker.name.as_str();
    let report = &worker.report;
    if let Some(problem) = tailnet_worker_problem(node, report, worker.minutes_ago) {
        return Some(problem);
    }
    let name = report.dns_name.clone().unwrap_or_default();
    let problem = |message: String| Some((message, None));
    if let Some(listener) = worker.listener_on_tailscale {
        return problem(format!(
            "{}.",
            tailnet_messages::listener_exclusive(node, &listener.to_string())
        ));
    }
    if let Some(held_by) = &report.port_443_held_by {
        return problem(tailnet_messages::slot_taken(&name, held_by));
    }
    if report.port_443_funnel {
        return problem(tailnet_messages::funnel(node));
    }
    if facts
        .serve_entries
        .iter()
        .any(|e| e.node_id == worker.id && e.hostname == name && e.removing)
    {
        return problem(format!(
            "An earlier setup of `{name}` is still being taken off {node}'s Serve config. Try \
             again in a minute."
        ));
    }
    None
}

/// Why `worker` cannot host a Tailscale Service now (ADR 065 D5): what
/// keeps tailscaled from serving at all, then a device with no tag.
pub fn service_host_problem(worker: &TailnetWorkerFact) -> Option<(String, Option<ReachAction>)> {
    let node = worker.name.as_str();
    let report = &worker.report;
    if let Some(problem) = tailnet_worker_problem(node, report, worker.minutes_ago) {
        return Some(problem);
    }
    if report.tags.is_empty() {
        let machine = report.dns_name.clone().unwrap_or_default();
        return Some((
            tailnet_messages::not_tagged(
                node,
                report.login.as_deref().unwrap_or("a user"),
                &machine,
            ),
            Some(
                ReachAction::new(ActionKind::RunCommand, format!("On {node}, as root, run"))
                    .with_command(format!(
                        "sudo tailscale up --advertise-tags={}",
                        tailscale::DEFAULT_TAG
                    )),
            ),
        ));
    }
    None
}

/// The tag a Service on `worker` is for: `tag:temps` when the worker has
/// it, else its first (ADR 065 D4).
pub fn service_tag(worker: &TailnetWorkerFact) -> String {
    let tags = &worker.report.tags;
    tags.iter()
        .find(|t| t.as_str() == tailscale::DEFAULT_TAG)
        .or_else(|| tags.first())
        .cloned()
        .unwrap_or_else(|| tailscale::DEFAULT_TAG.to_string())
}

/// The Service `host` names, with its tailnet: one valid label directly
/// under the MagicDNS suffix of a worker that reports one, and not a
/// machine's name. `myapp.example.ts.net` → `(svc:myapp, example.ts.net)`.
pub fn service_of(facts: &PlanFacts, host: &str) -> Option<(String, String)> {
    let host = temps_core::tailnet::normalize_name(host);
    if facts
        .tailnet_workers
        .iter()
        .any(|w| w.report.dns_name.as_deref() == Some(host.as_str()))
    {
        return None;
    }
    facts
        .tailnet_workers
        .iter()
        .filter_map(|w| w.report.tailnet.as_deref())
        .find_map(|tailnet| {
            let label = host.strip_suffix(&format!(".{tailnet}"))?;
            temps_core::tailnet::valid_service_label(label)
                .then(|| (tailscale::service_name(label), tailnet.to_string()))
        })
}

/// The worker that hosts a Service on `tailnet`: one whose owner allowed
/// Serve and that can host one now, the environment's first.
pub fn service_worker<'a>(facts: &'a PlanFacts, tailnet: &str) -> Option<&'a TailnetWorkerFact> {
    serve_candidates(facts)
        .into_iter()
        .filter(|w| w.report.tailnet.as_deref() == Some(tailnet))
        .find(|w| service_host_problem(w).is_none())
}

/// The cluster's credential, when a run can use it.
fn usable_credential(facts: &PlanFacts) -> Option<&TailscaleCredentialFact> {
    facts
        .tailscale_credential
        .as_ref()
        .filter(|c| c.refusal.is_none())
}

/// A Service address for this app on the first tailnet with a worker that
/// can host one: `<project>-<environment>.<tailnet>` (ADR 065 D1).
fn suggested_service_address(facts: &PlanFacts) -> Option<String> {
    let label = suggested_label(facts);
    if !temps_core::tailnet::valid_service_label(&label) {
        return None;
    }
    serve_candidates(facts)
        .into_iter()
        .filter(|w| service_host_problem(w).is_none())
        .filter_map(|w| w.report.tailnet.as_deref())
        .map(|tailnet| format!("{label}.{tailnet}"))
        .find(|c| !facts.taken_elsewhere.contains(c))
}

/// The address a private plan is for: as asked, else in ADR 065 D1's
/// order a Service when the cluster has a usable Tailscale credential, the
/// name of a worker that can serve it through Tailscale Serve, one under a
/// zone Temps manages (which a listener needs for its record and its DNS-01
/// certificate), and last a Service after pasting a credential.
pub fn planned_private_address(facts: &PlanFacts) -> Result<Option<String>, String> {
    if facts
        .requested
        .as_deref()
        .is_some_and(|r| !r.trim().is_empty())
    {
        return planned_address(facts);
    }
    let free = |c: &String| !facts.taken_elsewhere.contains(c);
    if usable_credential(facts).is_some() {
        if let Some(address) = suggested_service_address(facts) {
            return Ok(Some(address));
        }
    }
    if let Some(name) = serve_candidates(facts)
        .into_iter()
        .filter(|w| machine_name_problem(facts, w).is_none())
        .filter_map(|w| w.report.dns_name.clone())
        .find(free)
    {
        return Ok(Some(name));
    }
    let label = suggested_label(facts);
    if let Some(address) = facts
        .zones
        .iter()
        .filter(|z| z.verified)
        .map(|z| format!("{label}.{}", z.domain.trim_end_matches('.')))
        .find(|c| free(c) && !facts.attached_here.contains(c))
    {
        return Ok(Some(address));
    }
    Ok(suggested_service_address(facts))
}

/// The plan for a private address: a worker's own tailnet name through
/// Tailscale Serve (ADR 065 D1), or its private listener (ADR 064 D5).
fn plan_private(facts: &PlanFacts) -> ReachabilityPlan {
    let ask_address = facts
        .requested
        .as_deref()
        .is_none_or(|r| r.trim().is_empty());
    let address = match planned_private_address(facts) {
        Ok(address) => address,
        Err(error) => {
            return private_blocked(
                facts.requested.clone(),
                Some(error.clone()),
                PlanBlocked {
                    reason: error,
                    action: None,
                },
            )
        }
    };
    match address.as_deref().map(|a| tailnet_name(facts, a)) {
        Some(TailnetName::Machine(worker)) => plan_machine_name(facts, worker, address.clone()),
        Some(TailnetName::UnderTailnet)
            if address
                .as_deref()
                .is_some_and(|a| service_of(facts, a).is_some()) =>
        {
            plan_service(facts, address.clone())
        }
        Some(TailnetName::UnderTailnet) => {
            let reason = tailnet_messages::service_only(address.as_deref().unwrap_or_default());
            private_blocked(
                address,
                None,
                PlanBlocked {
                    reason,
                    action: None,
                },
            )
        }
        Some(TailnetName::Other) | None => {
            let listener = plan_listener(facts, address, ask_address);
            // Nothing asked for and no listener fits: a worker that allowed
            // Serve says what keeps its name from working instead.
            if listener.options.is_empty() && ask_address {
                if let Some((reason, action)) = serve_candidates(facts)
                    .first()
                    .and_then(|w| machine_name_problem(facts, w))
                {
                    return private_blocked(listener.address, None, PlanBlocked { reason, action });
                }
            }
            listener
        }
    }
}

/// The plan for `tailnet_users` (ADR 065 D6): a private plan, kept to the
/// ways in that can tell who a visitor is, on agents that check it.
fn plan_tailnet_users(facts: &PlanFacts) -> ReachabilityPlan {
    if facts.principals.is_empty() {
        return tailnet_users_blocked(
            facts,
            facts.requested.clone(),
            tailnet_messages::NO_PRINCIPALS.to_string(),
            None,
        );
    }
    tailnet_users_ways(facts)
}

fn tailnet_users_blocked(
    facts: &PlanFacts,
    address: Option<String>,
    reason: String,
    action: Option<ReachAction>,
) -> ReachabilityPlan {
    let mut plan = private_blocked(address, None, PlanBlocked { reason, action });
    plan.audience = Audience::TailnetUsers;
    plan.principals = facts.principals.clone();
    plan
}

/// The ways in a `tailnet_users` plan keeps, whoever is listed: those that
/// can tell who a visitor is, on agents that check it. [`choice`] calls it
/// before anyone is listed; a plan to apply goes through
/// [`plan_tailnet_users`], which refuses an empty list.
fn tailnet_users_ways(facts: &PlanFacts) -> ReachabilityPlan {
    let blocked = |address: Option<String>, reason: String, action: Option<ReachAction>| {
        tailnet_users_blocked(facts, address, reason, action)
    };
    let mut planned = plan_private(facts);
    planned.audience = Audience::TailnetUsers;
    planned.principals = facts.principals.clone();
    let who = AddressAudience {
        audience: Audience::TailnetUsers,
        principals: facts.principals.clone(),
        ..AddressAudience::default()
    }
    .describe();
    let listed = facts
        .principals
        .iter()
        .map(|p| p.value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let shown = planned
        .address
        .clone()
        .unwrap_or_else(|| "the address".to_string());
    let mut refusals: Vec<(String, Option<ReachAction>)> = Vec::new();
    planned.options.retain_mut(|option| {
        let node = option
            .way_in
            .strip_prefix("private:")
            .unwrap_or_default()
            .to_string();
        if option.kind == PlanOptionKind::PrivateWorker {
            let Some((worker, ip)) = private_worker(facts) else {
                return false;
            };
            // ADR-114 D7: the control plane does not check who a tailnet
            // visitor is yet.
            if worker.id == temps_core::CONTROL_PLANE_NODE_ID {
                refusals.push((
                    "This machine serves private addresses to everyone on your tailnet, but \
                     cannot yet limit one to chosen people. Use “Only my private network”, or \
                     deploy the app to a worker for that."
                        .to_string(),
                    None,
                ));
                return false;
            }
            // D6: whois answers only for Tailscale peers, and asking it
            // needs the worker owner's consent.
            if !worker.tailscale {
                refusals.push((tailnet_messages::users_need_tailnet(&ip.to_string()), None));
                return false;
            }
            let consent = facts
                .tailnet_workers
                .iter()
                .any(|w| w.id == worker.id && w.report.consent);
            if !consent {
                refusals.push((
                    tailnet_messages::users_need_consent(&worker.name),
                    Some(
                        ReachAction::new(
                            ActionKind::RunCommand,
                            format!("On {}, as root, run", worker.name),
                        )
                        .with_command(temps_core::tailnet::TAILSCALE_SERVE_COMMAND),
                    ),
                ));
                return false;
            }
        }
        if !facts.identity_workers.contains(&node) {
            refusals.push((
                format!(
                    "{shown} admits only listed tailnet users, and the agent on `{node}` cannot \
                     check who they are. Update the agent on `{node}`, then plan again."
                ),
                None,
            ));
            return false;
        }
        for step in &mut option.steps {
            if step.kind == StepKind::SetAudience {
                step.label = format!("Make {shown} private to your tailnet, {who}");
            }
        }
        option.warnings.insert(
            0,
            format!(
                "Only {listed} can open {shown}. Anyone else on your tailnet gets a page that \
                 says it admits only listed tailnet users. Changing who is listed needs a \
                 recent password or passkey confirmation."
            ),
        );
        true
    });
    for (index, option) in planned.options.iter_mut().enumerate() {
        option.recommended = index == 0;
    }
    if planned.options.is_empty() && planned.blocked.is_none() {
        let (reason, action) = refusals.into_iter().next().unwrap_or_else(|| {
            (
                "No way in that can tell who a tailnet visitor is is available.".to_string(),
                None,
            )
        });
        return blocked(planned.address.clone(), reason, action);
    }
    planned
}

/// The plan for a worker's own tailnet name (ADR 065 D1 option 3):
/// tailscaled serves it and gets its certificate, so nothing is written to
/// DNS or `domains`.
fn plan_machine_name(
    facts: &PlanFacts,
    worker: &TailnetWorkerFact,
    address: Option<String>,
) -> ReachabilityPlan {
    let blocked = |reason: String, action: Option<ReachAction>| {
        private_blocked(address.clone(), None, PlanBlocked { reason, action })
    };
    if facts.deployed && !facts.served_by_workers {
        return blocked(
            format!(
                "{} runs on the control plane, and a tailnet name is served by a worker, which \
                 only carries what workers serve. Deploy it to a worker first.",
                facts.environment_name
            ),
            None,
        );
    }
    if let Some((reason, action)) = machine_name_problem(facts, worker) {
        return blocked(reason, action);
    }
    let node = worker.name.as_str();
    let name = address.clone().unwrap_or_default();
    let attached = facts.attached_here.contains(&name);
    let mut warnings = vec![
        tailnet_messages::machine_name_cost(&name, node),
        format!(
            "Tailscale's certificate for `{name}` is in public certificate logs, so the name is \
             no longer secret. Only devices on your tailnet can connect to it."
        ),
    ];
    if attached
        && facts
            .audiences
            .iter()
            .any(|(a, au)| a == &name && *au == Audience::Public)
    {
        warnings.push(format!(
            "{name} is public now. This makes it private: only devices on your tailnet can open \
             it, and anyone else who uses it loses access. It needs a recent password or \
             passkey confirmation."
        ));
    }
    if !facts.deployed {
        warnings.push(format!(
            "Nothing is deployed to {} yet: it answers once a deployment runs on a worker.",
            facts.environment_name
        ));
    }
    let option = PlanOption {
        kind: PlanOptionKind::TailscaleServe,
        title: format!("Only my tailnet, at {name}"),
        description: format!(
            "{node} serves {name}, its own name in your tailnet, with Tailscale Serve: only \
             devices on your tailnet can open it, Tailscale gives it its certificate, and no DNS \
             record is added anywhere."
        ),
        actions: 1,
        recommended: true,
        way_in: format!("private:{node}"),
        steps: vec![
            step(
                StepKind::AttachDomain,
                if attached {
                    format!("{name} is already on {}", facts.environment_name)
                } else {
                    format!("Attach {name} to {}", facts.environment_name)
                },
            ),
            step(
                StepKind::SetAudience,
                format!("Make {name} private to your tailnet on {node}"),
            ),
            step(
                StepKind::ConfigureTailscaleServe,
                format!("Have {node} serve {name} with Tailscale Serve"),
            ),
            step(
                StepKind::CheckReachability,
                format!("Check {name} from {node} and from outside"),
            ),
        ],
        fields: Vec::new(),
        warnings,
        confirmation: None,
        required_permissions: permissions_for(PlanOptionKind::TailscaleServe)
            .iter()
            .map(ToString::to_string)
            .collect(),
    };
    ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: Audience::Private,
        address,
        address_error: None,
        options: vec![option],
        blocked: None,
        gate: None,
        principals: Vec::new(),
    }
}

/// The plan for the app's own name in the tailnet, a Tailscale Service
/// (ADR 065 D1 options 1, 2 and 4): tailscaled on a tagged worker serves it
/// and gets its certificate, the Service is created through the API only
/// when absent, and the tailnet admin approves the host.
fn plan_service(facts: &PlanFacts, address: Option<String>) -> ReachabilityPlan {
    let blocked = |reason: String, action: Option<ReachAction>| {
        private_blocked(address.clone(), None, PlanBlocked { reason, action })
    };
    let name = address.clone().unwrap_or_default();
    let Some((service, tailnet)) = service_of(facts, &name) else {
        return blocked(tailnet_messages::service_only(&name), None);
    };
    if facts.deployed && !facts.served_by_workers {
        return blocked(
            format!(
                "{} runs on the control plane, and a tailnet name is served by a worker, which \
                 only carries what workers serve. Deploy it to a worker first.",
                facts.environment_name
            ),
            None,
        );
    }
    let Some(worker) = service_worker(facts, &tailnet) else {
        // Say what keeps the likeliest worker from hosting it.
        let on_tailnet: Vec<&TailnetWorkerFact> = serve_candidates(facts)
            .into_iter()
            .filter(|w| w.report.tailnet.as_deref() == Some(tailnet.as_str()))
            .collect();
        return match on_tailnet.first().and_then(|w| service_host_problem(w)) {
            Some((reason, action)) => blocked(reason, action),
            None => blocked(
                format!(
                    "No worker on tailnet {tailnet} lets Temps use its Tailscale. Run `{}` on \
                     the worker that should serve {name}.",
                    temps_core::tailnet::TAILSCALE_SERVE_COMMAND
                ),
                None,
            ),
        };
    };
    if facts
        .serve_entries
        .iter()
        .any(|e| e.node_id == worker.id && e.hostname == name && e.removing)
    {
        return blocked(
            format!(
                "An earlier setup of `{name}` is still being taken off {}'s Serve config. Try \
                 again in a minute.",
                worker.name
            ),
            None,
        );
    }
    let node = worker.name.as_str();
    let tag = service_tag(worker);
    let (kind, fields) = match &facts.tailscale_credential {
        Some(credential) => match &credential.refusal {
            Some(refusal) => return blocked(refusal.clone(), None),
            None => (PlanOptionKind::TailscaleService, Vec::new()),
        },
        None => (
            PlanOptionKind::TailscaleServiceWithCredential,
            TailscaleFactory::fields(),
        ),
    };
    // ADR 065 D4: create only when absent; this install's own is found.
    let found = match facts.service_seen.as_ref().filter(|(s, _)| *s == service) {
        Some((_, ServiceSeen::Unreadable(error))) => {
            return blocked(
                format!("Temps could not read `{service}` from your tailnet: {error}"),
                None,
            )
        }
        Some((_, ServiceSeen::Found(definition))) => {
            match tailscale::ownership(definition, &facts.install_id) {
                Ownership::Foreign => {
                    return blocked(tailnet_messages::service_taken(&service), None)
                }
                Ownership::Ours { .. } => {
                    if let Some(why) = definition.differs_from_fixed(&tag) {
                        return blocked(
                            format!(
                                "{why}. Temps never changes a Service, so it cannot use it as it \
                                 is: put it back, or pick another name."
                            ),
                            None,
                        );
                    }
                    true
                }
            }
        }
        Some((_, ServiceSeen::Absent)) | None => false,
    };
    let attached = facts.attached_here.contains(&name);
    let mut warnings = vec![
        format!(
            "The tailnet admin approves {node} as the host of `{service}`, at \
             login.tailscale.com/admin/services, or once for every setup with this policy entry: \
             {}. Temps waits for it and never approves hosts itself.",
            tailscale::auto_approvers_snippet(&service, &tag)
        ),
        format!(
            "Tailscale's certificate for `{name}` is in public certificate logs, so the name is \
             no longer secret. Only devices on your tailnet can connect to it."
        ),
    ];
    if kind == PlanOptionKind::TailscaleServiceWithCredential {
        warnings.push(
            "The OAuth client's `services` scope is tailnet-wide: whoever holds its secret can \
             create, change and delete every Service in your tailnet. Temps seals it, never shows \
             it again, and asks for `services` and `devices:core:read` only."
                .to_string(),
        );
    }
    if attached
        && facts
            .audiences
            .iter()
            .any(|(a, au)| a == &name && *au == Audience::Public)
    {
        warnings.push(format!(
            "{name} is public now. This makes it private: only devices on your tailnet can open \
             it, and anyone else who uses it loses access. It needs a recent password or \
             passkey confirmation."
        ));
    }
    if !facts.deployed {
        warnings.push(format!(
            "Nothing is deployed to {} yet: it answers once a deployment runs on a worker.",
            facts.environment_name
        ));
    }
    let mut steps = Vec::new();
    if kind == PlanOptionKind::TailscaleServiceWithCredential {
        steps.push(step(
            StepKind::SaveTailscaleCredential,
            "Check the Tailscale OAuth client's scopes and save it",
        ));
    }
    steps.extend([
        step(
            StepKind::AttachDomain,
            if attached {
                format!("{name} is already on {}", facts.environment_name)
            } else {
                format!("Attach {name} to {}", facts.environment_name)
            },
        ),
        step(
            StepKind::SetAudience,
            format!("Make {name} private to your tailnet on {node}"),
        ),
        step(
            StepKind::DefineTailscaleService,
            if found {
                format!("Use `{service}`, which this Temps install made earlier")
            } else {
                format!("Create `{service}` in your tailnet, for hosts tagged {tag}")
            },
        ),
        step(
            StepKind::ConfigureTailscaleServe,
            format!("Have {node} serve and advertise `{service}`"),
        ),
        step(
            StepKind::WaitForServiceApproval,
            format!("Wait for the tailnet admin to approve {node} as its host"),
        ),
        step(
            StepKind::CheckReachability,
            format!("Check {name} from {node} and from outside"),
        ),
    ]);
    let option = PlanOption {
        kind,
        title: format!("Only my tailnet, at {name}"),
        description: format!(
            "{node} serves {name}, this app's own name in your tailnet, as the Tailscale Service \
             `{service}`: only devices on your tailnet can open it, Tailscale gives it its \
             certificate, the name stays if the app moves to another worker, and no DNS record \
             is added anywhere."
        ),
        actions: if kind == PlanOptionKind::TailscaleServiceWithCredential {
            3
        } else {
            2
        },
        recommended: true,
        way_in: format!("private:{node}"),
        steps,
        fields,
        warnings,
        confirmation: None,
        required_permissions: permissions_for(kind)
            .iter()
            .map(ToString::to_string)
            .collect(),
    };
    ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: Audience::Private,
        address,
        address_error: None,
        options: vec![option],
        blocked: None,
        gate: None,
        principals: Vec::new(),
    }
}
