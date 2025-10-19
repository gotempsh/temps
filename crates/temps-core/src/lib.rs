//! Core utilities and types shared across all Temps crates

pub mod audit;
pub mod config;
pub mod error;
pub mod error_builder;
pub mod jobs;
pub mod notifications;
pub mod openapi;
pub mod plugin;
pub mod problemdetails;
pub use problemdetails::ProblemDetails;
pub mod utils;
pub mod workflow;
pub mod workflow_executor;
pub mod stages;
pub mod repo_config;
pub mod types;
mod constants;
mod encryption;
mod request_metadata;
mod cookie_crypto;
mod app_settings;
// Re-export commonly used types
pub use audit::*;
pub use config::*;
pub use error::*;
pub use error_builder::*;
pub use jobs::*;
pub use utils::*;
pub use constants::*;

// Re-export external dependencies
pub use anyhow;
pub use async_trait;
pub use chrono;
pub use serde;
pub use serde_json;
pub use thiserror;
pub use tokio;
pub use tracing;
pub use uuid;
pub use request_metadata::RequestMetadata;
pub use cookie_crypto::{CookieCrypto, CryptoError};
pub use encryption::EncryptionService;
pub use app_settings::{AppSettings, DnsProviderSettings, LetsEncryptSettings, ScreenshotSettings};
pub use workflow::*;
pub use workflow_executor::*;
pub use stages::*;
pub use repo_config::*;
pub use types::*;

// Re-export standard datetime type for use across all crates
pub use types::UtcDateTime;