// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tailscale's part of a reachability run (ADR-116 D3, ADR 065): saving
//! a pasted OAuth client, defining the address's Service, declaring the
//! worker's Serve entry, and waiting for the admin to approve the worker as
//! the Service's host. The engine ([`crate::reachability::run`]) keeps the
//! journal and its rules; these steps reach it only through [`StepRun`].

use std::time::Duration;

use async_trait::async_trait;

use crate::reachability::model::{Intent, JournalState, RunResource, StepKind};
use crate::reachability::run::{
    tailscale_addrs, unrecorded_hosts, unrecorded_message, Begin, Found, Outcome, ProviderRun,
    ServeClaim, ServeEntryStatus, ServeRelease, ServeTarget, ServeWish, ServiceDeletion,
    ServiceEntryHost, ServicePlan, StepRun, UndoCheck, WriteError,
};
use crate::tailscale::{self, Ownership, ServiceDefinition, ServiceHost, TailscaleCredential};
use temps_core::tailnet::messages as tailnet_messages;
use temps_entities::tailnet_serve_entries::{REPORT_APPLIED, REPORT_CONFLICT, REPORT_ERROR};

/// What a run does with Tailscale and its Serve entries, behind a trait so
/// tests can stand in.
#[async_trait]
pub trait TailscaleOps: Send + Sync {
    /// The saved OAuth client named `name`, which an earlier attempt of the
    /// run may have saved.
    async fn find_credential(&self, name: &str) -> Result<Option<RunResource>, String>;
    /// Creates the `(node, address)` Serve entry with this run's reference,
    /// or adds a reference to the same one, in one transaction holding the
    /// entry's advisory lock (and the Service's first) (ADR 065 D4).
    async fn claim_serve_entry(&self, run_id: i32, wish: &ServeWish) -> Result<ServeClaim, String>;
    /// This run's reference to the `(node, address)` entry: `(entry id,
    /// created)`.
    async fn serve_reference(
        &self,
        run_id: i32,
        node_id: i32,
        address_id: &str,
    ) -> Result<Option<(i32, bool)>, String>;
    async fn serve_entry(
        &self,
        node_id: i32,
        address_id: &str,
    ) -> Result<Option<ServeEntryStatus>, String>;
    /// Drops this run's reference under the entry's lock; the last one
    /// decides what happens to the entry by the agent's last report.
    async fn release_serve_entry(
        &self,
        run_id: i32,
        node_id: i32,
        address_id: &str,
    ) -> Result<ServeRelease, String>;
    /// Deletes a `removing` entry the user decided to leave on the worker
    /// as it is, so nothing more is asked of the agent.
    async fn forget_serve_entry(&self, entry_id: i32) -> Result<(), String>;
    /// Proves a pasted Tailscale OAuth client's scopes and saves it, sealed,
    /// under `name` (ADR 065 D2); returns its row id.
    async fn save_tailscale_credential(
        &self,
        name: &str,
        credential: &TailscaleCredential,
    ) -> Result<i32, String>;
    /// Removes the credential row unless a Service setup still needs it:
    /// `Some(why)` when it is kept.
    async fn release_tailscale_credential(&self, id: i32) -> Result<Option<String>, String>;
    /// The Service `name` in the cluster's tailnet, as the API has it now.
    async fn read_service(&self, name: &str) -> Result<Option<ServiceDefinition>, String>;
    /// `PUT`, only ever to create (ADR 065 D4).
    async fn create_service(&self, definition: &ServiceDefinition) -> Result<(), WriteError>;
    /// The devices advertising `name`.
    async fn service_hosts(&self, name: &str) -> Result<Vec<ServiceHost>, String>;
    /// Every Serve entry naming `name`, on any node.
    async fn service_entries(&self, name: &str) -> Result<Vec<ServiceEntryHost>, String>;
    /// The Service run `creator_run_id` of this install journaled as
    /// `created`, with when: `None` when no journal records creating it.
    async fn service_creation(
        &self,
        name: &str,
        creator_run_id: i32,
    ) -> Result<Option<(ServiceDefinition, temps_core::DBDateTime)>, String>;
    /// The confirmed delete (ADR 065 D4): under the Service's lock, no
    /// Serve entry names it, no host advertises it, and a read made just
    /// before the `DELETE` still carries this install's marker for
    /// `creator_run_id` and equals `shown`; else it is kept.
    async fn delete_service(
        &self,
        tailnet: &str,
        name: &str,
        creator_run_id: i32,
        shown: &ServiceDefinition,
    ) -> Result<ServiceDeletion, String>;
}

