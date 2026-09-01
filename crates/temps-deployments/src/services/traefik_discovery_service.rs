// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-mostly admin surface over live Traefik-label route discovery.
//!
//! Discovery itself lives in `temps_deployer::traefik_discovery`: a background
//! reconciler that reads `traefik.*` labels off containers Temps did **not**
//! deploy and persists them as `traefik_discovered_routes` rows. This service
//! is the *operator's* view of that machinery — without it the only way to see
//! what was adopted (or why a labelled container isn't being routed) is to read
//! server logs or query Postgres by hand.
//!
//! Three questions it answers:
//!
//! 1. **Is discovery even on?** [`TraefikDiscoveryAdminService::status`] is the
//!    capability endpoint required by CLAUDE.md's *Feature Discoverability*
//!    rules: it returns `configured: false` plus the exact environment
//!    variables that would turn it on, so a client can tell "this build has no
//!    discovery" apart from "discovery is not turned on here".
//! 2. **What was discovered?** [`TraefikDiscoveryAdminService::list_routes`]
//!    lists the rows, annotated with the host collisions recorded by the last
//!    reconciliation so a route that isn't taking effect explains itself.
//! 3. **Can I suppress one?** [`TraefikDiscoveryAdminService::set_route_enabled`]
//!    flips the per-row kill switch without touching the container's labels.
//!
//! Routes are never *created* through this API — the reconciler owns every
//! insert. There is deliberately no create/delete here: a row deleted through
//! the API would reappear on the next reconciliation pass 30 seconds later,
//! which is exactly the kind of silently-undone action CLAUDE.md forbids.
//! `enabled = false` is the durable way to say "found it, don't route it".

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, DbErr, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use temps_deployer::traefik_discovery::{
    ConflictReason, ReconcileOutcome, TraefikDiscoveryHandle, ENABLED_ENV, NETWORK_ENV,
};
use temps_entities::domains;
use temps_entities::environment_domains;
use temps_entities::project_custom_domains;
use temps_entities::traefik_discovered_routes as discovered;
use temps_entities::traefik_route_certificates as certs;
use thiserror::Error;
use utoipa::ToSchema;

/// Default page size for the discovered-route listing.
const DEFAULT_PAGE_SIZE: u64 = 20;
/// Hard cap on page size, per the project-wide pagination convention.
const MAX_PAGE_SIZE: u64 = 100;

/// Error returned by the TLS provisioner trait when it cannot issue or save
/// a certificate for a discovered host.
#[derive(Debug, Error)]
pub enum TlsProvisionerError {
    #[error("Certificate operation for host '{host}' failed: {reason}")]
    Failed { host: String, reason: String },

    #[error(
        "Host '{host}' already has a domains row with verification_method='{stored}', \
             but the request declared '{declared}'. Declare the stored method or remove \
             the domain with DELETE /domains/{host} first."
    )]
    VerificationMethodConflict {
        host: String,
        stored: String,
        declared: String,
    },
}

/// Adapter boundary for ACME issuance and certificate persistence.
///
/// Implemented in the main binary (which can depend on both `temps-deployments`
/// and `temps-domains`). Keeping the trait here avoids adding a `temps-domains`
/// dependency to `temps-deployments` — and prevents the circular dependency that
/// would follow.
///
/// Both methods are called **after** the eight-step import validation chain and
/// the host-ownership check have passed; they must not duplicate those checks.
#[async_trait]
pub trait DiscoveredHostTlsProvisioner: Send + Sync {
    /// Ensure a `domains` row exists for `host` with `challenge_type` set, then
    /// call `request_challenge` to kick off ACME issuance (Path A).
    ///
    /// Returns `Err(TlsProvisionerError::VerificationMethodConflict)` when a
    /// `domains` row already exists with a **different** `verification_method`
    /// so the service can surface the 409 with both values named.
    async fn request_acme_cert(
        &self,
        host: &str,
        challenge_type: &str,
    ) -> Result<(), TlsProvisionerError>;

    /// Persist a validated certificate + key into the `domains` table via
    /// `CertificateRepository::save_certificate` (Path B). Returns the
    /// `domains.id` of the upserted row.
    async fn save_imported_cert(
        &self,
        host: &str,
        certificate_pem: &str,
        key_pem: &str,
        renewal_method: &str,
    ) -> Result<i32, TlsProvisionerError>;
}

#[derive(Debug, Error)]
pub enum TraefikDiscoveryAdminError {
    #[error("No discovered Traefik route exists for host '{host}'")]
    NotFound { host: String },

    #[error("Invalid host '{host}' for a discovered Traefik route: {message}")]
    Validation { host: String, message: String },

    #[error("Database error while {operation} for discovered Traefik routes: {source}")]
    Database {
        operation: String,
        #[source]
        source: DbErr,
    },

    /// The host is already owned by a Temps-managed resource (custom domain,
    /// environment domain, custom route, console hostname, or an existing
    /// `domains` row not belonging to this authorization record).
    /// The `owner` field names the conflicting resource.
    #[error(
        "Host '{host}' is already owned by '{owner}' and cannot be authorized \
             for TLS here. Remove the conflicting resource first."
    )]
    HostOwned { host: String, owner: String },

    /// `verification_method` in the existing `domains` row differs from the
    /// declared `challenge_type`. Both values are surfaced in the error so the
    /// operator can make an informed decision.
    #[error(
        "Host '{host}' already has verification_method='{stored}' but this \
             request declared '{declared}'. Either declare the stored method or \
             remove the domain with DELETE /domains/{host} first."
    )]
    VerificationMethodConflict {
        host: String,
        stored: String,
        declared: String,
    },

    /// The certificate material (from Path B import) failed the validation chain.
    #[error("Certificate validation failed for host '{host}': {reason}")]
    CertificateValidation { host: String, reason: String },

    /// The injected TLS provisioner returned an error (e.g. ACME order failed,
    /// encryption failed, DB write failed).
    #[error("TLS provisioner error for host '{host}': {reason}")]
    Upstream { host: String, reason: String },
}

impl TraefikDiscoveryAdminError {
    fn database(operation: &str, source: DbErr) -> Self {
        Self::Database {
            operation: operation.to_string(),
            source,
        }
    }
}

// ── Response DTOs ───────────────────────────────────────────────────────────

