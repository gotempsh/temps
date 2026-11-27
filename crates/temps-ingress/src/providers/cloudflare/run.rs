// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cloudflare's part of a reachability run (ADR-117 D3): the steps only a
//! Cloudflare zone or tunnel has, and the journal entries they write, which
//! only Cloudflare can find again and undo. The engine
//! ([`crate::reachability::run`]) keeps the journal and its rules; these steps
//! reach it only through [`StepRun`].
//!
//! The edge rules of ADR 070: a redirect from plain HTTP to HTTPS, and a WAF
//! rule refusing ports other than 80 and 443, each for one hostname. And the
//! Access application of ADR 065 D9 in front of an address. And the first
//! two steps of a new tunnel: checking the pasted API token, and creating the
//! tunnel's connector on the Cloudflare account.

use async_trait::async_trait;

use super::edge_rules::{same_rule, EdgeRuleError, EdgeRuleSeen};
use crate::reachability::access::{covers, run_app_name, AccessApp, AccessPolicyAnswer};
use crate::reachability::edge_rules::EdgeRulePurpose;
use crate::reachability::model::{AccessBinding, Intent, JournalState, RunResource, StepKind};
use crate::reachability::run::{
    wait_message, Begin, Found, Outcome, ProviderRun, StepRun, TunnelStatus, UndoCheck, WriteError,
};
use crate::reachability::token_check::TokenCheckResult;

/// Where a rule a run added lives at Cloudflare (ADR 070).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRuleAt {
    pub provider_id: i32,
    pub zone_id: String,
    pub ruleset_id: String,
    pub rule_id: String,
}

impl EdgeRuleAt {
    pub fn of(resource: &RunResource) -> Option<Self> {
        match resource {
            RunResource::EdgeRule {
                provider_id,
                zone_id,
                ruleset_id,
                rule_id,
                ..
            } => Some(Self {
                provider_id: *provider_id,
                zone_id: zone_id.clone(),
                ruleset_id: ruleset_id.clone(),
                rule_id: rule_id.clone(),
            }),
            _ => None,
        }
    }
}

/// What adding a rule did (ADR 070 D1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeRuleMade {
    /// Added: a [`RunResource::EdgeRule`].
    Created(Box<RunResource>),
    /// A rule doing the same was there already; used as found.
    AlreadyThere(String),
}

/// What a run does at Cloudflare, behind a trait so tests can stand in.
#[async_trait]
pub trait CloudflareOps: Send + Sync {
    /// What the pasted API token may do, for `zone` only.
    async fn check_token(&self, token: &str, zone: &str) -> TokenCheckResult;
    /// Creates a tunnel that publishes only the hostnames chosen for it.
    /// On failure, the id of a tunnel that was saved anyway.
    async fn create_tunnel(
        &self,
        name: &str,
        provider_id: i32,
        domain: &str,
        node_ids: &[i32],
    ) -> Result<(i32, TunnelStatus), (Option<i32>, String)>;
    /// Add `purpose`'s Cloudflare rule for `host` (ADR 070), with the token
    /// of `connector_id`'s DNS provider (else the one managing the zone), or
    /// find one doing the same already.
    async fn create_edge_rule(
        &self,
        run_id: i32,
        connector_id: Option<i32>,
        host: &str,
        purpose: EdgeRulePurpose,
    ) -> Result<EdgeRuleMade, EdgeRuleError>;
    /// The rule run `run_id` added for `host`, found by its reference.
    async fn find_edge_rule(
        &self,
        run_id: i32,
        connector_id: Option<i32>,
        host: &str,
        purpose: EdgeRulePurpose,
    ) -> Result<Option<RunResource>, String>;
    /// The rule `rule` names as Cloudflare has it now; `None` when gone.
    async fn edge_rule(&self, rule: &EdgeRuleAt) -> Result<Option<EdgeRuleSeen>, String>;
    async fn delete_edge_rule(&self, rule: &EdgeRuleAt) -> Result<(), String>;
    /// The Access application `name` protecting `host` in `provider_id`'s
    /// account, which an earlier attempt of the run may have created.
    async fn find_access_app(
        &self,
        provider_id: i32,
        name: &str,
        host: &str,
    ) -> Result<Option<RunResource>, String>;
    /// Access application `id` in `provider_id`'s account (ADR 065 D9);
    /// `None` when it no longer exists.
    async fn access_app(&self, provider_id: i32, id: &str) -> Result<Option<AccessApp>, String>;
    /// Creates the Access application `name` protecting `host`, letting in
    /// who `policy` says.
    async fn create_access_app(
        &self,
        provider_id: i32,
        name: &str,
        host: &str,
        policy: &AccessPolicyAnswer,
    ) -> Result<AccessApp, WriteError>;
    async fn delete_access_app(&self, provider_id: i32, id: &str) -> Result<(), String>;
}

