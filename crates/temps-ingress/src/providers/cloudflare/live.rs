// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The live [`CloudflareOps`]: what a reachability run does at Cloudflare,
//! with the zone's or the tunnel's DNS provider token (ADR 070).

use async_trait::async_trait;
use sea_orm::EntityTrait;
use temps_entities::ingress_connectors;
use tracing::info;

use super::edge_rules::{self, EdgeRuleCreated, EdgeRuleError, EdgeRuleSeen};
use super::run::{CloudflareOps, EdgeRuleAt, EdgeRuleMade};
use crate::reachability::access::{AccessApp, AccessPolicyAnswer};
use crate::reachability::edge_rules::EdgeRulePurpose;
use crate::reachability::model::RunResource;
use crate::reachability::run::{TunnelStatus, WriteError};
use crate::reachability::service::LiveOps;
use crate::reachability::token_check::{check_token, TokenCheckResult};
use crate::service::{CreateConnectorRequest, CreateError};
use serde_json::Value;

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[async_trait]
impl CloudflareOps for LiveOps {
    async fn create_edge_rule(
        &self,
        run_id: i32,
        connector_id: Option<i32>,
        host: &str,
        purpose: EdgeRulePurpose,
    ) -> Result<EdgeRuleMade, EdgeRuleError> {
        let refused = |message: String| EdgeRuleError {
            message,
            missing_permission: None,
            provider: None,
            maybe_applied: false,
        };
        let (provider_id, provider_name, token) = self
            .edge_credentials(connector_id, host)
            .await
            .map_err(refused)?;
        let with_provider = |mut error: EdgeRuleError| {
            error.provider = Some(provider_name.clone());
            error
        };
        let reference = self.edge_rule_ref(run_id, purpose).await.map_err(refused)?;
        let (zone, zone_id) = edge_rules::find_zone(&self.http, &token, host)
            .await
            .map_err(with_provider)?
            .ok_or_else(|| {
                with_provider(refused(format!(
                    "The token cannot see the Cloudflare zone of {host}."
                )))
            })?;
        if let Some(why) = edge_rules::rule_refusal(host, &zone, purpose) {
            return Err(with_provider(refused(why)));
        }
        let made = edge_rules::add_rule(&self.http, &token, &zone_id, purpose, host, &reference)
            .await
            .map_err(with_provider)?;
        Ok(match made {
            EdgeRuleCreated::Created {
                ruleset_id,
                rule_id,
                expression,
            } => {
                info!(run = run_id, host, %zone, rule = %rule_id, ?purpose, "added a Cloudflare edge rule");
                EdgeRuleMade::Created(Box::new(RunResource::EdgeRule {
                    hostname: host.to_string(),
                    purpose,
                    provider_id,
                    zone,
                    zone_id,
                    ruleset_id,
                    rule_id,
                    expression,
                }))
            }
            EdgeRuleCreated::AlreadyThere { description } => {
                EdgeRuleMade::AlreadyThere(description)
            }
        })
    }
    async fn find_edge_rule(
        &self,
        run_id: i32,
        connector_id: Option<i32>,
        host: &str,
        purpose: EdgeRulePurpose,
    ) -> Result<Option<RunResource>, String> {
        let (provider_id, _, token) = self.edge_credentials(connector_id, host).await?;
        let reference = self.edge_rule_ref(run_id, purpose).await?;
        let Some((zone, zone_id)) = edge_rules::find_zone(&self.http, &token, host)
            .await
            .map_err(|e| e.message)?
        else {
            return Ok(None);
        };
        Ok(
            edge_rules::find_rule(&self.http, &token, &zone_id, purpose, &reference)
                .await?
                .map(|(ruleset_id, rule)| RunResource::EdgeRule {
                    hostname: host.to_string(),
                    purpose,
                    provider_id,
                    zone,
                    zone_id,
                    ruleset_id,
                    rule_id: rule.id,
                    expression: rule.expression,
                }),
        )
    }
    async fn edge_rule(&self, rule: &EdgeRuleAt) -> Result<Option<EdgeRuleSeen>, String> {
        let token = self.provider_token(rule.provider_id).await?;
        edge_rules::read_rule(
            &self.http,
            &token,
            &rule.zone_id,
            &rule.ruleset_id,
            &rule.rule_id,
        )
        .await
    }
    async fn delete_edge_rule(&self, rule: &EdgeRuleAt) -> Result<(), String> {
        let token = self.provider_token(rule.provider_id).await?;
        edge_rules::delete_rule(
            &self.http,
            &token,
            &rule.zone_id,
            &rule.ruleset_id,
            &rule.rule_id,
        )
        .await
    }

