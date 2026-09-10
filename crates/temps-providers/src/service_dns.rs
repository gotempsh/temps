// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal-zone naming for managed external services (ADR-011).
//!
//! ## Why this module exists
//!
//! A managed service container publishes its port to `127.0.0.1` on its own
//! host and nowhere else — see `crate::utils::local_port_binding`, whose
//! loopback-only binding is a deliberate security property, not an
//! oversight. A consequence that is easy to miss: **`<node private
//! address>:<host port>` can never reach a managed service from another
//! node.** The port is not bound on that interface.
//!
//! The only address that works across nodes is the container's IP on the
//! multi-host overlay (`temps-network`), and the only stable way to hand
//! that IP to a workload is a name in the internal `*.temps.local` zone
//! served by the per-node Hickory resolvers (`temps-dns-resolver`).
//! Cluster members already work this way; standalone services (a single
//! Postgres/MySQL/MongoDB/Redis container) get the same treatment through
//! the helpers here.
//!
//! Everything in this module is pure so the naming decisions can be tested
//! without Docker, a database, or a cluster.

/// Internal DNS zone served by the per-node resolvers.
pub const INTERNAL_DNS_ZONE: &str = "temps.local";

/// Longest single DNS label permitted by RFC 1035.
const MAX_LABEL_LEN: usize = 63;

/// Turn a service name into a single, RFC 1035-legal DNS label.
///
/// Lowercases, maps every character that is not `a-z0-9` to `-`, collapses
/// runs of `-`, trims leading/trailing `-`, and truncates to 63 characters.
/// Returns `None` when nothing usable survives (e.g. a name made entirely
/// of punctuation), because publishing an empty or `-`-only label would
/// create an unresolvable record rather than fail visibly.
pub fn dns_label(raw: &str) -> Option<String> {
    let mut label = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' && label.ends_with('-') {
            continue;
        }
        label.push(mapped);
    }

    while label.ends_with('-') {
        label.pop();
    }
    let label = label.trim_start_matches('-').to_string();

    if label.len() > MAX_LABEL_LEN {
        let mut truncated = label[..MAX_LABEL_LEN].to_string();
        while truncated.ends_with('-') {
            truncated.pop();
        }
        return (!truncated.is_empty()).then_some(truncated);
    }

    (!label.is_empty()).then_some(label)
}

/// FQDN published for a standalone (non-cluster) managed service.
///
/// Deliberately the same shape as the cluster VIP record built by
/// `externalsvc::postgres_role_reconciler::drafts_for_snapshot`
/// (`<service>.temps.local`): a project linked to `orders-db` reaches it at
/// `orders-db.temps.local` whether that service happens to be a single
/// container or a Postgres cluster. A service is either standalone or a
/// cluster, never both, so the two writers can never contend for the name.
///
/// Prefers the stored `slug` (already normalized at creation time) and
/// falls back to normalizing `name` for rows created before slugs existed.
pub fn standalone_service_fqdn(name: &str, slug: Option<&str>) -> Option<String> {
    let label = slug
        .and_then(dns_label)
        .or_else(|| dns_label(name))
        .filter(|l| !l.is_empty())?;
    Some(format!("{}.{}", label, INTERNAL_DNS_ZONE))
}

/// Everything the deployment planner needs to address one linked external
/// service from a container running on a *different* node.
///
/// Produced by `ExternalServiceManager::get_service_cross_node_link`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCrossNodeLink {
    pub service_id: i32,
    pub service_name: String,
    /// Docker container name, exactly as it appears inside the env-var
    /// values the same-node path injects (`postgres://…@<container>:5432/…`).
    /// This is the needle the cross-node rewrite replaces.
    pub container_name: String,
    /// Node the service container runs on. `None` = control plane.
    pub node_id: Option<i32>,
    /// `<service>.temps.local`, or `None` when the service name yields no
    /// legal DNS label.
    pub fqdn: Option<String>,
    /// Whether an A record for `fqdn` is actually published in the internal
    /// zone right now. `false` means the overlay IP was never learned (the
    /// overlay isn't bootstrapped, or the container predates this code), so
    /// the name would resolve to NXDOMAIN.
    pub dns_record_published: bool,
}

/// Why a linked service cannot be reached from another node.
///
/// Carried into the deploy job so the failure is reported by the replica
/// that would actually have been broken, with the exact remediation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossNodeBlockReason {
    /// `AppSettings.cluster_dns.enabled` is off, so containers never get
    /// the `*.temps.local` resolver in `/etc/resolv.conf`.
    ClusterDnsDisabled,
    /// Cluster DNS is on, but no A record exists for this service — its
    /// overlay IP was never learned.
    DnsRecordMissing,
    /// The service name produces no legal DNS label, so no name can be
    /// published for it at all.
    NoDnsName,
}

