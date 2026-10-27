// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The primary address of an environment, the preview domain's status and
//! the deploy warning, for the surfaces outside this crate (ADR 068 D2, D4,
//! D5). The facts come from [`ReachabilityService`]; the decisions from
//! [`addresses`](super::addresses).
//!
//! Both answers are cached: an environment's address for
//! [`ADDRESS_TTL`] (the monitor and the project page ask often, and judging
//! an address can take a DNS lookup), the preview domain's status for
//! [`PREVIEW_TTL`] or until what it depends on changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use serde::Serialize;
use temps_core::environment_address::{
    EnvironmentAddress, EnvironmentAddressError, EnvironmentAddresses, PreviewDomainStatus,
    ReachabilityWarning,
};
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
use temps_entities::{deployment_containers, deployments, ingress_connectors, node_public_ingress};
use tracing::{info, warn};
use utoipa::ToSchema;

use super::addresses::{
    no_address_reason, preview_domain_status, primary_address, reachability_warning,
    PreviewDomainFacts, TunnelOffer, WarningFacts,
};
use super::compute::{is_local_only, Resolution};
use super::model::Audience;
use super::service::{resolve, ReachabilityError, ReachabilityService};

/// How long an environment's address is reused.
pub const ADDRESS_TTL: Duration = Duration::from_secs(60);
/// How long the preview domain's status is reused when nothing it depends
/// on changed (D2: "every 30 minutes").
pub const PREVIEW_TTL: Duration = Duration::from_secs(30 * 60);
/// Most environments whose address is kept: beyond it the oldest go.
const MAX_CACHED: usize = 1024;
/// A connector that carries traffic.
const CONNECTOR_READY: &str = "ready";

/// `GET /settings/preview-domain/status` (ADR 068 D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PreviewDomainStatusResponse {
    #[schema(example = "apps.example.com")]
    pub preview_domain: String,
    pub status: PreviewDomainStatus,
    /// Why, in words.
    pub reason: String,
    /// The tunnel that gives every environment an address instead, when
    /// one does (ADR 068 D3): apps are reachable without this domain.
    pub tunnel_environment_hosts: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub checked_at: DateTime<Utc>,
}

struct PreviewCache {
    key: String,
    at: Instant,
    response: PreviewDomainStatusResponse,
}

/// The batch key of an environment's warning notification (D5.4).
pub fn episode_key(environment_id: i32) -> String {
    format!("reachability:{environment_id}")
}

pub struct EnvironmentAddressService {
    reachability: Arc<ReachabilityService>,
    notifications: Arc<dyn NotificationService>,
    cache: Mutex<HashMap<i32, (Instant, EnvironmentAddress)>>,
    preview: tokio::sync::Mutex<Option<PreviewCache>>,
}

impl EnvironmentAddressService {
    pub fn new(
        reachability: Arc<ReachabilityService>,
        notifications: Arc<dyn NotificationService>,
    ) -> Self {
        Self {
            reachability,
            notifications,
            cache: Mutex::new(HashMap::new()),
            preview: tokio::sync::Mutex::new(None),
        }
    }

