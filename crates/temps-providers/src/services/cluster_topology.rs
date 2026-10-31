// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cluster member layout: port blocks, member lists, addresses and execution
//! routes.

use super::*;
use crate::externalsvc::ClusterRole;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use std::collections::{BTreeSet, HashMap};
use temps_entities::service_members;

/// Select the network endpoint advertised for a managed cluster member.
///
/// The local application-network address is deliberately considered only for
/// control-plane members (`node_id = None`). Docker bridge addresses are local
/// to one daemon and must never replace a remote member's overlay/underlay
/// address.
pub(super) fn select_member_dns_endpoint(
    node_id: Option<i32>,
    overlay_ip: Option<&str>,
    local_network_ip: Option<&str>,
    underlay_endpoint: Option<(String, i32)>,
    container_port: u16,
) -> Option<(String, i32)> {
    let valid_ip = |ip: &str| {
        let trimmed = ip.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };

    if let Some(ip) = overlay_ip.and_then(valid_ip) {
        return Some((ip, container_port as i32));
    }

    if node_id.is_none() {
        return local_network_ip
            .and_then(valid_ip)
            .map(|ip| (ip, container_port as i32));
    }

    underlay_endpoint
}

/// The address a control-plane cluster member advertises to, and publishes
/// its port on for, members running on worker nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ControlPlaneMemberAddress {
    pub(super) ip: std::net::IpAddr,
    /// `false` when the operator's private address is publicly routable and no
    /// WireGuard mesh is on, so the member's port is published on a public
    /// interface (the same trade-off a direct-mode worker makes).
    pub(super) private: bool,
}

/// Choose the control plane's cross-host address for cluster members.
///
/// Mirrors `nodes::Model::data_address` for workers: the operator's
/// `multi_node.private_address` when it is private, the control plane's
/// WireGuard mesh address when the private address is public and the mesh is
/// on, otherwise the configured address as-is. Never guesses: an address
/// derived from the default route is usually the public interface on a VPC
/// host, and publishing a database there is both unreachable over the VPC
/// and exposed to the internet.
pub(super) fn select_control_plane_member_address(
    private_address: Option<&str>,
    mesh_address: Option<std::net::IpAddr>,
) -> Result<ControlPlaneMemberAddress, String> {
    let configured = private_address.map(str::trim).filter(|a| !a.is_empty());
    let Some(raw) = configured else {
        return mesh_address
            .map(|ip| ControlPlaneMemberAddress { ip, private: true })
            .ok_or_else(|| "no control-plane private address is configured".to_string());
    };

    let ip = raw
        .parse::<std::net::IpAddr>()
        .or_else(|_| raw.parse::<std::net::SocketAddr>().map(|s| s.ip()))
        .map_err(|e| {
            format!(
                "the configured control-plane private address '{raw}' is not an IP address: {e}"
            )
        })?;
    let unusable = ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || match ip {
            std::net::IpAddr::V4(v4) => v4.is_link_local() || v4.is_broadcast(),
            std::net::IpAddr::V6(v6) => v6.is_unicast_link_local(),
        };
    if unusable {
        return Err(format!(
            "the configured control-plane private address '{raw}' cannot be reached from \
             another host (loopback, link-local, multicast, broadcast or unspecified)"
        ));
    }

    if temps_core::node_address::is_private_node_address(raw) {
        return Ok(ControlPlaneMemberAddress { ip, private: true });
    }
    Ok(match mesh_address {
        Some(mesh) => ControlPlaneMemberAddress {
            ip: mesh,
            private: true,
        },
        None => ControlPlaneMemberAddress { ip, private: false },
    })
}

/// First host port of the cluster port range.
pub(super) const CLUSTER_PORT_RANGE_START: u16 = 6000;

/// Host ports reserved per cluster: the monitor at the block's base, data
/// members at `base + ordinal`.
pub(super) const CLUSTER_PORT_BLOCK_SIZE: u16 = 10;