/// Cloudflare's steps for one run, over `ops`.
pub struct CloudflareRun<'a> {
    ops: &'a dyn CloudflareOps,
}

impl<'a> CloudflareRun<'a> {
    pub fn new(ops: &'a dyn CloudflareOps) -> Self {
        Self { ops }
    }

    /// Add `purpose`'s rule for the run's address, journaled first. A rule
    /// that cannot be added degrades the address, never fails the run.
    async fn edge_rule(&self, run: &mut dyn StepRun, purpose: EdgeRulePurpose) -> Outcome {
        let run_id = run.input().run_id;
        let host = run.input().address.clone();
        let connector_id = run
            .input()
            .tunnel
            .as_ref()
            .map(|t| t.id)
            .or(run.connector_id());
        let intent = Intent::EdgeRule {
            hostname: host.clone(),
            purpose,
            connector_id,
        };
        match run.begin(&intent).await {
            Err(hold) => hold.into(),
            Ok(Begin::Have(resource)) => {
                Outcome::Done(Some(format!("{} is in place.", resource.describe())))
            }
            Ok(Begin::Go) => match self
                .ops
                .create_edge_rule(run_id, connector_id, &host, purpose)
                .await
            {
                Ok(EdgeRuleMade::Created(resource)) => {
                    let detail = format!("Added the {}.", resource.describe());
                    run.finish(&intent.key(run_id), Some(*resource)).await;
                    Outcome::Done(Some(detail))
                }
                Ok(EdgeRuleMade::AlreadyThere(found)) => {
                    run.finish(&intent.key(run_id), None).await;
                    Outcome::Skipped(format!(
                        "The zone already has a rule doing this ({found}); used as it is, and \
                         undo leaves it."
                    ))
                }
                Err(error) if error.maybe_applied => {
                    run.finish_failed(&intent, true).await;
                    match run.entry(&intent.key(run_id)) {
                        Some(entry) if entry.state == JournalState::Done => Outcome::Done(
                            entry
                                .resource
                                .map(|r| format!("Added the {}.", r.describe())),
                        ),
                        Some(entry) if entry.state == JournalState::Absent => {
                            Outcome::Skipped(degraded_message(&host, purpose, &error))
                        }
                        Some(entry) => Outcome::Stalled(format!(
                            "{} {}",
                            error.message,
                            wait_message(&entry, chrono::Utc::now())
                        )),
                        None => Outcome::Failed(error.message),
                    }
                }
                Err(error) => {
                    run.finish(&intent.key(run_id), None).await;
                    tracing::warn!(
                        run = run_id,
                        host,
                        missing_permission = error.missing_permission,
                        "could not add a Cloudflare edge rule; publishing without it: {}",
                        error.message
                    );
                    Outcome::Skipped(degraded_message(&host, purpose, &error))
                }
            },
        }
    }

