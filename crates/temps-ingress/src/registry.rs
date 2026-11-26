// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress providers register here and describe themselves (ADR 058 D8).
//!
//! The console renders a provider's setup form, its recommendation and its
//! status from [`ProviderInfo`], and the service builds providers through a
//! [`ProviderFactory`], so adding one is a factory plus its
//! [`TunnelProvider`]: no console code and no `match` on the kind.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use temps_dns::DnsProviderService;
use temps_entities::{dns_providers, ingress_connectors};
use utoipa::ToSchema;

use crate::error::IngressError;
use crate::provider::{Capabilities, Reach, TunnelProvider};
use crate::reachability::run::ProviderRun;
use crate::reachability::service::LiveOps;

/// What a provider is, for the console.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderInfo {
    /// Stored on the connector, e.g. `cloudflare_tunnel`.
    pub kind: String,
    pub name: String,
    pub summary: String,
    /// What the operator needs before starting, one line each.
    pub requirements: Vec<String>,
    /// Whether the worker needs a public address. Tunnels do not.
    pub needs_public_ip: bool,
    /// Whether it works when the ISP puts the worker behind carrier-grade NAT.
    pub works_behind_cgnat: bool,
    pub dns: DnsHandling,
    pub fields: Vec<SetupField>,
    pub docs_url: Option<String>,
}

/// Who makes the published hostnames resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DnsHandling {
    /// Temps writes the records through the domain's DNS provider.
    Automatic,
    /// Temps shows the records and the operator adds them.
    Manual,
    /// The provider hands out its own hostname; there is nothing to add.
    ProviderHostname,
}

/// One question in a provider's setup form.
#[derive(Debug, Clone, Serialize, serde::Deserialize, ToSchema)]
pub struct SetupField {
    /// The key in `values` of the create request.
    pub key: String,
    pub label: String,
    pub help: Option<String>,
    pub kind: FieldKind,
    pub required: bool,
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    /// Stored encrypted, never returned by the API.
    Secret,
    /// One of the cluster's DNS providers, of `provider_type` when set.
    DnsProvider {
        provider_type: Option<String>,
    },
    /// A domain managed by the DNS provider chosen in the field
    /// `provider_field`, or with `allow_subdomain` a name under one.
    ManagedDomain {
        provider_field: String,
        #[serde(default)]
        allow_subdomain: bool,
    },
    /// Teams of this instance, answered as a list of team ids.
    Teams,
    /// Users of this instance, answered as a list of user ids.
    Users,
    /// One of `choices`, answered as its `value`.
    Choice {
        choices: Vec<FieldChoice>,
    },
    /// A list of text values, answered as an array of strings.
    TextList,
}

/// One answer a [`FieldKind::Choice`] field offers.
#[derive(Debug, Clone, Serialize, serde::Deserialize, ToSchema)]
pub struct FieldChoice {
    pub value: String,
    pub label: String,
    pub help: Option<String>,
}

/// A create request's answers, checked against the fields and resolved.
pub struct ProviderSetup {
    pub dns_provider: Option<dns_providers::Model>,
    pub domain: Option<String>,
    /// `text` answers by key.
    pub settings: BTreeMap<String, String>,
    /// `secret` answers by key, in clear: only for [`ProviderFactory::validate`].
    pub secrets: BTreeMap<String, String>,
}

/// What a factory may use to check answers and build its provider.
pub struct ProviderDeps<'a> {
    pub http: &'a reqwest::Client,
    pub dns_providers: &'a DnsProviderService,
}

#[async_trait]
pub trait ProviderFactory: Send + Sync {
    fn info(&self) -> ProviderInfo;

    /// What this provider's way in is like (ADR-116 D2): for `connector`
    /// when there is one, else for a way in that needs no connector row
    /// (a Tailscale machine name).
    fn capabilities(&self, connector: Option<&ingress_connectors::Model>) -> Capabilities;

    /// Provider-specific checks on answers the service already checked
    /// against the fields. Errors say what to change.
    async fn validate(
        &self,
        deps: &ProviderDeps<'_>,
        setup: &ProviderSetup,
    ) -> Result<(), IngressError>;

    /// The tunnel side, for a provider whose connector rows are tunnels.
    /// `None` for one whose rows are credentials (ADR 065 D2's
    /// `tailscale`): described and checked like a tunnel provider, stored
    /// on a connector row with its secrets sealed, but never synced,
    /// published through or run on a node.
    fn tunnels(&self) -> Option<&dyn TunnelFactory> {
        None
    }