/// The historical block for `service_id`, `6000 + service_id * 10`, when the
/// whole block fits in the TCP port range (service ids up to 5952). Kept so
/// every existing cluster keeps exactly the ports it has.
pub(super) fn legacy_cluster_port_base(service_id: i32) -> Option<u16> {
    let base = u32::try_from(service_id)
        .ok()?
        .checked_mul(u32::from(CLUSTER_PORT_BLOCK_SIZE))?
        .checked_add(u32::from(CLUSTER_PORT_RANGE_START))?;
    let last = base.checked_add(u32::from(CLUSTER_PORT_BLOCK_SIZE) - 1)?;
    (last <= u32::from(u16::MAX))
        .then_some(base)
        .and_then(|b| u16::try_from(b).ok())
}

/// Choose the port block for a new cluster.
///
/// `6000 + service_id * 10` used to be computed in `u16`, which overflowed
/// once ids reached 5954 (a panic in debug builds, and in release a wrapped
/// port that could land on another cluster's block). The historical block is
/// still used whenever it fits and is free; past that, the first 10-port
/// block that no other cluster member holds. `used_ports` are the host ports
/// of every *other* cluster's members.
pub(super) fn select_cluster_port_base(service_id: i32, used_ports: &BTreeSet<u16>) -> Option<u16> {
    let block_is_free = |base: u16| {
        let last = base.saturating_add(CLUSTER_PORT_BLOCK_SIZE - 1);
        used_ports.range(base..=last).next().is_none()
    };
    if let Some(base) = legacy_cluster_port_base(service_id).filter(|b| block_is_free(*b)) {
        return Some(base);
    }
    let last_base = u16::MAX - (CLUSTER_PORT_BLOCK_SIZE - 1);
    (CLUSTER_PORT_RANGE_START..=last_base)
        .step_by(usize::from(CLUSTER_PORT_BLOCK_SIZE))
        .find(|base| block_is_free(*base))
}

/// Host port of a member: the monitor holds the block's base, data members
/// `base + ordinal`.
pub(super) fn cluster_member_host_port(base: u16, ordinal: i32, is_monitor: bool) -> Option<u16> {
    if is_monitor {
        Some(base)
    } else {
        cluster_member_port(base, ordinal)
    }
}

/// `pg_advisory_xact_lock` key serializing cluster port-block reservation
/// ("TEMPSPRT"), so two concurrent creations cannot pick the same block.
pub(super) const CLUSTER_PORT_LOCK_KEY: i64 = 0x5445_4D50_5350_5254;

/// Host ports held by members of every cluster other than `service_id`.
pub(super) async fn used_cluster_ports<C: sea_orm::ConnectionTrait>(
    conn: &C,
    service_id: i32,
) -> Result<BTreeSet<u16>, ExternalServiceError> {
    Ok(service_members::Entity::find()
        .select_only()
        .column(service_members::Column::Port)
        .filter(service_members::Column::ServiceId.ne(service_id))
        .filter(service_members::Column::Port.is_not_null())
        .into_tuple::<Option<i32>>()
        .all(conn)
        .await?
        .into_iter()
        .flatten()
        .filter_map(|port| u16::try_from(port).ok())
        .collect())
}

/// Reject a member list that cannot fit in one cluster port block.
///
/// Checked before anything is saved: inside the background initialization
/// it surfaced as a cluster that was accepted and then failed, and a retry
/// with the stored member list failed the same way.
pub(super) fn validate_cluster_member_count(
    name: &str,
    requested: usize,
) -> Result<(), ExternalServiceError> {
    if requested > usize::from(CLUSTER_PORT_BLOCK_SIZE) {
        return Err(ExternalServiceError::ClusterMemberLimitExceeded {
            name: name.to_string(),
            requested,
            max: CLUSTER_PORT_BLOCK_SIZE,
        });
    }
    Ok(())
}