    /// Check the pasted token has every permission, for this zone only.
    async fn check_token(&self, run: &mut dyn StepRun) -> Outcome {
        let input = run.input();
        let (Some(token), Some(new)) = (&input.token, &input.new_tunnel) else {
            return Outcome::Failed(
                "Paste the Cloudflare API token again: Temps never stores it until the run \
                 saves it."
                    .into(),
            );
        };
        let result = self.ops.check_token(token, &new.zone).await;
        match result.refusal() {
            Some(reason) => Outcome::Failed(reason),
            None => Outcome::Done(Some(
                "Every permission is there, for this zone only.".into(),
            )),
        }
    }

    /// Create the new tunnel on the account the run saved, journaled first.
    async fn create_tunnel(&self, run: &mut dyn StepRun) -> Outcome {
        let run_id = run.input().run_id;
        let Some(new) = run.input().new_tunnel.clone() else {
            return Outcome::Skipped("No new tunnel.".into());
        };
        let Some(provider_id) = run.dns_provider_id() else {
            return Outcome::Failed("No Cloudflare account to create the tunnel with.".into());
        };
        let intent = Intent::Connector {
            name: new.name.clone(),
        };
        match run.begin(&intent).await {
            Err(hold) => hold.into(),
            Ok(Begin::Have(resource)) => {
                if let RunResource::Connector { id, .. } = *resource {
                    run.set_connector(id);
                }
                Outcome::Done(Some(format!(
                    "Tunnel {} for {} (created earlier).",
                    new.name, new.domain
                )))
            }
            Ok(Begin::Go) => match self
                .ops
                .create_tunnel(&new.name, provider_id, &new.domain, &[new.worker.id])
                .await
            {
                Ok((id, status)) => {
                    run.set_connector(id);
                    run.finish(
                        &intent.key(run_id),
                        Some(RunResource::Connector {
                            id,
                            name: new.name.clone(),
                        }),
                    )
                    .await;
                    if status.status == "error" {
                        return Outcome::Failed(
                            status
                                .last_error
                                .unwrap_or_else(|| "Cloudflare refused the tunnel.".into()),
                        );
                    }
                    Outcome::Done(Some(format!("Tunnel {} for {}.", new.name, new.domain)))
                }
                Err((saved, error)) => {
                    match saved {
                        Some(id) => {
                            run.finish(
                                &intent.key(run_id),
                                Some(RunResource::Connector {
                                    id,
                                    name: new.name.clone(),
                                }),
                            )
                            .await
                        }
                        None => run.finish_failed(&intent, true).await,
                    }
                    Outcome::Failed(error)
                }
            },
        }
    }

    /// Put a Cloudflare Access app in front of `host` (ADR 065 D9): the
    /// one the plan found, re-read so a deleted or moved app is not used,
    /// or one this run creates and journals.
    async fn access_app(&self, run: &mut dyn StepRun) -> Outcome {
        let run_id = run.input().run_id;
        let host = run.input().address.clone();
        let Some(access) = run.input().access.clone() else {
            return Outcome::Failed(format!(
                "This run has no Cloudflare Access app to put in front of {host}. Plan again."
            ));
        };
        if let Some(existing) = &access.existing {
            return match self.ops.access_app(access.provider_id, &existing.id).await {
                Ok(Some(app)) if covers(&app, &host) => {
                    run.protect_with(AccessBinding {
                        provider_id: access.provider_id,
                        app_id: app.id.clone(),
                        aud: app.aud.clone(),
                    });
                    Outcome::Skipped(format!(
                        "Access app {} already protects {host}; used as it is.",
                        app.name
                    ))
                }
                Ok(Some(app)) => Outcome::Failed(format!(
                    "Access app {} no longer protects {host} (it is for {}). Plan again.",
                    app.name, app.domain
                )),
                Ok(None) => Outcome::Failed(format!(
                    "Access app {} was deleted in Cloudflare after planning. Plan again.",
                    existing.name
                )),
                Err(error) => Outcome::Failed(error),
            };
        }
        let name = run_app_name(&access.name, run_id);
        let intent = Intent::AccessApp {
            provider_id: access.provider_id,
            name: name.clone(),
            hostname: host.clone(),
        };
        match run.begin(&intent).await {
            Err(hold) => hold.into(),
            Ok(Begin::Have(resource)) => {
                bind(run, &resource);
                Outcome::Done(Some(format!("{} protects {host}.", resource.describe())))
            }
            Ok(Begin::Go) => match self
                .ops
                .create_access_app(access.provider_id, &name, &host, &access.policy)
                .await
            {
                Ok(app) => {
                    let resource = RunResource::AccessApp {
                        provider_id: access.provider_id,
                        id: app.id,
                        name: app.name,
                        hostname: host.clone(),
                        aud: app.aud,
                        policies: app.policies,
                    };
                    bind(run, &resource);
                    let detail = format!("{} protects {host}.", resource.describe());
                    run.finish(&intent.key(run_id), Some(resource)).await;
                    Outcome::Done(Some(detail))
                }
                Err(error) => {
                    run.finish_failed(&intent, error.maybe_applied).await;
                    Outcome::Failed(error.message)
                }
            },
        }
    }

