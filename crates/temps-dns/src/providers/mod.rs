//! DNS provider implementations
//!
//! This module contains the DNS provider trait definitions and implementations
//! for various DNS providers including Cloudflare, Namecheap, Route53, etc.

pub mod azure;
pub mod cloudflare;
pub mod credentials;
pub mod digitalocean;
pub mod gcp;
pub mod namecheap;
pub mod route53;
pub mod traits;

// Re-export commonly used types
pub use azure::AzureProvider;
pub use cloudflare::CloudflareProvider;
pub use credentials::{
    AzureCredentials, CloudflareCredentials, DigitalOceanCredentials, GcpCredentials,
    NamecheapCredentials, ProviderCredentials, Route53Credentials,
};
pub use digitalocean::DigitalOceanProvider;
pub use gcp::GcpProvider;
pub use namecheap::NamecheapProvider;
pub use route53::Route53Provider;
pub use traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone, ManualDnsProvider,
};
