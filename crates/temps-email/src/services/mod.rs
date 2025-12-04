//! Email services

mod domain_service;
mod email_service;
mod provider_service;

pub use domain_service::{CreateDomainRequest, DomainService, DomainWithDnsRecords};
pub use email_service::{
    EmailService, EmailStats, ListEmailsOptions, SendEmailRequest, SendEmailResponse,
};
pub use provider_service::{
    CreateProviderRequest, ProviderCredentials, ProviderService, TestEmailResult,
};