    /// An Access app goes only while it is still this address's app, and,
    /// unless the user said to delete it anyway, while its policies are what
    /// the run created (ADR 065 D9).
    async fn check_access_app_undo(&self, resource: &RunResource, checked: bool) -> UndoCheck {
        let RunResource::AccessApp {
            provider_id,
            id,
            name,
            hostname,
            policies,
            ..
        } = resource
        else {
            return UndoCheck::Remove;
        };
        match self.ops.access_app(*provider_id, id).await {
            Ok(None) => UndoCheck::Gone,
            Ok(Some(app))
                if !app
                    .domain
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(hostname.trim_end_matches('.')) =>
            {
                UndoCheck::Left(format!(
                    "Left Access app {name}: it protects {} now, not {hostname}",
                    app.domain
                ))
            }
            Ok(Some(app)) if !checked && app.policies != *policies => {
                UndoCheck::Changed("its policies were changed".to_string())
            }
            Ok(Some(_)) => UndoCheck::Remove,
            Err(error) => UndoCheck::Unreadable(format!(
                "Temps could not read Access app {name} to check it is still the one this \
                 run created, so it touched nothing from there on: {error} Undo again to \
                 retry."
            )),
        }
    }
}

/// Record the Access app `resource` names as the address's gate.
fn bind(run: &mut dyn StepRun, resource: &RunResource) {
    if let RunResource::AccessApp {
        provider_id,
        id,
        aud,
        ..
    } = resource
    {
        run.protect_with(AccessBinding {
            provider_id: *provider_id,
            app_id: id.clone(),
            aud: aud.clone(),
        });
    }
}

