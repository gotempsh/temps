//! Queue Plugin implementation for the Temps plugin system
//!
//! This plugin provides job queue functionality including:
//! - BroadcastQueueService for event distribution
//! - Background job processing
//! - Queue management

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};

use crate::BroadcastQueueService;

// Global storage to keep the receiver alive and prevent channel closure
static KEEP_ALIVE_RECEIVER: Mutex<Option<tokio::sync::broadcast::Receiver<temps_core::Job>>> = Mutex::new(None);

/// Queue Plugin for managing job queues and background processing
pub struct QueuePlugin {
    queue_capacity: usize,
}

impl QueuePlugin {
    pub fn new(queue_capacity: usize) -> Self {
        Self { queue_capacity }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(1000)
    }
}

impl TempsPlugin for QueuePlugin {
    fn name(&self) -> &'static str {
        "queue"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("🔧 QueuePlugin: Starting service registration with capacity: {}", self.queue_capacity);

            // Create BroadcastQueueService with receiver to keep channel alive
            let (queue_service, keep_alive_receiver) =
                BroadcastQueueService::create_job_queue_arc_with_receiver(self.queue_capacity);

            tracing::debug!("📦 QueuePlugin: Created BroadcastQueueService, storing receiver to keep channel alive");

            // Store the receiver globally to prevent it from being dropped
            {
                let mut receiver_guard = KEEP_ALIVE_RECEIVER.lock().unwrap();
                *receiver_guard = Some(keep_alive_receiver);
            }
            tracing::debug!("🔒 QueuePlugin: Keep-alive receiver stored safely");

            tracing::debug!("📦 QueuePlugin: Registering JobQueue service");
            context.register_service(queue_service);
            tracing::debug!("✅ QueuePlugin: JobQueue service registered successfully");

            tracing::debug!("🎉 Queue plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, _context: &PluginContext) -> Option<PluginRoutes> {
        // Queue plugin doesn't expose HTTP routes
        None
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        // Queue plugin doesn't have public API endpoints
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_queue_plugin_name() {
        let queue_plugin = QueuePlugin::with_default_capacity();
        assert_eq!(queue_plugin.name(), "queue");
    }

    #[tokio::test]
    async fn test_queue_plugin_custom_capacity() {
        let queue_plugin = QueuePlugin::new(500);
        assert_eq!(queue_plugin.queue_capacity, 500);
    }
}