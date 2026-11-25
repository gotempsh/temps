// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cloudflare's part of a reachability run (ADR-116 D3): the steps only a
//! Cloudflare zone or tunnel has, and the journal entries they write, which
//! only Cloudflare can find again and undo. The engine
//! ([`crate::reachability::run`]) keeps the journal and its rules; these steps
//! reach it only through [`StepRun`].
//!
//! The edge rules of ADR 070: a redirect from plain HTTP to HTTPS, and a WAF
//! rule refusing ports other than 80 and 443, each for one hostname.

use async_trait::async_trait;

use super::edge_rules::{same_rule, EdgeRuleError, EdgeRuleSeen};
use crate::reachability::edge_rules::EdgeRulePurpose;
use crate::reachability::model::{Intent, JournalState, RunResource, StepKind};
use crate::reachability::run::{
    wait_message, Begin, Found, Outcome, ProviderRun, StepRun, UndoCheck,
};

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
}

#[async_trait]
impl ProviderRun for CloudflareRun<'_> {
    fn runs(&self, kind: StepKind) -> bool {
        matches!(
            kind,
            StepKind::RedirectToHttps | StepKind::StandardPortsOnly
        )
    }

    async fn step(&self, run: &mut dyn StepRun, kind: StepKind) -> Outcome {
        match kind {
            StepKind::RedirectToHttps => self.edge_rule(run, EdgeRulePurpose::HttpsRedirect).await,
            StepKind::StandardPortsOnly => {
                self.edge_rule(run, EdgeRulePurpose::StandardPorts).await
            }
            other => Outcome::Failed(format!("{other:?} is not a Cloudflare step.")),
        }
    }

    fn owns(&self, intent: &Intent) -> bool {
        matches!(intent, Intent::EdgeRule { .. })
    }

    fn owns_resource(&self, resource: &RunResource) -> bool {
        matches!(resource, RunResource::EdgeRule { .. })
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
            other => Err(format!(
                "{} is not Cloudflare's to look up.",
                other.describe()
            )),
        }
    }

    /// A rule goes only while it still does what the run wrote (ADR 070
    /// D2): its expression and action, and for the redirect its target,
    /// status and query string. Unless the user said to delete it anyway.
    async fn check_undo(&self, resource: &RunResource, checked: bool) -> UndoCheck {
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