/// The layout a cluster gets when the request does not name its members: a
/// monitor, a primary and one replica, all on the control plane.
///
/// Three members is the smallest set pg_auto_failover can fail over with, and
/// the control plane is where every member lands when `node_id` is omitted, so
/// "create a cluster" works on a single host. That protects against a member
/// container dying, not against the host dying: spread members across worker
/// nodes (`node_id`) for that.
pub(super) fn default_cluster_members() -> Vec<ClusterMemberRequest> {
    [
        ClusterRole::Monitor,
        ClusterRole::Primary,
        ClusterRole::Replica,
    ]
    .into_iter()
    .map(|role| ClusterMemberRequest {
        role: role.as_str().to_string(),
        node_id: None,
    })
    .collect()
}

/// Settle the member list of a new cluster before anything is saved.
///
/// An empty list gets [`default_cluster_members`]. An explicit list must hold
/// exactly one monitor and at least one data member, with known roles.
/// Without this check a cluster with no monitor was accepted, went through
/// "creating" and only then failed in the background with a bare "no monitor
/// member" error that never said what a valid request looks like.
pub(super) fn resolve_cluster_members(
    name: &str,
    members: &[ClusterMemberRequest],
) -> Result<Vec<ClusterMemberRequest>, ExternalServiceError> {
    if members.is_empty() {
        return Ok(default_cluster_members());
    }
    let invalid = |reason: String| ExternalServiceError::ParameterValidationFailed {
        service_id: 0,
        reason: format!(
            "Invalid members for cluster '{name}': {reason}. Expected one monitor and at least \
             one primary or replica, e.g. [{{\"role\":\"monitor\"}},{{\"role\":\"primary\"}},\
             {{\"role\":\"replica\"}}]; omit node_id to place a member on the control plane. \
             Omit members entirely to get that layout."
        ),
    };
    let known = [
        ClusterRole::Monitor.as_str(),
        ClusterRole::Primary.as_str(),
        ClusterRole::Replica.as_str(),
    ];
    if let Some(unknown) = members.iter().find(|m| !known.contains(&m.role.as_str())) {
        return Err(invalid(format!(
            "role '{}' is not one of {:?}",
            unknown.role, known
        )));
    }
    let monitors = members
        .iter()
        .filter(|m| m.role == ClusterRole::Monitor.as_str())
        .count();
    if monitors != 1 {
        return Err(invalid(format!(
            "found {monitors} monitor member(s), need exactly 1"
        )));
    }
    if members.len() == monitors {
        return Err(invalid(
            "the cluster has a monitor but no data member".to_string(),
        ));
    }
    Ok(members.to_vec())
}

/// Host port of the member with `ordinal` in the block starting at `base`,
/// or `None` when the ordinal does not fit in the cluster's block.
pub(super) fn cluster_member_port(base: u16, ordinal: i32) -> Option<u16> {
    u16::try_from(ordinal)
        .ok()
        .filter(|o| *o < CLUSTER_PORT_BLOCK_SIZE)
        .and_then(|o| base.checked_add(o))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceExecutionRoute {
    Local,
    Remote(i32),
}

/// `external_services.node_id` is the ownership boundary: NULL means the
/// control plane's Docker daemon, while a concrete ID means that worker's
/// private daemon and network namespace.
pub(crate) fn service_execution_route(node_id: Option<i32>) -> ServiceExecutionRoute {
    match node_id {
        Some(node_id) => ServiceExecutionRoute::Remote(node_id),
        None => ServiceExecutionRoute::Local,
    }
}

/// Which address a cluster member being added via `add_cluster_member`
/// should use to reach the monitor. See
/// `ExternalServiceManager::monitor_reachability_for_add`'s doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MonitorReachability {
    /// Monitor and the new member are both local to this control-plane's
    /// Docker host — container-name-level resolution already works.
    SameHost,
    /// The monitor lives on a remote node — always need its real underlay
    /// address, regardless of where the new member lands.
    MonitorNode(i32),
    /// Monitor is local but the new member is remote — it needs this
    /// control-plane host's private IP, not the monitor's container name.
    LocalControlPlane,
}