/// How an operator turns discovery on. Always returned, including when
/// discovery is already running, so the console/CLI can render the exact
/// invocation instead of sending the reader to the docs.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraefikDiscoverySetupResponse {
    /// Environment variable that opts this installation in.
    #[schema(example = "TEMPS_TRAEFIK_DISCOVERY_ENABLED")]
    pub enable_env_var: String,
    /// Environment variable overriding the watched Docker network.
    #[schema(example = "TEMPS_TRAEFIK_DISCOVERY_NETWORK")]
    pub network_env_var: String,
    /// A concrete, copy-pasteable example of enabling it.
    pub example: String,
    /// These are read once at process start: changing them needs a restart.
    pub requires_restart: bool,
}

impl TraefikDiscoverySetupResponse {
    fn new(network: &str) -> Self {
        Self {
            enable_env_var: ENABLED_ENV.to_string(),
            network_env_var: NETWORK_ENV.to_string(),
            example: format!("{ENABLED_ENV}=true {NETWORK_ENV}={network} temps serve"),
            requires_restart: true,
        }
    }
}

/// A labelled container that was found but deliberately **not** adopted.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraefikDiscoveryConflictResponse {
    pub host: String,
    pub container_id: String,
    pub container_name: String,
    /// Traefik router name the host came from.
    pub router_name: String,
    /// Machine-readable discriminator: `owned_by_temps_route` or
    /// `claimed_by_another_container`.
    pub reason: String,
    /// Human-readable explanation of the conflict.
    pub detail: String,
    /// Container that holds the host instead, when the conflict is between two
    /// discovered containers.
    pub winner_container_name: Option<String>,
}

impl TraefikDiscoveryConflictResponse {
    fn from_conflict(conflict: &temps_deployer::traefik_discovery::HostConflict) -> Self {
        let (reason, winner_container_name) = match &conflict.reason {
            ConflictReason::OwnedByTempsRoute => ("owned_by_temps_route", None),
            ConflictReason::ClaimedByAnotherContainer {
                winner_container_name,
            } => (
                "claimed_by_another_container",
                Some(winner_container_name.clone()),
            ),
        };
        Self {
            host: conflict.host.clone(),
            container_id: conflict.container_id.clone(),
            container_name: conflict.container_name.clone(),
            router_name: conflict.router_name.clone(),
            reason: reason.to_string(),
            detail: conflict.reason.to_string(),
            winner_container_name,
        }
    }
}

/// Summary of the most recent reconciliation pass.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraefikReconciliationResponse {
    pub network: String,
    pub containers_scanned: usize,
    /// Containers skipped because Temps deployed them (they already have a
    /// route and must never re-derive one from labels they control).
    pub skipped_temps_managed: usize,
    pub routes_upserted: usize,
    pub routes_unchanged: usize,
    pub routes_removed: usize,
    pub conflicts: Vec<TraefikDiscoveryConflictResponse>,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub completed_at: UtcDateTime,
}

impl From<&ReconcileOutcome> for TraefikReconciliationResponse {
    fn from(outcome: &ReconcileOutcome) -> Self {
        Self {
            network: outcome.network.clone(),
            containers_scanned: outcome.containers_scanned,
            skipped_temps_managed: outcome.skipped_temps_managed,
            routes_upserted: outcome.routes_upserted,
            routes_unchanged: outcome.routes_unchanged,
            routes_removed: outcome.routes_removed,
            conflicts: outcome
                .conflicts
                .iter()
                .map(TraefikDiscoveryConflictResponse::from_conflict)
                .collect(),
            completed_at: outcome.completed_at,
        }
    }
}

/// Capability + status of Traefik label discovery on this instance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraefikDiscoveryStatusResponse {
    /// `true` only when the watcher is actually running in this process.
    /// `false` means "not turned on here", never "not supported" — the setup
    /// block below always says how to turn it on.
    pub configured: bool,
    /// Whether `TEMPS_TRAEFIK_DISCOVERY_ENABLED` resolved to true. Can be
    /// `true` while `configured` is `false` (e.g. Docker unreachable).
    pub enabled: bool,
    /// Docker network being watched, or the one that *would* be watched.
    pub network: String,
    /// Interval of the full reconciliation safety net.
    pub poll_interval_seconds: u64,
    /// Why discovery isn't active, when `configured` is false.
    pub reason: Option<String>,
    pub setup: TraefikDiscoverySetupResponse,
    /// Rows currently in `traefik_discovered_routes` (all networks).
    pub discovered_route_count: u64,
    /// Of those, how many are enabled and therefore in the live route table.
    pub enabled_route_count: u64,
    /// Last reconciliation pass, absent until the first one completes.
    pub last_reconciliation: Option<TraefikReconciliationResponse>,
}

/// One row of `traefik_discovered_routes`, annotated for an operator.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraefikDiscoveredRouteResponse {
    pub id: i32,
    pub host: String,
    pub router_name: String,
    pub target_container_id: String,
    pub target_container_name: String,
    pub target_port: i32,
    /// Host-published port, used on baremetal installs where the proxy cannot
    /// resolve container names.
    pub target_host_port: Option<i32>,
    pub network: String,
    pub tls: bool,
    pub enabled: bool,
    /// Whether this route is currently served by the proxy.
    pub active: bool,
    /// Why it isn't, when `active` is false.
    pub inactive_reason: Option<String>,
    /// Other labelled containers that claim this host and lost the collision.
    /// Non-empty means someone's container is silently not being routed.
    pub contested_by: Vec<String>,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub last_seen_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub updated_at: UtcDateTime,
    /// TLS authorization state for this host (ADR-041). Present only when a
    /// `traefik_route_certificates` row exists. `null` means no operator has
    /// ever authorized TLS for this host — the container label's `tls` field
    /// (above) records what the label says, but this is what has been acted on.
    pub tls_certificate: Option<TraefikRouteTlsBlock>,
}

impl TraefikDiscoveredRouteResponse {
    fn from_model(
        model: discovered::Model,
        contested_by: Vec<String>,
        cert: Option<&certs::Model>,
    ) -> Self {
        let inactive_reason = if model.enabled {
            None
        } else {
            Some(format!(
                "Suppressed by an operator. Re-enable with: temps traefik-discovery routes enable {}",
                model.host
            ))
        };
        let tls_certificate = cert.map(|c| {
            TraefikRouteTlsBlock::from_cert_row(c, Some(model.target_container_name.as_str()))
        });
        Self {
            id: model.id,
            host: model.host,
            router_name: model.router_name,
            target_container_id: model.target_container_id,
            target_container_name: model.target_container_name,
            target_port: model.target_port,
            target_host_port: model.target_host_port,
            network: model.network,
            tls: model.tls,
            enabled: model.enabled,
            active: model.enabled,
            inactive_reason,
            contested_by,
            last_seen_at: model.last_seen_at,
            created_at: model.created_at,
            updated_at: model.updated_at,
            tls_certificate,
        }
    }
}

