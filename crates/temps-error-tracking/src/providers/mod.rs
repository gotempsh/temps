//! Error Provider Trait System
//!
//! Defines a common interface for error tracking providers (Sentry, Bugsnag, Rollbar, etc.)
//!
//! ## Architecture
//!
//! ```text
//! HTTP Request
//!     ↓
//! Provider Handler
//!     ↓
//! ErrorProvider::parse_and_ingest()
//!     ↓
//! Error Ingestion Service
//!     ↓
//! Database
//! ```

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

use crate::services::types::CreateErrorEventData;

pub mod sentry;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Authentication failed: {0}")]
    Authentication(String),

    #[error("Invalid payload: {0}")]
    InvalidPayload(String),

    #[error("Parsing error: {0}")]
    Parsing(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Not found: {0}")]
    NotFound(String),
}

/// Authentication context returned after validating credentials
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

/// Parsed error event from provider
#[derive(Debug, Clone)]
pub struct ParsedErrorEvent {
    /// Unique event ID from the provider
    pub event_id: String,

    /// Raw event data from the provider (stored in JSONB)
    pub raw_event: Value,

    /// Mapped error data in our internal format
    pub error_data: CreateErrorEventData,
}

/// Error Provider Trait
///
/// All error tracking providers (Sentry, Bugsnag, Rollbar, etc.) must implement this trait.
#[async_trait]
pub trait ErrorProvider: Send + Sync {
    /// Provider name (e.g., "sentry", "bugsnag", "rollbar")
    fn name(&self) -> &'static str;

    /// Authenticate the request using provider-specific credentials
    ///
    /// # Arguments
    /// * `project_id` - The project ID
    /// * `credentials` - Provider-specific credentials (DSN public key, API key, etc.)
    ///
    /// # Returns
    /// Authentication context including project, environment, and deployment IDs
    async fn authenticate(
        &self,
        project_id: i32,
        credentials: &str,
    ) -> Result<AuthContext, ProviderError>;

    /// Parse raw payload data into error events
    ///
    /// Providers may send single events or batches (envelopes).
    /// This method handles provider-specific parsing.
    ///
    /// # Arguments
    /// * `payload` - Raw bytes from HTTP request
    /// * `auth` - Authentication context from authenticate()
    ///
    /// # Returns
    /// Vector of parsed error events ready for storage
    async fn parse_events(
        &self,
        payload: &[u8],
        auth: &AuthContext,
    ) -> Result<Vec<ParsedErrorEvent>, ProviderError>;

    /// Parse a single JSON event
    ///
    /// For providers that support simple JSON POST (like /store/ endpoint).
    ///
    /// # Arguments
    /// * `event_json` - Parsed JSON event
    /// * `auth` - Authentication context
    ///
    /// # Returns
    /// Single parsed error event
    async fn parse_json_event(
        &self,
        event_json: Value,
        auth: &AuthContext,
    ) -> Result<ParsedErrorEvent, ProviderError>;
}

/// Provider registry for managing multiple error providers
pub struct ProviderRegistry {
    providers: std::collections::HashMap<&'static str, Arc<dyn ErrorProvider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: std::collections::HashMap::new(),
        }
    }

    /// Register a new error provider
    pub fn register(&mut self, provider: Arc<dyn ErrorProvider>) {
        let name = provider.name();
        self.providers.insert(name, provider);
    }

    /// Get a provider by name
    pub fn get(&self, name: &str) -> Option<Arc<dyn ErrorProvider>> {
        self.providers.get(name).cloned()
    }

    /// List all registered provider names
    pub fn list_providers(&self) -> Vec<&'static str> {
        self.providers.keys().copied().collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}
