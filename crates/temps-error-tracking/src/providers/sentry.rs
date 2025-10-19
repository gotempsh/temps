//! Sentry Error Provider Implementation

use async_trait::async_trait;
use relay_protocol::{FromValue, IntoValue};
use serde_json::Value;
use std::sync::Arc;

use super::{AuthContext, ErrorProvider, ParsedErrorEvent, ProviderError};
use crate::sentry::{dsn_service::DSNService, envelope::Envelope, envelope::EnvelopeItem, mapper};

/// Sentry error provider implementation
pub struct SentryProvider {
    dsn_service: Arc<DSNService>,
}

impl SentryProvider {
    pub fn new(dsn_service: Arc<DSNService>) -> Self {
        Self { dsn_service }
    }
}

#[async_trait]
impl ErrorProvider for SentryProvider {
    fn name(&self) -> &'static str {
        "sentry"
    }

    async fn authenticate(
        &self,
        project_id: i32,
        credentials: &str,
    ) -> Result<AuthContext, ProviderError> {
        let dsn = self
            .dsn_service
            .validate_dsn(project_id, credentials)
            .await
            .map_err(|e| ProviderError::Authentication(e.to_string()))?;

        Ok(AuthContext {
            project_id: dsn.project_id,
            environment_id: dsn.environment_id,
            deployment_id: dsn.deployment_id,
        })
    }

    async fn parse_events(
        &self,
        payload: &[u8],
        auth: &AuthContext,
    ) -> Result<Vec<ParsedErrorEvent>, ProviderError> {
        // Parse Sentry envelope
        let envelope = Envelope::from_slice(payload)
            .map_err(|e| ProviderError::Parsing(e.to_string()))?;

        let mut parsed_events = Vec::new();

        // Process each item in the envelope
        for item in envelope.items() {
            match item {
                EnvelopeItem::Event(event) | EnvelopeItem::Transaction(event) => {
                    // Extract event ID first
                    let event_id = event
                        .value()
                        .and_then(|e| e.id.value().map(|id| id.to_string()))
                        .ok_or_else(|| ProviderError::Validation("Event missing ID".to_string()))?;

                    // Serialize the complete Sentry event to JSON (preserving all data)
                    // Convert Annotated<Event> -> Event -> relay Value -> serde_json::Value
                    let raw_event = if let Some(ev) = event.value() {
                        // Convert Event to relay Value
                        let relay_val: relay_protocol::Value = ev.clone().into_value();
                        mapper::relay_value_to_json(&relay_val)
                            .unwrap_or_else(|| serde_json::json!({"error": "Failed to serialize event"}))
                    } else {
                        serde_json::json!({"error": "Event has no value"})
                    };

                    // Convert to internal format
                    let error_data = mapper::convert_sentry_event_to_error_data(
                        event,
                        raw_event.clone(),
                        auth.project_id,
                        auth.environment_id,
                        auth.deployment_id,
                    )
                    .map_err(|e| ProviderError::Parsing(e.to_string()))?;

                    parsed_events.push(ParsedErrorEvent {
                        event_id,
                        raw_event,
                        error_data,
                    });
                }
                EnvelopeItem::Session(_) => {
                    tracing::debug!("Sentry session item (not yet implemented)");
                }
                EnvelopeItem::SessionAggregates(_) => {
                    tracing::debug!("Sentry session aggregates item (not yet implemented)");
                }
                EnvelopeItem::ClientReport(_) => {
                    tracing::debug!("Sentry client report item (not yet implemented)");
                }
                EnvelopeItem::Span(_) => {
                    tracing::debug!("Sentry span item (not yet implemented)");
                }
            }
        }

        if parsed_events.is_empty() {
            return Err(ProviderError::Validation(
                "No valid events found in envelope".to_string(),
            ));
        }

        Ok(parsed_events)
    }

    async fn parse_json_event(
        &self,
        event_json: Value,
        auth: &AuthContext,
    ) -> Result<ParsedErrorEvent, ProviderError> {
        // Parse with Relay types
        let event = relay_event_schema::protocol::Event::from_value(event_json.clone().into());

        // Extract event ID
        let event_id = event
            .value()
            .and_then(|e| e.id.value().map(|id| id.to_string()))
            .ok_or_else(|| ProviderError::Validation("Event missing ID".to_string()))?;

        // Convert to internal format
        let error_data = mapper::convert_sentry_event_to_error_data(
            &event,
            event_json.clone(),
            auth.project_id,
            auth.environment_id,
            auth.deployment_id,
        )
        .map_err(|e| ProviderError::Parsing(e.to_string()))?;

        Ok(ParsedErrorEvent {
            event_id,
            raw_event: event_json,
            error_data,
        })
    }
}
