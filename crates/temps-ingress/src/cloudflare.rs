// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cloudflare Tunnel as a [`TunnelProvider`] (ADR 058).
//!
//! The tunnel is remotely managed (`config_src: cloudflare`): its routing
//! lives at Cloudflare, so nodes run `cloudflared tunnel run` with only the
//! connector token. One catch-all rule sends every hostname to the node's
//! tunnel origin listener, which routes by Host from the route snapshot, so
//! publishing a hostname is a DNS record and nothing else.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use async_trait::async_trait;
use base64::Engine as _;
use rand::Rng as _;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use temps_dns::CloudflareCredentials;
use tracing::{info, warn};

use crate::error::IngressError;
use crate::provider::{
    DnsIntent, TunnelContext, TunnelHandle, TunnelHealth, TunnelLink, TunnelProvider,
};
use crate::wire::{
    Artifact, ContainerSpec, DriverChoice, NodeConnectorSpec, ProcessSpec, Readiness,
};

pub const KIND: &str = "cloudflare_tunnel";
const API: &str = "https://api.cloudflare.com/client/v4";

/// cloudflared pinned by release and checksum. `TUNNEL_TOKEN_FILE` needs
/// 2025.4.0 or later.
const CLOUDFLARED_VERSION: &str = "2026.10.1";
const CLOUDFLARED_AMD64_SHA256: &str =
    "3bfc539f45d2620c71debb7a86be0efa737ed5ccbad9eb85c787116900e5c586";
const CLOUDFLARED_ARM64_SHA256: &str =
    "2c65dfabf616c40efc7545d107d7a57ad5e21377e2964b7e88af19a238074a46";
const CLOUDFLARED_IMAGE: &str = "cloudflare/cloudflared:2026.10.1@sha256:a15a3b50b8dabd57ac2090e9484c0247dba9dfa1f0df4d298d4317f9d69eb2d5";

pub struct CloudflareTunnelProvider {
    http: reqwest::Client,
    credentials: CloudflareCredentials,
    /// The DNS provider's name, for messages that tell the operator which
    /// token to fix.
    credential_label: String,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    success: bool,
    #[serde(default)]
    errors: Vec<ApiMessage>,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct Zone {
    account: ZoneAccount,
}

#[derive(Debug, Deserialize)]
struct ZoneAccount {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Tunnel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
    #[serde(default)]
    connections: Vec<TunnelConnection>,
}

/// An entry of a tunnel's `connections`. The origin IP is left out on
/// purpose: the console has no use for it.
#[derive(Debug, Deserialize)]
struct TunnelConnection {
    #[serde(default)]
    colo_name: Option<String>,
    #[serde(default)]
    opened_at: Option<String>,
    #[serde(default)]
    is_pending_reconnect: bool,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_version: Option<String>,
}

impl CloudflareTunnelProvider {
    pub fn new(
        http: reqwest::Client,
        credentials: CloudflareCredentials,
        credential_label: String,
    ) -> Self {
        Self {
            http,
            credentials,
            credential_label,
        }
    }

    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        operation: &str,
    ) -> Result<Option<T>, IngressError> {
        let mut request = self
            .http
            .request(method, format!("{API}{path}"))
            .bearer_auth(&self.credentials.api_token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            IngressError::Provider(format!(
                "Could not reach Cloudflare to {operation}: {error}. The control plane needs outbound HTTPS to api.cloudflare.com."
            ))
        })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let envelope: Envelope<T> = serde_json::from_str(&text).map_err(|error| {
            IngressError::Provider(format!(
                "Cloudflare answered {status} to {operation} with a body Temps could not read ({error})"
            ))
        })?;
        if envelope.success {
            return Ok(envelope.result);
        }
        let detail = envelope
            .errors
            .iter()
            .map(|e| format!("{} (code {})", e.message, e.code))
            .collect::<Vec<_>>()
            .join("; ");
        let denied = status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::UNAUTHORIZED
            || envelope
                .errors
                .iter()
                .any(|e| matches!(e.code, 10000 | 9109 | 10001));
        if denied {
            return Err(IngressError::Provider(format!(
                "Cloudflare refused the API token of DNS provider '{}' to {operation}: {detail}. \
                 Edit that token in Cloudflare (My Profile → API Tokens) so it has \
                 'Account · Cloudflare Tunnel · Edit' and 'Zone · DNS · Edit', then retry.",
                self.credential_label
            )));
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Err(IngressError::Provider(format!(
            "Cloudflare refused to {operation} ({status}): {detail}"
        )))
    }

