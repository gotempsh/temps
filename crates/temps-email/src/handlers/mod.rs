//! HTTP handlers for the email service

mod audit;
mod domains;
mod emails;
mod providers;
mod types;

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
}

#[derive(OpenApi)]
#[openapi(
    paths(
        // Providers
        providers::create_provider,
        providers::list_providers,
        providers::get_provider,
        providers::delete_provider,
        // Domains
        domains::create_domain,
        domains::list_domains,
        domains::get_domain,
        domains::verify_domain,
        domains::delete_domain,
        // Emails
        emails::send_email,
        emails::list_emails,
        emails::get_email,
        emails::get_email_stats,
    ),
    components(
        schemas(
            // Provider types
            types::CreateEmailProviderRequest,
            types::EmailProviderResponse,
            types::EmailProviderTypeRoute,
            types::SesCredentialsRequest,
            types::ScalewayCredentialsRequest,
            // Domain types
            types::CreateEmailDomainRequest,
            types::EmailDomainResponse,
            types::DnsRecordResponse,
            types::EmailDomainWithDnsResponse,
            // Email types
            types::SendEmailRequestBody,
            types::SendEmailResponseBody,
            types::EmailResponse,
            types::EmailStatsResponse,
            types::PaginatedEmailsResponse,
        )
    ),
    tags(
        (name = "Email Providers", description = "Email provider management endpoints"),
        (name = "Email Domains", description = "Email domain management and verification"),
        (name = "Emails", description = "Email sending and retrieval")
    )
)]
pub struct EmailApiDoc;