/// Paginated discovered routes, plus the hosts that were found and rejected.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraefikDiscoveredRouteListResponse {
    pub routes: Vec<TraefikDiscoveredRouteResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    /// Labelled containers found by the last reconciliation that were NOT
    /// adopted (host owned by a Temps route, or claimed by another container).
    /// These have no row of their own — without surfacing them here the
    /// operator sees nothing at all for a container they labelled.
    pub conflicts: Vec<TraefikDiscoveryConflictResponse>,
    /// `false` when the watcher isn't running, so a client can explain an
    /// empty list as "discovery is off" rather than "nothing was found".
    pub discovery_running: bool,
}

/// Body of `PATCH /traefik-discovery/routes/{host}/enabled`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateTraefikRouteEnabledRequest {
    /// `false` suppresses the route without touching the container's labels;
    /// the row stays visible so the operator can see what was found.
    pub enabled: bool,
}

// ── TLS-related DTOs ────────────────────────────────────────────────────────

/// TLS state for a single discovered route (ADR-041 §3/§4).
///
/// Absent when no `traefik_route_certificates` row exists for this host.
/// Never `null` on a host where `cert_authorized = true`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraefikRouteTlsBlock {
    /// The operator has explicitly authorized TLS for this host.
    pub cert_authorized: bool,
    /// `"acme"` or `"imported"`.
    pub source: Option<String>,
    /// `"http-01"` or `"dns-01"`.
    pub renewal_method: Option<String>,
    /// Certificate status as reported by the `domains` row, e.g. `"active"`.
    pub status: Option<String>,
    /// ISO 8601 expiry time of the current certificate, if one exists.
    pub not_after: Option<String>,
    /// Days until expiry.
    pub days_remaining: Option<i64>,
    /// `true` when the proxy is currently loading a cert for this host.
    pub serving: bool,
    /// Container ID that was authorized. Used for drift comparison.
    pub authorized_container_id: Option<String>,
    pub authorized_container_name: Option<String>,
    /// `true` when the currently-serving container differs from the one
    /// that was authorized. Requires operator acknowledgment.
    pub container_drift: bool,
    /// Name of the container that currently holds the host (for the drift UI).
    pub current_container_name: Option<String>,
    /// When drift was first detected.
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub container_drift_detected_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub authorized_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub imported_at: Option<UtcDateTime>,
}

impl TraefikRouteTlsBlock {
    /// Build from a `traefik_route_certificates` row and the current container
    /// name from `traefik_discovered_routes` (for drift display).
    pub fn from_cert_row(row: &certs::Model, current_container_name: Option<&str>) -> Self {
        // Use `container_drift_detected_at.is_some()` as the single source of
        // truth for drift. The reconciler (`check_certificate_drift_for`) sets
        // this field when it detects a mismatch by container ID; computing drift
        // by container name here would produce a different result on
        // `--force-recreate` (same name, different ID) and confuse operators
        // investigating an alarm.
        let container_drift = row.cert_authorized && row.container_drift_detected_at.is_some();
        Self {
            cert_authorized: row.cert_authorized,
            source: Some(row.source.clone()),
            renewal_method: Some(row.renewal_method.clone()),
            status: None, // filled in by service when domains row is loaded
            not_after: None,
            days_remaining: None,
            serving: false,
            authorized_container_id: Some(row.authorized_container_id.clone()),
            authorized_container_name: Some(row.authorized_container_name.clone()),
            container_drift,
            current_container_name: current_container_name.map(str::to_string),
            container_drift_detected_at: row.container_drift_detected_at,
            authorized_at: row.authorized_at,
            imported_at: row.imported_at,
        }
    }
}

/// Request body for Path A: operator-triggered ACME issuance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RequestDiscoveredRouteCertRequest {
    /// `"http-01"` or `"dns-01"`. Required — no silent default.
    pub challenge_type: String,
    /// Must be `true` when `challenge_type` is `"dns-01"` and no verified
    /// `auto_manage` zone covers this host. Lets the operator confirm they
    /// know renewal will require manual DNS updates.
    #[serde(default)]
    pub acknowledge_manual_dns_renewal: bool,
}

/// Request body for Path B: import from Traefik's `acme.json`.
///
/// `Debug` is hand-written: `acme_json` contains Traefik's private keys and
/// must never appear in logs. Only the host list, renewal method, dry-run flag,
/// and byte length are logged.
#[derive(Clone, Deserialize, ToSchema)]
pub struct ImportTraefikAcmeJsonRequest {
    /// Raw contents of the Traefik `acme.json` file (uploaded by the CLI or
    /// pasted in the console). **Never** a server-side file path.
    /// Redacted in `Debug` output — the field holds private key material.
    #[serde(skip_serializing)]
    pub acme_json: String,
    /// Hosts to import. Only hosts that appear in the document's certificates
    /// (by X.509 SAN, not JSON `domain.main`) are accepted.
    pub hosts: Vec<String>,
    /// `"http-01"` or `"dns-01"`. Stored as `verification_method` so the
    /// renewal scheduler knows how to renew.
    pub renewal_method: String,
    /// Required when `renewal_method` is `"dns-01"` and no auto-manage zone
    /// covers the host.
    #[serde(default)]
    pub acknowledge_manual_dns_renewal: bool,
    /// `true` → full parse and validation, no writes. The identical per-host
    /// verdicts are returned, giving the operator a preview before committing.
    #[serde(default)]
    pub dry_run: bool,
}

impl std::fmt::Debug for ImportTraefikAcmeJsonRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportTraefikAcmeJsonRequest")
            .field("hosts", &self.hosts)
            .field("renewal_method", &self.renewal_method)
            .field(
                "acknowledge_manual_dns_renewal",
                &self.acknowledge_manual_dns_renewal,
            )
            .field("dry_run", &self.dry_run)
            .field(
                "acme_json",
                &format!("<redacted, {} bytes>", self.acme_json.len()),
            )
            .finish()
    }
}

/// Per-host result from a Path B import.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportedHostVerdict {
    pub host: String,
    /// Whether the cert was written (or would be written on `dry_run: false`).
    pub success: bool,
    /// Human-readable failure reason when `success` is `false`.
    pub error: Option<String>,
    /// ISO 8601 expiry of the imported certificate.
    pub not_after: Option<String>,
    /// DNS SANs carried in the imported certificate.
    pub sans: Vec<String>,
}

/// Response body for the Path B import endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportTraefikAcmeJsonResponse {
    pub dry_run: bool,
    pub total_requested: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub verdicts: Vec<ImportedHostVerdict>,
}