    async fn account_id(&self, domain: &str) -> Result<String, IngressError> {
        if let Some(account) = self
            .credentials
            .account_id
            .as_ref()
            .filter(|a| !a.is_empty())
        {
            return Ok(account.clone());
        }
        // The tunnel may sit on a subdomain: walk up to the zone.
        let labels: Vec<&str> = domain.split('.').collect();
        for start in 0..labels.len().saturating_sub(1) {
            let candidate = labels[start..].join(".");
            let zones: Vec<Zone> = self
                .call(
                    reqwest::Method::GET,
                    &format!("/zones?name={candidate}"),
                    None,
                    &format!("look up the zone {candidate}"),
                )
                .await?
                .unwrap_or_default();
            if let Some(zone) = zones.into_iter().next() {
                return Ok(zone.account.id);
            }
        }
        Err(IngressError::Provider(format!(
            "The API token of DNS provider '{}' cannot see the zone of {domain}. \
             Give it access to that zone, or set the account id on the DNS provider.",
            self.credential_label
        )))
    }

    fn account_of(handle: &TunnelHandle) -> Result<&str, IngressError> {
        handle
            .settings
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                IngressError::Provider(
                    "This connector has no Cloudflare account recorded; delete and recreate it."
                        .into(),
                )
            })
    }

    async fn get_tunnel(&self, account: &str, id: &str) -> Result<Option<Tunnel>, IngressError> {
        let tunnel: Option<Tunnel> = self
            .call(
                reqwest::Method::GET,
                &format!("/accounts/{account}/cfd_tunnel/{id}"),
                None,
                "read the tunnel",
            )
            .await?;
        Ok(tunnel.filter(|t| t.deleted_at.is_none()))
    }

    async fn find_by_name(
        &self,
        account: &str,
        name: &str,
    ) -> Result<Option<Tunnel>, IngressError> {
        let tunnels: Vec<Tunnel> = self
            .call(
                reqwest::Method::GET,
                &format!("/accounts/{account}/cfd_tunnel?name={name}&is_deleted=false"),
                None,
                "list tunnels",
            )
            .await?
            .unwrap_or_default();
        Ok(tunnels.into_iter().find(|t| t.deleted_at.is_none()))
    }

    async fn token(&self, account: &str, id: &str) -> Result<String, IngressError> {
        self.call::<String>(
            reqwest::Method::GET,
            &format!("/accounts/{account}/cfd_tunnel/{id}/token"),
            None,
            "read the tunnel's connector token",
        )
        .await?
        .ok_or_else(|| IngressError::Provider("Cloudflare returned no connector token".into()))
    }

    /// Delete a tunnel [`TunnelProvider::ensure`] created moments ago when
    /// setting it up then failed. It has never run a connector, so deleting
    /// it cuts nothing. Returns the error to report: the setup failure, or
    /// both failures with the tunnel to remove by hand.
    async fn discard_new_tunnel(
        &self,
        account: &str,
        id: &str,
        name: &str,
        cause: IngressError,
    ) -> IngressError {
        match self
            .call::<Value>(
                reqwest::Method::DELETE,
                &format!("/accounts/{account}/cfd_tunnel/{id}"),
                None,
                "delete the tunnel whose setup failed",
            )
            .await
        {
            Ok(_) => {
                warn!(tunnel = %id, "deleted the new Cloudflare tunnel after its setup failed; the next sync creates it again");
                cause
            }
            Err(cleanup) => IngressError::Provider(format!(
                "{cause} The new tunnel '{name}' ({id}) could not be removed afterwards ({cleanup}), \
                 so the next sync will refuse it by name. Delete it in Cloudflare \
                 (Zero Trust → Networks → Tunnels), then sync."
            )),
        }
    }

    async fn configure(&self, account: &str, id: &str, edge_port: u16) -> Result<(), IngressError> {
        // One catch-all rule: the node's listener routes by Host.
        let body = json!({
            "config": {
                "ingress": [ { "service": format!("http://127.0.0.1:{edge_port}") } ]
            }
        });
        self.call::<Value>(
            reqwest::Method::PUT,
            &format!("/accounts/{account}/cfd_tunnel/{id}/configurations"),
            Some(body),
            "set the tunnel's routing",
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl TunnelProvider for CloudflareTunnelProvider {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn ensure(&self, ctx: &TunnelContext<'_>) -> Result<TunnelHandle, IngressError> {
        let account = match ctx
            .existing
            .as_ref()
            .and_then(|h| h.settings.get("account_id").and_then(Value::as_str))
        {
            Some(account) => account.to_string(),
            None => self.account_id(ctx.domain).await?,
        };
        let tunnel_name = tunnel_name_for(ctx.name, ctx.instance_id);

        // Only the tunnel this connector created is ever reused, by its id.
        // Adopting one by name could take over another install's tunnel in
        // the same account: its routing would be overwritten and its
        // traffic split onto these workers.
        let mut tunnel = None;
        if let Some(existing) = &ctx.existing {
            tunnel = self.get_tunnel(&account, &existing.external_id).await?;
            if tunnel.is_none() {
                warn!(connector_id = ctx.connector_id, tunnel = %existing.external_id,
                    "Cloudflare tunnel is gone; creating it again");
            }
        }
        let (tunnel, created_now) = match tunnel {
            Some(tunnel) => (tunnel, false),
            None => {
                if self.find_by_name(&account, &tunnel_name).await?.is_some() {
                    return Err(IngressError::Provider(format!(
                        "A tunnel named '{tunnel_name}' already exists in this Cloudflare account and is not this connector's. \
                         Rename the connector, or delete that tunnel in Cloudflare (Zero Trust → Networks → Tunnels) if nothing uses it, then sync."
                    )));
                }
                let created: Tunnel = self
                    .call(
                        reqwest::Method::POST,
                        &format!("/accounts/{account}/cfd_tunnel"),
                        Some(json!({ "name": tunnel_name, "config_src": "cloudflare" })),
                        "create the tunnel",
                    )
                    .await?
                    .ok_or_else(|| IngressError::Provider("Cloudflare created no tunnel".into()))?;
                info!(connector_id = ctx.connector_id, tunnel = %created.id, "created Cloudflare tunnel");
                (created, true)
            }
        };
        let finished = match self.configure(&account, &tunnel.id, ctx.edge_port).await {
            Ok(()) => self.token(&account, &tunnel.id).await,
            Err(error) => Err(error),
        };
        let secret = match finished {
            Ok(secret) => secret,
            // The caller saves the tunnel id only from a returned handle, so
            // a tunnel created here and left behind would be refused by name
            // on every later sync. Remove it so the next sync starts clean.
            Err(error) if created_now => {
                return Err(self
                    .discard_new_tunnel(&account, &tunnel.id, &tunnel_name, error)
                    .await)
            }
            Err(error) => return Err(error),
        };
        Ok(TunnelHandle {
            external_id: tunnel.id.clone(),
            settings: json!({
                "account_id": account,
                "tunnel_name": tunnel.name.clone().unwrap_or(tunnel_name),
                "cname_target": format!("{}.cfargotunnel.com", tunnel.id),
            }),
            secret,
        })
    }

    fn connector_spec(
        &self,
        connector_id: i32,
        handle: &TunnelHandle,
        edge_port: u16,
    ) -> NodeConnectorSpec {
        let args: Vec<String> = [
            "tunnel",
            "--no-autoupdate",
            "--metrics",
            "{metrics_addr}",
            "--grace-period",
            "30s",
            "run",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let env = BTreeMap::from([(
            "TUNNEL_TOKEN_FILE".to_string(),
            "{secret_file:token}".to_string(),
        )]);
        let readiness = Readiness::HttpReady {
            url: "http://{metrics_addr}/ready".into(),
        };
        let release = format!(
            "https://github.com/cloudflare/cloudflared/releases/download/{CLOUDFLARED_VERSION}"
        );
        let drivers = vec![
            DriverChoice::Process(ProcessSpec {
                artifacts: vec![
                    Artifact {
                        os: "linux".into(),
                        arch: "x86_64".into(),
                        url: format!("{release}/cloudflared-linux-amd64"),
                        sha256: CLOUDFLARED_AMD64_SHA256.into(),
                    },
                    Artifact {
                        os: "linux".into(),
                        arch: "aarch64".into(),
                        url: format!("{release}/cloudflared-linux-arm64"),
                        sha256: CLOUDFLARED_ARM64_SHA256.into(),
                    },
                ],
                args: args.clone(),
                env: env.clone(),
                readiness: readiness.clone(),
                stop_grace_secs: 35,
            }),
            DriverChoice::Container(ContainerSpec {
                image: CLOUDFLARED_IMAGE.into(),
                args,
                env,
                readiness,
                stop_grace_secs: 35,
            }),
        ];
        let secrets = BTreeMap::from([("token".to_string(), handle.secret.clone())]);
        let mut spec = NodeConnectorSpec {
            id: connector_id,
            provider: KIND.into(),
            revision: String::new(),
            edge_port,
            client_address_header: Some("CF-Connecting-IP".into()),
            drivers,
            secrets,
        };
        spec.revision = revision_of(&spec);
        spec
    }

    async fn publish(&self, handle: &TunnelHandle, _host: &str) -> Result<DnsIntent, IngressError> {
        Ok(DnsIntent::Cname {
            target: format!("{}.cfargotunnel.com", handle.external_id),
            proxied: true,
        })
    }

    async fn unpublish(&self, _handle: &TunnelHandle, _host: &str) -> Result<(), IngressError> {
        // The catch-all rule stays; removing the DNS record is enough.
        Ok(())
    }

    async fn health(&self, handle: &TunnelHandle) -> Result<TunnelHealth, IngressError> {
        let account = Self::account_of(handle)?;
        let tunnel = self
            .get_tunnel(account, &handle.external_id)
            .await?
            .ok_or_else(|| {
                IngressError::Provider(
                    "The tunnel was deleted in Cloudflare. Sync the connector to recreate it."
                        .into(),
                )
            })?;
        Ok(TunnelHealth {
            status: tunnel.status.unwrap_or_else(|| "unknown".into()),
            links: tunnel
                .connections
                .into_iter()
                .map(|c| TunnelLink {
                    location: c.colo_name,
                    opened_at: c.opened_at,
                    reconnecting: c.is_pending_reconnect,
                    connector_ref: c.client_id,
                    client_version: c.client_version,
                })
                .collect(),
        })
    }

    async fn rotate_secret(
        &self,
        handle: &TunnelHandle,
        keep_clients: &HashSet<String>,
    ) -> Result<Option<String>, IngressError> {
        let account = Self::account_of(handle)?;
        let id = &handle.external_id;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let tunnel_secret = base64::engine::general_purpose::STANDARD.encode(bytes);
        let rotated: Option<Value> = self
            .call(
                reqwest::Method::PATCH,
                &format!("/accounts/{account}/cfd_tunnel/{id}"),
                Some(json!({ "tunnel_secret": tunnel_secret })),
                "rotate the tunnel's secret",
            )
            .await?;
        if rotated.is_none() {
            return Err(IngressError::Provider(
                "The tunnel was deleted in Cloudflare. Sync the connector to recreate it.".into(),
            ));
        }
        let token = self.token(account, id).await?;
        info!(tunnel = %id, "rotated Cloudflare tunnel secret");

        // A connection already open survives the new secret; cut the ones
        // no remaining node holds so a removed node stops serving now.
        let live: BTreeSet<String> = self
            .get_tunnel(account, id)
            .await?
            .map(|t| {
                t.connections
                    .into_iter()
                    .filter_map(|c| c.client_id)
                    .collect()
            })
            .unwrap_or_default();
        let cut: Vec<&String> = live.iter().filter(|c| !keep_clients.contains(*c)).collect();
        if cut.iter().any(|client| !is_client_id(client)) {
            // A client that cannot be named in a URL cannot be cut alone;
            // cut everything, and the staying nodes reconnect with the new
            // secret.
            self.call::<Value>(
                reqwest::Method::DELETE,
                &format!("/accounts/{account}/cfd_tunnel/{id}/connections"),
                None,
                "disconnect the tunnel's connectors",
            )
            .await?;
            info!(tunnel = %id, "disconnected every connector of the tunnel");
            return Ok(Some(token));
        }
        for client in cut {
            self.call::<Value>(
                reqwest::Method::DELETE,
                &format!("/accounts/{account}/cfd_tunnel/{id}/connections?client_id={client}"),
                None,
                "disconnect a removed node's connector",
            )
            .await?;
            info!(tunnel = %id, client = %client, "disconnected a connector no node runs");
        }
        Ok(Some(token))
    }

    async fn delete(&self, handle: &TunnelHandle) -> Result<(), IngressError> {
        let account = Self::account_of(handle)?;
        let id = &handle.external_id;
        // Active connectors block deletion; clean them up first.
        let _ = self
            .call::<Value>(
                reqwest::Method::DELETE,
                &format!("/accounts/{account}/cfd_tunnel/{id}/connections"),
                None,
                "disconnect the tunnel's connectors",
            )
            .await?;
        let _ = self
            .call::<Value>(
                reqwest::Method::DELETE,
                &format!("/accounts/{account}/cfd_tunnel/{id}"),
                None,
                "delete the tunnel",
            )
            .await?;
        Ok(())
    }
}

/// `temps-<name>-<install>`: the install part keeps two installs that share
/// a Cloudflare account, and pick the same connector name, apart.
fn tunnel_name_for(name: &str, instance_id: &str) -> String {
    let install: String = instance_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if install.is_empty() {
        format!("temps-{name}")
    } else {
        format!("temps-{name}-{install}")
    }
}

/// A Cloudflare connector id (a UUID), safe to put in a query string.
fn is_client_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// A digest of everything the node runs, so it restarts exactly when that
/// changes.
pub(crate) fn revision_of(spec: &NodeConnectorSpec) -> String {
    let mut hasher = Sha256::new();
    hasher.update(spec.provider.as_bytes());
    hasher.update(spec.edge_port.to_be_bytes());
    hasher.update(serde_json::to_vec(&spec.drivers).unwrap_or_default());
    hasher.update(serde_json::to_vec(&spec.secrets).unwrap_or_default());
    hasher.update(
        spec.client_address_header
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    hex::encode(&hasher.finalize()[..12])
}

/// Registers Cloudflare Tunnel with the provider registry (ADR 058 D8).
pub struct CloudflareTunnelFactory;

/// Field keys of the setup form.
const FIELD_ACCOUNT: &str = "dns_provider_id";
const FIELD_DOMAIN: &str = "domain";

#[async_trait]
impl crate::registry::ProviderFactory for CloudflareTunnelFactory {
    fn info(&self) -> crate::registry::ProviderInfo {
        use crate::registry::{DnsHandling, FieldKind, ProviderInfo, SetupField};
        ProviderInfo {
            kind: KIND.to_string(),
            name: "Cloudflare Tunnel".to_string(),
            summary: "The worker connects out to Cloudflare, and visitors reach your apps through Cloudflare's network. No public IP, open port or port forward.".to_string(),
            requirements: vec![
                "A domain on Cloudflare (the free plan works)".to_string(),
                "That domain added in Temps under a Cloudflare DNS provider whose API token has 'Account · Cloudflare Tunnel · Edit' and 'Zone · DNS · Edit'".to_string(),
            ],
            needs_public_ip: false,
            works_behind_cgnat: true,
            dns: DnsHandling::Automatic,
            fields: vec![
                SetupField {
                    key: FIELD_ACCOUNT.to_string(),
                    label: "Cloudflare account".to_string(),
                    help: Some("The Cloudflare DNS provider whose token creates the tunnel and its DNS records.".to_string()),
                    kind: FieldKind::DnsProvider { provider_type: Some("cloudflare".to_string()) },
                    required: true,
                    placeholder: None,
                },
                SetupField {
                    key: FIELD_DOMAIN.to_string(),
                    label: "Domain".to_string(),
                    help: Some("Apps are published as hostnames under it. A subdomain such as lab.example.com keeps the tunnel to that part of the domain.".to_string()),
                    kind: FieldKind::ManagedDomain {
                        provider_field: FIELD_ACCOUNT.to_string(),
                        allow_subdomain: true,
                    },
                    required: true,
                    placeholder: None,
                },
            ],
            docs_url: Some("https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/".to_string()),
        }
    }

    async fn validate(
        &self,
        deps: &crate::registry::ProviderDeps<'_>,
        setup: &crate::registry::ProviderSetup,
    ) -> Result<(), IngressError> {
        let provider = setup.dns_provider.as_ref().ok_or_else(|| {
            IngressError::Validation(
                "Choose the Cloudflare account to create the tunnel in.".into(),
            )
        })?;
        // A specific message unless it is an active Cloudflare provider.
        deps.dns_providers.cloudflare_credentials(provider)?;
        Ok(())
    }

    async fn build(
        &self,
        deps: &crate::registry::ProviderDeps<'_>,
        connector: &temps_entities::ingress_connectors::Model,
        _secrets: &BTreeMap<String, String>,
    ) -> Result<Box<dyn TunnelProvider>, IngressError> {
        let provider_id = connector.dns_provider_id.ok_or_else(|| {
            IngressError::Validation(format!(
                "Connector '{}' lost its DNS provider. Delete it and create it again with a Cloudflare DNS provider.",
                connector.name
            ))
        })?;
        let provider = deps.dns_providers.get(provider_id).await?;
        let credentials = deps.dns_providers.cloudflare_credentials(&provider)?;
        Ok(Box::new(CloudflareTunnelProvider::new(
            deps.http.clone(),
            credentials,
            provider.name,
        )))
    }

    fn spec_provider(&self, http: &reqwest::Client) -> Box<dyn TunnelProvider> {
        Box::new(CloudflareTunnelProvider::new(
            http.clone(),
            CloudflareCredentials {
                api_token: String::new(),
                account_id: None,
            },
            String::new(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_names_carry_the_install() {
        assert_eq!(
            tunnel_name_for("default", "4CEEBD3C-7552-4d41-9249-9e881d0ba6af"),
            "temps-default-4ceebd3c"
        );
        assert_eq!(tunnel_name_for("default", ""), "temps-default");
    }

    #[test]
    fn only_uuid_like_client_ids_reach_a_url() {
        assert!(is_client_id("1bedc50d-42b3-473c-b108-ff3d10c0d925"));
        assert!(!is_client_id("x&client_id=other"));
        assert!(!is_client_id(""));
    }
}
