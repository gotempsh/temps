//! DNS service for Temps
//!
//! This crate provides DNS provider management capabilities for automatic DNS record configuration.
//!
//! # Features
//!
//! - **Multiple DNS Providers**: Support for Cloudflare, Namecheap, and more
//! - **Automatic DNS Management**: Automatically configure DNS records for domains
//! - **Secure Credential Storage**: Encrypted API credentials using AES-256-GCM
//! - **Manual Fallback**: When automatic management isn't available, provides instructions
//!
//! # Supported Providers
//!
//! - **Cloudflare**: Full support for zone management and all record types
//! - **Namecheap**: Support via XML API (requires whitelisted IP)
//! - **Route53**: (Planned) AWS Route 53 support
//! - **DigitalOcean**: (Planned) DigitalOcean DNS support
//!
//! # Usage
//!
//! The main entry point for other crates is the `DnsRecordService` which provides
//! a simple interface for setting DNS records:
//!
//! ```ignore
//! use temps_dns::DnsRecordService;
//!
//! // Check if automatic management is available
//! let can_manage = dns_service.can_auto_manage("example.com").await?;
//!
//! // Set an A record (automatically if possible, manual instructions otherwise)
//! let result = dns_service.set_a_record("example.com", "192.0.2.1", Some(300)).await?;
//!
//! if result.automatic {
//!     println!("DNS record set automatically");
//! } else {
//!     println!("Manual setup required: {:?}", result.manual_instructions);
//! }
//! ```

pub mod errors;
pub mod handlers;
pub mod plugin;
pub mod providers;
pub mod services;

// Re-export main types
pub use errors::DnsError;
pub use plugin::DnsPlugin;
pub use providers::{
    CloudflareCredentials, CloudflareProvider, DnsProvider, DnsProviderCapabilities,
    DnsProviderType, DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType, DnsZone,
    ManualDnsProvider, NamecheapCredentials, NamecheapProvider, ProviderCredentials,
};
pub use services::{
    DnsOperationResult, DnsProviderService, DnsRecordService, ManualDnsInstructions,
};