#[async_trait]
impl ProviderRun for CloudflareRun<'_> {
    fn runs(&self, kind: StepKind) -> bool {
        matches!(
            kind,
            StepKind::CheckToken
                | StepKind::CreateTunnel
                | StepKind::RedirectToHttps
                | StepKind::StandardPortsOnly
                | StepKind::AccessApp
        )
    }

    async fn step(&self, run: &mut dyn StepRun, kind: StepKind) -> Outcome {
        match kind {
            StepKind::RedirectToHttps => self.edge_rule(run, EdgeRulePurpose::HttpsRedirect).await,
            StepKind::StandardPortsOnly => {
                self.edge_rule(run, EdgeRulePurpose::StandardPorts).await
            }
            StepKind::CheckToken => self.check_token(run).await,
            StepKind::CreateTunnel => self.create_tunnel(run).await,
            StepKind::AccessApp => self.access_app(run).await,
            other => Outcome::Failed(format!("{other:?} is not a Cloudflare step.")),
        }
    }

    fn owns(&self, intent: &Intent) -> bool {
        matches!(intent, Intent::EdgeRule { .. } | Intent::AccessApp { .. })
    }

    fn owns_resource(&self, resource: &RunResource) -> bool {
        matches!(
            resource,
            RunResource::EdgeRule { .. } | RunResource::AccessApp { .. }
        )
    }

    /// Found by this run's reference at Cloudflare (ADR 070 D2).
    async fn look(&self, run_id: i32, intent: &Intent) -> Result<Found, String> {
        match intent {
            Intent::EdgeRule {
                hostname,
                purpose,
                connector_id,
            } => Ok(
                match self
                    .ops
                    .find_edge_rule(run_id, *connector_id, hostname, *purpose)
                    .await?
                {
                    Some(resource) => Found::Made(Box::new(resource)),
                    None => Found::Missing,
                },
            ),
            Intent::AccessApp {
                provider_id,
                name,
                hostname,
            } => Ok(
                match self
                    .ops
                    .find_access_app(*provider_id, name, hostname)
                    .await?
                {
                    Some(resource) => Found::Made(Box::new(resource)),
                    None => Found::Missing,
                },
            ),
            other => Err(format!(
                "{} is not Cloudflare's to look up.",
                other.describe()
            )),
        }
    }

    /// An Access app as [`Self::check_access_app_undo`] says. A rule goes
    /// only while it still does what the run wrote (ADR 070 D2): its
    /// expression and action, and for the redirect its target, status and
    /// query string. Unless the user said to delete it anyway.
    async fn check_undo(&self, resource: &RunResource, checked: bool) -> UndoCheck {
        if matches!(resource, RunResource::AccessApp { .. }) {
            return self.check_access_app_undo(resource, checked).await;
        }
        let RunResource::EdgeRule {
            hostname, purpose, ..
        } = resource
        else {
            return UndoCheck::Remove;
        };
        let Some(at) = EdgeRuleAt::of(resource) else {
            return UndoCheck::Remove;
        };
        match self.ops.edge_rule(&at).await {
            Ok(None) => UndoCheck::Gone,
            Ok(Some(rule)) if !checked && !same_rule(&rule, *purpose, hostname) => {
                UndoCheck::Changed(format!("it is now {}", rule.summary()))
            }
            Ok(Some(_)) => UndoCheck::Remove,
            Err(error) => UndoCheck::Unreadable(format!(
                "Temps could not read the {} to check it is still the one this run added, so \
                 it touched nothing from there on: {error} Undo again to retry.",
                purpose.describe(hostname)
            )),
        }
    }

    async fn remove(&self, resource: &RunResource) -> Result<(), String> {
        if let RunResource::AccessApp {
            provider_id, id, ..
        } = resource
        {
            return self.ops.delete_access_app(*provider_id, id).await;
        }
        match EdgeRuleAt::of(resource) {
            Some(at) => self.ops.delete_edge_rule(&at).await,
            None => Ok(()),
        }
    }
}

/// What a skipped edge rule step says (ADR 070 D3): what is missing,
/// whose token, what stays exposed, and the fix.
pub fn degraded_message(host: &str, purpose: EdgeRulePurpose, error: &EdgeRuleError) -> String {
    let whose = error
        .provider
        .as_deref()
        .map(|p| format!("The API token of DNS provider `{p}`"))
        .unwrap_or_else(|| "The Cloudflare API token".to_string());
    match error.missing_permission {
        Some(permission) => format!(
            "Not set up: {whose} lacks {permission}, so {}. {host} is published anyway. Add \
             {permission} to the token in Cloudflare (My Profile → API Tokens), then make the \
             address reachable again to add the rule.",
            purpose.exposure(host)
        ),
        None => format!(
            "Not set up: {} {host} is published anyway, but {}. Fix that in Cloudflare, then \
             make the address reachable again to add the rule.",
            error.message.trim_end_matches('.').to_string() + ".",
            purpose.exposure(host)
        ),
    }
}
