//! DNS services
//!
//! This module contains two unrelated services:
//!
//! - **External-facing** (`provider_service`, `record_service`): manages DNS
//!   records at third-party providers (Cloudflare, Route53, …) for user
//!   domains.
//! - **Internal-facing** (`dns_registry`): authoritative store for the
//!   `*.temps.local` zone served by per-node Hickory resolvers (ADR-011).
//!
//! They share zero code and serve completely different traffic. The crate
//! is the natural home for both because they share the *DNS* concept, but
//! consumers should import from the specific submodule they need.

pub mod deployment_publisher;
pub mod dns_registry;
pub mod provider_service;
pub mod record_service;

pub use deployment_publisher::DeploymentDnsPublisher;
pub use dns_registry::{
    ChangeSet, DnsRegistry, DnsRegistryError, EndpointDraft, OwnerKind, RecordType, ResolverHealth,
    StaleResolver, ZoneSnapshot,
};
pub use provider_service::{
    AddManagedDomainRequest, CreateProviderRequest, DnsProviderService, UpdateProviderRequest,
};
pub use record_service::{DnsOperationResult, DnsRecordService, ManualDnsInstructions};