// ── Service ─────────────────────────────────────────────────────────────────

/// Operator-facing view of Traefik label discovery.
pub struct TraefikDiscoveryAdminService {
    db: Arc<DatabaseConnection>,
    /// Startup-resolved discovery state. Carries the config even when the
    /// watcher isn't running, so a disabled instance still reports what it
    /// *would* do.
    handle: Arc<TraefikDiscoveryHandle>,
    /// Optional: TLS provisioner injected by the main binary.
    /// When `None`, TLS write operations (Path A / Path B) return
    /// `TraefikDiscoveryAdminError::Upstream` with a clear message.
    provisioner: Option<Arc<dyn DiscoveredHostTlsProvisioner>>,
}

impl TraefikDiscoveryAdminService {
    pub fn new(db: Arc<DatabaseConnection>, handle: Arc<TraefikDiscoveryHandle>) -> Self {
        Self {
            db,
            handle,
            provisioner: None,
        }
    }

    pub fn with_provisioner(mut self, provisioner: Arc<dyn DiscoveredHostTlsProvisioner>) -> Self {
        self.provisioner = Some(provisioner);
        self
    }

    /// Verify that `host` is not already claimed by a Temps-managed resource
    /// before writing a TLS authorization record. Called by both Path A and Path B.
    ///
    /// Checks, in order:
    /// 1. `environment_domains` — auto-generated `<env>.<project>.temps.local` and
    ///    custom environment subdomains.
    /// 2. `project_custom_domains` — operator-added custom domains.
    /// 3. `domains` — if a row exists for this host and it is NOT already linked
    ///    to the current `traefik_route_certificates` row (via `certificate_id`),
    ///    another resource owns it.
    ///
    /// The check is always evaluated fresh from the database — never from a
    /// stale in-memory set — so a domain added or removed between reconcile
    /// passes is still caught.
    async fn check_host_ownership(
        &self,
        host: &str,
        existing_cert_row: Option<&certs::Model>,
    ) -> Result<(), TraefikDiscoveryAdminError> {
        // 1. environment_domains
        let env_domain_count = environment_domains::Entity::find()
            .filter(environment_domains::Column::Domain.eq(host))
            .count(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database(
                    "checking environment domains for host ownership",
                    e,
                )
            })?;
        if env_domain_count > 0 {
            return Err(TraefikDiscoveryAdminError::HostOwned {
                host: host.to_string(),
                owner: "an environment subdomain".to_string(),
            });
        }

