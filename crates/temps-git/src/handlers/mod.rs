pub mod base;
pub mod github;
pub mod gitlab;
pub mod update_token;
pub mod types;
pub mod audit;
pub mod repositories;

use axum::Router;
use std::sync::Arc;
use crate::handlers::types::GitAppState as AppState;

// Re-export the API documentation
pub use base::GitProvidersApiDoc;

/// Configure all routes for git providers including base, GitHub, and GitLab
pub fn configure_routes() -> Router<Arc<AppState>> {
    // Combine all route modules
    base::configure_routes()
        .merge(github::configure_routes())
        .merge(gitlab::configure_routes())
}