/// Tailscale's steps for one run, over `ops`.
pub struct TailscaleRun<'a> {
    ops: &'a dyn TailscaleOps,
}

impl<'a> TailscaleRun<'a> {
    pub fn new(ops: &'a dyn TailscaleOps) -> Self {
        Self { ops }
    }

    /// ADR 065 D4: declare the worker's Serve entry, referenced by this
    /// run, then wait for its agent to apply it. Done only on `applied`;
    /// the agent never overwrites what it did not write, so neither does
    /// this step.
    async fn configure_serve(&self, run: &mut dyn StepRun, target: &ServeTarget) -> Outcome {
        let run_id = run.input().run_id;
        let host = run.input().address.clone();
        let node = target.node_name.clone();
        let address_id = match run.address_id().await {
            Ok(id) => id,
            Err(error) => return Outcome::Failed(error),
        };
        let intent = Intent::TailscaleServe {
            node_id: target.node_id,
            node_name: node.clone(),
            address_id: address_id.clone(),
            hostname: host.clone(),
            service: target.service.clone(),
        };
        match run.begin(&intent).await {
            Err(hold) => return hold.into(),
            Ok(Begin::Have(_)) => {}
            Ok(Begin::Go) => {
                let wish = ServeWish {
                    target: target.clone(),
                    address_id: address_id.clone(),
                    hostname: host.clone(),
                    service_addrs: service_resource(run).map_or_else(Vec::new, |r| match r {
                        RunResource::TailscaleService { definition, .. } => definition.addrs,
                        _ => Vec::new(),
                    }),
                };
                let key = intent.key(run_id);
                match self.ops.claim_serve_entry(run_id, &wish).await {
                    Ok(claim @ (ServeClaim::Created { .. } | ServeClaim::Found { .. })) => {
                        let (entry_id, created) = match claim {
                            ServeClaim::Created { entry_id } => (entry_id, true),
                            ServeClaim::Found { entry_id } => (entry_id, false),
                            ServeClaim::Differs(_) | ServeClaim::Removing => (0, false),
                        };
                        run.finish(
                            &key,
                            Some(RunResource::TailscaleServe {
                                node_id: target.node_id,
                                node_name: node.clone(),
                                address_id: address_id.clone(),
                                hostname: host.clone(),
                                entry_id,
                                created,
                            }),
                        )
                        .await;
                    }
                    Ok(ServeClaim::Differs(what)) => {
                        run.finish(&key, None).await;
                        return Outcome::Conflict(format!(
                            "{node} already has a Serve entry for {host} that another Temps \
                             setup asked for: {what}. Temps changes nothing there: undo that \
                             setup first, then resume this one."
                        ));
                    }
                    Ok(ServeClaim::Removing) => {
                        run.finish(&key, None).await;
                        return Outcome::Stalled(format!(
                            "{node} is still taking an earlier Serve entry for {host} off its \
                             config, and Temps never revives one being removed. Resume once \
                             it has; its agent reports every 30 seconds."
                        ));
                    }
                    Err(error) => {
                        run.finish_failed(&intent, true).await;
                        return Outcome::Failed(error);
                    }
                }
            }
        }
        let mut waited = Duration::ZERO;
        let mut last = format!(
            "{node} has not applied the Serve entry for {host} yet; its agent applies it on its \
             next pass, every 30 seconds."
        );
        loop {
            match self.ops.serve_entry(target.node_id, &address_id).await {
                Ok(None) => {
                    return Outcome::Failed(format!(
                        "The Serve entry for {host} on {node} is gone: something removed it \
                         while this run waited."
                    ))
                }
                Ok(Some(status)) if status.removing => {
                    return Outcome::Failed(format!(
                        "The Serve entry for {host} on {node} is being removed: another run's \
                         undo took it off while this run waited."
                    ))
                }
                Ok(Some(status)) if status.funnel => {
                    return Outcome::Conflict(tailnet_messages::funnel(&node))
                }
                Ok(Some(status)) => match status.report.as_deref() {
                    Some(REPORT_APPLIED) => {
                        return Outcome::Done(Some(format!(
                            "{node} serves {host} with Tailscale Serve."
                        )))
                    }
                    Some(REPORT_CONFLICT) => {
                        return Outcome::Conflict(tailnet_messages::slot_taken(
                            &host,
                            status
                                .detail
                                .as_deref()
                                .unwrap_or("a Serve entry Temps did not write"),
                        ))
                    }
                    Some(REPORT_ERROR) => {
                        last = format!(
                            "{node} could not apply the Serve entry for {host}: {}",
                            status.detail.unwrap_or_default()
                        )
                    }
                    _ => {
                        if let Some(error) = status.worker_error {
                            last = error;
                        }
                    }
                },
                Err(error) => last = format!("Could not read the Serve entry for {host}: {error}"),
            }
            if waited >= run.timing().publish {
                return Outcome::Stalled(last);
            }
            run.sleep(run.timing().poll).await;
            waited += run.timing().poll.max(Duration::from_millis(1));
        }
    }

