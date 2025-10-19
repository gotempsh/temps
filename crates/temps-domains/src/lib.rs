//! domains services and utilities

pub mod dns_provider;
pub mod tls;
pub mod handlers;
pub mod plugin;
pub mod domain_service;

// Re-export commonly used types
pub use dns_provider::{
    CloudflareDnsProvider, DnsProviderService, DummyDnsProvider, ManualDnsProvider,
    create_dns_provider_from_settings,
};

pub use tls::{
    Certificate, CertificateFilter, CertificateProvider, CertificateRepository,
    CertificateStatus, ChallengeType, DefaultCertificateRepository, LetsEncryptProvider,
    TlsError, TlsService, TlsServiceBuilder,
};

// Export plugin
pub use plugin::DomainsPlugin;

// Export handlers state for use in other contexts
pub use handlers::{configure_routes,create_domain_app_state, DomainAppState};

// Export domain service
pub use domain_service::{DomainService, DomainServiceError, ChallengeData};

// Keep the old TlsService available temporarily for backward compatibility
// This can be removed once all code is migrated to use the new abstracted version
pub use tls::service::TlsService as NewTlsService;