//! Sentry Ingestion Service
//!
//! Handles Sentry SDK event ingestion including:
//! - DSN authentication
//! - Envelope parsing
//! - Event mapping
//! - Error storage

use std::sync::Arc;
use thiserror::Error;

use super::{envelope::Envelope, mapper, dsn_service::DSNService};
use crate::services::error_tracking_service::ErrorTrackingService;

#[derive(Error, Debug)]
pub enum SentryIngestionError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Not found")]
    NotFound,
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Error tracking error: {0}")]
    ErrorTracking(String),
    #[error("Unauthorized: Invalid DSN")]
    Unauthorized,
    #[error("Invalid envelope: {0}")]
    InvalidEnvelope(String),
    #[error("Mapping error: {0}")]
    Mapping(String),
}

impl From<super::mapper::SentryMappingError> for SentryIngestionError {
    fn from(err: super::mapper::SentryMappingError) -> Self {
        SentryIngestionError::Mapping(err.to_string())
    }
}

impl From<super::envelope::EnvelopeError> for SentryIngestionError {
    fn from(err: super::envelope::EnvelopeError) -> Self {
        SentryIngestionError::InvalidEnvelope(err.to_string())
    }
}

impl From<super::types::SentryIngesterError> for SentryIngestionError {
    fn from(err: super::types::SentryIngesterError) -> Self {
        match err {
            super::types::SentryIngesterError::Database(e) => SentryIngestionError::Database(e),
            super::types::SentryIngesterError::ProjectNotFound => SentryIngestionError::NotFound,
            super::types::SentryIngesterError::InvalidDSN => SentryIngestionError::Unauthorized,
            super::types::SentryIngesterError::Validation(msg) => SentryIngestionError::Validation(msg),
        }
    }
}

#[derive(Clone)]
pub struct SentryIngestionService {
    error_tracking_service: Arc<ErrorTrackingService>,
    dsn_service: Arc<DSNService>,
}

impl SentryIngestionService {
    pub fn new(
        error_tracking_service: Arc<ErrorTrackingService>,
        dsn_service: Arc<DSNService>,
    ) -> Self {
        Self {
            error_tracking_service,
            dsn_service,
        }
    }

    /// Process Sentry envelope with DSN authentication
    pub async fn process_envelope(
        &self,
        project_id: i32,
        public_key: &str,
        envelope_data: &[u8],
    ) -> Result<Vec<String>, SentryIngestionError> {
        // 1. Validate DSN and get environment/deployment context
        let dsn_record = self.dsn_service
            .validate_dsn(project_id, public_key)
            .await?;

        let environment_id = dsn_record.environment_id;
        let deployment_id = dsn_record.deployment_id;

        // 2. Parse envelope
        let envelope = Envelope::from_slice(envelope_data)?;

        // 3. Process each item in the envelope
        let mut event_ids = Vec::new();

        for item in envelope.items() {
            match item {
                super::envelope::EnvelopeItem::Event(event)
                | super::envelope::EnvelopeItem::Transaction(event) => {
                    // Create raw event representation
                    let raw_event = serde_json::json!({
                        "event_id": event.value().and_then(|e| e.id.value().map(|id| id.to_string())),
                        "platform": event.value().and_then(|e| e.platform.as_str().map(|s| s.to_string())),
                    });

                    // Convert Sentry event to our format
                    let error_data = mapper::convert_sentry_event_to_error_data(
                        event,
                        raw_event,
                        project_id,
                        environment_id,
                        deployment_id,
                    )?;

                    // Store in database
                    let _group_id = self.error_tracking_service
                        .process_error_event(error_data)
                        .await
                        .map_err(|e| SentryIngestionError::ErrorTracking(e.to_string()))?;

                    // Extract event ID for response
                    if let Some(ev) = event.value() {
                        if let Some(id) = ev.id.value() {
                            event_ids.push(id.to_string());
                        }
                    }
                }
                super::envelope::EnvelopeItem::Session(_) => {
                    tracing::debug!("Received session item (not yet implemented)");
                }
                super::envelope::EnvelopeItem::SessionAggregates(_) => {
                    tracing::debug!("Received session aggregates item (not yet implemented)");
                }
                super::envelope::EnvelopeItem::ClientReport(_) => {
                    tracing::debug!("Received client report item (not yet implemented)");
                }
                super::envelope::EnvelopeItem::Span(_) => {
                    tracing::debug!("Received span item (not yet implemented)");
                }
            }
        }

        Ok(event_ids)
    }

    /// Process single JSON event (for /store/ endpoint)
    pub async fn process_json_event(
        &self,
        project_id: i32,
        public_key: &str,
        event_json: serde_json::Value,
    ) -> Result<String, SentryIngestionError> {
        // 1. Validate DSN
        let dsn_record = self.dsn_service
            .validate_dsn(project_id, public_key)
            .await?;

        let environment_id = dsn_record.environment_id;
        let deployment_id = dsn_record.deployment_id;

        // 2. Parse event with Relay types
        use relay_protocol::FromValue;
        let event = relay_event_schema::protocol::Event::from_value(event_json.clone().into());

        // 3. Extract event ID
        let event_id = event
            .value()
            .and_then(|e| e.id.value().map(|id| id.to_string()))
            .ok_or_else(|| SentryIngestionError::Validation("Event missing ID".to_string()))?;

        // 4. Convert to our format
        let error_data = mapper::convert_sentry_event_to_error_data(
            &event,
            event_json,
            project_id,
            environment_id,
            deployment_id,
        )?;

        // 5. Store in database
        self.error_tracking_service
            .process_error_event(error_data)
            .await
            .map_err(|e| SentryIngestionError::ErrorTracking(e.to_string()))?;

        Ok(event_id)
    }
}