    /// ADR 065 D2: prove a pasted OAuth client's scopes and save it.
    async fn save_tailscale_credential(&self, run: &mut dyn StepRun) -> Outcome {
        let run_id = run.input().run_id;
        let credential = run.input().tailscale_credential.clone();
        let Some(plan) = run.input().tailscale_service.clone() else {
            return Outcome::Skipped("No Tailscale Service.".into());
        };
        let intent = Intent::TailscaleCredential {
            name: plan.credential_name.clone(),
        };
        let key = intent.key(run_id);
        let resource = match run.begin(&intent).await {
            Err(hold) => return hold.into(),
            Ok(Begin::Have(resource)) => *resource,
            Ok(Begin::Go) => {
                let Some(credential) = &credential else {
                    run.finish(&key, None).await;
                    return Outcome::Failed(
                        "Paste the Tailscale OAuth client again: Temps never stores it until the \
                         run saves it."
                            .into(),
                    );
                };
                match self
                    .ops
                    .save_tailscale_credential(&plan.credential_name, credential)
                    .await
                {
                    Ok(id) => {
                        let resource = RunResource::TailscaleCredential {
                            id,
                            name: plan.credential_name.clone(),
                        };
                        run.finish(&key, Some(resource.clone())).await;
                        resource
                    }
                    Err(error) => {
                        run.finish_failed(&intent, true).await;
                        return Outcome::Failed(error);
                    }
                }
            }
        };
        match resource {
            RunResource::TailscaleCredential { name, .. } => {
                Outcome::Done(Some(format!("Every scope is there; saved as {name}.")))
            }
            _ => Outcome::Failed("The saved Tailscale OAuth client could not be found.".into()),
        }
    }

