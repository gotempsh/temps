// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

/// An operator suppressed or restored a single Traefik-discovered route.
///
/// Worth auditing rather than only logging: flipping this flag changes which
/// hostname the proxy serves for a container nobody deployed through Temps,
/// and it is the one write operation on the discovery surface.
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteToggledAudit {
    pub context: AuditContext,
    pub host: String,
    pub container_name: String,
    pub network: String,
    /// The value after the change.
    pub enabled: bool,
}

/// Operator authorized Temps to issue an ACME certificate for a discovered
/// Traefik route (Path A of ADR-041).
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteCertRequestedAudit {
    pub context: AuditContext,
    /// The hostname being authorized.
    pub host: String,
    /// Container that was serving the host at authorization time.
    pub container_id: String,
    pub container_name: String,
    /// "http-01" or "dns-01".
    pub renewal_method: String,
    /// DNS zone supplied for DNS-01 challenges, absent for HTTP-01.
    pub dns01_zone: Option<String>,
}

/// Operator imported an existing certificate from a Traefik `acme.json` file
/// for a discovered route (Path B of ADR-041).
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteCertImportedAudit {
    pub context: AuditContext,
    /// Hosts successfully imported in this call.
    pub imported_hosts: Vec<String>,
    /// Hosts that were present in the acme.json but failed validation.
    pub failed_hosts: Vec<String>,
    /// Total number of entries parsed from the document.
    pub entries_parsed: usize,
}

/// Operator removed TLS authorization from a discovered Traefik route.
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteCertDeauthorizedAudit {
    pub context: AuditContext,
    pub host: String,
}

macro_rules! impl_audit_operation {
    ($type:ty, $op:expr) => {
        impl AuditOperation for $type {
            fn operation_type(&self) -> String {
                $op.to_string()
            }

            fn user_id(&self) -> Option<i32> {
                Some(self.context.user_id)
            }

            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }

            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }

            fn serialize(&self) -> temps_core::AuditResult<String> {
                temps_core::serialize_audit_operation(self)
            }
        }
    };
}

impl_audit_operation!(
    TraefikDiscoveredRouteToggledAudit,
    "TRAEFIK_DISCOVERED_ROUTE_TOGGLED"
);
impl_audit_operation!(
    TraefikDiscoveredRouteCertRequestedAudit,
    "TRAEFIK_DISCOVERED_ROUTE_CERT_REQUESTED"
);
impl_audit_operation!(
    TraefikDiscoveredRouteCertImportedAudit,
    "TRAEFIK_DISCOVERED_ROUTE_CERT_IMPORTED"
);
impl_audit_operation!(
    TraefikDiscoveredRouteCertDeauthorizedAudit,
    "TRAEFIK_DISCOVERED_ROUTE_CERT_DEAUTHORIZED"
);
