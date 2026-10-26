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
use crate::provider::TunnelProvider;

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
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
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

    /// Provider-specific checks on answers the service already checked
    /// against the fields. Errors say what to change.
    async fn validate(
        &self,
        deps: &ProviderDeps<'_>,
        setup: &ProviderSetup,
    ) -> Result<(), IngressError>;

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
    factories: Vec<Arc<dyn ProviderFactory>>,
}

impl ProviderRegistry {
    /// Every provider that ships with Temps.
    pub fn builtin() -> Self {
        Self {
            factories: vec![Arc::new(crate::cloudflare::CloudflareTunnelFactory)],
        }
    }

    /// Exactly these providers, for tests.
    #[cfg(test)]
    pub(crate) fn of(factories: Vec<Arc<dyn ProviderFactory>>) -> Self {
        Self { factories }
    }

    pub fn get(&self, kind: &str) -> Result<Arc<dyn ProviderFactory>, IngressError> {
        self.factories
            .iter()
            .find(|f| f.info().kind == kind)
            .cloned()
            .ok_or_else(|| {
                let known: Vec<String> = self.factories.iter().map(|f| f.info().kind).collect();
                IngressError::Validation(format!(
                    "Unknown ingress provider '{kind}'. This control plane offers: {}.",
                    known.join(", ")
                ))
            })
    }

    pub fn infos(&self) -> Vec<ProviderInfo> {
        self.factories.iter().map(|f| f.info()).collect()
    }
}