    /// ADR 065 D4: create the Service only when absent, find this install's
    /// own, and check the whole definition and its hosts back.
    async fn define_service(&self, run: &mut dyn StepRun) -> Outcome {
        let run_id = run.input().run_id;
        let Some(plan) = run.input().tailscale_service.clone() else {
            return Outcome::Skipped("No Tailscale Service.".into());
        };
        let name = plan.name.clone();
        let marker = tailscale::marker(&plan.install_id, run_id);
        let intent = Intent::TailscaleService {
            tailnet: plan.tailnet.clone(),
            name: name.clone(),
            marker: marker.clone(),
        };
        let key = intent.key(run_id);
        let resource = match service_resource(run) {
            Some(resource) => resource,
            None => {
                let pending = run
                    .journal()
                    .iter()
                    .any(|e| e.key == key && e.state == JournalState::Pending);
                // A pending create is resolved by its lookup, never by a
                // second read that could take it for someone else's.
                let read = if pending {
                    None
                } else {
                    match self.ops.read_service(&name).await {
                        Ok(read) => Some(read),
                        Err(error) => {
                            return Outcome::Failed(format!(
                                "Temps could not read `{name}` from your tailnet, so it created \
                                 nothing: {error}"
                            ))
                        }
                    }
                };
                match read {
                    None | Some(None) => match run.begin(&intent).await {
                        Err(hold) => return hold.into(),
                        Ok(Begin::Have(resource)) => *resource,
                        Ok(Begin::Go) => {
                            let fixed = ServiceDefinition::fixed(&name, &plan.tag, &marker);
                            match self.ops.create_service(&fixed).await {
                                Ok(()) => {
                                    let resource = RunResource::TailscaleService {
                                        tailnet: plan.tailnet.clone(),
                                        name: name.clone(),
                                        created: true,
                                        creator_run_id: run_id,
                                        definition: fixed,
                                    };
                                    run.finish(&key, Some(resource.clone())).await;
                                    resource
                                }
                                Err(error) => {
                                    run.finish_failed(&intent, error.maybe_applied).await;
                                    return Outcome::Failed(error.message);
                                }
                            }
                        }
                    },
                    // A resumed run whose earlier PUT landed.
                    Some(Some(found)) if found.comment == marker => {
                        let resource = RunResource::TailscaleService {
                            tailnet: plan.tailnet.clone(),
                            name: name.clone(),
                            created: true,
                            creator_run_id: run_id,
                            definition: found,
                        };
                        run.record_found(&intent, resource.clone()).await;
                        resource
                    }
                    Some(Some(found)) => {
                        match tailscale::ownership(&found, &plan.install_id) {
                            Ownership::Ours { creator_run_id } => {
                                match plan.seen.as_ref().map(|seen| found.differs_from(seen)) {
                                    // D1 option 1: this install's, as the plan read it.
                                    Some(None) => {
                                        let resource = RunResource::TailscaleService {
                                            tailnet: plan.tailnet.clone(),
                                            name: name.clone(),
                                            created: false,
                                            creator_run_id,
                                            definition: found,
                                        };
                                        run.record_found(&intent, resource.clone()).await;
                                        resource
                                    }
                                    Some(Some(why)) => {
                                        return Outcome::Conflict(format!(
                                            "{why} since this run was planned. Temps wrote \
                                             nothing to it: check it, then plan again."
                                        ))
                                    }
                                    None => {
                                        return Outcome::Conflict(format!(
                                            "`{name}` was created by another setup of this Temps \
                                             install (run {creator_run_id}) after this run was \
                                             planned. Temps wrote nothing to it: plan again to \
                                             use it."
                                        ))
                                    }
                                }
                            }
                            Ownership::Foreign => {
                                return Outcome::Conflict(tailnet_messages::service_taken(&name))
                            }
                        }
                    }
                }
            }
        };
        self.check_service(run, resource, &plan, &marker).await
    }

    /// When the Service step is done (ADR 065 D4): its marker, the fixed
    /// definition, Tailscale addresses, and only recorded hosts.
    async fn check_service(
        &self,
        run: &mut dyn StepRun,
        resource: RunResource,
        plan: &ServicePlan,
        marker: &str,
    ) -> Outcome {
        let RunResource::TailscaleService {
            created,
            creator_run_id,
            ..
        } = resource.clone()
        else {
            return Outcome::Failed("The Tailscale Service could not be recorded.".into());
        };
        let name = plan.name.as_str();
        // Tailscale assigns the TailVIPs: wait for them to show.
        let mut waited = Duration::ZERO;
        let read = loop {
            match self.ops.read_service(name).await {
                Ok(Some(read)) if !read.addrs.is_empty() || waited >= run.timing().publish => {
                    break read
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Outcome::Failed(format!(
                        "`{name}` is gone from your tailnet: something deleted it while this run \
                         worked on it."
                    ))
                }
                Err(error) if waited >= run.timing().publish => {
                    return Outcome::Stalled(format!(
                        "Could not read `{name}` back from your tailnet: {error}"
                    ))
                }
                Err(_) => {}
            }
            run.sleep(run.timing().poll).await;
            waited += run.timing().poll.max(Duration::from_millis(1));
        };
        let owned = if created {
            (read.comment == marker).then_some(())
        } else {
            matches!(
                tailscale::ownership(&read, &plan.install_id),
                Ownership::Ours { .. }
            )
            .then_some(())
        };
        if owned.is_none() {
            return Outcome::Conflict(format!(
                "`{name}` now carries the comment `{}`, not this Temps install's marker. Temps \
                 writes nothing to it: check who changed it.",
                read.comment
            ));
        }
        if let Some(why) = read.differs_from_fixed(&plan.tag) {
            return Outcome::Conflict(format!("{why}. Temps never changes a Service: check it."));
        }
        if !tailscale_addrs(&read.addrs) {
            return Outcome::Conflict(format!(
                "`{name}` has the addresses {:?}, not Tailscale ones, so Temps does not trust it.",
                read.addrs
            ));
        }
        if !created {
            match self.ops.service_creation(name, creator_run_id).await {
                Ok(Some((journaled, _))) if journaled.addrs.is_empty() => {}
                Ok(Some((journaled, _))) => {
                    if let Some(why) = read.differs_from(&ServiceDefinition {
                        comment: read.comment.clone(),
                        ..journaled
                    }) {
                        return Outcome::Conflict(format!("{why}. Temps never changes a Service."));
                    }
                }
                Ok(None) => {
                    return Outcome::Conflict(format!(
                        "No setup of this Temps install records creating `{name}` (run \
                         {creator_run_id}), so Temps does not use it. Pick another name."
                    ))
                }
                Err(error) => return Outcome::Failed(error),
            }
        }
        if let Err(outcome) = self.unrecorded(run, name).await {
            return outcome;
        }
        // Journal what was read, with the addresses Tailscale assigned.
        let resource = RunResource::TailscaleService {
            tailnet: plan.tailnet.clone(),
            name: name.to_string(),
            created,
            creator_run_id,
            definition: read.clone(),
        };
        let key = Intent::TailscaleService {
            tailnet: plan.tailnet.clone(),
            name: name.to_string(),
            marker: marker.to_string(),
        }
        .key(run.input().run_id);
        run.update_resource(&key, resource).await;
        Outcome::Done(Some(format!(
            "`{name}` is in your tailnet at {}{}.",
            read.addrs.join(", "),
            if created {
                ""
            } else {
                ", as this Temps install made it"
            }
        )))
    }

