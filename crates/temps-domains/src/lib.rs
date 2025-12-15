//! domains services and utilities

pub mod dns_provider;
pub mod domain_service;
pub mod handlers;
pub mod plugin;
pub mod tls;

// Re-export commonly used types
pub use dns_provider::{
    create_dns_provider_from_settings, CloudflareDnsProvider, DnsProviderService, DummyDnsProvider,
    ManualDnsProvider,
};

pub use tls::{
    Certificate, CertificateFilter, CertificateProvider, CertificateRepository, CertificateStatus,
    ChallengeType, DefaultCertificateRepository, LetsEncryptProvider, TlsError, TlsService,
    TlsServiceBuilder,
};

// Export plugin
pub use plugin::DomainsPlugin;

// Export handlers state for use in other contexts
pub use handlers::{
    configure_routes, create_domain_app_state, create_domain_app_state_with_dns, DomainAppState,
};

// Export domain service
pub use domain_service::{ChallengeData, DomainService, DomainServiceError};

// Keep the old TlsService available temporarily for backward compatibility
// This can be removed once all code is migrated to use the new abstracted version
pub use tls::service::TlsService as NewTlsService;