/// Build the two matching pieces Docker requires to publish a cluster
/// member's dynamically assigned port.
///
/// Always published on loopback (the control plane's own probes use it).
/// When the cluster also has members on worker nodes, the port is published
/// on `control_plane_address` too: those members dial it to register with a
/// control-plane monitor and to replicate from a control-plane primary, and a
/// loopback-only binding refuses every one of those connections.
pub(super) fn cluster_member_port_config(
    container_port: u16,
    control_plane_address: Option<std::net::IpAddr>,
) -> (
    Vec<String>,
    HashMap<String, Option<Vec<bollard::models::PortBinding>>>,
) {
    let container_port_key = format!("{container_port}/tcp");
    let mut port_bindings =
        crate::utils::local_port_binding(&container_port_key, &container_port.to_string());
    if let Some(address) = control_plane_address {
        if let Some(Some(bindings)) = port_bindings.get_mut(&container_port_key) {
            bindings.push(bollard::models::PortBinding {
                host_ip: Some(address.to_string()),
                host_port: Some(container_port.to_string()),
            });
        }
    }
    (vec![container_port_key], port_bindings)
}

/// Whether a container's port bindings publish `port` on `address`.
pub(super) fn port_bindings_publish_on(
    bindings: Option<&HashMap<String, Option<Vec<bollard::models::PortBinding>>>>,
    port: u16,
    address: std::net::IpAddr,
) -> bool {
    let wanted_port = port.to_string();
    let Some(bindings) = bindings else {
        return false;
    };
    bindings.values().flatten().flatten().any(|binding| {
        binding.host_port.as_deref() == Some(wanted_port.as_str())
            && binding
                .host_ip
                .as_deref()
                .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
                == Some(address)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    use std::collections::BTreeSet;

    #[test]
    fn cluster_member_dynamic_port_is_exposed_and_bound_to_loopback() {
        let (exposed_ports, bindings) = cluster_member_port_config(6040, None);

        assert_eq!(exposed_ports, vec!["6040/tcp"]);
        let binding = bindings
            .get("6040/tcp")
            .and_then(Option::as_ref)
            .and_then(|entries| entries.first())
            .expect("the exposed dynamic port must have a matching host binding");
        assert_eq!(binding.host_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(binding.host_port.as_deref(), Some("6040"));
        assert_eq!(
            bindings
                .get("6040/tcp")
                .and_then(Option::as_ref)
                .map(Vec::len),
            Some(1),
            "a single-host cluster member must stay on loopback only"
        );
    }

    /// Regression: a control-plane monitor in a cluster whose data nodes run
    /// on workers was published on 127.0.0.1 only, so the workers' data
    /// nodes could never connect to it and the monitor reported zero data
    /// nodes forever.
    #[test]
    fn cross_host_cluster_member_is_also_published_on_control_plane_address() {
        let address: std::net::IpAddr = "10.52.0.10".parse().unwrap();
        let (exposed_ports, bindings) = cluster_member_port_config(6090, Some(address));

        assert_eq!(exposed_ports, vec!["6090/tcp"]);
        let entries = bindings
            .get("6090/tcp")
            .and_then(Option::as_ref)
            .expect("the exposed port must have host bindings");
        let hosts: Vec<(Option<&str>, Option<&str>)> = entries
            .iter()
            .map(|b| (b.host_ip.as_deref(), b.host_port.as_deref()))
            .collect();
        assert_eq!(
            hosts,
            vec![
                (Some("127.0.0.1"), Some("6090")),
                (Some("10.52.0.10"), Some("6090")),
            ]
        );
        assert!(
            entries
                .iter()
                .all(|b| b.host_ip.as_deref() != Some("0.0.0.0") && b.host_ip.is_some()),
            "a cluster member must never be published on every interface"
        );
        assert!(port_bindings_publish_on(Some(&bindings), 6090, address));
    }

    /// Docker takes the bare IPv6 address for a binding; only URIs bracket it.
    #[test]
    fn cross_host_binding_keeps_an_ipv6_address_bare() {
        let address: std::net::IpAddr = "fd00::10".parse().unwrap();
        let (_, bindings) = cluster_member_port_config(6090, Some(address));
        let hosts: Vec<Option<&str>> = bindings
            .get("6090/tcp")
            .and_then(Option::as_ref)
            .map(|entries| entries.iter().map(|b| b.host_ip.as_deref()).collect())
            .unwrap_or_default();
        assert_eq!(hosts, vec![Some("127.0.0.1"), Some("fd00::10")]);
        assert!(port_bindings_publish_on(Some(&bindings), 6090, address));
    }

    #[test]
    fn port_bindings_publish_on_requires_matching_address_and_port() {
        let address: std::net::IpAddr = "10.52.0.10".parse().unwrap();
        let (_, loopback_only) = cluster_member_port_config(6090, None);
        let (_, cross_host) = cluster_member_port_config(6090, Some(address));

        assert!(!port_bindings_publish_on(
            Some(&loopback_only),
            6090,
            address
        ));
        assert!(!port_bindings_publish_on(Some(&cross_host), 6091, address));
        assert!(!port_bindings_publish_on(
            Some(&cross_host),
            6090,
            "10.52.0.11".parse().unwrap()
        ));
        assert!(!port_bindings_publish_on(None, 6090, address));
        assert!(port_bindings_publish_on(Some(&cross_host), 6090, address));
    }

    #[test]
    fn control_plane_member_address_uses_configured_private_address() {
        let selected = select_control_plane_member_address(Some(" 10.52.0.10 "), None).unwrap();
        assert_eq!(
            selected.ip,
            "10.52.0.10".parse::<std::net::IpAddr>().unwrap()
        );
        assert!(selected.private);

        // `ip:port` is accepted the same way node registration accepts it.
        let selected = select_control_plane_member_address(Some("10.52.0.10:3100"), None).unwrap();
        assert_eq!(
            selected.ip,
            "10.52.0.10".parse::<std::net::IpAddr>().unwrap()
        );

        // A private address wins over the mesh, exactly like a worker's
        // `data_address`.
        let mesh: std::net::IpAddr = "10.201.0.1".parse().unwrap();
        let selected = select_control_plane_member_address(Some("10.52.0.10"), Some(mesh)).unwrap();
        assert_eq!(
            selected.ip,
            "10.52.0.10".parse::<std::net::IpAddr>().unwrap()
        );
    }

    #[test]
    fn control_plane_member_address_prefers_mesh_over_public_address() {
        let mesh: std::net::IpAddr = "10.201.0.1".parse().unwrap();
        let selected =
            select_control_plane_member_address(Some("203.0.113.10"), Some(mesh)).unwrap();
        assert_eq!(selected.ip, mesh);
        assert!(selected.private);

        let selected = select_control_plane_member_address(None, Some(mesh)).unwrap();
        assert_eq!(selected.ip, mesh);

        // Public and no mesh: honoured (as for a direct-mode worker), but
        // flagged so the caller warns.
        let selected = select_control_plane_member_address(Some("203.0.113.10"), None).unwrap();
        assert_eq!(
            selected.ip,
            "203.0.113.10".parse::<std::net::IpAddr>().unwrap()
        );
        assert!(!selected.private);
    }

    #[test]
    fn control_plane_member_address_never_guesses() {
        let missing = select_control_plane_member_address(None, None).unwrap_err();
        assert!(
            missing.contains("no control-plane private address"),
            "{missing}"
        );
        let blank = select_control_plane_member_address(Some("  "), None).unwrap_err();
        assert!(
            blank.contains("no control-plane private address"),
            "{blank}"
        );

        for unusable in ["127.0.0.1", "0.0.0.0", "169.254.1.2", "::1", "224.0.0.1"] {
            let err = select_control_plane_member_address(Some(unusable), None).unwrap_err();
            assert!(err.contains(unusable), "{unusable}: {err}");
            assert!(err.contains("cannot be reached"), "{unusable}: {err}");
        }

        let garbage = select_control_plane_member_address(Some("cp.internal"), None).unwrap_err();
        assert!(
            garbage.contains("'cp.internal' is not an IP address"),
            "{garbage}"
        );
    }

    /// Existing clusters keep exactly the ports they have today.
    #[test]
    fn cluster_port_base_keeps_the_historical_block_while_it_fits() {
        assert_eq!(legacy_cluster_port_base(0), Some(6000));
        assert_eq!(legacy_cluster_port_base(3), Some(6030));
        assert_eq!(legacy_cluster_port_base(9), Some(6090));
        // 65520..=65529 is the last whole block.
        assert_eq!(legacy_cluster_port_base(5952), Some(65520));
        assert_eq!(legacy_cluster_port_base(5953), None);
        assert_eq!(legacy_cluster_port_base(-1), None);

        let none_used = BTreeSet::new();
        assert_eq!(select_cluster_port_base(9, &none_used), Some(6090));
    }

    /// Regression: `6000u16 + (service_id as u16 * 10)` overflowed from
    /// service id 5954 (a panic in debug builds, a wrapped port that could
    /// collide with another cluster in release).
    #[test]
    fn cluster_port_base_past_the_formula_takes_the_first_free_block() {
        let used: BTreeSet<u16> = [6000, 6001, 6002, 6015].into_iter().collect();
        for service_id in [5953, 5954, 6000, 70_000, i32::MAX] {
            // 6000..=6009 and 6010..=6019 hold members; 6020 is the first
            // whole free block.
            assert_eq!(
                select_cluster_port_base(service_id, &used),
                Some(6020),
                "service {service_id}"
            );
        }
    }

    #[test]
    fn cluster_port_base_avoids_a_historical_block_taken_by_another_cluster() {
        // Service 3's historical block is held by a cluster that was placed
        // there because its own id had no block.
        let used: BTreeSet<u16> = [6030, 6031].into_iter().collect();
        assert_eq!(select_cluster_port_base(3, &used), Some(6000));
    }

    #[test]
    fn cluster_port_base_is_none_when_every_block_is_held() {
        let used: BTreeSet<u16> = (CLUSTER_PORT_RANGE_START..=u16::MAX)
            .step_by(usize::from(CLUSTER_PORT_BLOCK_SIZE))
            .collect();
        assert_eq!(select_cluster_port_base(7000, &used), None);
        assert_eq!(select_cluster_port_base(1, &used), None);
    }

    #[test]
    fn cluster_member_port_stays_inside_the_block() {
        assert_eq!(cluster_member_port(6030, 0), Some(6030));
        assert_eq!(cluster_member_port(6030, 9), Some(6039));
        assert_eq!(
            cluster_member_port(6030, 10),
            None,
            "would spill into the next cluster"
        );
        assert_eq!(cluster_member_port(6030, -1), None);
        assert_eq!(cluster_member_port(65520, 9), Some(65529));
    }

    #[test]
    fn cluster_member_count_is_bounded_by_the_port_block() {
        assert!(validate_cluster_member_count("ha-pg", 3).is_ok());
        assert!(validate_cluster_member_count("ha-pg", 10).is_ok());
        let err = validate_cluster_member_count("ha-pg", 11).unwrap_err();
        assert!(matches!(
            &err,
            ExternalServiceError::ClusterMemberLimitExceeded { name, requested: 11, max: 10 }
                if name == "ha-pg"
        ));
        assert!(err.to_string().contains("at most 10"), "{err}");
    }

    #[test]
    fn cluster_without_members_gets_monitor_primary_and_replica_on_the_control_plane() {
        let members = resolve_cluster_members("ha-pg", &[]).unwrap();
        let roles: Vec<&str> = members.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["monitor", "primary", "replica"]);
        assert!(members.iter().all(|m| m.node_id.is_none()));
    }

    #[test]
    fn explicit_cluster_members_are_kept_as_given() {
        let given = vec![
            member("monitor", None),
            member("primary", Some(2)),
            member("replica", Some(3)),
        ];
        let resolved = resolve_cluster_members("ha-pg", &given).unwrap();
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[1].node_id, Some(2));
        assert_eq!(resolved[2].node_id, Some(3));
    }

    #[test]
    fn cluster_without_a_monitor_is_rejected_with_the_expected_shape() {
        let err =
            resolve_cluster_members("ha-pg", &[member("primary", None), member("replica", None)])
                .unwrap_err();
        assert!(matches!(
            err,
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
        let msg = err.to_string();
        assert!(msg.contains("ha-pg"), "{msg}");
        assert!(msg.contains("found 0 monitor"), "{msg}");
        assert!(msg.contains("\"role\":\"monitor\""), "{msg}");
    }

    #[test]
    fn cluster_with_two_monitors_is_rejected() {
        let err = resolve_cluster_members(
            "ha-pg",
            &[
                member("monitor", None),
                member("monitor", None),
                member("primary", None),
            ],
        )
        .unwrap_err();
        assert!(err.to_string().contains("found 2 monitor"), "{err}");
    }

    #[test]
    fn cluster_with_only_a_monitor_is_rejected() {
        let err = resolve_cluster_members("ha-pg", &[member("monitor", None)]).unwrap_err();
        assert!(err.to_string().contains("no data member"), "{err}");
    }

    #[test]
    fn cluster_with_an_unknown_role_is_rejected() {
        let err =
            resolve_cluster_members("ha-pg", &[member("monitor", None), member("leader", None)])
                .unwrap_err();
        assert!(err.to_string().contains("role 'leader'"), "{err}");
    }

    #[test]
    fn default_cluster_layout_fits_the_port_block() {
        assert!(validate_cluster_member_count("ha-pg", default_cluster_members().len()).is_ok());
    }

    #[test]
    fn local_member_dns_uses_shared_network_ip_and_container_port() {
        let endpoint = select_member_dns_endpoint(
            None,
            None,
            Some("172.19.0.7"),
            Some(("10.52.0.10".to_string(), 6011)),
            5432,
        );

        assert_eq!(endpoint, Some(("172.19.0.7".to_string(), 5432)));
    }

    #[test]
    fn local_member_dns_never_publishes_loopback_only_underlay_fallback() {
        let endpoint = select_member_dns_endpoint(
            None,
            None,
            None,
            Some(("10.52.0.10".to_string(), 6011)),
            5432,
        );

        assert_eq!(endpoint, None);
    }

    #[test]
    fn remote_member_dns_preserves_overlay_then_underlay_behavior() {
        let overlay = select_member_dns_endpoint(
            Some(7),
            Some("10.99.0.12"),
            Some("172.19.0.7"),
            Some(("10.52.0.11".to_string(), 6012)),
            5432,
        );
        assert_eq!(overlay, Some(("10.99.0.12".to_string(), 5432)));

        let underlay = select_member_dns_endpoint(
            Some(7),
            None,
            Some("172.19.0.7"),
            Some(("10.52.0.11".to_string(), 6012)),
            5432,
        );
        assert_eq!(underlay, Some(("10.52.0.11".to_string(), 6012)));
    }

    #[test]
    fn service_execution_without_node_id_stays_local() {
        assert_eq!(service_execution_route(None), ServiceExecutionRoute::Local);
    }

    #[test]
    fn service_execution_with_node_id_targets_owning_worker() {
        assert_eq!(
            service_execution_route(Some(17)),
            ServiceExecutionRoute::Remote(17)
        );
    }
}