    /// Every host advertising the Service must match one of this address's
    /// Serve entries for it (ADR 065 D4); `Err` the outcome when one does
    /// not, or the hosts cannot be read.
    async fn unrecorded(
        &self,
        run: &mut dyn StepRun,
        name: &str,
    ) -> Result<Vec<ServiceHost>, Outcome> {
        let hosts = self.ops.service_hosts(name).await.map_err(|error| {
            Outcome::Failed(format!(
                "Temps could not read which machines host `{name}`, which it checks before \
                 trusting it: {error}"
            ))
        })?;
        let entries = self
            .ops
            .service_entries(name)
            .await
            .map_err(Outcome::Failed)?;
        let address_id = match run.address_id().await {
            Ok(id) => id,
            Err(error) => return Err(Outcome::Failed(error)),
        };
        let foreign = unrecorded_hosts(&hosts, &entries, Some(&address_id));
        if foreign.is_empty() {
            Ok(hosts)
        } else {
            Err(Outcome::Conflict(unrecorded_message(name, &foreign)))
        }
    }

    /// ADR 065 D4: wait for the tailnet admin to approve the worker as the
    /// Service's host. It never approves; waiting is not a failure.
    async fn wait_for_approval(&self, run: &mut dyn StepRun) -> Outcome {
        let (Some(plan), Some(target)) = (
            run.input().tailscale_service.clone(),
            run.input().serve.clone(),
        ) else {
            return Outcome::Skipped("No Tailscale Service.".into());
        };
        let name = plan.name.as_str();
        let node = target.node_name.as_str();
        let mut waited = Duration::ZERO;
        let mut last;
        loop {
            // Deleted after the define step: no one recreates it behind
            // the user's back, so the run stops and undo takes it apart.
            if let Ok(None) = self.ops.read_service(name).await {
                return Outcome::Failed(format!(
                    "`{name}` is gone from your tailnet: something deleted it after this run \
                     defined it. Undo, then make the address reachable again."
                ));
            }
            let hosts = match self.unrecorded(run, name).await {
                Ok(hosts) => hosts,
                Err(outcome) => return outcome,
            };
            let mine = match self.ops.service_entries(name).await {
                Ok(entries) => entries.into_iter().find(|e| e.node_id == target.node_id),
                Err(error) => return Outcome::Failed(error),
            };
            let host = mine
                .as_ref()
                .and_then(|e| hosts.iter().find(|h| e.hosts(h)));
            match host.map(|h| h.approved) {
                Some(Some(true)) => {
                    return Outcome::Done(Some(format!("{node} is the approved host of `{name}`.")))
                }
                Some(_) => last = tailnet_messages::not_approved(name, node, &plan.tag),
                None => {
                    last = format!(
                        "`{node}` does not advertise `{name}` yet; its agent does on its next \
                         pass, every 30 seconds. Then {}",
                        tailnet_messages::not_approved(name, node, &plan.tag)
                    )
                }
            }
            if waited >= run.timing().publish {
                return Outcome::Stalled(last);
            }
            run.sleep(run.timing().poll).await;
            waited += run.timing().poll.max(Duration::from_millis(1));
        }
    }
}

