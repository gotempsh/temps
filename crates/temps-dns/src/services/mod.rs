//! DNS services
//!
//! This module contains the services for DNS provider and record management.

pub mod provider_service;
pub mod record_service;

pub use provider_service::{
    AddManagedDomainRequest, CreateProviderRequest, DnsProviderService, UpdateProviderRequest,
};
pub use record_service::{DnsOperationResult, DnsRecordService, ManualDnsInstructions};
