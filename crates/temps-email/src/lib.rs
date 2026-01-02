//! Email service for Temps
//!
//! This crate provides email sending capabilities through multiple providers:
//! - AWS SES
//! - Scaleway Transactional Email
//!
//! Features:
//! - Domain management with DNS verification (SPF, DKIM)
//! - Email sending with storage
//! - Provider credential encryption

pub mod dns;
pub mod errors;
pub mod handlers;
pub mod plugin;
pub mod providers;
pub mod services;

// Re-export main types
pub use errors::EmailError;
pub use plugin::EmailPlugin;
pub use providers::{EmailProvider, EmailProviderType};
pub use services::{
    DomainService, EmailService, ProviderService, ValidateEmailRequest, ValidateEmailResponse,
    ValidationService,
};