/// The Tailscale Service this run defined, as journaled.
fn service_resource(run: &dyn StepRun) -> Option<RunResource> {
    let name = &run.input().tailscale_service.as_ref()?.name;
    run.journal().iter().find_map(|e| match &e.resource {
        Some(r @ RunResource::TailscaleService { name: n, .. })
            if n == name && e.state == JournalState::Done =>
        {
            Some(r.clone())
        }
        _ => None,
    })
}

#[async_trait]
impl ProviderRun for TailscaleRun<'_> {
    fn runs(&self, kind: StepKind) -> bool {
        matches!(
            kind,
            StepKind::ConfigureTailscaleServe
                | StepKind::SaveTailscaleCredential
                | StepKind::DefineTailscaleService
                | StepKind::WaitForServiceApproval
        )
    }

    async fn step(&self, run: &mut dyn StepRun, kind: StepKind) -> Outcome {
        match kind {
            StepKind::ConfigureTailscaleServe => {
                let Some(target) = run.input().serve.clone() else {
                    return Outcome::Skipped("No worker serves it through Tailscale.".into());
                };
                self.configure_serve(run, &target).await
            }
            StepKind::SaveTailscaleCredential => self.save_tailscale_credential(run).await,
            StepKind::DefineTailscaleService => self.define_service(run).await,
            StepKind::WaitForServiceApproval => self.wait_for_approval(run).await,
            other => Outcome::Failed(format!("{other:?} is not a Tailscale step.")),
        }
    }

    fn owns(&self, intent: &Intent) -> bool {
        matches!(
            intent,
            Intent::TailscaleServe { .. }
                | Intent::TailscaleService { .. }
                | Intent::TailscaleCredential { .. }
        )
    }

    /// Undo of Tailscale's resources is still the engine's (ADR-116 D8).
    fn owns_resource(&self, _resource: &RunResource) -> bool {
        false
    }

    async fn look(&self, run_id: i32, intent: &Intent) -> Result<Found, String> {
        match intent {
            // The row and this run's reference to it: Temps' own rows.
            Intent::TailscaleServe {
                node_id,
                node_name,
                address_id,
                hostname,
                ..
            } => Ok(
                match self
                    .ops
                    .serve_reference(run_id, *node_id, address_id)
                    .await?
                {
                    Some((entry_id, created)) => {
                        Found::Made(Box::new(RunResource::TailscaleServe {
                            node_id: *node_id,
                            node_name: node_name.clone(),
                            address_id: address_id.clone(),
                            hostname: hostname.clone(),
                            entry_id,
                            created,
                        }))
                    }
                    None => Found::Missing,
                },
            ),
            // Created by this run only when it carries this run's marker.
            Intent::TailscaleService {
                tailnet,
                name,
                marker,
            } => Ok(match self.ops.read_service(name).await? {
                None => Found::Missing,
                Some(definition) if definition.comment == *marker => {
                    Found::Made(Box::new(RunResource::TailscaleService {
                        tailnet: tailnet.clone(),
                        name: name.clone(),
                        created: true,
                        creator_run_id: run_id,
                        definition,
                    }))
                }
                Some(definition) => Found::Changed(format!(
                    "`{name}` with the comment `{}`, which this run did not write",
                    definition.comment
                )),
            }),
            // The saved row of that name.
            Intent::TailscaleCredential { name } => {
                Ok(match self.ops.find_credential(name).await? {
                    Some(resource) => Found::Made(Box::new(resource)),
                    None => Found::Missing,
                })
            }
            other => Err(format!(
                "{} is not Tailscale's to look up.",
                other.describe()
            )),
        }
    }

    async fn check_undo(&self, _resource: &RunResource, _checked: bool) -> UndoCheck {
        UndoCheck::Remove
    }

    async fn remove(&self, _resource: &RunResource) -> Result<(), String> {
        Ok(())
    }
}