    async fn find_access_app(
        &self,
        provider_id: i32,
        name: &str,
        host: &str,
    ) -> Result<Option<RunResource>, String> {
        Ok(self
            .deps
            .access
            .app_named(provider_id, name, host)
            .await?
            .map(|app| RunResource::AccessApp {
                provider_id,
                id: app.id,
                name: app.name,
                hostname: host.to_string(),
                aud: app.aud,
                policies: app.policies,
            }))
    }

    async fn access_app(&self, provider_id: i32, id: &str) -> Result<Option<AccessApp>, String> {
        self.deps.access.app(provider_id, id).await
    }

    async fn create_access_app(
        &self,
        provider_id: i32,
        name: &str,
        host: &str,
        policy: &AccessPolicyAnswer,
    ) -> Result<AccessApp, WriteError> {
        self.deps
            .access
            .create_app(provider_id, name, host, policy)
            .await
            .map_err(|error| WriteError {
                message: error.message,
                maybe_applied: error.maybe_applied,
            })
    }

    async fn delete_access_app(&self, provider_id: i32, id: &str) -> Result<(), String> {
        self.deps.access.delete_app(provider_id, id).await
    }

    async fn check_token(&self, token: &str, zone: &str) -> TokenCheckResult {
        check_token(&self.http, token, Some(zone)).await
    }

    async fn create_tunnel(
        &self,
        name: &str,
        provider_id: i32,
        domain: &str,
        node_ids: &[i32],
    ) -> Result<(i32, TunnelStatus), (Option<i32>, String)> {
        let mut values = std::collections::BTreeMap::new();
        values.insert("dns_provider_id".to_string(), Value::from(provider_id));
        values.insert("domain".to_string(), Value::from(domain));
        let request = CreateConnectorRequest {
            name: name.to_string(),
            provider: super::KIND.to_string(),
            values,
            secrets: Default::default(),
            node_ids: node_ids.to_vec(),
            // Publish only what runs choose, so a tunnel on a whole zone
            // never puts other apps under it online by surprise.
            auto_publish: Some(false),
            host_suffix: None,
        };
        match self.deps.connectors.create(request).await {
            Ok(view) => Ok((
                view.id,
                TunnelStatus {
                    status: view.status,
                    last_error: view.last_error,
                    replicas: Vec::new(),
                },
            )),
            Err(CreateError::NotSaved(error)) => Err((None, error.to_string())),
            Err(CreateError::Saved {
                connector_id,
                error,
                ..
            }) => Err((Some(connector_id), error.to_string())),
        }
    }
}

impl LiveOps {
    /// The Cloudflare token a run's edge rules for `host` use (ADR 070):
    /// the tunnel's DNS provider's, else the provider managing the zone.
    async fn edge_credentials(
        &self,
        connector_id: Option<i32>,
        host: &str,
    ) -> Result<(i32, String, String), String> {
        let from_connector = match connector_id {
            Some(id) => ingress_connectors::Entity::find_by_id(id)
                .one(self.deps.db.as_ref())
                .await
                .map_err(text)?
                .and_then(|c| c.dns_provider_id),
            None => None,
        };
        let provider = match from_connector {
            Some(id) => self.deps.dns_providers.get(id).await.map_err(text)?,
            None => {
                self.deps
                    .dns_providers
                    .find_provider_for_domain(host)
                    .await
                    .map_err(text)?
                    .ok_or_else(|| {
                        format!("No Cloudflare DNS provider in Temps manages the zone of {host}.")
                    })?
                    .0
            }
        };
        let credentials = self
            .deps
            .dns_providers
            .cloudflare_credentials(&provider)
            .map_err(text)?;
        Ok((provider.id, provider.name, credentials.api_token))
    }

    async fn provider_token(&self, provider_id: i32) -> Result<String, String> {
        let provider = self
            .deps
            .dns_providers
            .get(provider_id)
            .await
            .map_err(text)?;
        self.deps
            .dns_providers
            .cloudflare_credentials(&provider)
            .map(|c| c.api_token)
            .map_err(text)
    }

    async fn edge_rule_ref(&self, run_id: i32, purpose: EdgeRulePurpose) -> Result<String, String> {
        let install = self.deps.records.instance_id().await.map_err(text)?;
        Ok(edge_rules::rule_ref(&install, run_id, purpose))
    }
}