    /// Its part of planning, for a provider that plans audiences whole
    /// (ADR-116 D3).
    fn plans(&self) -> Option<&dyn crate::reachability::plan::ProviderPlan> {
        None
    }

    /// This provider's part of a reachability run, over the live effects
    /// (ADR-116 D3). `None` for a provider with no steps of its own.
    fn run<'a>(&self, _live: &'a LiveOps) -> Option<Box<dyn ProviderRun + 'a>> {
        None
    }
}

/// How a provider whose connectors are tunnels builds them.
#[async_trait]
pub trait TunnelFactory: Send + Sync {
    /// The provider for a stored connector, with its credentials.
    /// `secrets` are the connector's `secret` answers, decrypted.
    async fn build(
        &self,
        deps: &ProviderDeps<'_>,
        connector: &ingress_connectors::Model,
        secrets: &BTreeMap<String, String>,
    ) -> Result<Box<dyn TunnelProvider>, IngressError>;

    /// A provider good enough to build node specs, which need no credential.
    fn spec_provider(&self, http: &reqwest::Client) -> Box<dyn TunnelProvider>;
}

/// The providers this control plane offers.
#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn ProviderFactory>>,
}

impl ProviderRegistry {
    /// Every provider that ships with Temps.
    pub fn builtin() -> Self {
        Self {
            providers: crate::providers::builtin(),
        }
    }

    /// Exactly these providers, for tests.
    #[cfg(test)]
    pub(crate) fn of(providers: Vec<Arc<dyn ProviderFactory>>) -> Self {
        Self { providers }
    }

    fn find(&self, kind: &str) -> Option<&Arc<dyn ProviderFactory>> {
        self.providers.iter().find(|f| f.info().kind == kind)
    }

    fn kinds(&self, tunnels: bool) -> Vec<String> {
        self.providers
            .iter()
            .filter(|f| f.tunnels().is_some() == tunnels)
            .map(|f| f.info().kind)
            .collect()
    }

    /// Whether connector rows of `kind` are credentials, which every tunnel
    /// path skips. An unknown kind is not.
    pub fn is_credential(&self, kind: &str) -> bool {
        self.find(kind).is_some_and(|f| f.tunnels().is_none())
    }

    /// The kinds whose connector rows are credentials.
    pub fn credential_kinds(&self) -> Vec<String> {
        self.kinds(false)
    }

    /// Who can reach what `kind` serves, when the kind is registered.
    pub fn reach(&self, kind: &str) -> Option<Reach> {
        self.find(kind).map(|f| f.capabilities(None).reach)
    }

    /// The provider that plans `audience` whole, the first registered.
    pub fn planner(
        &self,
        audience: crate::reachability::model::Audience,
    ) -> Option<&dyn crate::reachability::plan::ProviderPlan> {
        self.providers
            .iter()
            .filter_map(|f| f.plans())
            .find(|p| p.plans(audience))
    }

    /// Whether Temps writes the DNS of what `connector` publishes, through
    /// whichever DNS provider manages the zone. An unknown kind does not.
    pub fn writes_dns(&self, connector: &ingress_connectors::Model) -> bool {
        self.find(&connector.provider)
            .is_some_and(|f| f.capabilities(Some(connector)).dns == DnsHandling::Automatic)
    }

    /// The credential provider of `kind`.
    pub fn credential(&self, kind: &str) -> Result<Arc<dyn ProviderFactory>, IngressError> {
        self.find(kind)
            .filter(|f| f.tunnels().is_none())
            .cloned()
            .ok_or_else(|| {
                IngressError::Validation(format!(
                    "Unknown credential provider '{kind}'. This control plane offers: {}.",
                    self.kinds(false).join(", ")
                ))
            })
    }

    /// The credential providers, for the console.
    pub fn credential_infos(&self) -> Vec<ProviderInfo> {
        self.providers
            .iter()
            .filter(|f| f.tunnels().is_none())
            .map(|f| f.info())
            .collect()
    }

    /// The tunnel provider of `kind`.
    pub fn get(&self, kind: &str) -> Result<Arc<dyn ProviderFactory>, IngressError> {
        if self.is_credential(kind) {
            return Err(IngressError::Validation(format!(
                "'{kind}' is a credential, not a tunnel: Temps adds it while making a private \
                 app reachable."
            )));
        }
        self.find(kind).cloned().ok_or_else(|| {
            IngressError::Validation(format!(
                "Unknown ingress provider '{kind}'. This control plane offers: {}.",
                self.kinds(true).join(", ")
            ))
        })
    }