impl CrossNodeBlockReason {
    /// Operator-facing explanation. Names the concrete setting/state, never
    /// a generic "not reachable".
    pub fn detail(&self, service_name: &str) -> String {
        match self {
            CrossNodeBlockReason::ClusterDnsDisabled => format!(
                "Cluster DNS is disabled, so '{}' has no address that works from another node. \
                 Managed service ports bind to 127.0.0.1 on their own host by design, which is \
                 unreachable over the private network.",
                service_name
            ),
            CrossNodeBlockReason::DnsRecordMissing => format!(
                "No internal DNS record is published for '{}', so its name does not resolve. \
                 The service container has no overlay network address yet.",
                service_name
            ),
            CrossNodeBlockReason::NoDnsName => format!(
                "Service name '{}' cannot be turned into a DNS name, so no internal record can \
                 be published for it.",
                service_name
            ),
        }
    }

    /// What the operator should actually do next.
    pub fn remedy(&self) -> &'static str {
        match self {
            CrossNodeBlockReason::ClusterDnsDisabled => {
                "Enable cluster DNS under Settings > Worker Nodes, or pin this environment to \
                 the node that runs the service."
            }
            CrossNodeBlockReason::DnsRecordMissing => {
                "Restart the service so it re-attaches to the overlay network and re-publishes \
                 its DNS record, or pin this environment to the node that runs the service."
            }
            CrossNodeBlockReason::NoDnsName => {
                "Rename the service to something containing letters or digits, then redeploy."
            }
        }
    }

    /// Console path that configures the missing piece. Deep-links to the
    /// exact page rather than to documentation.
    pub fn setup_path(&self, service_id: i32) -> String {
        match self {
            // The cluster-DNS switch lives on the Worker Nodes settings page.
            CrossNodeBlockReason::ClusterDnsDisabled => "/settings/nodes".to_string(),
            CrossNodeBlockReason::DnsRecordMissing | CrossNodeBlockReason::NoDnsName => {
                format!("/storage/{}", service_id)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_label_normalizes_case_and_separators() {
        assert_eq!(dns_label("Orders DB").as_deref(), Some("orders-db"));
        assert_eq!(dns_label("orders_db").as_deref(), Some("orders-db"));
        assert_eq!(dns_label("orders___db").as_deref(), Some("orders-db"));
        assert_eq!(dns_label("--orders--").as_deref(), Some("orders"));
    }

    #[test]
    fn dns_label_rejects_names_with_no_usable_characters() {
        assert_eq!(dns_label(""), None);
        assert_eq!(dns_label("   "), None);
        assert_eq!(dns_label("!!!"), None);
    }

    #[test]
    fn dns_label_truncates_to_rfc1035_limit_without_trailing_hyphen() {
        let long = format!("{}-x", "a".repeat(62));
        let label = dns_label(&long).expect("label");
        assert_eq!(label.len(), MAX_LABEL_LEN - 1);
        assert!(!label.ends_with('-'), "got {label}");
    }

    #[test]
    fn standalone_fqdn_prefers_slug_over_name() {
        assert_eq!(
            standalone_service_fqdn("Orders DB", Some("orders-db")).as_deref(),
            Some("orders-db.temps.local")
        );
    }

    #[test]
    fn standalone_fqdn_falls_back_to_name_when_slug_is_missing_or_unusable() {
        assert_eq!(
            standalone_service_fqdn("Orders DB", None).as_deref(),
            Some("orders-db.temps.local")
        );
        assert_eq!(
            standalone_service_fqdn("Orders DB", Some("!!!")).as_deref(),
            Some("orders-db.temps.local")
        );
    }

    #[test]
    fn standalone_fqdn_is_none_when_nothing_can_be_labelled() {
        assert_eq!(standalone_service_fqdn("!!!", None), None);
    }

    #[test]
    fn block_reasons_name_the_service_and_point_somewhere_actionable() {
        for reason in [
            CrossNodeBlockReason::ClusterDnsDisabled,
            CrossNodeBlockReason::DnsRecordMissing,
            CrossNodeBlockReason::NoDnsName,
        ] {
            let detail = reason.detail("orders-db");
            assert!(detail.contains("orders-db"), "got {detail}");
            assert!(!reason.remedy().is_empty());
            let path = reason.setup_path(42);
            assert!(path.starts_with('/'), "got {path}");
        }
    }

    #[test]
    fn setup_path_deep_links_to_the_page_that_fixes_it() {
        assert_eq!(
            CrossNodeBlockReason::ClusterDnsDisabled.setup_path(42),
            "/settings/nodes"
        );
        assert_eq!(
            CrossNodeBlockReason::DnsRecordMissing.setup_path(42),
            "/storage/42"
        );
    }
}
