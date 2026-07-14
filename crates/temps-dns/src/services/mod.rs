//! DNS services
//!
//! This module contains two unrelated services:
//!
//! - **External-facing** (`provider_service`, `record_service`,
//!   `managed_records`): manages DNS records at third-party providers
//!   (Cloudflare, Route53, …) for user domains. `managed_records` is the
//!   ownership-guarded path for public A/AAAA/CNAME records (ADR-031);
//!   `record_service` is the raw upsert path kept for ACME challenge TXT
//!   records temps unambiguously owns.
//! - **Internal-facing** (`dns_registry`): authoritative store for the
//!   `*.temps.local` zone served by per-node Hickory resolvers (ADR-011).
//!
//! They share zero code and serve completely different traffic. The crate
//! is the natural home for both because they share the *DNS* concept, but
//! consumers should import from the specific submodule they need.

pub mod deployment_publisher;
pub mod dns_registry;
pub mod managed_records;
pub mod provider_service;
pub mod record_service;

pub use deployment_publisher::DeploymentDnsPublisher;
pub use dns_registry::{
    ChangeSet, DnsRegistry, DnsRegistryError, EndpointDraft, OwnerKind, RecordType, ResolverHealth,
    StaleResolver, ZoneSnapshot,
};
pub use managed_records::{ManagedDnsRecordService, OwnershipScope, RecordOwnership};
pub use provider_service::{
    AddManagedDomainRequest, CreateProviderRequest, DnsProviderService, UpdateProviderRequest,
};
pub use record_service::{DnsOperationResult, DnsRecordService, ManualDnsInstructions};
