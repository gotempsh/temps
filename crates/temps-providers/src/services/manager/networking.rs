// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service addresses, overlay attachment and standalone service DNS.

use crate::externalsvc::{ServiceConfig, ServiceType};
use crate::services::*;
use sea_orm::EntityTrait;
use temps_entities::{external_services, nodes};
use tracing::{debug, info, warn};

/// TTL for the `<service>.temps.local` A record of a standalone managed
/// service. Matches the Tier-2 cluster-member TTL: a standalone container's
/// overlay IP only changes when the container is recreated, and 30s bounds
/// how long a consumer can keep dialling a dead address after that.
const STANDALONE_SERVICE_DNS_TTL: i32 = 30;

impl ExternalServiceManager {
    pub async fn get_local_address(
        &self,
        service: external_services::Model,
    ) -> Result<String, ExternalServiceError> {
        // Get service parameters
        let service_config = self.get_service_config(service.id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Create service instance
        let service_instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type,
            &service_config.parameters,
        )?;

        // Get local address from service instance
        let address = service_instance
            .get_local_address(service_config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get local address: {}", e),
            })?;

        info!(
            "Retrieved local address {} for service {}",
            address, service.id
        );
        Ok(address)
    }

    /// The address the control plane dials to reach a standalone service on
    /// a worker node: the node's data address, where the worker's agent
    /// publishes the service's host port (see `nodes::Model::data_address`).
    ///
    /// `None` for a service on the control plane (no node, or node 0). A
    /// worker that has no node record is an error rather than `None`, because
    /// falling back to the stored loopback host would reach the control plane,
    /// and possibly an unrelated service there on the same port.
    pub async fn worker_data_address(
        &self,
        service: &external_services::Model,
    ) -> Result<Option<String>, ExternalServiceError> {
        let Some(node_id) = service
            .node_id
            .filter(|id| *id != Self::CONTROL_PLANE_NODE_ID)
        else {
            return Ok(None);
        };
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Service {} ({}) is placed on node {}, which has no node record",
                    service.id, service.name, node_id
                ),
            })?;
        Ok(Some(node.data_address().to_string()))
    }

    /// Get the effective address components for a service.
    ///
    /// Returns `(container_name, internal_port, host_port)` where:
    /// - `container_name` is the Docker container name used in connection strings
    /// - `internal_port` is the port inside the container (e.g., 5432 for Postgres)
    /// - `host_port` is the mapped port on the host machine
    ///
    /// `host_port` is only meaningful **on the service's own host**: managed
    /// service ports bind to `127.0.0.1` (see `crate::utils::local_port_binding`),
    /// so `<other node>:<host_port>` is never reachable. Cross-node addressing
    /// goes through [`Self::get_service_cross_node_link`] instead.
    pub async fn get_service_effective_address(
        &self,
        service_id: i32,
    ) -> Result<(String, String, String), ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let parameters = self.get_service_parameters(service_id).await?;
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Use Docker container name and internal port directly — these match what
        // get_runtime_env_vars() puts in env var values (always Docker container names,
        // regardless of DeploymentMode). This is critical for cross-node env var rewriting.
        // An imported service's real container name (stored raw in parameters, since
        // get_docker_container_name() only knows the derived `{type}-{name}` form)
        // wins over the derived one.
        let container_name = service_config
            .parameters
            .get("container_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| service_instance.get_docker_container_name());
        let internal_port = service_instance.get_docker_internal_port();

        // get_local_address returns "localhost:{host_port}" — we need the host port
        // for the replacement target (private_address:host_port)
        let local_address = service_instance
            .get_local_address(service_config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get local address: {}", e),
            })?;
        let host_port = local_address
            .rsplit(':')
            .next()
            .unwrap_or(&internal_port)
            .to_string();

        Ok((container_name, internal_port, host_port))
    }

    /// Best-effort dual-attach of a locally-managed container to the
    /// multi-host overlay, returning the IP it ended up with there.
    ///
    /// Managed service containers are created on `temps-app-network` only,
    /// which is a per-host bridge — an address on it means nothing to a
    /// container on another node. Attaching to the overlay is what gives
    /// the container a genuinely routable cross-node IP, and therefore
    /// something a DNS A record can usefully point at.
    ///
    /// Returns `None` when the overlay is not bootstrapped on this host
    /// (single-node installs), which is not an error: single-node installs
    /// never need a cross-node address in the first place.
    async fn attach_container_to_overlay(&self, container_ref: &str) -> Option<String> {
        // One read of the selected overlay (ADR 059), so the network
        // validated is the one connected to.
        let names = temps_network::overlay_name::current();
        let network_config = temps_network::NetworkConfig::default().with_overlay_names(&names);
        let overlay = names.docker_network;
        let docker = match self.docker.get() {
            Some(d) => d,
            None => {
                debug!(
                    container = container_ref,
                    "Docker unavailable; skipping overlay attach"
                );
                return None;
            }
        };
        if let Err(error) =
            temps_network::docker::validate_owned_network(docker, &network_config).await
        {
            debug!(
                container = container_ref,
                overlay = %overlay,
                error = %error,
                "Temps-owned overlay network is unavailable; skipping attach"
            );
            return None;
        }

        let req = bollard::models::NetworkConnectRequest {
            container: container_ref.to_string(),
            ..Default::default()
        };
        match docker.connect_network(&overlay, req).await {
            Ok(()) => {
                info!(
                    container = container_ref,
                    overlay = %overlay,
                    "Attached managed service container to overlay"
                );
            }
            // 403 from /networks/<id>/connect means "already connected".
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 403, ..
            }) => {
                debug!(
                    container = container_ref,
                    overlay = %overlay,
                    "Managed service container already attached to overlay"
                );
            }
            Err(e) => {
                warn!(
                    container = container_ref,
                    overlay = %overlay,
                    error = %e,
                    "Failed to attach managed service container to overlay"
                );
                return None;
            }
        }

        self.lookup_container_network_ip(container_ref, &overlay)
            .await
    }

    /// Publish (or refresh) the `<service>.temps.local` A record for a
    /// **standalone** managed service.
    ///
    /// This is the counterpart of the Tier-2/Tier-3 registration cluster
    /// members already get. Without it a single-container Postgres has no
    /// name at all, and the only address a cross-node consumer could be
    /// handed is `<node private address>:<host port>` — which is
    /// permanently unreachable because the port is bound to loopback on
    /// the service's host.
    ///
    /// Cluster services are skipped: `postgres_role_reconciler` owns
    /// `<service>.temps.local` for those and would fight this writer.
    ///
    /// Best-effort by design — a service that provisioned correctly must
    /// not be failed because the overlay isn't up. When no record can be
    /// published, [`Self::get_service_cross_node_link`] reports
    /// `dns_record_published: false` and the deploy path refuses to hand
    /// out a broken address instead of silently doing so.
    pub async fn register_standalone_service_dns(
        &self,
        service_id: i32,
    ) -> Result<Option<String>, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        if service.topology == "cluster" {
            debug!(
                service_id,
                "Skipping standalone DNS registration for cluster service"
            );
            return Ok(None);
        }

        let Some(fqdn) =
            crate::service_dns::standalone_service_fqdn(&service.name, service.slug.as_deref())
        else {
            warn!(
                service_id,
                service_name = %service.name,
                "Service name yields no legal DNS label; no internal record published"
            );
            return Ok(None);
        };

        let (container_name, internal_port, _host_port) =
            self.get_service_effective_address(service_id).await?;

        let overlay_ip = match service.node_id {
            // Control plane: we own this Docker daemon, so attach + inspect.
            None => self.attach_container_to_overlay(&container_name).await,
            // Worker node: the agent attached the container when it created
            // it and reported the overlay IP back; we persisted it as an
            // inferred parameter. We cannot inspect a remote daemon here.
            Some(_) => self
                .get_service_parameters(service_id)
                .await?
                .get("compute_ip")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|ip| !ip.is_empty())
                .map(str::to_string),
        };

        let Some(ip) = overlay_ip else {
            warn!(
                service_id,
                service_name = %service.name,
                fqdn = %fqdn,
                node_id = ?service.node_id,
                "No overlay address for managed service; {} will not resolve until the \
                 overlay network is bootstrapped and the service restarted",
                fqdn
            );
            return Ok(None);
        };

        let target_port = internal_port.parse::<i32>().ok();
        let draft = temps_dns::EndpointDraft {
            fqdn: fqdn.clone(),
            record_type: temps_dns::InternalRecordType::A,
            target_ip: Some(ip.clone()),
            target_port,
            ttl: STANDALONE_SERVICE_DNS_TTL,
            owner_kind: temps_dns::InternalOwnerKind::ServiceRole,
            owner_id: service_id as i64,
            node_id: service.node_id,
        };

        self.dns_registry
            .replace_endpoints_for_owner(
                temps_dns::InternalOwnerKind::ServiceRole,
                service_id as i64,
                &[draft],
            )
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to publish internal DNS record {} for service {} ({}): {}",
                    fqdn, service_id, service.name, e
                ),
            })?;

        info!(
            service_id,
            fqdn = %fqdn,
            ip = %ip,
            port = ?target_port,
            "Published internal DNS A record for standalone managed service"
        );
        Ok(Some(fqdn))
    }

    /// How a container running on a **different node** should address this
    /// service, and whether that address can actually work right now.
    ///
    /// The caller (the deployment planner) uses `container_name` as the
    /// needle to rewrite in already-built connection strings and `fqdn` as
    /// the replacement. It must *not* fall back to
    /// `<private address>:<host port>`: managed service ports bind to
    /// `127.0.0.1` on their own host, so that form is unreachable from
    /// anywhere else and produces a connection string that fails silently.
    pub async fn get_service_cross_node_link(
        &self,
        service_id: i32,
    ) -> Result<crate::service_dns::ServiceCrossNodeLink, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let (container_name, _internal_port, _host_port) =
            self.get_service_effective_address(service_id).await?;

        let fqdn =
            crate::service_dns::standalone_service_fqdn(&service.name, service.slug.as_deref());

        // A published record is what makes the name resolve. Cluster
        // services and standalone services both own their records under
        // `ServiceRole` + `external_services.id`, so one lookup covers both.
        let published = match fqdn.as_deref() {
            Some(name) => self
                .dns_registry
                .list_by_owner(temps_dns::InternalOwnerKind::ServiceRole, service_id as i64)
                .await
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to read internal DNS records for service {} ({}): {}",
                        service_id, service.name, e
                    ),
                })?
                .iter()
                .any(|r| r.fqdn == name),
            None => false,
        };

        Ok(crate::service_dns::ServiceCrossNodeLink {
            service_id,
            service_name: service.name,
            container_name,
            node_id: service.node_id,
            fqdn,
            dns_record_published: published,
        })
    }
}
