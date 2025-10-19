//! Cron Configuration Service Adapter
//!
//! Provides a simple wrapper service that can be used in the deployment workflow
//! This is a stub implementation - the actual CronService should be injected via the plugin system

use async_trait::async_trait;
use std::sync::Arc;

use crate::jobs::configure_crons::{CronConfig, CronConfigError, CronConfigService};

/// Generic adapter that allows any type with a compatible configure_crons method
/// to be used as a CronConfigService in deployment workflows
pub struct CronServiceAdapter<F>
where
    F: Fn(i32, i32, Vec<CronConfig>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CronConfigError>> + Send>> + Send + Sync,
{
    configure_fn: F,
}

impl<F> CronServiceAdapter<F>
where
    F: Fn(i32, i32, Vec<CronConfig>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CronConfigError>> + Send>> + Send + Sync,
{
    pub fn new(configure_fn: F) -> Self {
        Self { configure_fn }
    }
}

#[async_trait]
impl<F> CronConfigService for CronServiceAdapter<F>
where
    F: Fn(i32, i32, Vec<CronConfig>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CronConfigError>> + Send>> + Send + Sync,
{
    async fn configure_crons(
        &self,
        project_id: i32,
        environment_id: i32,
        cron_configs: Vec<CronConfig>,
    ) -> Result<(), CronConfigError> {
        (self.configure_fn)(project_id, environment_id, cron_configs).await
    }
}