        // 2. project_custom_domains
        let custom_domain_count = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(host))
            .count(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database(
                    "checking custom domains for host ownership",
                    e,
                )
            })?;
        if custom_domain_count > 0 {
            return Err(TraefikDiscoveryAdminError::HostOwned {
                host: host.to_string(),
                owner: "a project custom domain".to_string(),
            });
        }

        // 3. domains — a row for this host owned by a different certificate.
        let domain_row = domains::Entity::find()
            .filter(domains::Column::Domain.eq(host))
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database("checking domains table for host ownership", e)
            })?;

        if let Some(domain) = domain_row {
            // If the existing cert row already points to this domain, we own it.
            let owned_by_us = existing_cert_row
                .and_then(|cert| cert.certificate_id)
                .map(|cert_id| cert_id == domain.id)
                .unwrap_or(false);
            if !owned_by_us {
                return Err(TraefikDiscoveryAdminError::HostOwned {
                    host: host.to_string(),
                    owner: format!("a domains row (id={}, status={})", domain.id, domain.status),
                });
            }
        }

        Ok(())
    }

    /// Capability + status. Never fails on "discovery is off" — that is a
    /// successful answer of `configured: false` with a reason and a setup
    /// block, which is what makes the onboarding state renderable.
    pub async fn status(
        &self,
    ) -> Result<TraefikDiscoveryStatusResponse, TraefikDiscoveryAdminError> {
        let config = self.handle.config();

        let discovered_route_count = discovered::Entity::find()
            .count(self.db.as_ref())
            .await
            .map_err(|e| TraefikDiscoveryAdminError::database("counting discovered routes", e))?;
        let enabled_route_count = discovered::Entity::find()
            .filter(discovered::Column::Enabled.eq(true))
            .count(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database("counting enabled discovered routes", e)
            })?;

        Ok(TraefikDiscoveryStatusResponse {
            configured: self.handle.is_running(),
            enabled: config.enabled,
            network: config.network.clone(),
            poll_interval_seconds: config.poll_interval.as_secs(),
            reason: self.handle.unavailable_reason().map(str::to_string),
            setup: TraefikDiscoverySetupResponse::new(&config.network),
            discovered_route_count,
            enabled_route_count,
            last_reconciliation: self
                .handle
                .last_outcome()
                .as_ref()
                .map(TraefikReconciliationResponse::from),
        })
    }

    /// List discovered routes, newest-seen first, annotated with the host
    /// collisions the last reconciliation recorded.
    pub async fn list_routes(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<TraefikDiscoveredRouteListResponse, TraefikDiscoveryAdminError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);

        let paginator = discovered::Entity::find()
            .order_by_desc(discovered::Column::LastSeenAt)
            .order_by_asc(discovered::Column::Host)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator
            .num_items()
            .await
            .map_err(|e| TraefikDiscoveryAdminError::database("counting discovered routes", e))?;
        let models = paginator
            .fetch_page(page - 1)
            .await
            .map_err(|e| TraefikDiscoveryAdminError::database("listing discovered routes", e))?;

        let outcome = self.handle.last_outcome();
        let conflicts: Vec<TraefikDiscoveryConflictResponse> = outcome
            .as_ref()
            .map(|o| {
                o.conflicts
                    .iter()
                    .map(TraefikDiscoveryConflictResponse::from_conflict)
                    .collect()
            })
            .unwrap_or_default();

        // Bulk-fetch cert rows for this page's hosts in one query.
        let hosts_on_page: Vec<String> = models.iter().map(|m| m.host.clone()).collect();
        let cert_rows: Vec<certs::Model> = if hosts_on_page.is_empty() {
            vec![]
        } else {
            certs::Entity::find()
                .filter(certs::Column::Host.is_in(hosts_on_page))
                .all(self.db.as_ref())
                .await
                .map_err(|e| {
                    TraefikDiscoveryAdminError::database("loading cert rows for route list", e)
                })?
        };
        let certs_by_host: std::collections::HashMap<&str, &certs::Model> =
            cert_rows.iter().map(|c| (c.host.as_str(), c)).collect();

        let routes = models
            .into_iter()
            .map(|model| {
                let contested_by = contenders_for_host(&conflicts, &model.host);
                let cert = certs_by_host.get(model.host.as_str()).copied();
                TraefikDiscoveredRouteResponse::from_model(model, contested_by, cert)
            })
            .collect();

        Ok(TraefikDiscoveredRouteListResponse {
            routes,
            total,
            page,
            page_size,
            conflicts,
            discovery_running: self.handle.is_running(),
        })
    }

    /// Flip the per-route kill switch.
    ///
    /// Deliberately a plain column UPDATE: the migration's row-level trigger
    /// fires `notify_route_table_change()` when `enabled` changes, so every
    /// control plane node (and the split-mode `temps proxy` process) reloads
    /// its route table through the existing `route_table_changes` channel. A
    /// manual reload call here would bypass that path for the other nodes and
    /// double-load on this one.
    pub async fn set_route_enabled(
        &self,
        host: &str,
        enabled: bool,
    ) -> Result<TraefikDiscoveredRouteResponse, TraefikDiscoveryAdminError> {
        let normalized = host.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(TraefikDiscoveryAdminError::Validation {
                host: host.to_string(),
                message: "host must not be empty".to_string(),
            });
        }

        let model = discovered::Entity::find()
            .filter(discovered::Column::Host.eq(normalized.clone()))
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database("looking up a discovered route by host", e)
            })?
            .ok_or_else(|| TraefikDiscoveryAdminError::NotFound {
                host: normalized.clone(),
            })?;

        // Already in the requested state: skip the write so an idempotent
        // call can't trigger a needless route-table reload on every node.
        if model.enabled == enabled {
            let contested_by = self.contenders_for(&model.host);
            return Ok(TraefikDiscoveredRouteResponse::from_model(
                model,
                contested_by,
                None,
            ));
        }

        // Only `enabled` (plus `updated_at`, refreshed by
        // `ActiveModelBehavior::before_save`) ends up in the SET clause: every
        // other column stays `Unchanged`. That matters because the row-level
        // trigger's WHEN filter is what decides whether a route-table reload
        // is broadcast, and rewriting untouched columns would be noise.
        let mut active: discovered::ActiveModel = model.into();
        active.enabled = Set(enabled);
        let updated = active.update(self.db.as_ref()).await.map_err(|e| match e {
            DbErr::RecordNotUpdated | DbErr::RecordNotFound(_) => {
                TraefikDiscoveryAdminError::NotFound {
                    host: normalized.clone(),
                }
            }
            other => TraefikDiscoveryAdminError::database(
                "updating the enabled flag of a discovered route",
                other,
            ),
        })?;

        let contested_by = self.contenders_for(&updated.host);
        Ok(TraefikDiscoveredRouteResponse::from_model(
            updated,
            contested_by,
            None,
        ))
    }

    // ── TLS authorization service methods (ADR-041) ──────────────────────────

    /// Path A: authorize ACME issuance for a discovered host.
    ///
    /// Validates the host is a live discovered route, checks host ownership,
    /// persists the authorization record, then delegates to the injected
    /// `DiscoveredHostTlsProvisioner` to create/update the `domains` row and
    /// kick off the ACME challenge.
    pub async fn authorize_acme_cert(
        &self,
        host: &str,
        request: &RequestDiscoveredRouteCertRequest,
        user_id: i32,
    ) -> Result<certs::Model, TraefikDiscoveryAdminError> {
        use sea_orm::IntoActiveModel;

        let host = host.trim().to_ascii_lowercase();
        if host.is_empty() {
            return Err(TraefikDiscoveryAdminError::Validation {
                host: host.clone(),
                message: "host must not be empty".to_string(),
            });
        }
        if !matches!(request.challenge_type.as_str(), "http-01" | "dns-01") {
            return Err(TraefikDiscoveryAdminError::Validation {
                host: host.clone(),
                message: format!(
                    "challenge_type must be 'http-01' or 'dns-01', got '{}'",
                    request.challenge_type
                ),
            });
        }

        // Step 3: host must be an enabled discovered route on the current network.
        let network = self.handle.config().network.clone();
        let route = discovered::Entity::find()
            .filter(discovered::Column::Host.eq(host.clone()))
            .filter(discovered::Column::Enabled.eq(true))
            .filter(discovered::Column::Network.eq(network.clone()))
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database(
                    "looking up discovered route for TLS authorization",
                    e,
                )
            })?
            .ok_or_else(|| TraefikDiscoveryAdminError::NotFound { host: host.clone() })?;

        // Step 4: look up any existing cert row — needed both for the ownership
        // check below and for the upsert in Step 5.
        let existing = certs::Entity::find()
            .filter(certs::Column::Host.eq(host.clone()))
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database("looking up cert authorization record", e)
            })?;

        // Step 4b: host ownership check — verify the host is not already claimed
        // by an environment subdomain, a project custom domain, or a `domains`
        // row that a different cert record (or no cert record) owns. Evaluated
        // fresh from the DB, never from a stale reconcile-time set.
        self.check_host_ownership(&host, existing.as_ref()).await?;

        // Step 5: delegate to provisioner BEFORE writing the authorization record
        // so a provisioner failure doesn't leave a permanently-authorized row
        // with no certificate behind it.
        if let Some(prov) = &self.provisioner {
            prov.request_acme_cert(&host, &request.challenge_type)
                .await
                .map_err(|e| match e {
                    TlsProvisionerError::VerificationMethodConflict {
                        stored, declared, ..
                    } => TraefikDiscoveryAdminError::VerificationMethodConflict {
                        host: host.clone(),
                        stored,
                        declared,
                    },
                    TlsProvisionerError::Failed { reason, .. } => {
                        TraefikDiscoveryAdminError::Upstream {
                            host: host.clone(),
                            reason,
                        }
                    }
                })?;
        }

        // Step 6: persist or update the authorization record now that the
        // provisioner has confirmed success.
        let now = chrono::Utc::now();
        let cert_row = if let Some(existing) = existing {
            let mut active = existing.into_active_model();
            active.cert_authorized = Set(true);
            active.authorized_at = Set(Some(now));
            active.authorized_by_user_id = Set(Some(user_id));
            active.authorized_network = Set(network.clone());
            active.authorized_container_id = Set(route.target_container_id.clone());
            active.authorized_container_name = Set(route.target_container_name.clone());
            active.renewal_method = Set(request.challenge_type.clone());
            active.source = Set("acme".to_string());
            // Clear any existing drift state when re-authorizing.
            active.container_drift_detected_at = Set(None);
            active.last_drift_alarmed_container_id = Set(None);
            active.update(self.db.as_ref()).await.map_err(|e| {
                TraefikDiscoveryAdminError::database("updating cert authorization record", e)
            })?
        } else {
            certs::ActiveModel {
                host: Set(host.clone()),
                cert_authorized: Set(true),
                authorized_at: Set(Some(now)),
                authorized_by_user_id: Set(Some(user_id)),
                authorized_network: Set(network.clone()),
                authorized_container_id: Set(route.target_container_id.clone()),
                authorized_container_name: Set(route.target_container_name.clone()),
                container_drift_detected_at: Set(None),
                last_drift_alarmed_container_id: Set(None),
                renewal_method: Set(request.challenge_type.clone()),
                source: Set("acme".to_string()),
                certificate_id: Set(None),
                imported_at: Set(None),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database("inserting cert authorization record", e)
            })?
        };

        Ok(cert_row)
    }

    /// DELETE path: clear `cert_authorized` on the authorization record.
    ///
    /// Does NOT delete the `domains` row or the certificate — deleting live
    /// key material as a side effect of deauthorization would be a surprise.
    /// The existing domain-deletion endpoint is the way to do that.
    pub async fn deauthorize_cert(&self, host: &str) -> Result<(), TraefikDiscoveryAdminError> {
        use sea_orm::IntoActiveModel;

        let host = host.trim().to_ascii_lowercase();
        let row = certs::Entity::find()
            .filter(certs::Column::Host.eq(host.clone()))
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                TraefikDiscoveryAdminError::database("looking up cert authorization record", e)
            })?
            .ok_or_else(|| TraefikDiscoveryAdminError::NotFound { host: host.clone() })?;

        let mut active = row.into_active_model();
        active.cert_authorized = Set(false);
        active
            .update(self.db.as_ref())
            .await
            .map_err(|e| TraefikDiscoveryAdminError::database("deauthorizing cert", e))?;

        Ok(())
    }

    /// Path B: import certificates from a Traefik `acme.json` document.
    ///
    /// Runs the full 8-step validation chain (via `cert_validator`) for each
    /// requested host, then writes authorization records and certificate
    /// material (unless `dry_run` is true).
    pub async fn import_acme_json(
        &self,
        request: &ImportTraefikAcmeJsonRequest,
        user_id: i32,
    ) -> Result<ImportTraefikAcmeJsonResponse, TraefikDiscoveryAdminError> {
        use crate::services::cert_validator::{
            find_entries_for_host, parse_acme_json, validate_cert_entry, RawCertEntry,
        };
        use sea_orm::IntoActiveModel;

        if !matches!(request.renewal_method.as_str(), "http-01" | "dns-01") {
            return Err(TraefikDiscoveryAdminError::Validation {
                host: String::new(),
                message: format!(
                    "renewal_method must be 'http-01' or 'dns-01', got '{}'",
                    request.renewal_method
                ),
            });
        }

        // Parse the acme.json document once.
        let entries = parse_acme_json(&request.acme_json).map_err(|e| {
            TraefikDiscoveryAdminError::Validation {
                host: String::new(),
                message: format!("Failed to parse acme.json: {e}"),
            }
        })?;

        let network = self.handle.config().network.clone();
        let now = chrono::Utc::now();

        let mut verdicts: Vec<ImportedHostVerdict> = Vec::with_capacity(request.hosts.len());
        let mut succeeded = 0usize;
        let mut failed = 0usize;

        for host in &request.hosts {
            let host_normalized = host.trim().to_ascii_lowercase();

            // Step 1: host must be an enabled discovered route on the current network.
            let route = match discovered::Entity::find()
                .filter(discovered::Column::Host.eq(host_normalized.clone()))
                .filter(discovered::Column::Enabled.eq(true))
                .filter(discovered::Column::Network.eq(network.clone()))
                .one(self.db.as_ref())
                .await
                .map_err(|e| {
                    TraefikDiscoveryAdminError::database("looking up discovered route", e)
                })? {
                Some(r) => r,
                None => {
                    failed += 1;
                    verdicts.push(ImportedHostVerdict {
                        host: host_normalized.clone(),
                        success: false,
                        error: Some(format!(
                            "No enabled discovered route for '{host_normalized}' on network '{network}'"
                        )),
                        not_after: None,
                        sans: vec![],
                    });
                    continue;
                }
            };

            // Find matching entries in the parsed document by X.509 SAN.
            let matching = find_entries_for_host(&entries, &host_normalized);
            if matching.is_empty() {
                failed += 1;
                verdicts.push(ImportedHostVerdict {
                    host: host_normalized.clone(),
                    success: false,
                    error: Some(format!(
                        "No certificate in acme.json covers '{host_normalized}' (checked X.509 SANs)"
                    )),
                    not_after: None,
                    sans: vec![],
                });
                continue;
            }

            // Use the first matching entry (there should usually be exactly one).
            let acme_entry = matching[0];
            let raw = RawCertEntry {
                host: host_normalized.clone(),
                certificate_pem: acme_entry.certificate_pem.clone(),
                key_pem: acme_entry.key_pem.clone(),
            };

            // Steps 2–6, 8: run the full validation chain.
            let validated = match validate_cert_entry(&raw) {
                Ok(v) => v,
                Err(e) => {
                    failed += 1;
                    verdicts.push(ImportedHostVerdict {
                        host: host_normalized.clone(),
                        success: false,
                        error: Some(e.to_string()),
                        not_after: None,
                        sans: vec![],
                    });
                    continue;
                }
            };

            let not_after_str = validated.not_after.to_rfc3339();
            let sans = validated.sans.clone();

            // Fetch the existing cert authorization record for this host now,
            // before any write.  We need it both for the ownership check and to
            // decide whether to INSERT or UPDATE below.
            let existing_cert = certs::Entity::find()
                .filter(certs::Column::Host.eq(host_normalized.clone()))
                .one(self.db.as_ref())
                .await
                .map_err(|e| {
                    TraefikDiscoveryAdminError::database("looking up cert authorization record", e)
                })?;

            // Check host ownership: reject hosts that belong to another resource
            // (environment subdomain, custom domain, or a different cert row in
            // the `domains` table) before writing anything.
            if let Err(e) = self
                .check_host_ownership(&host_normalized, existing_cert.as_ref())
                .await
            {
                failed += 1;
                verdicts.push(ImportedHostVerdict {
                    host: host_normalized.clone(),
                    success: false,
                    error: Some(e.to_string()),
                    not_after: Some(not_after_str),
                    sans,
                });
                continue;
            }

            if !request.dry_run {
                // Write: delegate to provisioner, then upsert authorization record.
                let certificate_id = if let Some(prov) = &self.provisioner {
                    match prov
                        .save_imported_cert(
                            &host_normalized,
                            &validated.certificate_pem,
                            &validated.key_pem,
                            &request.renewal_method,
                        )
                        .await
                    {
                        Ok(id) => Some(id),
                        Err(e) => {
                            failed += 1;
                            verdicts.push(ImportedHostVerdict {
                                host: host_normalized.clone(),
                                success: false,
                                error: Some(format!("Provisioner error: {e}")),
                                not_after: Some(not_after_str),
                                sans,
                            });
                            continue;
                        }
                    }
                } else {
                    None
                };

                if let Some(existing) = existing_cert {
                    let mut active = existing.into_active_model();
                    active.cert_authorized = Set(true);
                    active.authorized_at = Set(Some(now));
                    active.authorized_by_user_id = Set(Some(user_id));
                    active.authorized_network = Set(network.clone());
                    active.authorized_container_id = Set(route.target_container_id.clone());
                    active.authorized_container_name = Set(route.target_container_name.clone());
                    active.renewal_method = Set(request.renewal_method.clone());
                    active.source = Set("imported".to_string());
                    active.certificate_id = Set(certificate_id);
                    active.imported_at = Set(Some(now));
                    active.container_drift_detected_at = Set(None);
                    active.last_drift_alarmed_container_id = Set(None);
                    active.update(self.db.as_ref()).await.map_err(|e| {
                        TraefikDiscoveryAdminError::database(
                            "updating cert authorization record",
                            e,
                        )
                    })?;
                } else {
                    certs::ActiveModel {
                        host: Set(host_normalized.clone()),
                        cert_authorized: Set(true),
                        authorized_at: Set(Some(now)),
                        authorized_by_user_id: Set(Some(user_id)),
                        authorized_network: Set(network.clone()),
                        authorized_container_id: Set(route.target_container_id.clone()),
                        authorized_container_name: Set(route.target_container_name.clone()),
                        container_drift_detected_at: Set(None),
                        last_drift_alarmed_container_id: Set(None),
                        renewal_method: Set(request.renewal_method.clone()),
                        source: Set("imported".to_string()),
                        certificate_id: Set(certificate_id),
                        imported_at: Set(Some(now)),
                        ..Default::default()
                    }
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        TraefikDiscoveryAdminError::database(
                            "inserting cert authorization record",
                            e,
                        )
                    })?;
                }
            }

            succeeded += 1;
            verdicts.push(ImportedHostVerdict {
                host: host_normalized.clone(),
                success: true,
                error: None,
                not_after: Some(not_after_str),
                sans,
            });
        }

        Ok(ImportTraefikAcmeJsonResponse {
            dry_run: request.dry_run,
            total_requested: request.hosts.len(),
            succeeded,
            failed,
            verdicts,
        })
    }

    /// Container names that lost a collision for `host` in the last pass.
    fn contenders_for(&self, host: &str) -> Vec<String> {
        let Some(outcome) = self.handle.last_outcome() else {
            return Vec::new();
        };
        outcome
            .conflicts
            .iter()
            .filter(|c| {
                c.host.eq_ignore_ascii_case(host)
                    && matches!(c.reason, ConflictReason::ClaimedByAnotherContainer { .. })
            })
            .map(|c| c.container_name.clone())
            .collect()
    }
}