    /// D2, recomputed when the preview domain, the tunnels or the nodes'
    /// public ingress changed, and at least every [`PREVIEW_TTL`].
    pub async fn preview_domain_status(
        &self,
    ) -> Result<PreviewDomainStatusResponse, ReachabilityError> {
        let settings = self.reachability.settings().await?;
        let db = self.reachability.deps.db.as_ref();
        let connectors = ingress_connectors::Entity::find().all(db).await?;
        let ingress = node_public_ingress::Entity::find()
            .select_only()
            .column(node_public_ingress::Column::NodeId)
            .column(node_public_ingress::Column::UpdatedAt)
            .into_tuple::<(i32, DateTime<Utc>)>()
            .all(db)
            .await?;
        let key = format!(
            "{}|{:?}|{:?}|{}|{}",
            settings.preview_domain,
            settings.edge_target,
            settings.external_url,
            connectors
                .iter()
                .map(|c| format!("{}:{}:{}:{}", c.id, c.domain, c.status, c.updated_at))
                .collect::<Vec<_>>()
                .join(","),
            ingress
                .iter()
                .map(|(id, at)| format!("{id}:{at}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        let mut cached = self.preview.lock().await;
        if let Some(entry) = cached.as_ref() {
            if entry.key == key && entry.at.elapsed() < PREVIEW_TTL {
                return Ok(entry.response.clone());
            }
        }
        let domain = settings
            .preview_domain
            .trim()
            .trim_start_matches("*.")
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let probe = if domain.is_empty() || is_local_only(&domain) {
            Resolution::Skipped
        } else {
            let label = format!(
                "temps-probe-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            );
            match resolve(&format!("{label}.{domain}")).await {
                Ok(ips) if ips.is_empty() => Resolution::NoRecord,
                Ok(ips) => Resolution::Ips(ips),
                Err(error) => Resolution::Failed(error),
            }
        };
        let zones: Vec<String> = connectors
            .iter()
            .filter(|c| c.status == CONNECTOR_READY)
            .map(|c| c.domain.clone())
            .collect();
        let directs = self.reachability.directs(&settings).await;
        let (status, reason) = preview_domain_status(
            &PreviewDomainFacts {
                preview_domain: &domain,
                probe,
                tunnel_zones: &zones,
            },
            &directs,
        );
        let response = PreviewDomainStatusResponse {
            preview_domain: settings.preview_domain.clone(),
            status,
            reason,
            tunnel_environment_hosts: connectors
                .iter()
                .find(|c| c.environment_hosts && !crate::service::is_deleting(c))
                .map(|c| c.name.clone()),
            checked_at: Utc::now(),
        };
        info!(
            preview_domain = %response.preview_domain,
            status = status.as_str(),
            "classified the preview domain"
        );
        *cached = Some(PreviewCache {
            key,
            at: Instant::now(),
            response: response.clone(),
        });
        Ok(response)
    }

    /// The environment's address, judged now.
    pub async fn compute(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<EnvironmentAddress, ReachabilityError> {
        let reach = &self.reachability;
        let scope = reach.scope(project_id, environment_id).await?;
        let settings = reach.settings().await?;
        let gathered = reach
            .gather(&scope.project, &scope.environment, &settings.preview_domain)
            .await?;
        let directs = reach.directs(&settings).await;
        let addresses = reach
            .judge(
                &gathered.addresses,
                &directs,
                gathered.deployed,
                &scope.environment.name,
            )
            .await;
        let primary = primary_address(&addresses);
        let no_address = if primary.is_none() {
            no_address_reason(&addresses)
        } else {
            None
        };

        let warning = if no_address.is_some() && gathered.deployed {
            let preview = self.preview_domain_status().await?.status;
            let db = reach.deps.db.as_ref();
            // A tunnel only carries what workers serve (ADR 058), so it is
            // offered only for an environment running on workers.
            let connectors = if gathered.on_workers {
                ingress_connectors::Entity::find()
                    .filter(ingress_connectors::Column::Status.eq(CONNECTOR_READY))
                    .all(db)
                    .await?
            } else {
                Vec::new()
            };
            // The host a tunnel's environment hosts would give it (D3). A
            // tunnel that already has them on is the only one offered (at
            // most one may), and an environment with a private or
            // signed-in address is never offered a public tunnel host.
            let restricted = addresses.iter().any(|a| a.audience != Audience::Public);
            let label = super::environment_hosts::environment_host_label(
                &scope.project.slug,
                &scope.environment,
                settings.production_hostname_template.as_deref(),
            );
            let connectors: Vec<_> = if restricted {
                Vec::new()
            } else if connectors.iter().any(|c| c.environment_hosts) {
                connectors
                    .into_iter()
                    .filter(|c| c.environment_hosts)
                    .collect()
            } else {
                connectors
            };
            let tunnels: Vec<TunnelOffer> = connectors
                .into_iter()
                .map(|c| TunnelOffer {
                    connector_id: c.id,
                    hostname: super::environment_hosts::natural_host(&label, &c.domain),
                    name: c.name,
                })
                .collect();
            let portless = match scope.environment.current_deployment_id {
                Some(id) => self.is_portless(id).await?,
                None => false,
            };
            reachability_warning(&WarningFacts {
                environment_id,
                label: &scope.environment.subdomain,
                project_slug: &scope.project.slug,
                deployed: gathered.deployed,
                hosted_externally: scope.project.hosting
                    == temps_entities::types::Hosting::External,
                portless,
                addresses: &addresses,
                preview,
                tunnels: &tunnels,
                public_ingress: !directs.is_empty(),
            })
        } else {
            None
        };

        let address = EnvironmentAddress {
            environment_id,
            primary,
            no_address_reason: no_address,
            warning,
        };
        self.remember(&address);
        Ok(address)
    }

    /// A deployment with containers none of which declares a port, and no
    /// static files: a background worker nobody opens.
    async fn is_portless(&self, deployment_id: i32) -> Result<bool, ReachabilityError> {
        let db = self.reachability.deps.db.as_ref();
        let static_dir = deployments::Entity::find_by_id(deployment_id)
            .one(db)
            .await?
            .and_then(|d| d.static_dir_location);
        if static_dir.is_some() {
            return Ok(false);
        }
        let ports: Vec<Option<i32>> = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .select_only()
            .column(deployment_containers::Column::ContainerPort)
            .into_tuple::<Option<i32>>()
            .all(db)
            .await?;
        Ok(!ports.is_empty() && ports.iter().all(Option::is_none))
    }

    fn remember(&self, address: &EnvironmentAddress) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        cache.retain(|_, (at, _)| at.elapsed() < ADDRESS_TTL);
        if cache.len() >= MAX_CACHED {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(id, _)| *id)
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(address.environment_id, (Instant::now(), address.clone()));
    }

    fn fresh(&self, environment_id: i32) -> Option<EnvironmentAddress> {
        let cache = self.cache.lock().ok()?;
        cache
            .get(&environment_id)
            .filter(|(at, _)| at.elapsed() < ADDRESS_TTL)
            .map(|(_, address)| address.clone())
    }

    /// Open or close the environment's notification episode (D5.4).
    async fn notify(&self, project_id: i32, address: &EnvironmentAddress) {
        let key = episode_key(address.environment_id);
        let result = match &address.warning {
            Some(warning) => self
                .notifications
                .open_episode(&key, notification(project_id, warning))
                .await
                .map(|sent| {
                    if sent {
                        info!(
                            environment_id = address.environment_id,
                            "sent the reachability warning notification"
                        );
                    }
                }),
            // Only a reachable address clears it: "being set up" or "cannot
            // tell" leaves the episode as it is, so a flaky lookup never
            // sends it again.
            None if address.primary.is_some() => self.notifications.close_episode(&key).await,
            None => Ok(()),
        };
        if let Err(error) = result {
            warn!(
                environment_id = address.environment_id,
                %error,
                "could not update the reachability warning notification"
            );
        }
    }
}

fn notification(project_id: i32, warning: &ReachabilityWarning) -> NotificationData {
    let mut metadata = HashMap::new();
    metadata.insert("project_id".to_string(), project_id.to_string());
    metadata.insert(
        "environment_id".to_string(),
        warning.environment_id.to_string(),
    );
    metadata.insert("kind".to_string(), "reachability_warning".to_string());
    NotificationData {
        title: format!("Nobody can reach {} yet", warning.environment),
        message: warning.message(),
        notification_type: NotificationType::Warning,
        priority: NotificationPriority::Normal,
        severity: Some("warning".to_string()),
        metadata,
        ..Default::default()
    }
}

fn address_error(
    project_id: i32,
    environment_id: i32,
    error: ReachabilityError,
) -> EnvironmentAddressError {
    EnvironmentAddressError {
        project_id,
        environment_id,
        reason: error.to_string(),
    }
}

#[async_trait]
impl EnvironmentAddresses for EnvironmentAddressService {
    async fn address(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<EnvironmentAddress, EnvironmentAddressError> {
        if let Some(address) = self.fresh(environment_id) {
            return Ok(address);
        }
        self.compute(project_id, environment_id)
            .await
            .map_err(|e| address_error(project_id, environment_id, e))
    }

    fn cached(&self, environment_id: i32) -> Option<EnvironmentAddress> {
        let cache = self.cache.lock().ok()?;
        cache
            .get(&environment_id)
            .map(|(_, address)| address.clone())
    }

    async fn deployment_completed(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Option<ReachabilityWarning> {
        match self.compute(project_id, environment_id).await {
            Ok(address) => {
                self.notify(project_id, &address).await;
                address.warning
            }
            Err(error) => {
                warn!(
                    project_id,
                    environment_id,
                    %error,
                    "could not tell whether the deployed environment is reachable"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::environment_address::{ReachabilityFix, ReachabilityFixKind};

    #[test]
    fn the_episode_key_is_per_environment() {
        assert_eq!(episode_key(42), "reachability:42");
    }

    #[test]
    fn the_notification_carries_the_warning_words() {
        let warning = ReachabilityWarning {
            environment_id: 9,
            environment: "shop-production".into(),
            headline: "Deployed, but nobody can reach shop-production yet".into(),
            reason: "shop-production.localho.st only resolves to this computer.".into(),
            fixes: vec![ReachabilityFix {
                kind: ReachabilityFixKind::CustomDomain,
                label: "Add a domain to this environment".into(),
                href: None,
                command: None,
                connector_id: None,
                hostname: None,
            }],
        };
        let data = notification(3, &warning);
        assert_eq!(data.title, "Nobody can reach shop-production yet");
        assert!(data.message.contains("only resolves to this computer"));
        assert!(data.message.contains("add a domain to this environment"));
        assert_eq!(
            data.metadata.get("environment_id").map(String::as_str),
            Some("9")
        );
        assert!(
            !data.bypass_throttling,
            "the episode decides, not the caller"
        );
    }
}
