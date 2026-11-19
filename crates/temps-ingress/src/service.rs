// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress connectors: create the tunnel, hand nodes what to run, and keep
//! one DNS record per served hostname pointing at it (ADR 058).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_core::secret_string::SecretString;
use temps_core::tunnel_origin::TUNNEL_ORIGIN_PORT;
use temps_core::{DBDateTime, EncryptionService, CONTROL_PLANE_NODE_ID};
use temps_dns::{
    DnsProviderService, DnsRecordContent, DnsRecordRequest, DnsRecordType, ManagedDnsRecordService,
    OwnershipScope,
};
use temps_entities::{
    environment_domains, ingress_connector_nodes, ingress_connectors, ingress_hostname_choices,
    ingress_publications, node_public_ingress, nodes,
};
use temps_routes::CachedPeerTable;
use tracing::{debug, info, warn};
use utoipa::ToSchema;

use crate::error::{ControlPlaneTunnelsUnavailable, IngressError};
use crate::provider::{DnsIntent, TunnelContext, TunnelHandle, TunnelProvider};
use crate::registry::{FieldKind, ProviderDeps, ProviderInfo, ProviderRegistry, ProviderSetup};
use crate::wire::{NodeConnectorSpec, NodeConnectorStatusReport, NodeConnectorsResponse};

/// The DNS controller name stamped on the records this service owns.
const DNS_CONTROLLER: &str = "ingress-connector";
/// Every connector's tunnel origin port, on workers and on the control
/// plane alike (ADR-107 D3).
const DEFAULT_EDGE_PORT: i32 = TUNNEL_ORIGIN_PORT as i32;
/// Keys in `settings` that hold the setup answers. Everything else there
/// belongs to the provider, which rewrites it on every `ensure`.
const SETUP_KEY: &str = "setup";
const SETUP_SECRETS_KEY: &str = "setup_secrets";
/// Every node handed the current tunnel secret since it was last rotated.
/// When one of them stops running the tunnel the secret is rotated, so it
/// cannot keep serving the tunnel's traffic.
const TOKEN_HOLDERS_KEY: &str = "token_holders";
/// How `last_error` starts when rotating the secret failed, so a later
/// success clears only that error.
const ROTATION_ERROR: &str = "Could not revoke the tunnel secret";
/// A publication whose DNS record still points at a tunnel that was since
/// replaced: it is published again, or its record removed.
const STALE: &str = "stale";
/// A connector being removed (ADR 064 D4): sync, reconcile and node specs
/// skip it entirely, so its tunnel is never recreated nor its hostnames
/// republished, and the reconciler finishes removing it. Never reverted.
pub const STATUS_DELETING: &str = "deleting";

/// Whether a connector is being removed.
pub fn is_deleting(connector: &ingress_connectors::Model) -> bool {
    connector.status == STATUS_DELETING
}

/// The connectors sync and reconcile act on: every tunnel not being
/// removed. Credential rows (ADR 065's `tailscale`) are never tunnels.
pub fn live_connectors(models: Vec<ingress_connectors::Model>) -> Vec<ingress_connectors::Model> {
    models
        .into_iter()
        .filter(|m| !is_deleting(m) && !crate::registry::is_credential_kind(&m.provider))
        .collect()
}
/// `values` with the `secrets` answers added, after checking each names a
/// credential field of the provider and is not also given in `values`.
fn merge_secret_answers(
    info: &ProviderInfo,
    values: &BTreeMap<String, serde_json::Value>,
    secrets: &BTreeMap<String, SecretString>,
) -> Result<BTreeMap<String, serde_json::Value>, IngressError> {
    let mut merged = values.clone();
    for (key, secret) in secrets {
        let Some(field) = info.fields.iter().find(|f| &f.key == key) else {
            return Err(IngressError::Validation(format!(
                "{} has no setting '{key}'.",
                info.name
            )));
        };
        if !matches!(field.kind, FieldKind::Secret) {
            return Err(IngressError::Validation(format!(
                "{} ({key}) of {} is not a credential; give it in `values`, not `secrets`.",
                field.label, info.name
            )));
        }
        if values.contains_key(key) {
            return Err(IngressError::Validation(format!(
                "{} ({key}) of {} is given in both `values` and `secrets`; give it once.",
                field.label, info.name
            )));
        }
        merged.insert(
            key.clone(),
            serde_json::Value::String(secret.expose().to_string()),
        );
    }
    Ok(merged)
}

/// Fail when `values` answers a credential field of the provider. The error
/// names the field, never its value.
fn reject_plain_secret_answers(
    info: &ProviderInfo,
    values: &BTreeMap<String, serde_json::Value>,
) -> Result<(), IngressError> {
    let plain: Vec<String> = info
        .fields
        .iter()
        .filter(|f| matches!(f.kind, FieldKind::Secret) && values.contains_key(&f.key))
        .map(|f| format!("{} ({})", f.label, f.key))
        .collect();
    if plain.is_empty() {
        return Ok(());
    }
    Err(IngressError::Validation(format!(
        "{} takes {} as a credential: the assistant must pass it in `secrets` as a \
         `secret_ref:` handle from `request_secret`, never in `values`.",
        info.name,
        plain.join(", ")
    )))
}

