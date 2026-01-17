//! Core utilities and types shared across all Temps crates

pub mod audit;
pub mod config;
pub mod deployment;
pub mod error;
pub mod error_builder;
pub mod jobs;
pub mod notifications;
pub mod openapi;
pub mod plugin;
pub mod problemdetails;
pub use problemdetails::ProblemDetails;
mod app_settings;
mod constants;
mod cookie_crypto;
mod encryption;
pub mod repo_config;
mod request_metadata;
pub mod stages;
pub mod templates;
pub mod types;
pub mod url_validation;
pub mod utils;
pub mod workflow;
pub mod workflow_executor;
// Re-export commonly used types
pub use audit::*;
pub use config::*;
pub use constants::*;
pub use deployment::*;
pub use error::*;
pub use error_builder::*;
pub use jobs::*;
pub use utils::*;

// Re-export external dependencies
pub use anyhow;
pub use app_settings::{
    AppSettings, DiskSpaceAlertSettings, DnsProviderSettings, DockerRegistrySettings,
    LetsEncryptSettings, RateLimitSettings, ScreenshotSettings, SecurityHeadersSettings,
};
pub use async_trait;
pub use chrono;
pub use cookie_crypto::{CookieCrypto, CryptoError};
pub use encryption::EncryptionService;
pub use repo_config::*;
pub use request_metadata::RequestMetadata;
pub use serde;
pub use serde_json;
pub use stages::*;
pub use templates::*;
pub use thiserror;
pub use tokio;
pub use tracing;
pub use types::*;
pub use uuid;
pub use workflow::*;
pub use workflow_executor::*;

// Re-export standard datetime type for use across all crates
pub use types::UtcDateTime;