    /// The tunnel providers, for the console.
    pub fn infos(&self) -> Vec<ProviderInfo> {
        self.providers
            .iter()
            .filter(|f| f.tunnels().is_some())
            .map(|f| f.info())
            .collect()
    }

    /// A tunnel of `kind` for a stored connector.
    pub async fn build(
        &self,
        kind: &str,
        deps: &ProviderDeps<'_>,
        connector: &ingress_connectors::Model,
        secrets: &BTreeMap<String, String>,
    ) -> Result<Box<dyn TunnelProvider>, IngressError> {
        let factory = self.get(kind)?;
        let tunnels = factory
            .tunnels()
            .ok_or_else(|| IngressError::Validation(format!("'{kind}' builds no tunnels.")))?;
        tunnels.build(deps, connector, secrets).await
    }

    /// Every provider's part of a reachability run, over the live effects.
    pub fn runs<'a>(&self, live: &'a LiveOps) -> Vec<Box<dyn ProviderRun + 'a>> {
        self.providers.iter().filter_map(|f| f.run(live)).collect()
    }

    /// A credential-free tunnel of `kind`, for node specs.
    pub fn spec_provider(
        &self,
        kind: &str,
        http: &reqwest::Client,
    ) -> Option<Box<dyn TunnelProvider>> {
        let factory = self.get(kind).ok()?;
        factory.tunnels().map(|t| t.spec_provider(http))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Origin;

    #[test]
    fn credentials_and_tunnels_are_told_apart_by_the_provider_not_its_name() {
        let registry = ProviderRegistry::builtin();
        assert!(registry.is_credential(crate::providers::tailscale::KIND));
        assert!(!registry.is_credential(crate::providers::cloudflare::KIND));
        assert!(!registry.is_credential("not_a_provider"));
        assert_eq!(
            registry.credential_kinds(),
            vec![crate::providers::tailscale::KIND]
        );
        let tunnels: Vec<String> = registry.infos().into_iter().map(|i| i.kind).collect();
        assert_eq!(tunnels, vec![crate::providers::cloudflare::KIND]);
        let error = registry
            .get(crate::providers::tailscale::KIND)
            .err()
            .expect("refused");
        assert!(
            error.to_string().contains("is a credential, not a tunnel"),
            "{error}"
        );
        let error = registry
            .credential(crate::providers::cloudflare::KIND)
            .err()
            .expect("refused");
        assert!(
            error.to_string().contains("Unknown credential provider"),
            "{error}"
        );
    }

    #[test]
    fn every_provider_says_the_same_about_dns_in_its_info_and_capabilities() {
        let registry = ProviderRegistry::builtin();
        for factory in &registry.providers {
            let info = factory.info();
            assert_eq!(factory.capabilities(None).dns, info.dns, "{}", info.kind);
        }
    }

    /// The headers nodes are told to trust are the ones the provider
    /// declares for its origin (ADR-116 D4).
    #[test]
    fn a_tunnel_spec_trusts_exactly_what_its_origin_declares() {
        let registry = ProviderRegistry::builtin();
        let http = reqwest::Client::new();
        for kind in registry.kinds(true) {
            let factory = registry.get(&kind).expect("registered");
            let provider = registry
                .spec_provider(&kind, &http)
                .expect("a spec provider");
            let handle = crate::provider::TunnelHandle {
                external_id: "t-1".into(),
                settings: serde_json::json!({}),
                secret: "s".into(),
            };
            let spec = provider.connector_spec(1, &handle, 8443);
            match factory.capabilities(None).origin {
                Origin::LoopbackHttp {
                    client_address_header,
                    ja4,
                } => {
                    assert_eq!(
                        spec.client_address_header.as_deref(),
                        Some(client_address_header)
                    );
                    assert_eq!(
                        spec.client_ja4
                            .as_ref()
                            .map(|j| (j.header.as_str(), j.source.as_str())),
                        ja4.map(|j| (j.header, j.source))
                    );
                }
                Origin::ServeSocket { .. } => {
                    panic!("{kind}: a tunnel's connector reaches the loopback origin")
                }
            }
        }
    }
}
