//! DNS provider implementations
//!
//! This module contains the DNS provider trait definitions and implementations
//! for various DNS providers including Cloudflare, Namecheap, Route53, etc.

pub mod cloudflare;
pub mod credentials;
pub mod namecheap;
pub mod traits;

// Re-export commonly used types
pub use cloudflare::CloudflareProvider;
pub use credentials::{
    CloudflareCredentials, DigitalOceanCredentials, NamecheapCredentials, ProviderCredentials,
    Route53Credentials,
};
pub use namecheap::NamecheapProvider;
pub use traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone, ManualDnsProvider,
};
