// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Queue Plugin implementation for the Temps plugin system
//!
//! This plugin provides job queue functionality including:
//! - BroadcastQueueService for event distribution
//! - Background job processing
//! - Queue management

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::JobQueue;

/// Queue Plugin for managing job queues and background processing.
///
/// Accepts a pre-created `Arc<dyn JobQueue>` so the same queue instance can be
/// shared with components that start before the plugin system (e.g., route table
/// listeners).
pub struct QueuePlugin {
    queue: Arc<dyn JobQueue>,
}

impl QueuePlugin {
    /// Create a queue plugin that registers the given queue into the service context.
    pub fn new(queue: Arc<dyn JobQueue>) -> Self {
        Self { queue }
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
            tracing::debug!("QueuePlugin: Registering pre-created JobQueue service");
            context.register_service(self.queue.clone());
            tracing::debug!("QueuePlugin: JobQueue service registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, _context: &PluginContext) -> Option<PluginRoutes> {
        None
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_queue_plugin_name() {
        let (queue, _receiver) =
            crate::BroadcastQueueService::create_job_queue_arc_with_receiver(100);
        let queue_plugin = QueuePlugin::new(queue);
        assert_eq!(queue_plugin.name(), "queue");
    }
}