const DEFAULT_PAGE_SIZE: u64 = 20;
const MAX_PAGE_SIZE: u64 = 100;
/// Most connectors one cluster may have. The console matches names against
/// all of them (`web/src/lib/tunnel-domains.ts` loads at most this many), so
/// the count is bounded where they are created.
pub const MAX_CONNECTORS: u64 = 500;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateConnectorRequest {
    /// Lowercase letters, digits and hyphens; names the tunnel `temps-<name>`.
    pub name: String,
    /// The provider's `kind`, from `GET /ingress/providers`.
    pub provider: String,
    /// Answers to the provider's setup fields, by field key. DNS provider
    /// fields take the provider's id; every other field takes a string.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub values: BTreeMap<String, serde_json::Value>,
    /// Answers to the provider's credential fields (kind `secret`, such as an
    /// OAuth client secret), by field key. Same as giving them in `values`,
    /// but published as credentials, so the AI assistant passes a
    /// `secret_ref:` handle here and never sees the value. The assistant may
    /// not put a credential field in `values`. A key cannot be in both.
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretString>,
    /// Nodes that run a connector replica.
    pub node_ids: Vec<i32>,
    /// Publish every worker-served hostname under the domain (the default),
    /// or only the hostnames chosen for this tunnel.
    #[serde(default)]
    pub auto_publish: Option<bool>,
    /// Name this tunnel's environment hosts `<label>-<host_suffix>.<domain>`
    /// (ADR 068 D3), e.g. `lab` with domain `example.com` gives
    /// `shop-production-lab.example.com`: one level under the zone, which
    /// Cloudflare's free certificate covers. Without it they are
    /// `<label>.<domain>`. 1-20 lowercase letters, digits or single hyphens,
    /// starting and ending with a letter or digit; only for a provider set
    /// up with a domain. Creating the tunnel generates no environment
    /// hostnames (with `auto_publish` it still publishes the worker-served
    /// hostnames already under the domain): environment hosts start when
    /// they are turned on for it
    /// (`PUT /ingress/connectors/{id}/environment-hosts`).
    #[serde(default)]
    #[schema(example = "lab")]
    pub host_suffix: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetConnectorNodesRequest {
    pub node_ids: Vec<i32>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateConnectorRequest {
    /// Publish every worker-served hostname under the domain (true), or only
    /// hostnames chosen for this tunnel (false).
    pub auto_publish: Option<bool>,
}

/// How one hostname reaches visitors, as the operator chose it (ADR 058 D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostnameMode {
    /// The most specific tunnel whose domain covers it and that publishes
    /// automatically.
    Automatic,
    /// Through the tunnel in `connector_id`.
    Tunnel,
    /// Through no tunnel: the operator points its DNS somewhere else.
    Off,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetHostnameChoiceRequest {
    pub mode: HostnameMode,
    /// Required with mode `tunnel`.
    pub connector_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostnameCandidate {
    pub connector_id: i32,
    pub name: String,
    pub domain: String,
    pub auto_publish: bool,
}

/// Where a hostname stands with the tunnels, and why.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostnameView {
    pub hostname: String,
    pub mode: HostnameMode,
    /// The tunnel chosen for it, with mode `tunnel`.
    pub chosen_connector_id: Option<i32>,
    /// The tunnel that publishes it, or would once `detail` is dealt with.
    pub connector_id: Option<i32>,
    pub connector_name: Option<String>,
    /// `published`, `pending`, `error` or `not_published`.
    pub state: String,
    /// What is happening; when it is not published, why and what to do.
    pub detail: String,
    /// Whether workers serve it. A tunnel only publishes those.
    pub served_by_workers: bool,
    /// The tunnels whose domain covers it.
    pub candidates: Vec<HostnameCandidate>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub published_at: Option<DBDateTime>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectorView {
    pub id: i32,
    pub name: String,
    pub provider: String,
    pub dns_provider_id: Option<i32>,
    pub domain: String,
    pub external_id: Option<String>,
    /// Where published hostnames point.
    pub dns_target: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
    pub edge_port: i32,
    pub auto_publish: bool,
    /// Publishes every environment as `<label>.<domain>` (ADR 068 D3), or
    /// `<label>-<host_suffix>.<domain>` with a host suffix. At most one
    /// tunnel does.
    pub environment_hosts: bool,
    /// Suffix of its environment hosts' first label; `null` for none.
    pub host_suffix: Option<String>,
    pub nodes: Vec<ConnectorNodeView>,
    pub publications: Vec<PublicationView>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DBDateTime,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectorNodeView {
    pub node_id: i32,
    pub node_name: String,
    pub driver: Option<String>,
    pub state: String,
    pub ready_connections: Option<i32>,
    pub last_error: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub reported_at: Option<DBDateTime>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicationView {
    pub hostname: String,
    /// `published`, `error`, or `stale` (its record points at a tunnel that
    /// was replaced; it is being published again).
    pub state: String,
    pub last_error: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub published_at: Option<DBDateTime>,
}

/// A tunnel as far as planning a hostname through it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelFact {
    pub id: i32,
    pub name: String,
    pub provider: String,
    pub dns_provider_id: Option<i32>,
    pub domain: String,
    /// The managed zone its domain is in; `None` when no longer managed.
    pub zone: Option<String>,
    pub ready: bool,
    pub has_nodes: bool,
    /// Runs on the control plane (node 0).
    pub on_control_plane: bool,
    /// Runs on at least one worker.
    pub on_workers: bool,
    pub auto_publish: bool,
    /// Publishes every environment's host (ADR 068 D3).
    pub environment_hosts: bool,
    /// Its environment hosts are `<label>-<suffix>.<domain>`.
    pub host_suffix: Option<String>,
}

/// Which tunnel would publish a hostname, by ADR 058 D9's selection (the
/// same one behind `GET /ingress/hostnames`), before asking whether workers
/// serve it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PublicationIntent {
    /// This tunnel: chosen for the hostname, or picked automatically.
    Tunnel { connector_id: i32, by_choice: bool },
    /// The hostname's choice keeps it off tunnels.
    ChosenOff,
    /// The tunnel chosen for it no longer exists.
    ChosenMissing,
    /// Only tunnels that publish just the hostnames chosen for them cover it.
    OnlyChosen { connector_id: i32 },
    /// No tunnel's domain covers it.
    NoTunnel,
}

/// What planning a hostname through the tunnels needs, read at once.
#[derive(Debug, Clone, Default)]
pub struct TunnelFacts {
    pub tunnels: Vec<TunnelFact>,
    /// Hostnames workers serve: what a tunnel on any worker can carry.
    pub served: HashSet<String>,
    /// Hostnames the control plane serves: what a tunnel only on the
    /// control plane can carry (ADR-107 D4).
    pub served_by_control_plane: HashSet<String>,
    /// Every hostname choice, sorted.
    pub choices: Vec<(String, Option<i32>)>,
    /// [`PublicationIntent`] of each hostname asked about.
    pub intents: HashMap<String, PublicationIntent>,
}

/// ADR 064 D9 on the tunnel side: may `host` be moved onto `tunnel`? The
/// reachability module implements it from the environments' IP rules.
#[async_trait::async_trait]
pub trait TunnelMoveGuard: Send + Sync {
    /// `Err` with the refusal, naming the rule, when an IP allow list of
    /// only private ranges applies to the environment serving `host`.
    async fn check_move(&self, host: &str, tunnel: &str) -> Result<(), String>;

    /// `Err` with the refusal when `host` must stay on a tunnel: a
    /// Cloudflare Access address (ADR 065 D9), which Access only sees
    /// through one.
    async fn check_move_off(&self, _host: &str) -> Result<(), String> {
        Ok(())
    }
}

/// ADR 068 D3: keeps the hosts a tunnel generates for environments in line
/// with the environments. The reachability module implements it; it runs
/// at the start of every reconcile, under the reconcile lock.
#[async_trait::async_trait]
pub trait EnvironmentHostSync: Send + Sync {
    async fn sync(&self);
}

/// One page of tunnels, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectorList {
    pub connectors: Vec<ConnectorView>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// A tunnel's live connections, read from the provider on request.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectorLinksView {
    /// The provider's own word for the tunnel (Cloudflare: `healthy`,
    /// `degraded`, `down`, `inactive`).
    pub status: String,
    pub links: Vec<LinkView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LinkView {
    /// The worker holding it, when its connector could be matched.
    pub node_id: Option<i32>,
    pub node_name: Option<String>,
    /// The provider's data centre, such as `mad01`.
    pub location: Option<String>,
    pub opened_at: Option<String>,
    pub reconnecting: bool,
    pub client_version: Option<String>,
}

/// Why creating a connector failed, and whether it was saved first.
#[derive(Debug)]
pub enum CreateError {
    /// Refused before anything was written.
    NotSaved(Box<IngressError>),
    /// The connector was saved, then assigning its workers or setting it up
    /// with the provider failed. It stays, so a sync can finish it.
    Saved {
        connector_id: i32,
        connector_name: String,
        error: Box<IngressError>,
    },
}

impl From<IngressError> for CreateError {
    fn from(error: IngressError) -> Self {
        Self::NotSaved(Box::new(error))
    }
}

pub struct IngressConnectorService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    dns_providers: Arc<DnsProviderService>,
    records: Arc<ManagedDnsRecordService>,
    peer_table: Arc<CachedPeerTable>,
    http: reqwest::Client,
    registry: ProviderRegistry,
    /// Registered by the proxy plugin, after this one; see
    /// [`Self::set_request_policy`].
    request_policy: OnceLock<Arc<temps_core::RequestPolicyGateSlot>>,
    reconcile_lock: tokio::sync::Mutex<()>,
    /// See [`Self::set_move_guard`].
    move_guard: OnceLock<Arc<dyn TunnelMoveGuard>>,
    /// Serializes read-modify-writes of a connector's `settings` (token
    /// holders), so a node recorded while the reconciler rotates is kept.
    settings_lock: tokio::sync::Mutex<()>,
    /// See [`Self::set_environment_host_sync`].
    environment_hosts: OnceLock<Arc<dyn EnvironmentHostSync>>,
    /// Set when this process runs connectors on the control plane
    /// (ADR-107 D2); see [`Self::attach_control_plane`].
    control_plane: OnceLock<crate::control_plane::ControlPlaneHandle>,
}

impl IngressConnectorService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        dns_providers: Arc<DnsProviderService>,
        records: Arc<ManagedDnsRecordService>,
        peer_table: Arc<CachedPeerTable>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            db,
            encryption,
            dns_providers,
            records,
            peer_table,
            http,
            registry: ProviderRegistry::builtin(),
            request_policy: OnceLock::new(),
            reconcile_lock: tokio::sync::Mutex::new(()),
            move_guard: OnceLock::new(),
            settings_lock: tokio::sync::Mutex::new(()),
            environment_hosts: OnceLock::new(),
            control_plane: OnceLock::new(),
        }
    }

    /// This process runs connectors placed on the control plane. Until it
    /// is called node 0 is refused. Later calls are ignored.
    pub fn attach_control_plane(&self, handle: crate::control_plane::ControlPlaneHandle) {
        let _ = self.control_plane.set(handle);
    }

    /// Whether a connector placed on the control plane runs here; `Err` says
    /// why not and what to do instead (ADR-107 D2).
    pub fn control_plane_tunnels(&self) -> Result<(), ControlPlaneTunnelsUnavailable> {
        match self.control_plane.get() {
            Some(handle) => handle.ready(),
            None => Err(ControlPlaneTunnelsUnavailable::SeparateProxy),
        }
    }

    /// Make the control plane look at its connectors now.
    fn wake_control_plane(&self) {
        if let Some(handle) = self.control_plane.get() {
            handle.wake();
        }
    }

    /// Whether a custom request policy keeps traffic on the control plane
    /// decides what workers serve, so it decides what a tunnel may publish.
    /// Until it is known nothing is published.
    pub fn set_request_policy(&self, gate: Arc<temps_core::RequestPolicyGateSlot>) {
        let _ = self.request_policy.set(gate);
    }

    /// Lets hostname choices refuse a move onto a tunnel that an IP allow
    /// list of only private ranges would make useless (ADR 064 D9). Later
    /// calls are ignored.
    pub fn set_move_guard(&self, guard: Arc<dyn TunnelMoveGuard>) {
        let _ = self.move_guard.set(guard);
    }

    /// Registered by the reachability module (ADR 068 D3).
    pub fn set_environment_host_sync(&self, sync: Arc<dyn EnvironmentHostSync>) {
        let _ = self.environment_hosts.set(sync);
    }

    // ----- provider plumbing -------------------------------------------

    fn deps(&self) -> ProviderDeps<'_> {
        ProviderDeps {
            http: &self.http,
            dns_providers: &self.dns_providers,
        }
    }

    /// The DNS zone the connector's domain lives in: the domain itself, or
    /// the managed domain above it when the tunnel is on a subdomain.
    pub(crate) async fn zone_of(
        &self,
        connector: &ingress_connectors::Model,
    ) -> Result<String, IngressError> {
        match self
            .dns_providers
            .find_provider_for_domain(&connector.domain)
            .await?
        {
            Some((_, managed)) => Ok(managed.domain.trim_end_matches('.').to_ascii_lowercase()),
            None => Err(not_managed(&connector.domain)),
        }
    }

    /// [`Self::zone_of`] for many connectors in one query. A connector whose
    /// domain is no longer managed, or ambiguously so, has no zone.
    async fn zones_of(
        &self,
        connectors: &[ingress_connectors::Model],
    ) -> Result<HashMap<i32, Option<String>>, IngressError> {
        let domains: Vec<String> = connectors.iter().map(|c| c.domain.clone()).collect();
        let found = self
            .dns_providers
            .find_providers_for_domains(&domains)
            .await?;
        Ok(connectors
            .iter()
            .map(|connector| {
                let zone = match found.get(&connector.domain) {
                    Some(Ok(Some((_, managed)))) => {
                        Some(managed.domain.trim_end_matches('.').to_ascii_lowercase())
                    }
                    Some(Err(error)) => {
                        debug!(connector_id = connector.id, %error, "no zone for ingress connector");
                        None
                    }
                    _ => {
                        debug!(connector_id = connector.id, domain = %connector.domain, "no managed zone covers ingress connector");
                        None
                    }
                };
                (connector.id, zone)
            })
            .collect())
    }

    /// The providers this control plane offers, for the console.
    pub fn providers(&self) -> Vec<ProviderInfo> {
        self.registry.infos()
    }

    /// Refuse a credential answer given in plain `values`.
    ///
    /// The console may send every answer in `values`. A request the AI
    /// assistant issued may not: a credential there is a value the model typed,
    /// so it has already passed through the model provider and the chat. Such a
    /// request must put it in `secrets` as a `secret_ref:` handle, which the
    /// tool layer resolves from the user's private form.
    pub fn ensure_secret_answers_sealed(
        &self,
        request: &CreateConnectorRequest,
    ) -> Result<(), IngressError> {
        let info = self.registry.get(&request.provider)?.info();
        reject_plain_secret_answers(&info, &request.values)
    }

    /// The credential providers this control plane offers (ADR 065 D2).
    pub fn credential_providers(&self) -> Vec<ProviderInfo> {
        self.registry.credential_infos()
    }

    /// Save a credential provider's answers as a connector row no tunnel
    /// path reads (ADR 065 D2): checked against the fields, proven by the
    /// provider, the secrets sealed with the encryption service. Returns the
    /// row id.
    pub async fn save_credential(
        &self,
        kind: &str,
        name: &str,
        values: &BTreeMap<String, serde_json::Value>,
    ) -> Result<i32, IngressError> {
        let factory = self.registry.credential(kind)?;
        let info = factory.info();
        let setup = self.resolve_setup(&info, values).await?;
        factory.validate(&self.deps(), &setup).await?;
        let mut settings = serde_json::Map::new();
        settings.insert(SETUP_KEY.into(), serde_json::json!(setup.settings));
        let json = serde_json::to_string(&setup.secrets)
            .map_err(|e| IngressError::Encryption(e.to_string()))?;
        let sealed = self
            .encryption
            .encrypt_string(&json)
            .map_err(|e| IngressError::Encryption(e.to_string()))?;
        settings.insert(SETUP_SECRETS_KEY.into(), serde_json::Value::String(sealed));
        let now = Utc::now();
        let row = ingress_connectors::ActiveModel {
            name: Set(name.to_string()),
            provider: Set(info.kind.clone()),
            dns_provider_id: Set(None),
            domain: Set(String::new()),
            settings: Set(serde_json::Value::Object(settings)),
            edge_port: Set(DEFAULT_EDGE_PORT),
            auto_publish: Set(false),
            status: Set("ready".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|error| match error {
            sea_orm::DbErr::Query(_) | sea_orm::DbErr::Exec(_)
                if error.to_string().contains("ingress_connectors_name_key") =>
            {
                IngressError::Validation(format!(
                    "A connector or credential named '{name}' already exists."
                ))
            }
            other => IngressError::Database(other),
        })?;
        Ok(row.id)
    }

    /// The cluster's credential of `kind`, the oldest first, with its
    /// `text` and decrypted `secret` answers.
    #[allow(clippy::type_complexity)]
    pub async fn credential(
        &self,
        kind: &str,
    ) -> Result<
        Option<(
            ingress_connectors::Model,
            BTreeMap<String, String>,
            BTreeMap<String, String>,
        )>,
        IngressError,
    > {
        let Some(row) = ingress_connectors::Entity::find()
            .filter(ingress_connectors::Column::Provider.eq(kind))
            .order_by_asc(ingress_connectors::Column::Id)
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(None);
        };
        let settings: BTreeMap<String, String> = row
            .settings
            .get(SETUP_KEY)
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                IngressError::Validation(format!(
                    "The settings of credential {} ('{}') cannot be read: {e}",
                    row.id, row.name
                ))
            })?
            .unwrap_or_default();
        let secrets = self.setup_secrets(&row)?;
        Ok(Some((row, settings, secrets)))
    }

    /// The credential row `id`, if it is one.
    pub async fn credential_row(
        &self,
        id: i32,
    ) -> Result<Option<ingress_connectors::Model>, IngressError> {
        Ok(ingress_connectors::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .filter(|m| crate::registry::is_credential_kind(&m.provider)))
    }

    /// Remove the credential row `id`; one that is not a credential, or is
    /// gone, is left alone.
    pub async fn delete_credential(&self, id: i32) -> Result<(), IngressError> {
        if self.credential_row(id).await?.is_some() {
            ingress_connectors::Entity::delete_by_id(id)
                .exec(self.db.as_ref())
                .await?;
        }
        Ok(())
    }

    async fn provider_for(
        &self,
        connector: &ingress_connectors::Model,
    ) -> Result<Box<dyn TunnelProvider>, IngressError> {
        let factory = self.registry.get(&connector.provider)?;
        let secrets = self.setup_secrets(connector)?;
        factory.build(&self.deps(), connector, &secrets).await
    }

    /// A provider good enough to build node specs, which need no credential.
    fn spec_provider(&self, kind: &str) -> Option<Box<dyn TunnelProvider>> {
        self.registry
            .get(kind)
            .ok()
            .map(|factory| factory.spec_provider(&self.http))
    }

    /// The connector's `secret` setup answers, decrypted.
    fn setup_secrets(
        &self,
        connector: &ingress_connectors::Model,
    ) -> Result<BTreeMap<String, String>, IngressError> {
        let Some(sealed) = connector
            .settings
            .get(SETUP_SECRETS_KEY)
            .and_then(|v| v.as_str())
        else {
            return Ok(BTreeMap::new());
        };
        let json = self
            .encryption
            .decrypt_string(sealed)
            .map_err(|e| IngressError::Encryption(e.to_string()))?;
        serde_json::from_str(&json).map_err(|e| IngressError::Encryption(e.to_string()))
    }

    /// Check the answers against the provider's fields and resolve them.
    async fn resolve_setup(
        &self,
        info: &ProviderInfo,
        values: &BTreeMap<String, serde_json::Value>,
    ) -> Result<ProviderSetup, IngressError> {
        let mut setup = ProviderSetup {
            dns_provider: None,
            domain: None,
            settings: BTreeMap::new(),
            secrets: BTreeMap::new(),
        };
        for key in values.keys() {
            if !info.fields.iter().any(|f| &f.key == key) {
                return Err(IngressError::Validation(format!(
                    "{} has no setting '{key}'.",
                    info.name
                )));
            }
        }
        // DNS providers first: a managed domain is checked against one.
        let mut fields: Vec<_> = info.fields.iter().collect();
        fields.sort_by_key(|f| !matches!(f.kind, FieldKind::DnsProvider { .. }));
        for field in fields {
            let value = values.get(&field.key).filter(|v| match v {
                serde_json::Value::Null => false,
                serde_json::Value::String(s) => !s.trim().is_empty(),
                _ => true,
            });
            let Some(value) = value else {
                if field.required {
                    return Err(IngressError::Validation(format!(
                        "{} is required.",
                        field.label
                    )));
                }
                continue;
            };
            let text = || -> Result<String, IngressError> {
                match value {
                    serde_json::Value::String(s) => Ok(s.trim().to_string()),
                    serde_json::Value::Number(n) => Ok(n.to_string()),
                    _ => Err(IngressError::Validation(format!(
                        "{} must be text.",
                        field.label
                    ))),
                }
            };
            match &field.kind {
                FieldKind::Text => {
                    setup.settings.insert(field.key.clone(), text()?);
                }
                FieldKind::Secret => {
                    setup.secrets.insert(field.key.clone(), text()?);
                }
                FieldKind::DnsProvider { provider_type } => {
                    let id: i32 = text()?.parse().map_err(|_| {
                        IngressError::Validation(format!("{} must be a DNS provider.", field.label))
                    })?;
                    let provider = self.dns_providers.get(id).await?;
                    if let Some(wanted) = provider_type {
                        if !provider.provider_type.eq_ignore_ascii_case(wanted) {
                            return Err(IngressError::Validation(format!(
                                "{} needs a {wanted} DNS provider; '{}' is {}.",
                                field.label, provider.name, provider.provider_type
                            )));
                        }
                    }
                    setup.dns_provider = Some(provider);
                }
                FieldKind::Teams
                | FieldKind::Users
                | FieldKind::Choice { .. }
                | FieldKind::TextList => {
                    return Err(IngressError::Validation(format!(
                        "{} ({}) is not something an ingress provider is set up with.",
                        field.label, field.key
                    )));
                }
                FieldKind::ManagedDomain {
                    allow_subdomain, ..
                } => {
                    let domain = text()?.trim_end_matches('.').to_ascii_lowercase();
                    if !is_dns_name(&domain) {
                        return Err(IngressError::Validation(format!(
                            "{domain} is not a valid domain name."
                        )));
                    }
                    let managing = self.dns_providers.find_provider_for_domain(&domain).await?;
                    if let Some((_, zone)) = &managing {
                        let zone = zone.domain.trim_end_matches('.').to_ascii_lowercase();
                        if zone != domain && !allow_subdomain {
                            return Err(IngressError::Validation(format!(
                                "{} takes a whole managed domain; use {zone}.",
                                field.label
                            )));
                        }
                    }
                    match (&setup.dns_provider, managing) {
                        (Some(chosen), Some((managing, _))) if managing.id != chosen.id => {
                            return Err(IngressError::Validation(format!(
                                "{domain} is managed by DNS provider '{}', not '{}'. Pick that provider, or move the domain.",
                                managing.name, chosen.name
                            )))
                        }
                        (Some(chosen), None) => {
                            return Err(IngressError::Validation(format!(
                                "{domain} is not a managed domain of DNS provider '{}'. Add and verify it under Settings → DNS first.",
                                chosen.name
                            )))
                        }
                        (None, None) => {
                            return Err(IngressError::Validation(format!(
                                "{domain} is not a managed domain. Add and verify it under Settings → DNS first."
                            )))
                        }
                        _ => {}
                    }
                    setup.domain = Some(domain);
                }
            }
        }
        Ok(setup)
    }

    fn handle_of(
        &self,
        connector: &ingress_connectors::Model,
    ) -> Result<Option<TunnelHandle>, IngressError> {
        let (Some(external_id), Some(secret)) =
            (&connector.external_id, &connector.secret_encrypted)
        else {
            return Ok(None);
        };
        let secret = self
            .encryption
            .decrypt_string(secret)
            .map_err(|e| IngressError::Encryption(e.to_string()))?;
        Ok(Some(TunnelHandle {
            external_id: external_id.clone(),
            settings: connector.settings.clone(),
            secret,
        }))
    }

    /// The tunnel `id`. A credential row is not one.
    /// Read connector `id` for a read-modify-write of its row, locked
    /// (`FOR UPDATE`) until `txn` ends. The in-process `settings_lock` only
    /// orders writers of one service instance; `temps serve` runs a second
    /// one for the control plane's connectors (ADR-107 D2), so the row lock
    /// is what keeps their token-holder and status updates from overwriting
    /// each other.
    async fn find_for_update(
        txn: &DatabaseTransaction,
        id: i32,
    ) -> Result<ingress_connectors::Model, IngressError> {
        ingress_connectors::Entity::find_by_id(id)
            .lock_exclusive()
            .one(txn)
            .await?
            .filter(|m| !crate::registry::is_credential_kind(&m.provider))
            .ok_or(IngressError::NotFound(id))
    }

    async fn find(&self, id: i32) -> Result<ingress_connectors::Model, IngressError> {
        ingress_connectors::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .filter(|m| !crate::registry::is_credential_kind(&m.provider))
            .ok_or(IngressError::NotFound(id))
    }

    // ----- admin operations --------------------------------------------

    /// One page of tunnels, newest first: `page` from 1, `page_size` 20 by
    /// default and at most 100.
    pub async fn list(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<ConnectorList, IngressError> {
        let (page, page_size) = page_bounds(page, page_size);
        let paginator = ingress_connectors::Entity::find()
            .filter(ingress_connectors::Column::Provider.ne(crate::tailscale::KIND))
            .order_by_desc(ingress_connectors::Column::CreatedAt)
            .order_by_desc(ingress_connectors::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let connectors = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;
        Ok(ConnectorList {
            connectors: self.views(connectors).await?,
            total,
            page,
            page_size,
        })
    }

    pub async fn get(&self, id: i32) -> Result<ConnectorView, IngressError> {
        let connector = self.find(id).await?;
        self.views(vec![connector])
            .await?
            .pop()
            .ok_or(IngressError::NotFound(id))
    }

    /// Views of `connectors`, in order, from three queries however many
    /// there are.
    async fn views(
        &self,
        connectors: Vec<ingress_connectors::Model>,
    ) -> Result<Vec<ConnectorView>, IngressError> {
        if connectors.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i32> = connectors.iter().map(|c| c.id).collect();
        let mut node_rows: HashMap<i32, Vec<ingress_connector_nodes::Model>> = HashMap::new();
        for row in ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::ConnectorId.is_in(ids.iter().copied()))
            .order_by_asc(ingress_connector_nodes::Column::NodeId)
            .all(self.db.as_ref())
            .await?
        {
            node_rows.entry(row.connector_id).or_default().push(row);
        }
        let names = node_names(
            self.db.as_ref(),
            node_rows.values().flatten().map(|n| n.node_id),
        )
        .await?;
        let mut publications: HashMap<i32, Vec<ingress_publications::Model>> = HashMap::new();
        for row in ingress_publications::Entity::find()
            .filter(ingress_publications::Column::ConnectorId.is_in(ids))
            .order_by_asc(ingress_publications::Column::Hostname)
            .all(self.db.as_ref())
            .await?
        {
            publications.entry(row.connector_id).or_default().push(row);
        }
        Ok(connectors
            .into_iter()
            .map(|connector| {
                let node_rows = node_rows.remove(&connector.id).unwrap_or_default();
                let publications = publications.remove(&connector.id).unwrap_or_default();
                connector_view(connector, node_rows, &names, publications)
            })
            .collect())
    }

    /// The tunnel's connections as the provider sees them, each matched to
    /// the worker whose connector holds it.
    pub async fn links(&self, id: i32) -> Result<ConnectorLinksView, IngressError> {
        let connector = self.find(id).await?;
        let handle = self.handle_of(&connector)?.ok_or_else(|| {
            IngressError::Validation(
                "The tunnel is not created yet, so it has no connections.".into(),
            )
        })?;
        let health = self.provider_for(&connector).await?.health(&handle).await?;
        let node_rows = ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::ConnectorId.eq(id))
            .all(self.db.as_ref())
            .await?;
        let names = node_names(self.db.as_ref(), node_rows.iter().map(|n| n.node_id)).await?;
        let by_ref: HashMap<String, i32> = node_rows
            .into_iter()
            .filter_map(|n| n.connector_ref.map(|r| (r, n.node_id)))
            .collect();
        let mut links: Vec<LinkView> = health
            .links
            .into_iter()
            .map(|link| {
                let node_id = link
                    .connector_ref
                    .as_ref()
                    .and_then(|r| by_ref.get(r).copied());
                LinkView {
                    node_id,
                    node_name: node_id.and_then(|id| names.get(&id).cloned()),
                    location: link.location,
                    opened_at: link.opened_at,
                    reconnecting: link.reconnecting,
                    client_version: link.client_version,
                }
            })
            .collect();
        links.sort_by(|a, b| (a.node_id, &a.location).cmp(&(b.node_id, &b.location)));
        Ok(ConnectorLinksView {
            status: health.status,
            links,
        })
    }

    pub async fn create(
        self: &Arc<Self>,
        request: CreateConnectorRequest,
    ) -> Result<ConnectorView, CreateError> {
        let name = request.name.trim().to_ascii_lowercase();
        if name.is_empty()
            || name.len() > 50
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(IngressError::Validation(
                "The name must be 1-50 lowercase letters, digits or hyphens.".into(),
            )
            .into());
        }
        let existing = ingress_connectors::Entity::find()
            .count(self.db.as_ref())
            .await
            .map_err(IngressError::from)?;
        if existing >= MAX_CONNECTORS {
            return Err(IngressError::Validation(format!(
                "This cluster already has {existing} tunnels, the most it supports is \
                 {MAX_CONNECTORS}. Delete a tunnel you no longer use, then add this one."
            ))
            .into());
        }
        let factory = self.registry.get(&request.provider)?;
        let info = factory.info();
        // Whether the workers allow it comes first: no point in answers a
        // worker will refuse.
        self.validate_nodes(&request.node_ids).await?;
        let values = merge_secret_answers(&info, &request.values, &request.secrets)?;
        let setup = self.resolve_setup(&info, &values).await?;
        let host_suffix = host_suffix_for(&info, &setup, request.host_suffix.as_deref())?;
        factory.validate(&self.deps(), &setup).await?;

        let mut settings = serde_json::Map::new();
        if !setup.settings.is_empty() {
            settings.insert(SETUP_KEY.into(), serde_json::json!(setup.settings));
        }
        if !setup.secrets.is_empty() {
            let json = serde_json::to_string(&setup.secrets)
                .map_err(|e| IngressError::Encryption(e.to_string()))?;
            let sealed = self
                .encryption
                .encrypt_string(&json)
                .map_err(|e| IngressError::Encryption(e.to_string()))?;
            settings.insert(SETUP_SECRETS_KEY.into(), serde_json::Value::String(sealed));
        }
        let now = Utc::now();
        let connector = ingress_connectors::ActiveModel {
            name: Set(name),
            provider: Set(info.kind.clone()),
            dns_provider_id: Set(setup.dns_provider.as_ref().map(|p| p.id)),
            domain: Set(setup.domain.clone().unwrap_or_default()),
            settings: Set(serde_json::Value::Object(settings)),
            edge_port: Set(DEFAULT_EDGE_PORT),
            auto_publish: Set(request.auto_publish.unwrap_or(true)),
            host_suffix: Set(host_suffix),
            status: Set("pending".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|error| match error {
            sea_orm::DbErr::Query(_) | sea_orm::DbErr::Exec(_)
                if error.to_string().contains("ingress_connectors_name_key") =>
            {
                IngressError::Validation("A connector with this name already exists.".into())
            }
            other => IngressError::Database(other),
        })?;
        // From here the connector is saved, so a failure is reported with it:
        // the write happened and must be audited as one.
        let id = connector.id;
        let name = connector.name.clone();
        let setup = match self.replace_nodes(id, &request.node_ids).await {
            Ok(()) => self.ensure(connector).await.map(|_| ()),
            Err(error) => Err(error),
        };
        if setup.is_ok() {
            self.wake_control_plane();
            let service = Arc::clone(self);
            tokio::spawn(async move { service.reconcile_all().await });
        }
        let view = self.get(id).await;
        match (setup, view) {
            (Ok(()), Ok(view)) => Ok(view),
            (Ok(()), Err(error)) | (Err(error), _) => Err(CreateError::Saved {
                connector_id: id,
                connector_name: name,
                error: Box::new(error),
            }),
        }
    }

    pub async fn set_nodes(
        self: &Arc<Self>,
        id: i32,
        node_ids: Vec<i32>,
    ) -> Result<ConnectorView, IngressError> {
        self.find(id).await?;
        self.validate_nodes(&node_ids).await?;
        self.replace_nodes(id, &node_ids).await?;
        self.wake_control_plane();
        let service = Arc::clone(self);
        tokio::spawn(async move { service.reconcile_all().await });
        self.get(id).await
    }

    pub async fn update(
        self: &Arc<Self>,
        id: i32,
        request: UpdateConnectorRequest,
    ) -> Result<ConnectorView, IngressError> {
        let connector = self.find(id).await?;
        if let Some(auto_publish) = request.auto_publish {
            // ADR 064 D9: turning automatic publishing on is refused when it
            // would publish a hostname whose environment admits only
            // private ranges, which no visitor through a tunnel has.
            if auto_publish && !connector.auto_publish {
                if let Some(guard) = self.move_guard.get() {
                    let mut plan = self.plan().await?;
                    for host in plan.published_automatically_once_on(id) {
                        guard
                            .check_move(&host, &connector.name)
                            .await
                            .map_err(|refusal| {
                                IngressError::Validation(format!(
                                    "Automatic publishing stays off for tunnel {}: {refusal} \
                                     Or keep it off and choose the hostnames this tunnel \
                                     publishes.",
                                    connector.name
                                ))
                            })?;
                    }
                }
            }
            let mut row: ingress_connectors::ActiveModel = connector.into();
            row.auto_publish = Set(auto_publish);
            row.updated_at = Set(Utc::now());
            row.update(self.db.as_ref()).await?;
            self.reconcile_all().await;
        }
        self.get(id).await
    }

    /// Every hostname a tunnel could publish, plus every one with a choice
    /// or a publication, with where it stands.
    pub async fn hostnames(&self) -> Result<Vec<HostnameView>, IngressError> {
        let plan = self.plan().await?;
        let publications = self.publications_by_host().await?;
        let mut hosts: BTreeSet<String> = self
            .control_plane_hostnames()
            .into_iter()
            .filter(|host| !plan.candidates(host).is_empty())
            .collect();
        hosts.extend(plan.choices.keys().cloned());
        hosts.extend(publications.keys().cloned());
        Ok(hosts
            .iter()
            .map(|host| plan.view(host, publications.get(host)))
            .collect())
    }

    /// [`Self::hostname`] for several hostnames, planning once. Invalid
    /// hostnames are left out.
    pub async fn hostname_views(
        &self,
        hosts: &[String],
    ) -> Result<Vec<HostnameView>, IngressError> {
        let plan = self.plan().await?;
        let publications = self.publications_by_host().await?;
        Ok(hosts
            .iter()
            .filter_map(|host| normalize_host(host).ok())
            .map(|host| plan.view(&host, publications.get(&host)))
            .collect())
    }

    /// Every tunnel with what decides whether it can publish a hostname, the
    /// hostnames workers serve (the only ones a tunnel can carry), every
    /// hostname choice, and which tunnel would publish each of `hosts`.
    pub async fn tunnel_facts(&self, hosts: &[String]) -> Result<TunnelFacts, IngressError> {
        let plan = self.plan().await?;
        let tunnels = plan
            .connectors
            .iter()
            .map(|c| TunnelFact {
                id: c.model.id,
                name: c.model.name.clone(),
                provider: c.model.provider.clone(),
                dns_provider_id: c.model.dns_provider_id,
                domain: c.model.domain.clone(),
                zone: c.zone.clone(),
                ready: c.model.status == "ready",
                has_nodes: c.placement.has_nodes(),
                on_control_plane: c.placement.control_plane,
                on_workers: c.placement.workers,
                auto_publish: c.model.auto_publish,
                environment_hosts: c.model.environment_hosts,
                host_suffix: c.model.host_suffix.clone(),
            })
            .collect();
        let mut choices: Vec<(String, Option<i32>)> = plan
            .choices
            .iter()
            .map(|(host, choice)| (host.clone(), *choice))
            .collect();
        choices.sort();
        let intents = hosts
            .iter()
            .filter_map(|host| normalize_host(host).ok())
            .map(|host| {
                let intent = plan.intent(&host);
                (host, intent)
            })
            .collect();
        Ok(TunnelFacts {
            tunnels,
            served: plan.served,
            served_by_control_plane: plan.served_by_control_plane,
            choices,
            intents,
        })
    }

    /// The operator's choice for a hostname: `None` without one,
    /// `Some(None)` kept off tunnels, `Some(Some(id))` through that tunnel.
    pub async fn hostname_choice(
        &self,
        hostname: &str,
    ) -> Result<Option<Option<i32>>, IngressError> {
        let host = normalize_host(hostname)?;
        Ok(ingress_hostname_choices::Entity::find()
            .filter(ingress_hostname_choices::Column::Hostname.eq(&host))
            .one(self.db.as_ref())
            .await?
            .map(|choice| choice.connector_id))
    }

    pub async fn hostname(&self, hostname: &str) -> Result<HostnameView, IngressError> {
        let host = normalize_host(hostname)?;
        let plan = self.plan().await?;
        let publications = self.publications_by_host().await?;
        Ok(plan.view(&host, publications.get(&host)))
    }

    /// Whether `host` may be published through tunnel `id`: the tunnel
    /// exists, its zone holds the hostname, and moving it there is allowed.
    async fn check_tunnel_choice(&self, host: &str, id: i32) -> Result<(), IngressError> {
        let connector = self.find(id).await?;
        let zone = self.zone_of(&connector).await?;
        record_label(host, &connector.domain, &zone)?;
        if let Some(guard) = self.move_guard.get() {
            guard
                .check_move(host, &connector.name)
                .await
                .map_err(IngressError::Validation)?;
        }
        Ok(())
    }

    /// Choose tunnel `connector_id` for `hostname`, only while its choice is
    /// still `expected` (what the caller read, as `hostname_choice` returns
    /// it): a choice someone made in between is never replaced unseen. A
    /// reachability run decides from what it read, so it writes this way.
    /// Already choosing this tunnel counts as done, so a resumed run whose
    /// write landed before it stopped does not fail.
    pub async fn choose_tunnel_if_unchanged(
        &self,
        hostname: &str,
        connector_id: i32,
        expected: Option<Option<i32>>,
    ) -> Result<(), IngressError> {
        let host = normalize_host(hostname)?;
        self.check_tunnel_choice(&host, connector_id).await?;
        let now = Utc::now();
        let written = match expected {
            None => {
                let inserted = ingress_hostname_choices::ActiveModel {
                    hostname: Set(host.clone()),
                    connector_id: Set(Some(connector_id)),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(self.db.as_ref())
                .await;
                match inserted {
                    Ok(_) => true,
                    // `hostname` is unique: a row someone inserted meanwhile
                    // refuses this. Any other failure is reported as it is.
                    Err(error) => {
                        if self.hostname_choice(&host).await?.is_none() {
                            return Err(error.into());
                        }
                        false
                    }
                }
            }
            Some(previous) => {
                let column = ingress_hostname_choices::Column::ConnectorId;
                ingress_hostname_choices::Entity::update_many()
                    .col_expr(column, Expr::value(Some(connector_id)))
                    .col_expr(
                        ingress_hostname_choices::Column::UpdatedAt,
                        Expr::value(now),
                    )
                    .filter(ingress_hostname_choices::Column::Hostname.eq(&host))
                    .filter(match previous {
                        Some(id) => column.eq(id),
                        None => column.is_null(),
                    })
                    .exec(self.db.as_ref())
                    .await?
                    .rows_affected
                    == 1
            }
        };
        if !written {
            let current = self.hostname_choice(&host).await?;
            if current != Some(Some(connector_id)) {
                return Err(IngressError::Validation(format!(
                    "{host}'s tunnel choice changed while this setup ran, so it was left as \
                     the other change set it. Plan again to see it, or change it under \
                     Expose your apps."
                )));
            }
        }
        self.reconcile_all().await;
        Ok(())
    }

    /// Record the operator's choice for one hostname and apply it now.
    pub async fn set_hostname_choice(
        &self,
        hostname: &str,
        request: SetHostnameChoiceRequest,
    ) -> Result<HostnameView, IngressError> {
        let host = normalize_host(hostname)?;
        let existing = ingress_hostname_choices::Entity::find()
            .filter(ingress_hostname_choices::Column::Hostname.eq(&host))
            .one(self.db.as_ref())
            .await?;
        let connector_id = match request.mode {
            HostnameMode::Automatic => {
                if let Some(row) = existing {
                    ingress_hostname_choices::Entity::delete_by_id(row.id)
                        .exec(self.db.as_ref())
                        .await?;
                }
                self.reconcile_all().await;
                return self.hostname(&host).await;
            }
            HostnameMode::Off => {
                if let Some(guard) = self.move_guard.get() {
                    guard
                        .check_move_off(&host)
                        .await
                        .map_err(IngressError::Validation)?;
                }
                None
            }
            HostnameMode::Tunnel => {
                let id = request.connector_id.ok_or_else(|| {
                    IngressError::Validation("Pick the tunnel to publish it through.".into())
                })?;
                self.check_tunnel_choice(&host, id).await?;
                Some(id)
            }
        };
        let now = Utc::now();
        match existing {
            Some(row) => {
                let mut row: ingress_hostname_choices::ActiveModel = row.into();
                row.connector_id = Set(connector_id);
                row.updated_at = Set(now);
                row.update(self.db.as_ref()).await?;
            }
            None => {
                ingress_hostname_choices::ActiveModel {
                    hostname: Set(host.clone()),
                    connector_id: Set(connector_id),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(self.db.as_ref())
                .await?;
            }
        }
        self.reconcile_all().await;
        self.hostname(&host).await
    }

    async fn publications_by_host(
        &self,
    ) -> Result<HashMap<String, ingress_publications::Model>, IngressError> {
        Ok(ingress_publications::Entity::find()
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|p| (p.hostname.clone(), p))
            .collect())
    }

    /// Re-check the tunnel at the provider and republish now.
    pub async fn sync(self: &Arc<Self>, id: i32) -> Result<ConnectorView, IngressError> {
        let connector = self.find(id).await?;
        if is_deleting(&connector) {
            return Err(IngressError::Validation(format!(
                "Tunnel {} is being removed, so it is not synced. Temps finishes removing it in \
                 the background.",
                connector.name
            )));
        }
        self.ensure(connector).await?;
        self.reconcile_all().await;
        self.get(id).await
    }

    /// Remove a connector: mark it `deleting`, then remove its records,
    /// its tunnel at the provider and its row ([`Self::finish_deletion`]).
    pub async fn delete(&self, id: i32) -> Result<(), IngressError> {
        let connector = self.find(id).await?;
        let tunnel_id = self.mark_deleting(id, &connector.name).await?;
        self.finish_deletion(id, tunnel_id.as_deref()).await
    }

    /// Mark connector `id` named `name` `deleting` before anything is
    /// removed; returns its provider tunnel id. From here sync never
    /// recreates it and reconcile never republishes it.
    pub async fn mark_deleting(&self, id: i32, name: &str) -> Result<Option<String>, IngressError> {
        // Under the lock `ensure` and the secret rotation write the row
        // with, so neither can put back a status read before this.
        let _guard = self.settings_lock.lock().await;
        let txn = self.db.begin().await?;
        let connector = Self::find_for_update(&txn, id).await?;
        if connector.name != name {
            return Err(IngressError::Validation(format!(
                "Connector {id} is `{}` now, not `{name}`; nothing was removed.",
                connector.name
            )));
        }
        if !is_deleting(&connector) {
            ingress_connectors::Entity::update_many()
                .col_expr(
                    ingress_connectors::Column::Status,
                    Expr::value(STATUS_DELETING),
                )
                .col_expr(
                    ingress_connectors::Column::UpdatedAt,
                    Expr::value(Utc::now()),
                )
                .filter(ingress_connectors::Column::Id.eq(id))
                .exec(&txn)
                .await?;
            txn.commit().await?;
            info!(connector_id = id, "ingress connector marked deleting");
            // A connector being removed is no longer in node 0's specs:
            // stop it there now rather than at the next poll.
            self.wake_control_plane();
        }
        Ok(connector.external_id)
    }

    /// Finish removing a `deleting` connector: its DNS records, then its
    /// tunnel at the provider, then its row. Already gone counts as done.
    /// When the row's tunnel is no longer `tunnel_id` the tunnel is left
    /// alone and the mismatch reported.
    pub async fn finish_deletion(
        &self,
        id: i32,
        tunnel_id: Option<&str>,
    ) -> Result<(), IngressError> {
        let connector = match self.find(id).await {
            Ok(connector) => connector,
            Err(IngressError::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error),
        };
        if !is_deleting(&connector) {
            return Err(IngressError::Validation(format!(
                "Tunnel {} is not being removed; mark it first.",
                connector.name
            )));
        }
        if tunnel_id.is_some() && connector.external_id.as_deref() != tunnel_id {
            return Err(IngressError::Validation(format!(
                "Tunnel {} is now tunnel {} at the provider, not {}, so Temps did not delete it. \
                 Delete it under Expose your apps if it should go.",
                connector.name,
                connector.external_id.as_deref().unwrap_or("none"),
                tunnel_id.unwrap_or("none")
            )));
        }
        let publications = ingress_publications::Entity::find()
            .filter(ingress_publications::Column::ConnectorId.eq(id))
            .all(self.db.as_ref())
            .await?;
        // Sync's own cleanup (ADR 058): the records it published for this
        // tunnel carry its marker and go before the tunnel does, so none is
        // left pointing at a deleted tunnel. A zone Temps cannot read now is
        // retried on the next pass; one no longer managed has nothing Temps
        // could remove.
        let zone = if publications.iter().any(|p| has_record(&p.state)) {
            match self.zone_of(&connector).await {
                Ok(zone) => Some(zone),
                Err(IngressError::Validation(_)) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        for publication in publications {
            if has_record(&publication.state) {
                self.remove_record(&connector, zone.as_deref(), &publication.hostname)
                    .await?;
            }
        }
        if let Some(handle) = self.handle_of(&connector)? {
            self.provider_for(&connector).await?.delete(&handle).await?;
        }
        let generated_hosts = connector.environment_hosts;
        // Its generated environment hosts go with the row (ADR 068 D3).
        ingress_connectors::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        if generated_hosts {
            if let Err(error) = self
                .db
                .execute_unprepared("NOTIFY route_table_changes, 'environment_hosts'")
                .await
            {
                warn!(%error, connector_id = id, "could not notify route table listeners");
            }
        }
        info!(connector_id = id, "ingress connector deleted");
        Ok(())
    }

    /// Every hostname connector `id` publishes or would publish now: its
    /// publications, and what the plan sends through it (with
    /// `auto_publish`, hostnames nobody chose for it by hand).
    pub async fn hostnames_served(&self, id: i32) -> Result<BTreeSet<String>, IngressError> {
        let mut hosts: BTreeSet<String> = ingress_publications::Entity::find()
            .filter(ingress_publications::Column::ConnectorId.eq(id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|p| p.hostname.trim_end_matches('.').to_ascii_lowercase())
            .collect();
        let plan = self.plan().await?;
        hosts.extend(
            plan.servable_hosts()
                .filter(|host| matches!(plan.decide(host), Decision::Publish(c) if c == id))
                .cloned(),
        );
        Ok(hosts)
    }

    /// Finish every removal an interrupted undo or delete left `deleting`.
    async fn finish_deletions(&self) {
        let deleting = match ingress_connectors::Entity::find()
            .filter(ingress_connectors::Column::Status.eq(STATUS_DELETING))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "could not read connectors being removed");
                return;
            }
        };
        for connector in deleting {
            if let Err(error) = self
                .finish_deletion(connector.id, connector.external_id.as_deref())
                .await
            {
                warn!(connector_id = connector.id, %error, "could not finish removing a tunnel yet; retrying next pass");
            }
        }
    }

    /// Check a placement: every id is the control plane (ADR-107 D1) or an
    /// active worker.
    async fn validate_nodes(&self, node_ids: &[i32]) -> Result<(), IngressError> {
        let workers: Vec<i32> = node_ids
            .iter()
            .copied()
            .filter(|id| *id != CONTROL_PLANE_NODE_ID)
            .collect();
        let found = if workers.is_empty() {
            Vec::new()
        } else {
            nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(workers.iter().copied()))
                .all(self.db.as_ref())
                .await?
        };
        let facts: Vec<NodeFacts<'_>> = found.iter().map(NodeFacts::of).collect();
        check_placement(node_ids, &facts)?;
        if node_ids.contains(&CONTROL_PLANE_NODE_ID) {
            self.control_plane_tunnels()?;
        }
        if workers.is_empty() {
            return Ok(());
        }
        let node_ids = workers.as_slice();
        // A tunnel exposes a worker as much as an open port does, so only a
        // worker that delegated public ingress takes one (ADR 058 D7). Agents
        // that report no mode predate the rule. The agent enforces this too.
        let modes = node_public_ingress::Entity::find()
            .filter(node_public_ingress::Column::NodeId.is_in(node_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        for row in modes {
            let mode = row.mode.as_deref().unwrap_or("delegated");
            if mode != "delegated" {
                let name = found
                    .iter()
                    .find(|n| n.id == row.node_id)
                    .map_or_else(|| row.node_id.to_string(), |n| n.name.clone());
                return Err(IngressError::NotAllowed(format!(
                    "Worker '{name}' does not let Temps expose it (its public ingress is '{mode}'). On that worker, run `temps agent ingress delegated` as root, then try again."
                )));
            }
        }
        Ok(())
    }

    async fn replace_nodes(&self, connector_id: i32, node_ids: &[i32]) -> Result<(), IngressError> {
        let wanted: BTreeSet<i32> = node_ids.iter().copied().collect();
        // Recorded before a new node can fetch the secret, so it is rotated
        // once that node leaves even if it leaves right away.
        self.add_token_holders(connector_id, &wanted).await?;
        let existing = ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::ConnectorId.eq(connector_id))
            .all(self.db.as_ref())
            .await?;
        for row in &existing {
            if !wanted.contains(&row.node_id) {
                ingress_connector_nodes::Entity::delete_by_id(row.id)
                    .exec(self.db.as_ref())
                    .await?;
            }
        }
        for node_id in wanted {
            if existing.iter().any(|row| row.node_id == node_id) {
                continue;
            }
            ingress_connector_nodes::ActiveModel {
                connector_id: Set(connector_id),
                node_id: Set(node_id),
                state: Set("pending".into()),
                created_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await?;
        }
        Ok(())
    }

    /// Create or adopt the provider-side tunnel and store the outcome on the
    /// row, so a failure is visible on the connector rather than lost.
    async fn ensure(&self, connector: ingress_connectors::Model) -> Result<(), IngressError> {
        if is_deleting(&connector) {
            return Ok(());
        }
        let outcome = async {
            let provider = self.provider_for(&connector).await?;
            let existing = self.handle_of(&connector)?;
            let instance_id = self.records.instance_id().await?;
            provider
                .ensure(&TunnelContext {
                    connector_id: connector.id,
                    name: &connector.name,
                    domain: &connector.domain,
                    edge_port: connector.edge_port as u16,
                    instance_id: &instance_id,
                    existing,
                })
                .await
        }
        .await;
        // Settings the service owns may have changed while the provider was
        // called (token holders); keep the stored ones.
        let _guard = self.settings_lock.lock().await;
        let txn = self.db.begin().await?;
        let current = Self::find_for_update(&txn, connector.id)
            .await
            .unwrap_or_else(|_| connector.clone());
        if is_deleting(&current) {
            drop(txn);
            // Removed meanwhile: nothing it made is kept.
            if let Ok(handle) = &outcome {
                if let Ok(provider) = self.provider_for(&connector).await {
                    let _ = provider.delete(handle).await;
                }
            }
            return Ok(());
        }
        let mut row: ingress_connectors::ActiveModel = connector.clone().into();
        row.updated_at = Set(Utc::now());
        match outcome {
            Ok(handle) => {
                let secret = self
                    .encryption
                    .encrypt_string(&handle.secret)
                    .map_err(|e| IngressError::Encryption(e.to_string()))?;
                let replaced = tunnel_replaced(current.external_id.as_deref(), &handle.external_id);
                row.external_id = Set(Some(handle.external_id));
                row.settings = Set(keep_setup(&current.settings, handle.settings));
                row.secret_encrypted = Set(Some(secret));
                row.status = Set("ready".into());
                row.last_error = Set(None);
                row.update(&txn).await?;
                if replaced {
                    // Every record points at the tunnel that is gone. Marked
                    // stale, they are published again through the new one (or
                    // removed) by the reconcile that follows.
                    let stale = ingress_publications::Entity::update_many()
                        .col_expr(ingress_publications::Column::State, Expr::value(STALE))
                        .col_expr(
                            ingress_publications::Column::PublishedAt,
                            Expr::value(Option::<DBDateTime>::None),
                        )
                        .col_expr(
                            ingress_publications::Column::UpdatedAt,
                            Expr::value(Utc::now()),
                        )
                        .filter(ingress_publications::Column::ConnectorId.eq(connector.id))
                        .exec(&txn)
                        .await?;
                    info!(
                        connector_id = connector.id,
                        publications = stale.rows_affected,
                        "tunnel was replaced; republishing its hostnames"
                    );
                }
                txn.commit().await?;
                Ok(())
            }
            Err(error) => {
                row.status = Set("error".into());
                row.last_error = Set(Some(error.to_string()));
                row.update(&txn).await?;
                txn.commit().await?;
                Err(error)
            }
        }
    }

    // ----- secret rotation ---------------------------------------------

    /// Record `node_ids` as holders of the connector's secret, before any of
    /// them can fetch it.
    async fn add_token_holders(
        &self,
        connector_id: i32,
        node_ids: &BTreeSet<i32>,
    ) -> Result<(), IngressError> {
        let assigned: BTreeSet<i32> = ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::ConnectorId.eq(connector_id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|row| row.node_id)
            .collect();
        self.change_token_holders(
            connector_id,
            |stored| holders_with(stored, &assigned, node_ids),
            None,
        )
        .await
    }

    /// Re-read the connector and replace its token holders with
    /// `change(stored)`, storing `secret` with them when one was issued.
    async fn change_token_holders(
        &self,
        connector_id: i32,
        change: impl FnOnce(Option<BTreeSet<i32>>) -> BTreeSet<i32>,
        secret: Option<String>,
    ) -> Result<(), IngressError> {
        let _guard = self.settings_lock.lock().await;
        let txn = self.db.begin().await?;
        let connector = Self::find_for_update(&txn, connector_id).await?;
        let stored = token_holders(&connector.settings);
        let holders = change(stored.clone());
        if secret.is_none() && stored.as_ref() == Some(&holders) {
            return Ok(());
        }
        let mut settings = connector.settings.clone();
        if let serde_json::Value::Object(map) = &mut settings {
            map.insert(TOKEN_HOLDERS_KEY.into(), serde_json::json!(holders));
        }
        let rotated = secret.is_some();
        let clear_error = rotated
            && !is_deleting(&connector)
            && connector
                .last_error
                .as_deref()
                .is_some_and(|e| e.starts_with(ROTATION_ERROR));
        let mut row: ingress_connectors::ActiveModel = connector.into();
        row.settings = Set(settings);
        if let Some(secret) = secret {
            row.secret_encrypted = Set(Some(secret));
            row.updated_at = Set(Utc::now());
        }
        if clear_error {
            row.status = Set("ready".into());
            row.last_error = Set(None);
        }
        row.update(&txn).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Rotate the secret of every tunnel a node that held it no longer runs
    /// (removed from the connector, or deleted from the cluster). The new
    /// secret changes the spec revision, so the nodes that stay restart with
    /// it; the provider also cuts the departed nodes' open connections, or
    /// every connection when it cannot tell them apart. The departed nodes
    /// stop counting as holders only once both have happened.
    async fn revoke_departed(&self) -> Result<(), IngressError> {
        let connectors = live_connectors(
            ingress_connectors::Entity::find()
                .order_by_asc(ingress_connectors::Column::Id)
                .all(self.db.as_ref())
                .await?,
        );
        if connectors.is_empty() {
            return Ok(());
        }
        let assignments = ingress_connector_nodes::Entity::find()
            .all(self.db.as_ref())
            .await?;
        for connector in connectors {
            let rows: Vec<&ingress_connector_nodes::Model> = assignments
                .iter()
                .filter(|row| row.connector_id == connector.id)
                .collect();
            let current: BTreeSet<i32> = rows.iter().map(|row| row.node_id).collect();
            let Some(holders) = token_holders(&connector.settings) else {
                self.change_token_holders(
                    connector.id,
                    |stored| holders_with(stored, &current, &BTreeSet::new()),
                    None,
                )
                .await?;
                continue;
            };
            let departed: BTreeSet<i32> = holders.difference(&current).copied().collect();
            if departed.is_empty() {
                if !current.is_subset(&holders) {
                    self.add_token_holders(connector.id, &current).await?;
                }
                continue;
            }
            let Some(handle) = self.handle_of(&connector)? else {
                // No secret was ever issued, so there is nothing to revoke.
                self.change_token_holders(
                    connector.id,
                    |stored| holders_after_revoke(stored, &departed, &current),
                    None,
                )
                .await?;
                continue;
            };
            let keep = connections_to_keep(&rows);
            let outcome = async {
                self.provider_for(&connector)
                    .await?
                    .rotate_secret(&handle, &keep)
                    .await
            }
            .await;
            let departed_list: Vec<i32> = departed.iter().copied().collect();
            match outcome {
                Ok(Some(secret)) => {
                    let sealed = self
                        .encryption
                        .encrypt_string(&secret)
                        .map_err(|e| IngressError::Encryption(e.to_string()))?;
                    info!(
                        connector_id = connector.id,
                        departed = ?departed_list,
                        cut_all = keep.is_empty(),
                        "rotated tunnel secret after nodes left"
                    );
                    self.change_token_holders(
                        connector.id,
                        |stored| holders_after_revoke(stored, &departed, &current),
                        Some(sealed),
                    )
                    .await?;
                }
                Ok(None) => {
                    let names = self.node_names(&departed_list).await?;
                    warn!(
                        connector_id = connector.id,
                        departed = ?departed_list,
                        "tunnel provider cannot rotate its secret"
                    );
                    self.record_rotation_error(
                        connector,
                        format!(
                            "{ROTATION_ERROR}: this provider cannot rotate it, so {names} can still run this tunnel. Delete the connector and create it again to cut them off."
                        ),
                    )
                    .await?;
                }
                Err(error) => {
                    let names = self.node_names(&departed_list).await?;
                    warn!(connector_id = connector.id, departed = ?departed_list, %error, "could not rotate tunnel secret");
                    self.record_rotation_error(
                        connector,
                        format!(
                            "{ROTATION_ERROR} after {names} stopped running this tunnel, so they can still run it: {error} Temps retries every minute; sync the connector to retry now."
                        ),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn record_rotation_error(
        &self,
        connector: ingress_connectors::Model,
        message: String,
    ) -> Result<(), IngressError> {
        if connector.last_error.as_deref() == Some(message.as_str()) {
            return Ok(());
        }
        // Never over a `deleting` status set since the row was read.
        ingress_connectors::Entity::update_many()
            .col_expr(ingress_connectors::Column::Status, Expr::value("error"))
            .col_expr(
                ingress_connectors::Column::LastError,
                Expr::value(Some(message)),
            )
            .col_expr(
                ingress_connectors::Column::UpdatedAt,
                Expr::value(Utc::now()),
            )
            .filter(ingress_connectors::Column::Id.eq(connector.id))
            .filter(ingress_connectors::Column::Status.ne(STATUS_DELETING))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// `worker 'a', node 7`: the departed nodes, by name while they exist.
    async fn node_names(&self, ids: &[i32]) -> Result<String, IngressError> {
        let found = nodes::Entity::find()
            .filter(nodes::Column::Id.is_in(ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        Ok(ids
            .iter()
            .map(|id| {
                found
                    .iter()
                    .find(|n| n.id == *id)
                    .map_or_else(|| format!("node {id}"), |n| format!("worker '{}'", n.name))
            })
            .collect::<Vec<_>>()
            .join(", "))
    }

    // ----- publications ------------------------------------------------

    /// What the tunnels should publish right now, from the connectors, the
    /// operator's choices and what workers serve.
    async fn plan(&self) -> Result<Plan, IngressError> {
        let models = live_connectors(
            ingress_connectors::Entity::find()
                .order_by_asc(ingress_connectors::Column::Id)
                .all(self.db.as_ref())
                .await?,
        );
        let mut placements: HashMap<i32, Placement> = HashMap::new();
        for row in ingress_connector_nodes::Entity::find()
            .all(self.db.as_ref())
            .await?
        {
            placements
                .entry(row.connector_id)
                .or_default()
                .add(row.node_id);
        }
        let mut zones = self.zones_of(&models).await?;
        let connectors: Vec<PlannedConnector> = models
            .into_iter()
            .map(|model| PlannedConnector {
                placement: placements.remove(&model.id).unwrap_or_default(),
                zone: zones.remove(&model.id).flatten(),
                model,
            })
            .collect();
        let mut choices: HashMap<String, Option<i32>> = ingress_hostname_choices::Entity::find()
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|choice| (choice.hostname, choice.connector_id))
            .collect();
        let gate = self.request_policy.get();
        // A tunnel publishes a host for every node that runs it, whether or
        // not that node's agent runs Shield (ADR 066, D10), so a host whose
        // environment acts on verdicts stays on the control plane.
        let routes = temps_routes::route_sync::worker_public_routes(
            self.db.as_ref(),
            &self.peer_table,
            gate.is_none_or(|gate| !gate.supports_worker_ingress()),
            false,
        )
        .await
        .map_err(|(_, error)| IngressError::Database(error))?;
        // ADR 068 D3: a host a tunnel generated for an environment, and that
        // environment's service hosts, go through that tunnel. A choice the
        // operator made for one of them wins.
        let generated: HashMap<String, i32> = environment_domains::Entity::find()
            .filter(environment_domains::Column::GeneratedByConnectorId.is_not_null())
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .filter_map(|row| {
                row.generated_by_connector_id
                    .map(|id| (row.domain.trim_end_matches('.').to_ascii_lowercase(), id))
            })
            .collect();
        let served: HashSet<String> = routes
            .routes
            .iter()
            .map(|route| route.host.trim_end_matches('.').to_ascii_lowercase())
            .collect();
        // ADR-107 D4: the control-plane proxy serves every public hostname
        // in its route table, applying every policy that keeps one off
        // workers. Only read when a tunnel runs there.
        let served_by_control_plane = if connectors.iter().any(|c| c.placement.control_plane) {
            self.control_plane_hostnames()
        } else {
            HashSet::new()
        };
        // A generated host only the control plane serves belongs to its
        // tunnel as much as one workers serve.
        assign_generated(
            &mut choices,
            &generated,
            served.iter().chain(&served_by_control_plane),
        );
        let mut blocking = routes.blocking_policies;
        if gate.is_none() {
            blocking = vec!["the control plane is still starting".into()];
        }
        Ok(Plan {
            connectors,
            choices,
            served,
            served_by_control_plane,
            blocking,
        })
    }

    /// Every public hostname the control-plane proxy answers (ADR-107 D4):
    /// its routes, and sleeping on-demand environments, which have no route
    /// until a visit through the proxy wakes them.
    fn control_plane_hostnames(&self) -> HashSet<String> {
        self.peer_table
            .public_hostnames()
            .into_iter()
            .chain(self.peer_table.public_sleeping_hostnames())
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
            .collect()
    }

    /// Bring every tunnel's DNS records in line with the plan.
    pub async fn reconcile_all(&self) {
        let _guard = self.reconcile_lock.lock().await;
        if let Some(sync) = self.environment_hosts.get() {
            sync.sync().await;
        }
        if let Err(error) = self.reconcile().await {
            warn!(%error, "ingress connector reconcile failed");
        }
    }

    async fn reconcile(&self) -> Result<(), IngressError> {
        self.finish_deletions().await;
        self.revoke_departed().await?;
        let plan = self.plan().await?;
        let rows = ingress_publications::Entity::find()
            .all(self.db.as_ref())
            .await?;
        if plan.connectors.is_empty() && rows.is_empty() {
            return Ok(());
        }
        let desired: HashMap<&str, i32> = plan
            .servable_hosts()
            .filter_map(|host| match plan.decide(host) {
                Decision::Publish(id) => Some((host.as_str(), id)),
                Decision::Skip { .. } => None,
            })
            .collect();

        // Withdraw first: a hostname moving between tunnels frees its row,
        // and its record is replaced in place by the new tunnel's.
        let mut kept: HashMap<String, ingress_publications::Model> = HashMap::new();
        for row in rows {
            match desired.get(row.hostname.as_str()) {
                Some(id) if *id == row.connector_id => {
                    kept.insert(row.hostname.clone(), row);
                }
                Some(id) => {
                    info!(host = %row.hostname, from = row.connector_id, to = id, "moving hostname to another tunnel");
                    ingress_publications::Entity::delete_by_id(row.id)
                        .exec(self.db.as_ref())
                        .await?;
                }
                None => {
                    let Some(connector) = plan.connector(row.connector_id) else {
                        continue;
                    };
                    // A tunnel in trouble keeps its records: a provider outage
                    // must not unpublish everything it carries.
                    if connector.model.status != "ready" || connector.zone.is_none() {
                        continue;
                    }
                    if has_record(&row.state) {
                        if let Ok(provider) = self.provider_for(&connector.model).await {
                            if let Some(handle) = self.handle_of(&connector.model)? {
                                if let Err(error) = provider.unpublish(&handle, &row.hostname).await
                                {
                                    warn!(host = %row.hostname, %error, "tunnel provider refused to unpublish");
                                }
                            }
                        }
                        if let Err(error) = self
                            .remove_record(
                                &connector.model,
                                connector.zone.as_deref(),
                                &row.hostname,
                            )
                            .await
                        {
                            warn!(host = %row.hostname, %error, "could not remove tunnel record");
                            continue;
                        }
                    }
                    ingress_publications::Entity::delete_by_id(row.id)
                        .exec(self.db.as_ref())
                        .await?;
                    info!(connector_id = row.connector_id, host = %row.hostname, "unpublished hostname");
                }
            }
        }

        let mut providers: HashMap<i32, Result<Box<dyn TunnelProvider>, String>> = HashMap::new();
        for (host, id) in desired {
            let existing = kept.get(host);
            if existing.is_some_and(|row| row.state == "published") {
                continue;
            }
            let Some(connector) = plan.connector(id) else {
                continue;
            };
            let Some(handle) = self.handle_of(&connector.model)? else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(slot) = providers.entry(id) {
                slot.insert(
                    self.provider_for(&connector.model)
                        .await
                        .map_err(|error| error.to_string()),
                );
            }
            let outcome = match &providers[&id] {
                Ok(provider) => {
                    self.publish(
                        &connector.model,
                        connector.zone.as_deref(),
                        provider.as_ref(),
                        &handle,
                        host,
                    )
                    .await
                }
                Err(error) => Err(IngressError::Provider(error.clone())),
            };
            self.save_publication(id, host, existing, outcome).await?;
        }
        Ok(())
    }

    /// `zone` is the connector's managed zone, looked up once per pass.
    async fn publish(
        &self,
        connector: &ingress_connectors::Model,
        zone: Option<&str>,
        provider: &dyn TunnelProvider,
        handle: &TunnelHandle,
        host: &str,
    ) -> Result<(), IngressError> {
        let zone = zone.ok_or_else(|| not_managed(&connector.domain))?;
        let label = record_label(host, &connector.domain, zone)?;
        match provider.publish(handle, host).await? {
            DnsIntent::Cname { target, proxied } => {
                self.records
                    .set_managed_record(
                        host,
                        DnsRecordRequest {
                            name: label,
                            content: DnsRecordContent::CNAME { target },
                            ttl: Some(1),
                            proxied,
                        },
                        Some(proxied),
                        OwnershipScope::for_controller(DNS_CONTROLLER),
                    )
                    .await
                    .map_err(|error| {
                        IngressError::Provider(format!(
                            "Could not point {host} at the tunnel: {error}. If another Temps automation \
                             (generated hostnames or domain delivery) already manages this record, \
                             turn that off for {} first.",
                            connector.domain
                        ))
                    })?;
            }
            DnsIntent::None => {}
        }
        info!(connector_id = connector.id, %host, "published hostname through tunnel");
        Ok(())
    }

    async fn remove_record(
        &self,
        connector: &ingress_connectors::Model,
        zone: Option<&str>,
        host: &str,
    ) -> Result<(), IngressError> {
        let Some(zone) = zone else {
            return Ok(());
        };
        let Ok(label) = record_label(host, &connector.domain, zone) else {
            return Ok(());
        };
        match self
            .records
            .remove_managed_record(
                host,
                &label,
                DnsRecordType::CNAME,
                OwnershipScope::for_controller(DNS_CONTROLLER),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(temps_dns::DnsError::RecordNotFound(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn save_publication(
        &self,
        connector_id: i32,
        host: &str,
        existing: Option<&ingress_publications::Model>,
        outcome: Result<(), IngressError>,
    ) -> Result<(), IngressError> {
        let now = Utc::now();
        let (state, error, published_at) = match outcome {
            Ok(()) => ("published", None, Some(now)),
            Err(error) => {
                debug!(%host, %error, "publication failed");
                ("error", Some(error.to_string()), None)
            }
        };
        match existing {
            Some(row) => {
                let mut row: ingress_publications::ActiveModel = row.clone().into();
                row.state = Set(state.into());
                row.last_error = Set(error);
                row.published_at = Set(published_at);
                row.updated_at = Set(now);
                row.update(self.db.as_ref()).await?;
            }
            None => {
                ingress_publications::ActiveModel {
                    connector_id: Set(connector_id),
                    hostname: Set(host.into()),
                    state: Set(state.into()),
                    last_error: Set(error),
                    published_at: Set(published_at),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(self.db.as_ref())
                .await?;
            }
        }
        Ok(())
    }

    // ----- node side ---------------------------------------------------

    /// Whether `node_id` runs any connector; its route snapshot then carries
    /// the public routes its tunnel origin listener serves.
    pub async fn node_has_connectors(&self, node_id: i32) -> Result<bool, IngressError> {
        Ok(ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::NodeId.eq(node_id))
            .one(self.db.as_ref())
            .await?
            .is_some())
    }

    pub async fn node_specs(&self, node_id: i32) -> Result<NodeConnectorsResponse, IngressError> {
        let assigned: Vec<i32> = ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::NodeId.eq(node_id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|row| row.connector_id)
            .collect();
        let connectors = live_connectors(
            ingress_connectors::Entity::find()
                .filter(ingress_connectors::Column::Id.is_in(assigned))
                .all(self.db.as_ref())
                .await?,
        );
        let mut specs: Vec<NodeConnectorSpec> = Vec::new();
        for connector in connectors {
            let Some(handle) = self.handle_of(&connector)? else {
                continue;
            };
            let Some(provider) = self.spec_provider(&connector.provider) else {
                continue;
            };
            // Recorded before the secret leaves, so removing this node later
            // always rotates it, whatever the reconciler has seen.
            if !token_holders(&connector.settings).is_some_and(|h| h.contains(&node_id)) {
                self.add_token_holders(connector.id, &BTreeSet::from([node_id]))
                    .await?;
            }
            specs.push(provider.connector_spec(connector.id, &handle, connector.edge_port as u16));
        }
        Ok(NodeConnectorsResponse { connectors: specs })
    }

    pub async fn record_status(
        &self,
        node_id: i32,
        report: NodeConnectorStatusReport,
    ) -> Result<(), IngressError> {
        let now = Utc::now();
        let mut assigned: HashMap<i32, ingress_connector_nodes::Model> =
            ingress_connector_nodes::Entity::find()
                .filter(ingress_connector_nodes::Column::NodeId.eq(node_id))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|row| (row.connector_id, row))
                .collect();
        for status in report.reports {
            let Some(row) = assigned.remove(&status.connector_id) else {
                continue;
            };
            let mut row: ingress_connector_nodes::ActiveModel = row.into();
            row.driver = Set(status.driver);
            row.state = Set(status.state);
            row.ready_connections = Set(status.ready_connections);
            row.connector_ref = Set(status.connector_ref);
            row.last_error = Set(status.last_error);
            row.reported_at = Set(Some(now));
            row.update(self.db.as_ref()).await?;
        }
        Ok(())
    }

    /// Reconcile on every route-table change and once a minute.
    pub fn start_reconciler(self: &Arc<Self>) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let notifier = service.peer_table.generation_notifier();
            loop {
                tokio::select! {
                    _ = notifier.notified() => {
                        // Let a burst of route changes settle.
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
                service.reconcile_all().await;
            }
        });
    }
}

/// What a placement check reads of a `nodes` row.
#[derive(Debug, Clone, Copy)]
struct NodeFacts<'a> {
    id: i32,
    name: &'a str,
    role: &'a str,
    status: &'a str,
}

impl<'a> NodeFacts<'a> {
    fn of(node: &'a nodes::Model) -> Self {
        Self {
            id: node.id,
            name: &node.name,
            role: &node.role,
            status: &node.status,
        }
    }
}

/// What `validate_nodes` decides from the rows alone: the control plane is
/// always accepted, every other id must be an active worker.
fn check_placement(node_ids: &[i32], found: &[NodeFacts<'_>]) -> Result<(), IngressError> {
    for id in node_ids {
        if *id == CONTROL_PLANE_NODE_ID {
            continue;
        }
        match found.iter().find(|n| n.id == *id) {
            None => return Err(IngressError::UnknownNode { node_id: *id }),
            Some(node) if node.role != "worker" => {
                return Err(IngressError::NotAWorker {
                    node_id: node.id,
                    name: node.name.to_string(),
                    role: node.role.to_string(),
                })
            }
            Some(node) if node.status != "active" => {
                return Err(IngressError::InactiveWorker {
                    node_id: node.id,
                    name: node.name.to_string(),
                    status: node.status.to_string(),
                })
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// The name the console shows for a node running a connector.
pub const CONTROL_PLANE_NODE_NAME: &str = "Control plane";

/// Names of the nodes in `ids`, the control plane included, in one query.
async fn node_names(
    db: &sea_orm::DatabaseConnection,
    ids: impl IntoIterator<Item = i32>,
) -> Result<HashMap<i32, String>, sea_orm::DbErr> {
    let ids: BTreeSet<i32> = ids.into_iter().collect();
    let mut names: HashMap<i32, String> = HashMap::new();
    if ids.contains(&CONTROL_PLANE_NODE_ID) {
        names.insert(CONTROL_PLANE_NODE_ID, CONTROL_PLANE_NODE_NAME.to_string());
    }
    let workers: Vec<i32> = ids
        .into_iter()
        .filter(|id| *id != CONTROL_PLANE_NODE_ID)
        .collect();
    if !workers.is_empty() {
        names.extend(
            nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(workers))
                .all(db)
                .await?
                .into_iter()
                .map(|n| (n.id, n.name)),
        );
    }
    Ok(names)
}

fn connector_view(
    connector: ingress_connectors::Model,
    node_rows: Vec<ingress_connector_nodes::Model>,
    names: &HashMap<i32, String>,
    publications: Vec<ingress_publications::Model>,
) -> ConnectorView {
    ConnectorView {
        id: connector.id,
        name: connector.name,
        provider: connector.provider,
        dns_provider_id: connector.dns_provider_id,
        domain: connector.domain,
        external_id: connector.external_id,
        dns_target: connector
            .settings
            .get("cname_target")
            .and_then(|v| v.as_str())
            .map(String::from),
        status: connector.status,
        last_error: connector.last_error,
        edge_port: connector.edge_port,
        auto_publish: connector.auto_publish,
        environment_hosts: connector.environment_hosts,
        host_suffix: connector.host_suffix,
        nodes: node_rows
            .into_iter()
            .map(|n| ConnectorNodeView {
                node_name: names.get(&n.node_id).cloned().unwrap_or_default(),
                node_id: n.node_id,
                driver: n.driver,
                state: n.state,
                ready_connections: n.ready_connections,
                last_error: n.last_error,
                reported_at: n.reported_at,
            })
            .collect(),
        publications: publications
            .into_iter()
            .map(|p| PublicationView {
                hostname: p.hostname,
                state: p.state,
                last_error: p.last_error,
                published_at: p.published_at,
            })
            .collect(),
        created_at: connector.created_at,
    }
}

/// `(page, page_size)`: page from 1, size 20 by default and at most 100.
fn page_bounds(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    (
        page.unwrap_or(1).max(1),
        page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE),
    )
}

/// Whether a publication in `state` has a DNS record out there.
fn has_record(state: &str) -> bool {
    state == "published" || state == STALE
}

/// The checked `host_suffix` for a new tunnel: only a provider set up with a
/// domain names hosts under one.
fn host_suffix_for(
    info: &ProviderInfo,
    setup: &ProviderSetup,
    requested: Option<&str>,
) -> Result<Option<String>, IngressError> {
    let suffix = crate::reachability::environment_hosts::normalize_host_suffix(requested)
        .map_err(IngressError::Validation)?;
    match (&suffix, &setup.domain) {
        (Some(suffix), None) => Err(IngressError::Validation(format!(
            "{} is not set up with a domain, so it has no hostnames to add the suffix \
             '{suffix}' to. Leave the hostname suffix empty.",
            info.name
        ))),
        _ => Ok(suffix),
    }
}

/// The record name of `host` inside `domain`. Cloudflare's free certificate
/// covers one level under the zone, and tunnel records are always proxied,
/// so deeper names are refused with what to use instead.
pub(crate) fn record_label(host: &str, domain: &str, zone: &str) -> Result<String, IngressError> {
    if !covers(domain, zone, host) {
        return Err(IngressError::Validation(format!(
            "{host} is not under {domain}"
        )));
    }
    let label = host
        .strip_suffix(&format!(".{zone}"))
        .ok_or_else(|| IngressError::Validation(format!("{host} is not under {zone}")))?;
    // On the zone itself, keep hostnames where Cloudflare's free certificate
    // reaches. A tunnel on a subdomain was chosen knowing that it does not
    // (the console says so), so its hostnames may go deeper.
    if domain == zone && label.contains('.') {
        return Err(IngressError::Validation(format!(
            "{host} is more than one level below {zone}. Cloudflare's free certificate only \
             covers *.{zone}, so use a hostname like {}.{zone}.",
            label.replace('.', "-")
        )));
    }
    Ok(label.to_string())
}

/// The tunnel that generated `host`, or the environment host a Flat service
/// host `<label>--<service>.<domain>` belongs to (ADR 068 D3).
fn generated_connector(generated: &HashMap<String, i32>, host: &str) -> Option<i32> {
    if let Some(id) = generated.get(host) {
        return Some(*id);
    }
    let (first, rest) = host.split_once('.')?;
    let (label, _) = first.split_once("--")?;
    generated.get(&format!("{label}.{rest}")).copied()
}

/// Whether a tunnel on `domain` (in `zone`) publishes `host`: anything under
/// the domain, and a subdomain itself. Never the zone apex.
pub(crate) fn covers(domain: &str, zone: &str, host: &str) -> bool {
    host.ends_with(&format!(".{domain}")) || (host == domain && domain != zone)
}

/// Where a connector runs (ADR-107 D4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Placement {
    control_plane: bool,
    workers: bool,
}

impl Placement {
    fn add(&mut self, node_id: i32) {
        if node_id == CONTROL_PLANE_NODE_ID {
            self.control_plane = true;
        } else {
            self.workers = true;
        }
    }

    fn has_nodes(self) -> bool {
        self.control_plane || self.workers
    }
}

/// ADR 068 D3: each served `hosts` entry a tunnel generated goes through that
/// tunnel, unless the operator chose otherwise for it.
fn assign_generated<'a>(
    choices: &mut HashMap<String, Option<i32>>,
    generated: &HashMap<String, i32>,
    hosts: impl Iterator<Item = &'a String>,
) {
    if generated.is_empty() {
        return;
    }
    for host in hosts {
        if let Some(id) = generated_connector(generated, host) {
            choices.entry(host.clone()).or_insert(Some(id));
        }
    }
}

struct PlannedConnector {
    model: ingress_connectors::Model,
    /// `None` when its domain is no longer managed; it then covers nothing.
    zone: Option<String>,
    placement: Placement,
}

struct Plan {
    connectors: Vec<PlannedConnector>,
    choices: HashMap<String, Option<i32>>,
    /// Hostnames workers serve: all a tunnel with a worker replica can carry,
    /// since the provider spreads requests over every replica.
    served: HashSet<String>,
    /// Hostnames the control plane serves: all a tunnel that runs only on
    /// the control plane can carry. Empty when no tunnel runs there.
    served_by_control_plane: HashSet<String>,
    /// Cluster-wide policies that keep every hostname off workers.
    blocking: Vec<String>,
}

enum Decision {
    Publish(i32),
    Skip { connector: Option<i32>, why: String },
}

impl Plan {
    /// Every hostname some tunnel could carry, each once.
    fn servable_hosts(&self) -> impl Iterator<Item = &String> {
        self.served.iter().chain(
            self.served_by_control_plane
                .iter()
                .filter(|host| !self.served.contains(*host)),
        )
    }

    /// The served hostnames tunnel `id` would publish by itself, with no
    /// choice made for them, once its `auto_publish` is on.
    fn published_automatically_once_on(&mut self, id: i32) -> Vec<String> {
        for connector in &mut self.connectors {
            if connector.model.id == id {
                connector.model.auto_publish = true;
            }
        }
        let placement = self.connector(id).map(|c| c.placement).unwrap_or_default();
        let mut hosts: Vec<String> = self
            .servable_hosts()
            // Only what its own replicas would serve: another tunnel on the
            // control plane widens the servable set beyond what a
            // worker-only tunnel can publish.
            .filter(|host| self.carries(placement, host))
            .filter(|host| {
                self.intent(host)
                    == PublicationIntent::Tunnel {
                        connector_id: id,
                        by_choice: false,
                    }
            })
            .cloned()
            .collect();
        hosts.sort();
        hosts
    }

    /// Whether every replica of a tunnel placed so serves `host` (ADR-107
    /// D4): workers decide when there are any, as they serve less.
    fn carries(&self, placement: Placement, host: &str) -> bool {
        if placement.workers {
            self.served.contains(host)
        } else {
            placement.control_plane && self.served_by_control_plane.contains(host)
        }
    }

    fn connector(&self, id: i32) -> Option<&PlannedConnector> {
        self.connectors.iter().find(|c| c.model.id == id)
    }

    fn candidates(&self, host: &str) -> Vec<&PlannedConnector> {
        self.connectors
            .iter()
            .filter(|c| {
                c.zone
                    .as_deref()
                    .is_some_and(|zone| covers(&c.model.domain, zone, host))
            })
            .collect()
    }

    /// Which tunnel would publish `host`. A choice wins; otherwise the most
    /// specific automatic tunnel covering it, the oldest on a tie, so two
    /// tunnels never fight over a record.
    fn intent(&self, host: &str) -> PublicationIntent {
        match self.choices.get(host) {
            Some(None) => PublicationIntent::ChosenOff,
            Some(Some(id)) => match self.connector(*id) {
                Some(_) => PublicationIntent::Tunnel {
                    connector_id: *id,
                    by_choice: true,
                },
                None => PublicationIntent::ChosenMissing,
            },
            None => {
                let candidates = self.candidates(host);
                let automatic =
                    candidates
                        .iter()
                        .filter(|c| c.model.auto_publish)
                        .max_by(|a, b| {
                            a.model
                                .domain
                                .len()
                                .cmp(&b.model.domain.len())
                                .then(b.model.id.cmp(&a.model.id))
                        });
                match (automatic, candidates.first()) {
                    (Some(connector), _) => PublicationIntent::Tunnel {
                        connector_id: connector.model.id,
                        by_choice: false,
                    },
                    (None, Some(connector)) => PublicationIntent::OnlyChosen {
                        connector_id: connector.model.id,
                    },
                    (None, None) => PublicationIntent::NoTunnel,
                }
            }
        }
    }

    /// Whether the tunnel [`Self::intent`] picks publishes `host` now.
    fn decide(&self, host: &str) -> Decision {
        let skip_unowned = |why: String| Decision::Skip {
            connector: None,
            why,
        };
        let chosen = match self.intent(host) {
            PublicationIntent::ChosenOff => {
                return skip_unowned(
                    "Kept off tunnels by choice. Point its DNS where it should go.".into(),
                )
            }
            PublicationIntent::ChosenMissing => {
                return skip_unowned("The tunnel chosen for it no longer exists.".into())
            }
            PublicationIntent::OnlyChosen { connector_id } => {
                return skip_unowned(format!(
                    "Tunnel '{}' covers it but only publishes hostnames chosen for it. Choose that tunnel for this hostname to publish it.",
                    self.connector(connector_id)
                        .map(|c| c.model.name.as_str())
                        .unwrap_or_default()
                ))
            }
            PublicationIntent::NoTunnel => {
                return skip_unowned("No tunnel's domain covers it.".into())
            }
            PublicationIntent::Tunnel { connector_id, .. } => match self.connector(connector_id) {
                Some(connector) => connector,
                None => return skip_unowned("The tunnel chosen for it no longer exists.".into()),
            },
        };
        let id = chosen.model.id;
        let name = &chosen.model.name;
        let skip = |why: String| Decision::Skip {
            connector: Some(id),
            why,
        };
        let Some(zone) = chosen.zone.as_deref() else {
            return skip(format!(
                "Tunnel '{name}' is on {}, which is no longer a managed domain. Add it again under DNS providers.",
                chosen.model.domain
            ));
        };
        if let Err(error) = record_label(host, &chosen.model.domain, zone) {
            return skip(format!("Tunnel '{name}' cannot publish it: {error}"));
        }
        if chosen.model.status != "ready" {
            return skip(match &chosen.model.last_error {
                Some(error) => format!("Tunnel '{name}' is not ready: {error}"),
                None => format!("Tunnel '{name}' is not ready yet."),
            });
        }
        if !chosen.placement.has_nodes() {
            return skip(format!(
                "Tunnel '{name}' runs nowhere. Run it on the control plane or on a worker from the tunnel's page."
            ));
        }
        if chosen.placement.workers {
            // Every replica must answer it: workers serve less than the
            // control plane, so they decide (ADR-107 D4).
            if !self.carries(chosen.placement, host) {
                return skip(if !self.blocking.is_empty() {
                    format!(
                        "Workers serve no hostnames while {}, so a tunnel on a worker cannot carry it.",
                        self.blocking.join(", ")
                    )
                } else if chosen.placement.control_plane {
                    format!(
                        "Workers do not serve it, and tunnel '{name}' also runs on a worker, which would answer it with a 404. Run tunnel '{name}' on the control plane only to publish it."
                    )
                } else {
                    "Workers do not serve it, so a tunnel on a worker cannot carry it: it runs on the control plane, or needs a redirect, wake-up, attack mode, a security policy or path routing, which only the control plane applies. Run the tunnel on the control plane to publish it.".into()
                });
            }
        } else if !self.carries(chosen.placement, host) {
            return skip(
                "Nothing serves it yet: deploy the environment it belongs to, then it is published."
                    .into(),
            );
        }
        Decision::Publish(id)
    }

    fn view(&self, host: &str, publication: Option<&ingress_publications::Model>) -> HostnameView {
        let (mode, chosen_connector_id) = match self.choices.get(host) {
            None => (HostnameMode::Automatic, None),
            Some(None) => (HostnameMode::Off, None),
            Some(Some(id)) => (HostnameMode::Tunnel, Some(*id)),
        };
        let name_of = |id: i32| self.connector(id).map(|c| c.model.name.clone());
        let (connector_id, state, detail) = match self.decide(host) {
            Decision::Publish(id) => {
                let name = name_of(id).unwrap_or_default();
                match publication.filter(|p| p.connector_id == id) {
                    Some(p) if p.state == "published" => (
                        Some(id),
                        "published",
                        format!("Published through tunnel '{name}'. Cloudflare serves HTTPS."),
                    ),
                    Some(p) if p.state == "error" => (
                        Some(id),
                        "error",
                        p.last_error
                            .clone()
                            .unwrap_or_else(|| "Publishing failed.".into()),
                    ),
                    _ => (
                        Some(id),
                        "pending",
                        format!("Being published through tunnel '{name}'."),
                    ),
                }
            }
            Decision::Skip { connector, why } => match publication {
                Some(p) if p.state == "published" => (
                    Some(p.connector_id),
                    "published",
                    format!(
                        "{why} Its record through tunnel '{}' stays until that is resolved.",
                        name_of(p.connector_id).unwrap_or_default()
                    ),
                ),
                Some(p) if p.state == STALE => (
                    Some(p.connector_id),
                    "error",
                    format!(
                        "{why} Its record still points at the previous instance of tunnel '{}', which no longer exists, until that is resolved.",
                        name_of(p.connector_id).unwrap_or_default()
                    ),
                ),
                _ => (connector, "not_published", why),
            },
        };
        HostnameView {
            hostname: host.to_string(),
            mode,
            chosen_connector_id,
            connector_name: connector_id.and_then(name_of),
            connector_id,
            state: state.into(),
            detail,
            served_by_workers: self.served.contains(host),
            candidates: self
                .candidates(host)
                .into_iter()
                .map(|c| HostnameCandidate {
                    connector_id: c.model.id,
                    name: c.model.name.clone(),
                    domain: c.model.domain.clone(),
                    auto_publish: c.model.auto_publish,
                })
                .collect(),
            published_at: publication.and_then(|p| p.published_at),
        }
    }
}

fn is_dns_name(name: &str) -> bool {
    name.len() <= 253
        && name.split('.').count() >= 2
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

pub(crate) fn normalize_host(hostname: &str) -> Result<String, IngressError> {
    let host = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    if !is_dns_name(&host) {
        return Err(IngressError::Validation(format!(
            "{hostname} is not a valid hostname."
        )));
    }
    Ok(host)
}

/// The nodes recorded as holding the current secret; `None` before the
/// first reconcile records them.
fn token_holders(settings: &serde_json::Value) -> Option<BTreeSet<i32>> {
    serde_json::from_value(settings.get(TOKEN_HOLDERS_KEY)?.clone()).ok()
}

/// The holders once `added` is recorded. Never recorded before, they start
/// from every node `assigned` to the connector.
fn holders_with(
    stored: Option<BTreeSet<i32>>,
    assigned: &BTreeSet<i32>,
    added: &BTreeSet<i32>,
) -> BTreeSet<i32> {
    let mut holders = stored.unwrap_or_else(|| assigned.clone());
    holders.extend(added);
    holders
}

/// The holders once the secret is rotated away from `departed`. Nodes
/// recorded meanwhile are kept, and so is every node still assigned.
fn holders_after_revoke(
    stored: Option<BTreeSet<i32>>,
    departed: &BTreeSet<i32>,
    current: &BTreeSet<i32>,
) -> BTreeSet<i32> {
    let mut holders: BTreeSet<i32> = stored
        .unwrap_or_default()
        .difference(departed)
        .copied()
        .collect();
    holders.extend(current);
    holders
}

/// The connections a rotation keeps: those the staying, running nodes say
/// are theirs. When one of them has not said, nothing tells its connection
/// from a departed node's, so none is kept and every node reconnects with
/// the new secret.
fn connections_to_keep(rows: &[&ingress_connector_nodes::Model]) -> HashSet<String> {
    rows.iter()
        .filter(|row| row.state == "ready")
        .map(|row| row.connector_ref.clone())
        .collect::<Option<HashSet<String>>>()
        .unwrap_or_default()
}

/// Whether `ensure` replaced the tunnel publications point at.
fn not_managed(domain: &str) -> IngressError {
    IngressError::Validation(format!(
        "{domain} is no longer a managed domain, or under one. Add it again under DNS providers."
    ))
}

fn tunnel_replaced(before: Option<&str>, after: &str) -> bool {
    before.is_some_and(|before| before != after)
}

/// The provider's new settings plus the keys the service owns: the setup
/// answers and the token holders.
fn keep_setup(old: &serde_json::Value, new: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut merged) = new else {
        return new;
    };
    for key in [SETUP_KEY, SETUP_SECRETS_KEY, TOKEN_HOLDERS_KEY] {
        if let Some(value) = old.get(key) {
            merged.insert(key.into(), value.clone());
        }
    }
    serde_json::Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replica(
        node_id: i32,
        state: &str,
        connector_ref: Option<&str>,
    ) -> ingress_connector_nodes::Model {
        ingress_connector_nodes::Model {
            id: node_id,
            connector_id: 1,
            node_id,
            driver: Some("process".into()),
            state: state.into(),
            ready_connections: None,
            connector_ref: connector_ref.map(String::from),
            last_error: None,
            reported_at: None,
            created_at: Utc::now(),
        }
    }

    fn set(ids: &[i32]) -> BTreeSet<i32> {
        ids.iter().copied().collect()
    }

    #[test]
    fn rotation_keeps_the_connections_of_running_nodes_that_named_theirs() {
        let a = replica(1, "ready", Some("aaaa"));
        let b = replica(2, "starting", None);
        let keep = connections_to_keep(&[&a, &b]);
        assert_eq!(keep, HashSet::from(["aaaa".to_string()]));
    }

    #[test]
    fn rotation_cuts_every_connection_when_a_running_node_did_not_name_its_own() {
        let a = replica(1, "ready", Some("aaaa"));
        let b = replica(2, "ready", None);
        assert!(connections_to_keep(&[&a, &b]).is_empty());
    }

    #[test]
    fn holders_start_from_the_assignment_and_only_grow() {
        assert_eq!(
            holders_with(None, &set(&[1, 2]), &set(&[3])),
            set(&[1, 2, 3])
        );
        assert_eq!(
            holders_with(Some(set(&[4])), &set(&[1]), &set(&[3])),
            set(&[3, 4])
        );
    }

    #[test]
    fn a_revoke_drops_only_the_departed_and_keeps_nodes_recorded_meanwhile() {
        // 2 left; 5 fetched the secret while the provider rotated it.
        let after = holders_after_revoke(Some(set(&[1, 2, 5])), &set(&[2]), &set(&[1]));
        assert_eq!(after, set(&[1, 5]));
        assert_eq!(
            holders_after_revoke(None, &set(&[2]), &set(&[1])),
            set(&[1])
        );
    }

    fn setup_with_domain(domain: Option<&str>) -> ProviderSetup {
        ProviderSetup {
            dns_provider: None,
            domain: domain.map(str::to_string),
            settings: BTreeMap::new(),
            secrets: BTreeMap::new(),
        }
    }

    /// A tunnel's host suffix is checked when it is created: normalised,
    /// refused when malformed, and refused for a provider with no domain to
    /// put hostnames under.
    #[test]
    fn a_new_tunnels_host_suffix_is_checked() {
        use crate::registry::ProviderFactory;
        let info = crate::cloudflare::CloudflareTunnelFactory.info();
        let zone = setup_with_domain(Some("example.com"));
        assert_eq!(host_suffix_for(&info, &zone, None).unwrap(), None);
        assert_eq!(host_suffix_for(&info, &zone, Some("  ")).unwrap(), None);
        assert_eq!(
            host_suffix_for(&info, &zone, Some("Lab")).unwrap(),
            Some("lab".to_string())
        );
        let malformed = host_suffix_for(&info, &zone, Some("lab.home")).unwrap_err();
        assert!(
            matches!(&malformed, IngressError::Validation(m) if m.contains("'lab.home'")),
            "{malformed}"
        );
        let no_domain = host_suffix_for(&info, &setup_with_domain(None), Some("lab")).unwrap_err();
        assert!(
            matches!(&no_domain, IngressError::Validation(m)
                if m.contains("not set up with a domain") && m.contains("'lab'")),
            "{no_domain}"
        );
    }

    fn connector_model(id: i32, domain: &str, auto_publish: bool) -> ingress_connectors::Model {
        ingress_connectors::Model {
            id,
            name: format!("tunnel-{id}"),
            provider: "cloudflare_tunnel".into(),
            dns_provider_id: Some(3),
            domain: domain.into(),
            external_id: None,
            settings: serde_json::json!({}),
            secret_encrypted: None,
            edge_port: 443,
            auto_publish,
            environment_hosts: false,
            host_suffix: None,
            status: "ready".into(),
            last_error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// ADR 068 D3: a generated environment host, and its Flat service
    /// hosts, belong to the tunnel that generated it; nothing else does.
    #[test]
    fn generated_hosts_and_their_service_hosts_map_to_their_tunnel() {
        let generated: HashMap<String, i32> = [
            ("shop-staging.tunnel.example.com".to_string(), 7),
            ("shop-production-a1b2c3.tunnel.example.com".to_string(), 7),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            generated_connector(&generated, "shop-staging.tunnel.example.com"),
            Some(7)
        );
        assert_eq!(
            generated_connector(&generated, "shop-staging--api.tunnel.example.com"),
            Some(7)
        );
        assert_eq!(
            generated_connector(&generated, "shop-production-a1b2c3--web.tunnel.example.com"),
            Some(7)
        );
        assert_eq!(
            generated_connector(&generated, "shop-staging.other.example.com"),
            None
        );
        assert_eq!(
            generated_connector(&generated, "blog.tunnel.example.com"),
            None
        );
        assert_eq!(
            generated_connector(&generated, "api--shop-staging.tunnel.example.com"),
            None,
            "the Standard form is not a tunnel service host"
        );
    }

    /// ADR 064 D9: what turning `auto_publish` on would publish, which the
    /// guard checks against private-only allow lists before it is saved.
    #[test]
    fn auto_publish_would_publish_only_unchosen_hostnames_under_its_domain() {
        let mut plan = Plan {
            connectors: vec![
                PlannedConnector {
                    model: connector_model(1, "example.com", false),
                    zone: Some("example.com".into()),
                    placement: Placement {
                        control_plane: false,
                        workers: true,
                    },
                },
                PlannedConnector {
                    model: connector_model(2, "apps.example.com", true),
                    zone: Some("example.com".into()),
                    placement: Placement {
                        control_plane: false,
                        workers: true,
                    },
                },
            ],
            choices: HashMap::from([("chosen.example.com".to_string(), None)]),
            served: [
                "internal.example.com",
                "chosen.example.com",
                "x.apps.example.com",
                "other.example.org",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            served_by_control_plane: HashSet::new(),
            blocking: Vec::new(),
        };
        // A choice wins, a more specific automatic tunnel wins, and other
        // domains are not its to publish.
        assert_eq!(
            plan.published_automatically_once_on(1),
            vec!["internal.example.com".to_string()]
        );
    }

    /// A worker-only tunnel's guard looks only at hostnames its workers
    /// serve, even when another tunnel on the control plane serves more.
    #[test]
    fn auto_publish_on_a_worker_tunnel_ignores_what_only_the_control_plane_serves() {
        let mut plan = Plan {
            connectors: vec![
                PlannedConnector {
                    model: connector_model(1, "example.com", false),
                    zone: Some("example.com".into()),
                    placement: Placement {
                        control_plane: false,
                        workers: true,
                    },
                },
                PlannedConnector {
                    model: connector_model(2, "other.example.org", true),
                    zone: Some("example.org".into()),
                    placement: Placement {
                        control_plane: true,
                        workers: false,
                    },
                },
            ],
            choices: HashMap::new(),
            served: HashSet::from(["on-worker.example.com".to_string()]),
            served_by_control_plane: HashSet::from([
                "on-worker.example.com".to_string(),
                "on-control-plane.example.com".to_string(),
            ]),
            blocking: Vec::new(),
        };
        assert_eq!(
            plan.published_automatically_once_on(1),
            vec!["on-worker.example.com".to_string()]
        );
    }

    /// ADR 068 D3 with ADR-107 D4: a generated host the control plane alone
    /// serves still belongs to the tunnel that generated it, and an
    /// operator's choice still wins.
    #[test]
    fn a_generated_host_only_the_control_plane_serves_keeps_its_tunnel() {
        let generated = HashMap::from([("shop.tunnel.example.com".to_string(), 7)]);
        let mut choices = HashMap::from([("chosen.tunnel.example.com".to_string(), None)]);
        let served: HashSet<String> = HashSet::new();
        let control_plane: HashSet<String> = HashSet::from([
            "shop.tunnel.example.com".to_string(),
            "unrelated.example.com".to_string(),
        ]);
        assign_generated(
            &mut choices,
            &generated,
            served.iter().chain(&control_plane),
        );
        assert_eq!(choices.get("shop.tunnel.example.com"), Some(&Some(7)));
        assert_eq!(choices.get("unrelated.example.com"), None);
        assert_eq!(choices.get("chosen.tunnel.example.com"), Some(&None));
    }

    fn node<'a>(id: i32, name: &'a str, role: &'a str, status: &'a str) -> NodeFacts<'a> {
        NodeFacts {
            id,
            name,
            role,
            status,
        }
    }

    /// ADR-107 D1: node 0 is the control plane, which has no `nodes` row.
    #[test]
    fn the_control_plane_is_a_valid_placement_without_a_node_row() {
        check_placement(&[CONTROL_PLANE_NODE_ID], &[]).expect("control plane");
        check_placement(&[], &[]).expect("no placement");
    }

    #[test]
    fn an_unknown_node_is_refused_with_its_id_and_the_control_plane_hint() {
        let workers = [node(4, "worker-a", "worker", "active")];
        let error = check_placement(&[CONTROL_PLANE_NODE_ID, 4, 17], &workers)
            .expect_err("node 17 has no row");
        assert!(matches!(error, IngressError::UnknownNode { node_id: 17 }));
        assert_eq!(
            error.to_string(),
            "Node 17 does not exist (use 0 for the control plane)."
        );
        let problem = temps_core::problemdetails::Problem::from(error);
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn non_workers_and_inactive_workers_keep_their_messages() {
        let rows = [
            node(5, "builder-a", "builder", "active"),
            node(6, "worker-b", "worker", "offline"),
        ];
        let error = check_placement(&[CONTROL_PLANE_NODE_ID, 5], &rows).expect_err("builder");
        assert!(matches!(error, IngressError::NotAWorker { node_id: 5, .. }));
        assert_eq!(
            error.to_string(),
            "Node 'builder-a' is a builder, not a worker; only workers run connectors."
        );
        let error = check_placement(&[6], &rows).expect_err("offline");
        assert!(matches!(
            error,
            IngressError::InactiveWorker { node_id: 6, .. }
        ));
        assert_eq!(
            error.to_string(),
            "Worker 'worker-b' is offline, so it cannot run a connector. Bring it back (its agent must be running and connected) or pick another worker."
        );
    }

    #[test]
    fn the_control_plane_and_an_active_worker_together_are_accepted() {
        let rows = [node(4, "worker-a", "worker", "active")];
        check_placement(&[CONTROL_PLANE_NODE_ID, 4], &rows).expect("mixed placement");
    }

    fn placement_plan(placement: Placement) -> Plan {
        Plan {
            connectors: vec![PlannedConnector {
                model: connector_model(1, "example.com", true),
                zone: Some("example.com".into()),
                placement,
            }],
            choices: HashMap::new(),
            served: HashSet::from(["on-worker.example.com".to_string()]),
            served_by_control_plane: HashSet::from([
                "on-worker.example.com".to_string(),
                "on-control-plane.example.com".to_string(),
            ]),
            blocking: Vec::new(),
        }
    }

    fn publishes(plan: &Plan, host: &str) -> bool {
        matches!(plan.decide(host), Decision::Publish(1))
    }

    fn why_not(plan: &Plan, host: &str) -> String {
        match plan.decide(host) {
            Decision::Skip { why, .. } => why,
            Decision::Publish(id) => panic!("{host} is published through {id}"),
        }
    }

    /// ADR-107 D4: a tunnel only on the control plane carries what the
    /// control-plane proxy serves, which includes what workers serve.
    #[test]
    fn a_control_plane_tunnel_publishes_what_the_control_plane_serves() {
        let plan = placement_plan(Placement {
            control_plane: true,
            workers: false,
        });
        assert!(publishes(&plan, "on-control-plane.example.com"));
        assert!(publishes(&plan, "on-worker.example.com"));
        assert!(why_not(&plan, "nowhere.example.com").contains("Nothing serves it"));
        let mut hosts: Vec<&String> = plan.servable_hosts().collect();
        hosts.sort();
        assert_eq!(
            hosts,
            vec!["on-control-plane.example.com", "on-worker.example.com"],
            "each servable host once"
        );
    }

    /// Every replica must answer: with a worker in the placement, only what
    /// workers serve is published, and the reason says how to get the rest.
    #[test]
    fn a_mixed_tunnel_publishes_only_what_workers_serve() {
        let plan = placement_plan(Placement {
            control_plane: true,
            workers: true,
        });
        assert!(publishes(&plan, "on-worker.example.com"));
        let why = why_not(&plan, "on-control-plane.example.com");
        assert!(why.contains("also runs on a worker"), "{why}");
        assert!(why.contains("control plane only"), "{why}");
    }

    #[test]
    fn a_worker_tunnel_still_publishes_only_what_workers_serve() {
        let plan = placement_plan(Placement {
            control_plane: false,
            workers: true,
        });
        assert!(publishes(&plan, "on-worker.example.com"));
        let why = why_not(&plan, "on-control-plane.example.com");
        assert!(why.contains("Run the tunnel on the control plane"), "{why}");
    }

    /// Worker-wide blocking policies do not hold back a tunnel that runs
    /// only on the control plane, which applies those policies itself.
    #[test]
    fn worker_blocking_policies_do_not_stop_a_control_plane_tunnel() {
        let mut plan = placement_plan(Placement {
            control_plane: true,
            workers: false,
        });
        // While a policy blocks workers they serve nothing.
        plan.blocking = vec!["the control plane is still starting".into()];
        plan.served.clear();
        assert!(publishes(&plan, "on-control-plane.example.com"));
        assert!(publishes(&plan, "on-worker.example.com"));
        plan.connectors[0].placement.workers = true;
        assert!(why_not(&plan, "on-worker.example.com").contains("still starting"));
    }

    #[test]
    fn a_tunnel_placed_nowhere_says_where_to_run_it() {
        let plan = placement_plan(Placement::default());
        assert!(why_not(&plan, "on-worker.example.com").contains("control plane or on a worker"));
    }

    #[test]
    fn only_a_different_tunnel_id_marks_publications_stale() {
        assert!(tunnel_replaced(Some("old"), "new"));
        assert!(!tunnel_replaced(Some("same"), "same"));
        assert!(!tunnel_replaced(None, "first"));
    }

    #[test]
    fn stale_publications_still_have_a_record_to_remove() {
        assert!(has_record("published"));
        assert!(has_record(STALE));
        assert!(!has_record("error"));
    }

    mod deleting {
        //! ADR 064 D4: a connector marked `deleting` is never recreated by
        //! sync and its removal is finished by the reconciler.
        use super::super::*;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        #[derive(Default)]
        struct Calls {
            ensure: AtomicUsize,
            delete: AtomicUsize,
            fail_delete_once: AtomicBool,
        }

        struct FakeTunnels(Arc<Calls>);

        #[async_trait::async_trait]
        impl TunnelProvider for FakeTunnels {
            fn kind(&self) -> &'static str {
                "fake_tunnel"
            }
            async fn ensure(&self, ctx: &TunnelContext<'_>) -> Result<TunnelHandle, IngressError> {
                // A real provider recreates a tunnel deleted at the provider.
                self.0.ensure.fetch_add(1, Ordering::SeqCst);
                Ok(TunnelHandle {
                    external_id: format!("recreated-{}", ctx.connector_id),
                    settings: serde_json::json!({}),
                    secret: "s".into(),
                })
            }
            fn connector_spec(&self, id: i32, _h: &TunnelHandle, port: u16) -> NodeConnectorSpec {
                NodeConnectorSpec {
                    id,
                    provider: "fake_tunnel".into(),
                    revision: String::new(),
                    edge_port: port,
                    client_address_header: None,
                    client_ja4: None,
                    drivers: Vec::new(),
                    secrets: BTreeMap::new(),
                }
            }
            async fn publish(
                &self,
                _h: &TunnelHandle,
                _host: &str,
            ) -> Result<DnsIntent, IngressError> {
                Ok(DnsIntent::None)
            }
            async fn unpublish(&self, _h: &TunnelHandle, _host: &str) -> Result<(), IngressError> {
                Ok(())
            }
            async fn health(
                &self,
                _h: &TunnelHandle,
            ) -> Result<crate::provider::TunnelHealth, IngressError> {
                Ok(crate::provider::TunnelHealth {
                    status: "healthy".into(),
                    links: Vec::new(),
                })
            }
            async fn delete(&self, _h: &TunnelHandle) -> Result<(), IngressError> {
                self.0.delete.fetch_add(1, Ordering::SeqCst);
                if self.0.fail_delete_once.swap(false, Ordering::SeqCst) {
                    return Err(IngressError::Provider("the provider timed out".into()));
                }
                Ok(())
            }
        }

        struct FakeFactory(Arc<Calls>);

        #[async_trait::async_trait]
        impl crate::registry::ProviderFactory for FakeFactory {
            fn info(&self) -> ProviderInfo {
                ProviderInfo {
                    kind: "fake_tunnel".into(),
                    name: "Fake".into(),
                    summary: String::new(),
                    requirements: Vec::new(),
                    needs_public_ip: false,
                    works_behind_cgnat: true,
                    dns: crate::registry::DnsHandling::ProviderHostname,
                    fields: Vec::new(),
                    docs_url: None,
                }
            }
            async fn validate(
                &self,
                _deps: &ProviderDeps<'_>,
                _setup: &ProviderSetup,
            ) -> Result<(), IngressError> {
                Ok(())
            }
            async fn build(
                &self,
                _deps: &ProviderDeps<'_>,
                _connector: &ingress_connectors::Model,
                _secrets: &BTreeMap<String, String>,
            ) -> Result<Box<dyn TunnelProvider>, IngressError> {
                Ok(Box::new(FakeTunnels(self.0.clone())))
            }
            fn spec_provider(&self, _http: &reqwest::Client) -> Box<dyn TunnelProvider> {
                Box::new(FakeTunnels(self.0.clone()))
            }
        }

        async fn setup() -> Option<(
            temps_test_utils::TestDatabase,
            Arc<IngressConnectorService>,
            Arc<Calls>,
            i32,
        )> {
            setup_with(Some(temps_core::tunnel_origin::TUNNEL_ORIGIN_PORT)).await
        }

        /// `origin`: the port the proxy's tunnel origin is bound on, when
        /// this process runs control-plane connectors at all.
        async fn setup_with(
            origin: Option<u16>,
        ) -> Option<(
            temps_test_utils::TestDatabase,
            Arc<IngressConnectorService>,
            Arc<Calls>,
            i32,
        )> {
            let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
                Ok(db) => db,
                Err(error) => {
                    eprintln!(
                        "Docker/Postgres unavailable; skipping deleting-connector test: {error}"
                    );
                    return None;
                }
            };
            let db = test_db.connection_arc();
            let encryption =
                Arc::new(EncryptionService::new("0123456789abcdef0123456789abcdef").expect("key"));
            let providers = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
            let records = Arc::new(ManagedDnsRecordService::new(
                db.clone(),
                providers.clone(),
                encryption.clone(),
            ));
            let calls = Arc::new(Calls::default());
            let mut service = IngressConnectorService::new(
                db.clone(),
                encryption.clone(),
                providers,
                records,
                Arc::new(CachedPeerTable::new(db.clone())),
            );
            service.registry = ProviderRegistry::of(vec![Arc::new(FakeFactory(calls.clone()))]);
            if let Some(port) = origin {
                let tunnel_origin = temps_core::tunnel_origin::ControlPlaneTunnelOrigin::new();
                tunnel_origin.mark_bound(port);
                service.attach_control_plane(crate::control_plane::ControlPlaneHandle::new(
                    tunnel_origin,
                    Arc::new(tokio::sync::Notify::new()),
                ));
            }
            let now = Utc::now();
            let connector = ingress_connectors::ActiveModel {
                name: Set("homelab".into()),
                provider: Set("fake_tunnel".into()),
                dns_provider_id: Set(None),
                domain: Set("example.com".into()),
                external_id: Set(Some("tunnel-1".into())),
                settings: Set(serde_json::json!({})),
                secret_encrypted: Set(Some(encryption.encrypt_string("s").expect("seal"))),
                edge_port: Set(8443),
                auto_publish: Set(true),
                status: Set("ready".into()),
                last_error: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("connector");
            Some((test_db, Arc::new(service), calls, connector.id))
        }

        /// Undo marks the tunnel `deleting`, then is interrupted before the
        /// provider removed it. Neither a sync nor a reconcile tick brings it
        /// back; the tick finishes the removal instead.
        #[tokio::test]
        async fn an_undone_tunnel_is_never_recreated_by_sync() {
            let Some((test_db, service, calls, id)) = setup().await else {
                return;
            };
            let tunnel_id = service.mark_deleting(id, "homelab").await.expect("mark");
            assert_eq!(tunnel_id.as_deref(), Some("tunnel-1"));
            // The provider call fails: undo was interrupted half-way.
            calls.fail_delete_once.store(true, Ordering::SeqCst);
            assert!(service
                .finish_deletion(id, tunnel_id.as_deref())
                .await
                .is_err());
            let row = service.find(id).await.expect("still there");
            assert_eq!(row.status, STATUS_DELETING);

            // A sync is refused, and never recreates the tunnel.
            let error = service.sync(id).await.expect_err("refused");
            assert!(error.to_string().contains("being removed"), "{error}");
            assert_eq!(calls.ensure.load(Ordering::SeqCst), 0);

            // The reconciler's tick finishes the removal, never reverting it.
            service.reconcile_all().await;
            assert_eq!(calls.ensure.load(Ordering::SeqCst), 0);
            assert_eq!(calls.delete.load(Ordering::SeqCst), 2);
            assert!(matches!(
                service.find(id).await,
                Err(IngressError::NotFound(_))
            ));
            drop(test_db);
        }

        /// A reachability run writes the tunnel choice it decided from only
        /// while the hostname still has the choice it read: one someone made
        /// meanwhile is kept and reported, never replaced unseen.
        #[tokio::test]
        async fn a_run_never_replaces_a_choice_made_while_it_routed() {
            let Some((test_db, service, _calls, id)) = setup().await else {
                return;
            };
            let db = test_db.connection_arc();
            let now = Utc::now();
            let provider = temps_entities::dns_providers::ActiveModel {
                name: Set("choice-race".into()),
                provider_type: Set("cloudflare".into()),
                credentials: Set("{}".into()),
                is_active: Set(true),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("provider");
            temps_entities::dns_managed_domains::ActiveModel {
                provider_id: Set(provider.id),
                domain: Set("example.com".into()),
                auto_manage: Set(true),
                proxied_by_default: Set(false),
                verified: Set(true),
                generated_hostname_mode: Set("standard".into()),
                sync_generated_records: Set(false),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("managed zone");
            let choose = |host: &'static str, expected| {
                let service = service.clone();
                async move { service.choose_tunnel_if_unchanged(host, id, expected).await }
            };
            let choice = |host: &'static str| {
                let service = service.clone();
                async move { service.hostname_choice(host).await.expect("read") }
            };

            // Nothing chosen, nothing changed: the choice is recorded.
            choose("shop.example.com", None)
                .await
                .expect("first choice");
            assert_eq!(choice("shop.example.com").await, Some(Some(id)));
            // Resumed after the write landed: already done, not a failure.
            choose("shop.example.com", None).await.expect("resumed");

            // Someone set "no tunnel" after the run read "nothing chosen".
            ingress_hostname_choices::ActiveModel {
                hostname: Set("blog.example.com".into()),
                connector_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("other choice");
            let error = choose("blog.example.com", None)
                .await
                .expect_err("kept the other choice");
            assert!(
                error
                    .to_string()
                    .contains("blog.example.com's tunnel choice changed"),
                "{error}"
            );
            assert_eq!(choice("blog.example.com").await, Some(None));

            // The run read "no tunnel" and confirmed replacing it: written.
            choose("blog.example.com", Some(None))
                .await
                .expect("confirmed change");
            assert_eq!(choice("blog.example.com").await, Some(Some(id)));

            // The run read "no tunnel", but it was changed back meanwhile.
            ingress_hostname_choices::Entity::update_many()
                .col_expr(
                    ingress_hostname_choices::Column::ConnectorId,
                    Expr::value(Option::<i32>::None),
                )
                .filter(ingress_hostname_choices::Column::Hostname.eq("shop.example.com"))
                .exec(db.as_ref())
                .await
                .expect("set off");
            assert!(choose("shop.example.com", Some(Some(id + 1000)))
                .await
                .is_err());
            assert_eq!(choice("shop.example.com").await, Some(None));
            drop(test_db);
        }

        /// A connector marked `deleting` is not served to nodes and its name
        /// is checked, so a stale undo cannot remove a different tunnel.
        #[tokio::test]
        async fn marking_needs_the_same_connector() {
            let Some((test_db, service, calls, id)) = setup().await else {
                return;
            };
            assert!(service.mark_deleting(id, "office").await.is_err());
            assert_eq!(service.find(id).await.expect("row").status, "ready");
            // Removing a tunnel whose provider id moved on is refused.
            service.mark_deleting(id, "homelab").await.expect("mark");
            assert!(service.finish_deletion(id, Some("tunnel-0")).await.is_err());
            assert_eq!(calls.delete.load(Ordering::SeqCst), 0);
            drop(test_db);
        }

        async fn insert_node(db: &DatabaseConnection, name: &str, role: &str, status: &str) -> i32 {
            db.execute_unprepared(&format!(
                "INSERT INTO nodes (name, token_hash, address, private_address, role, status, labels, capacity) \
                 VALUES ('{name}', 'hash-{name}', '192.0.2.10', '10.0.0.10', '{role}', '{status}', '{{}}', '{{}}')"
            ))
            .await
            .expect("node");
            nodes::Entity::find()
                .filter(nodes::Column::Name.eq(name))
                .one(db)
                .await
                .expect("query")
                .expect("inserted")
                .id
        }

        fn placed(view: &ConnectorView) -> Vec<(i32, String)> {
            view.nodes
                .iter()
                .map(|n| (n.node_id, n.node_name.clone()))
                .collect()
        }

        /// ADR-107 D1 end to end on the schema: node 0 is stored, shown as
        /// the control plane, and a mixed placement keeps the worker.
        #[tokio::test]
        async fn the_control_plane_can_be_placed_alone_or_with_a_worker() {
            let Some((test_db, service, _calls, id)) = setup().await else {
                return;
            };
            let db = test_db.connection_arc();
            let worker = insert_node(db.as_ref(), "worker-a", "worker", "active").await;

            service
                .validate_nodes(&[CONTROL_PLANE_NODE_ID])
                .await
                .expect("node 0");
            service
                .replace_nodes(id, &[CONTROL_PLANE_NODE_ID])
                .await
                .expect("placed on the control plane");
            assert_eq!(
                placed(&service.get(id).await.expect("view")),
                vec![(CONTROL_PLANE_NODE_ID, CONTROL_PLANE_NODE_NAME.to_string())]
            );

            service
                .validate_nodes(&[CONTROL_PLANE_NODE_ID, worker])
                .await
                .expect("mixed");
            service
                .replace_nodes(id, &[CONTROL_PLANE_NODE_ID, worker])
                .await
                .expect("mixed placement");
            assert_eq!(
                placed(&service.get(id).await.expect("view")),
                vec![
                    (CONTROL_PLANE_NODE_ID, CONTROL_PLANE_NODE_NAME.to_string()),
                    (worker, "worker-a".to_string())
                ]
            );
            let plan = service.plan().await.expect("plan");
            assert_eq!(
                plan.connector(id).expect("planned").placement,
                Placement {
                    control_plane: true,
                    workers: true
                }
            );

            // Removing the worker keeps the control-plane replica.
            service
                .replace_nodes(id, &[CONTROL_PLANE_NODE_ID])
                .await
                .expect("back to the control plane");
            assert_eq!(service.get(id).await.expect("view").nodes.len(), 1);
            drop(test_db);
        }

        /// ADR-107 D4: a sleeping on-demand app has no route, but the
        /// control-plane proxy wakes it on a visit, so a tunnel there keeps
        /// publishing it rather than deleting its record until it wakes.
        #[tokio::test]
        async fn a_control_plane_tunnel_keeps_publishing_a_sleeping_app() {
            let Some((test_db, service, _calls, id)) = setup().await else {
                return;
            };
            service
                .replace_nodes(id, &[CONTROL_PLANE_NODE_ID])
                .await
                .expect("placed on the control plane");
            service
                .peer_table
                .insert_sleeping_host_for_test("sleeping-app.example.com");

            let plan = service.plan().await.expect("plan");
            assert!(plan
                .served_by_control_plane
                .contains("sleeping-app.example.com"));
            let placement = plan.connector(id).expect("planned").placement;
            assert!(plan.carries(placement, "sleeping-app.example.com"));
            // A worker replica would answer it with a 404: still not carried
            // by a mixed placement.
            assert!(!plan.carries(
                Placement {
                    control_plane: true,
                    workers: true
                },
                "sleeping-app.example.com"
            ));
            drop(test_db);
        }

        #[tokio::test]
        async fn placements_name_the_node_that_cannot_run_a_connector() {
            let Some((test_db, service, _calls, _id)) = setup().await else {
                return;
            };
            let db = test_db.connection_arc();
            let builder = insert_node(db.as_ref(), "builder-a", "builder", "active").await;
            let offline = insert_node(db.as_ref(), "worker-b", "worker", "offline").await;
            let missing = builder + offline + 1000;

            let error = service
                .validate_nodes(&[CONTROL_PLANE_NODE_ID, missing])
                .await
                .expect_err("unknown");
            assert_eq!(
                error.to_string(),
                format!("Node {missing} does not exist (use 0 for the control plane).")
            );
            assert!(matches!(
                service.validate_nodes(&[builder]).await,
                Err(IngressError::NotAWorker { node_id, .. }) if node_id == builder
            ));
            assert!(matches!(
                service.validate_nodes(&[CONTROL_PLANE_NODE_ID, offline]).await,
                Err(IngressError::InactiveWorker { node_id, .. }) if node_id == offline
            ));
            drop(test_db);
        }

        /// The control plane's connector loop reads node 0's specs and writes
        /// its status to the same row the console reads (ADR-107 D2).
        #[tokio::test]
        async fn node_zero_gets_specs_and_reports_status_like_a_worker() {
            let Some((test_db, service, _calls, id)) = setup().await else {
                return;
            };
            assert!(service
                .node_specs(CONTROL_PLANE_NODE_ID)
                .await
                .expect("specs")
                .connectors
                .is_empty());
            service
                .replace_nodes(id, &[CONTROL_PLANE_NODE_ID])
                .await
                .expect("placed");
            assert!(service
                .node_has_connectors(CONTROL_PLANE_NODE_ID)
                .await
                .expect("has"));
            let specs = service
                .node_specs(CONTROL_PLANE_NODE_ID)
                .await
                .expect("specs");
            assert_eq!(specs.connectors.len(), 1);
            assert_eq!(specs.connectors[0].id, id);
            assert_eq!(specs.connectors[0].edge_port, 8443);
            // The secret holder list includes the control plane, so moving
            // the tunnel off it later rotates the secret.
            assert!(
                token_holders(&service.find(id).await.expect("row").settings)
                    .is_some_and(|h| h.contains(&CONTROL_PLANE_NODE_ID))
            );

            service
                .record_status(
                    CONTROL_PLANE_NODE_ID,
                    NodeConnectorStatusReport {
                        reports: vec![crate::wire::ConnectorStatus {
                            connector_id: id,
                            driver: Some("process".into()),
                            state: "ready".into(),
                            ready_connections: Some(4),
                            connector_ref: Some("connector-ref-1".into()),
                            last_error: None,
                            restarts: 0,
                        }],
                    },
                )
                .await
                .expect("status");
            let view = service.get(id).await.expect("view");
            assert_eq!(view.nodes.len(), 1);
            assert_eq!(view.nodes[0].node_id, CONTROL_PLANE_NODE_ID);
            assert_eq!(view.nodes[0].state, "ready");
            assert_eq!(view.nodes[0].driver.as_deref(), Some("process"));
            drop(test_db);
        }

        /// A console without its proxy in the same process (`--role
        /// console`) has nothing to carry a control-plane tunnel: node 0 is
        /// refused with why, and workers are still accepted.
        #[tokio::test]
        async fn node_zero_is_refused_where_the_control_plane_cannot_run_it() {
            let Some((test_db, service, _calls, _id)) = setup_with(None).await else {
                return;
            };
            let error = service
                .validate_nodes(&[CONTROL_PLANE_NODE_ID])
                .await
                .expect_err("no control-plane runtime");
            assert!(matches!(
                error,
                IngressError::ControlPlaneUnavailable(
                    ControlPlaneTunnelsUnavailable::SeparateProxy
                )
            ));
            assert!(error.to_string().contains("--role console"), "{error}");
            assert_eq!(
                temps_core::problemdetails::Problem::from(error).status_code,
                axum::http::StatusCode::CONFLICT
            );
            let worker = insert_node(
                test_db.connection_arc().as_ref(),
                "worker-a",
                "worker",
                "active",
            )
            .await;
            service.validate_nodes(&[worker]).await.expect("a worker");
            drop(test_db);
        }

        #[derive(Default)]
        struct FakeRuntime {
            applied: std::sync::Mutex<Vec<Vec<i32>>>,
        }

        #[async_trait::async_trait]
        impl crate::control_plane::LocalConnectorRuntime for FakeRuntime {
            async fn apply(
                &self,
                specs: Vec<NodeConnectorSpec>,
            ) -> Vec<crate::wire::ConnectorStatus> {
                self.applied
                    .lock()
                    .expect("lock")
                    .push(specs.iter().map(|s| s.id).collect());
                Vec::new()
            }
            async fn statuses(&self) -> Vec<crate::wire::ConnectorStatus> {
                self.applied
                    .lock()
                    .expect("lock")
                    .last()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|connector_id| crate::wire::ConnectorStatus {
                        connector_id,
                        driver: Some("process".into()),
                        state: "ready".into(),
                        ready_connections: Some(2),
                        connector_ref: None,
                        last_error: None,
                        restarts: 0,
                    })
                    .collect()
            }
        }

        /// ADR-107 D2: the control plane's loop hands node 0's specs to the
        /// local runtime only once the proxy's origin is bound on the spec's
        /// port, and reports the result in the row the console reads.
        #[tokio::test]
        async fn the_control_plane_loop_runs_node_zero_specs_and_reports_them() {
            let Some((test_db, service, _calls, id)) = setup_with(None).await else {
                return;
            };
            let origin = temps_core::tunnel_origin::ControlPlaneTunnelOrigin::new();
            let runtime = Arc::new(FakeRuntime::default());
            let connectors = crate::control_plane::ControlPlaneConnectors::attach(
                service.clone(),
                runtime.clone(),
                crate::control_plane::ControlPlaneHandle::for_origin(origin.clone()),
            );
            // The fixture's connector listens on 8443; bind the origin there.
            origin.mark_bound(8443);
            service
                .replace_nodes(id, &[CONTROL_PLANE_NODE_ID])
                .await
                .expect("placed");

            connectors.sync().await.expect("sync");
            assert_eq!(*runtime.applied.lock().expect("lock"), vec![vec![id]]);
            let view = service.get(id).await.expect("view");
            assert_eq!(view.nodes[0].state, "ready");
            assert_eq!(view.nodes[0].ready_connections, Some(2));

            // The proxy took tunnel traffic on another port: nothing runs,
            // and the row says why.
            origin.mark_bound(7480);
            connectors.sync().await.expect("sync");
            assert_eq!(
                runtime.applied.lock().expect("lock").last(),
                Some(&Vec::new())
            );
            let view = service.get(id).await.expect("view");
            assert_eq!(view.nodes[0].state, "down");
            assert!(view.nodes[0]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("port 8443")));
            drop(test_db);
        }

        /// The console's service and the one `temps serve` runs node 0's
        /// connectors with record token holders on the same row: neither
        /// may overwrite the other's (a lost holder is never revoked).
        #[tokio::test]
        async fn two_service_instances_never_lose_each_others_token_holders() {
            let Some((test_db, first, _calls, id)) = setup_with(None).await else {
                return;
            };
            let db = test_db.connection_arc();
            let encryption =
                Arc::new(EncryptionService::new("0123456789abcdef0123456789abcdef").expect("key"));
            let providers = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
            let second = Arc::new(IngressConnectorService::new(
                db.clone(),
                encryption.clone(),
                providers.clone(),
                Arc::new(ManagedDnsRecordService::new(
                    db.clone(),
                    providers,
                    encryption,
                )),
                Arc::new(CachedPeerTable::new(db.clone())),
            ));
            let mut writes = tokio::task::JoinSet::new();
            for node in 1..=24 {
                let service = if node % 2 == 0 {
                    first.clone()
                } else {
                    second.clone()
                };
                writes.spawn(async move {
                    service
                        .add_token_holders(id, &BTreeSet::from([node]))
                        .await
                        .expect("holder recorded");
                });
            }
            while let Some(done) = writes.join_next().await {
                done.expect("writer");
            }
            let row = first.find(id).await.expect("connector");
            assert_eq!(
                token_holders(&row.settings),
                Some((1..=24).collect::<BTreeSet<i32>>())
            );
            drop(test_db);
        }

        /// `temps serve` runs node 0's connectors on its own service
        /// instance, so a console that fails to start does not stop them;
        /// the console's instance shares the handle, accepts node 0 and
        /// wakes the loop when a placement changes, long before its poll.
        #[tokio::test]
        async fn the_consoles_service_wakes_the_loop_serve_runs() {
            let Some((test_db, serve_side, _calls, id)) = setup_with(None).await else {
                return;
            };
            let db = test_db.connection_arc();
            let encryption =
                Arc::new(EncryptionService::new("0123456789abcdef0123456789abcdef").expect("key"));
            let providers = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
            let console = IngressConnectorService::new(
                db.clone(),
                encryption.clone(),
                providers.clone(),
                Arc::new(ManagedDnsRecordService::new(
                    db.clone(),
                    providers,
                    encryption,
                )),
                Arc::new(CachedPeerTable::new(db.clone())),
            );
            assert!(console
                .validate_nodes(&[CONTROL_PLANE_NODE_ID])
                .await
                .is_err());

            let origin = temps_core::tunnel_origin::ControlPlaneTunnelOrigin::new();
            origin.mark_bound(8443);
            let handle = crate::control_plane::ControlPlaneHandle::for_origin(origin);
            let runtime = Arc::new(FakeRuntime::default());
            let connectors = crate::control_plane::ControlPlaneConnectors::attach(
                serve_side.clone(),
                runtime.clone(),
                handle.clone(),
            );
            let shutdown = Arc::new(tokio::sync::Notify::new());
            let task = connectors.start(shutdown.clone());
            console.attach_control_plane(handle);
            console
                .validate_nodes(&[CONTROL_PLANE_NODE_ID])
                .await
                .expect("node 0 once attached");

            console
                .replace_nodes(id, &[CONTROL_PLANE_NODE_ID])
                .await
                .expect("placed");
            console.wake_control_plane();
            let started = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if runtime
                        .applied
                        .lock()
                        .expect("lock")
                        .iter()
                        .any(|ids| ids == &vec![id])
                    {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            assert!(started.is_ok(), "the wake did not reach the loop");
            shutdown.notify_one();
            task.await.expect("loop ends");
            drop(test_db);
        }
    }

    #[test]
    fn pages_default_to_twenty_and_cap_at_one_hundred() {
        assert_eq!(page_bounds(None, None), (1, 20));
        assert_eq!(page_bounds(Some(0), Some(0)), (1, 1));
        assert_eq!(page_bounds(Some(3), Some(500)), (3, 100));
    }
}

#[cfg(test)]
mod secret_answer_tests {
    use super::*;
    use crate::registry::{DnsHandling, SetupField};

    fn info() -> ProviderInfo {
        let field = |key: &str, kind: FieldKind| SetupField {
            key: key.into(),
            label: format!("Label of {key}"),
            help: None,
            kind,
            required: true,
            placeholder: None,
        };
        ProviderInfo {
            kind: "fixture_tunnel".into(),
            name: "Fixture tunnel".into(),
            summary: String::new(),
            requirements: Vec::new(),
            needs_public_ip: false,
            works_behind_cgnat: true,
            dns: DnsHandling::ProviderHostname,
            fields: vec![
                field("client_id", FieldKind::Text),
                field("client_secret", FieldKind::Secret),
            ],
            docs_url: None,
        }
    }

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::json!(v)))
            .collect()
    }

    #[test]
    fn a_sealed_answer_reaches_the_setup_as_the_real_value() {
        let secrets: BTreeMap<String, SecretString> = [(
            "client_secret".to_string(),
            SecretString::new("fixture-value"),
        )]
        .into();
        let merged = merge_secret_answers(&info(), &values(&[("client_id", "id")]), &secrets)
            .expect("a credential field may be sealed");
        assert_eq!(merged["client_secret"], "fixture-value");
        assert_eq!(merged["client_id"], "id");
    }

    #[test]
    fn sealed_answers_must_name_a_credential_field_once() {
        let sealed = |key: &str| -> BTreeMap<String, SecretString> {
            [(key.to_string(), SecretString::new("fixture-value"))].into()
        };
        let not_a_credential = merge_secret_answers(&info(), &values(&[]), &sealed("client_id"))
            .expect_err("a text field is not a credential");
        assert!(not_a_credential.to_string().contains("client_id"));
        let unknown =
            merge_secret_answers(&info(), &values(&[]), &sealed("nope")).expect_err("unknown key");
        assert!(unknown.to_string().contains("nope"));
        let twice = merge_secret_answers(
            &info(),
            &values(&[("client_secret", "plain")]),
            &sealed("client_secret"),
        )
        .expect_err("given twice");
        assert!(twice.to_string().contains("both"));
        for error in [not_a_credential, unknown, twice] {
            assert!(!error.to_string().contains("fixture-value"), "{error}");
        }
    }

    #[test]
    fn a_plain_credential_answer_is_refused_without_echoing_it() {
        let error = reject_plain_secret_answers(
            &info(),
            &values(&[("client_id", "id"), ("client_secret", "typed-in-chat")]),
        )
        .expect_err("a credential in values is refused for the assistant");
        let text = error.to_string();
        assert!(
            text.contains("client_secret") && text.contains("secrets"),
            "{text}"
        );
        assert!(!text.contains("typed-in-chat"), "{text}");
        assert!(reject_plain_secret_answers(&info(), &values(&[("client_id", "id")])).is_ok());
    }
}
