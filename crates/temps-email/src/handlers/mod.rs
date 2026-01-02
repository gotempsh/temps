//! HTTP handlers for the email service

mod audit;
mod domains;
mod emails;
mod providers;
mod types;
mod validation;

pub use types::AppState;

use axum::Router;
use std::sync::Arc;
use utoipa::OpenApi;

/// Configure email routes
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(providers::routes())
        .merge(domains::routes())
        .merge(emails::routes())
        .merge(validation::routes())
}

#[derive(OpenApi)]
#[openapi(
    paths(
        // Providers
        providers::create_provider,
        providers::list_providers,
        providers::get_provider,
        providers::delete_provider,
        providers::test_provider,
        // Domains
        domains::create_domain,
        domains::list_domains,
        domains::get_domain,
        domains::get_domain_by_name,
        domains::get_domain_dns_records,
        domains::verify_domain,
        domains::delete_domain,
        domains::setup_dns,
        // Emails
        emails::send_email,
        emails::list_emails,
        emails::get_email,
        emails::get_email_stats,
        // Validation
        validation::validate_email,
    ),
    components(
        schemas(
            // Provider types
            types::CreateEmailProviderRequest,
            types::EmailProviderResponse,
            types::EmailProviderTypeRoute,
            types::SesCredentialsRequest,
            types::ScalewayCredentialsRequest,
            types::TestEmailResponse,
            // Domain types
            types::CreateEmailDomainRequest,
            types::EmailDomainResponse,
            types::DnsRecordResponse,
            types::EmailDomainWithDnsResponse,
            types::SetupDnsRequest,
            types::SetupDnsResponse,
            types::DnsRecordSetupResult,
            // Email types
            types::SendEmailRequestBody,
            types::SendEmailResponseBody,
            types::EmailResponse,
            types::EmailStatsResponse,
            types::PaginatedEmailsResponse,
            // Validation types
            validation::ValidateEmailRequest,
            validation::ValidateEmailResponse,
            validation::ProxyRequest,
            validation::ReachabilityStatus,
            validation::SyntaxResult,
            validation::MxResult,
            validation::MiscResult,
            validation::SmtpResult,
        )
    ),
    tags(
        (name = "Email Providers", description = "Email provider management endpoints"),
        (name = "Email Domains", description = "Email domain management and verification"),
        (name = "Emails", description = "Email sending and retrieval"),
        (name = "Email Validation", description = "Email address validation and verification")
    )
)]
pub struct EmailApiDoc;