/// Container names that lost a collision for `host`, read from an already
/// converted conflict list (avoids re-reading the outcome per row).
fn contenders_for_host(conflicts: &[TraefikDiscoveryConflictResponse], host: &str) -> Vec<String> {
    conflicts
        .iter()
        .filter(|c| c.host.eq_ignore_ascii_case(host) && c.reason == "claimed_by_another_container")
        .map(|c| c.container_name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_deployer::traefik_discovery::{HostConflict, TraefikDiscoveryConfig};

    fn route_model(host: &str, enabled: bool) -> discovered::Model {
        let now = Utc::now();
        discovered::Model {
            id: 1,
            host: host.to_string(),
            router_name: "app".to_string(),
            target_container_id: "abc123".to_string(),
            target_container_name: "whoami".to_string(),
            target_port: 80,
            target_host_port: None,
            network: "temps".to_string(),
            tls: false,
            enabled,
            last_seen_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    fn disabled_handle() -> Arc<TraefikDiscoveryHandle> {
        Arc::new(TraefikDiscoveryHandle::not_running(
            TraefikDiscoveryConfig::resolve(None, None, "temps"),
            format!("{ENABLED_ENV} is not set to 'true'"),
        ))
    }

    /// Sea-ORM's `.count()` / paginator `num_items()` execute
    /// `SELECT COUNT(*) AS num_items ...` and read the result as a `BigInt`.
    fn count_row(n: i64) -> std::collections::BTreeMap<String, sea_orm::Value> {
        let mut row = std::collections::BTreeMap::new();
        row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(n)));
        row
    }

    #[tokio::test]
    async fn status_reports_not_configured_with_a_setup_path_when_disabled() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(0)], vec![count_row(0)]])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let status = service.status().await.expect("status must not fail");

        assert!(!status.configured, "a disabled instance is not configured");
        assert!(!status.enabled);
        assert_eq!(status.network, "temps");
        assert_eq!(status.poll_interval_seconds, 30);
        assert_eq!(
            status.reason.as_deref(),
            Some("TEMPS_TRAEFIK_DISCOVERY_ENABLED is not set to 'true'"),
            "an unconfigured feature must say exactly why it is off"
        );
        assert_eq!(
            status.setup.enable_env_var,
            "TEMPS_TRAEFIK_DISCOVERY_ENABLED"
        );
        assert_eq!(
            status.setup.network_env_var,
            "TEMPS_TRAEFIK_DISCOVERY_NETWORK"
        );
        assert!(
            status
                .setup
                .example
                .contains("TEMPS_TRAEFIK_DISCOVERY_ENABLED=true"),
            "the setup example must be copy-pasteable, got {:?}",
            status.setup.example
        );
        assert!(status.setup.requires_restart);
        assert!(status.last_reconciliation.is_none());
    }

    #[tokio::test]
    async fn status_reports_route_counts() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(4)], vec![count_row(3)]])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let status = service.status().await.expect("status must not fail");

        assert_eq!(status.discovered_route_count, 4);
        assert_eq!(
            status.enabled_route_count, 3,
            "a suppressed row must still be counted as discovered but not as enabled"
        );
    }

    #[tokio::test]
    async fn status_surfaces_a_database_failure_with_context() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let err = service
            .status()
            .await
            .expect_err("a DB failure must surface, not be swallowed");
        assert!(
            matches!(err, TraefikDiscoveryAdminError::Database { .. }),
            "expected a Database error, got {err:?}"
        );
        assert!(
            err.to_string().contains("counting discovered routes"),
            "error must name the operation, got {err}"
        );
    }

    #[tokio::test]
    async fn list_routes_returns_rows_and_marks_disabled_ones_inactive() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(2)]])
            .append_query_results([vec![
                route_model("app.example.com", true),
                route_model("suppressed.example.com", false),
            ]])
            // Cert rows lookup: no certs authorized for these hosts.
            .append_query_results([Vec::<certs::Model>::new()])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let list = service
            .list_routes(None, None)
            .await
            .expect("listing must succeed");

        assert_eq!(list.total, 2);
        assert_eq!(list.page, 1);
        assert_eq!(list.page_size, 20);
        assert!(!list.discovery_running);
        assert_eq!(list.routes.len(), 2);

        let enabled_route = &list.routes[0];
        assert_eq!(enabled_route.host, "app.example.com");
        assert!(enabled_route.active);
        assert!(enabled_route.inactive_reason.is_none());
        assert!(enabled_route.contested_by.is_empty());

        let suppressed = &list.routes[1];
        assert!(!suppressed.active);
        assert!(
            suppressed
                .inactive_reason
                .as_deref()
                .is_some_and(|r| r.contains("temps traefik-discovery routes enable")),
            "a suppressed route must tell the operator how to undo it, got {:?}",
            suppressed.inactive_reason
        );
    }

    #[tokio::test]
    async fn list_routes_clamps_page_size_to_the_project_maximum() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(0)]])
            .append_query_results([Vec::<discovered::Model>::new()])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let list = service
            .list_routes(Some(0), Some(5_000))
            .await
            .expect("listing must succeed");

        assert_eq!(list.page_size, MAX_PAGE_SIZE);
        assert_eq!(list.page, 1, "page 0 must be normalized to the first page");
        assert!(list.routes.is_empty());
    }

    #[tokio::test]
    async fn list_routes_surfaces_conflicts_that_have_no_row() {
        let outcome = ReconcileOutcome {
            network: "temps".to_string(),
            containers_scanned: 3,
            skipped_temps_managed: 1,
            routes_upserted: 1,
            routes_unchanged: 0,
            routes_removed: 0,
            conflicts: vec![
                HostConflict {
                    host: "app.example.com".to_string(),
                    container_id: "loser".to_string(),
                    container_name: "whoami-2".to_string(),
                    router_name: "app".to_string(),
                    reason: ConflictReason::ClaimedByAnotherContainer {
                        winner_container_name: "whoami".to_string(),
                    },
                },
                HostConflict {
                    host: "console.example.com".to_string(),
                    container_id: "sneaky".to_string(),
                    container_name: "impostor".to_string(),
                    router_name: "console".to_string(),
                    reason: ConflictReason::OwnedByTempsRoute,
                },
            ],
            completed_at: Utc::now(),
        };
        let handle = Arc::new(TraefikDiscoveryHandle::not_running(
            TraefikDiscoveryConfig::resolve(Some("true"), None, "temps"),
            "watcher stopped",
        ));
        // The handle used here has no live service, so drive the conflict path
        // through the list conversion directly as well as through the service.
        let conflicts: Vec<TraefikDiscoveryConflictResponse> = outcome
            .conflicts
            .iter()
            .map(TraefikDiscoveryConflictResponse::from_conflict)
            .collect();

        assert_eq!(conflicts[0].reason, "claimed_by_another_container");
        assert_eq!(
            conflicts[0].winner_container_name.as_deref(),
            Some("whoami")
        );
        assert_eq!(conflicts[1].reason, "owned_by_temps_route");
        assert!(conflicts[1].winner_container_name.is_none());
        assert!(
            conflicts[1].detail.contains("Temps-managed route"),
            "the conflict must explain itself, got {:?}",
            conflicts[1].detail
        );

        assert_eq!(
            contenders_for_host(&conflicts, "APP.EXAMPLE.COM"),
            vec!["whoami-2".to_string()],
            "host matching must be case-insensitive"
        );
        assert!(
            contenders_for_host(&conflicts, "console.example.com").is_empty(),
            "a host owned by a Temps route is not a contender for a discovered row"
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(0)]])
            .append_query_results([Vec::<discovered::Model>::new()])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), handle);
        let list = service.list_routes(None, None).await.expect("listing");
        assert!(
            list.conflicts.is_empty(),
            "conflicts come from a live watcher; a stopped one reports none"
        );
    }

    #[tokio::test]
    async fn set_route_enabled_persists_the_new_value() {
        let updated = route_model("app.example.com", false);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![route_model("app.example.com", true)]])
            .append_query_results([vec![updated.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let route = service
            .set_route_enabled("APP.example.com ", false)
            .await
            .expect("toggling must succeed");

        assert!(!route.enabled);
        assert!(!route.active);
        assert!(route.inactive_reason.is_some());
    }

    #[tokio::test]
    async fn set_route_enabled_is_a_no_op_when_already_in_that_state() {
        // Only ONE query result is queued: a second DB round trip would panic
        // the mock, which is exactly the regression we want to catch — an
        // unnecessary UPDATE fires PG NOTIFY and reloads every node's routes.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![route_model("app.example.com", true)]])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let route = service
            .set_route_enabled("app.example.com", true)
            .await
            .expect("a redundant toggle must succeed");

        assert!(route.enabled);
    }

    #[tokio::test]
    async fn set_route_enabled_unknown_host_returns_not_found_with_the_host() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<discovered::Model>::new()])
            .into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let err = service
            .set_route_enabled("nope.example.com", false)
            .await
            .expect_err("an unknown host must not silently succeed");

        assert!(
            matches!(&err, TraefikDiscoveryAdminError::NotFound { host } if host == "nope.example.com"),
            "expected NotFound carrying the host, got {err:?}"
        );
        assert!(err.to_string().contains("nope.example.com"));
    }

    #[tokio::test]
    async fn set_route_enabled_rejects_a_blank_host() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = TraefikDiscoveryAdminService::new(Arc::new(db), disabled_handle());

        let err = service
            .set_route_enabled("   ", true)
            .await
            .expect_err("a blank host is a validation error, not a lookup");

        assert!(
            matches!(err, TraefikDiscoveryAdminError::Validation { .. }),
            "expected Validation, got {err:?}"
        );
    }
}
