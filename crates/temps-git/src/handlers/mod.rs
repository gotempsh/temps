pub mod audit;
pub mod base;
pub mod github;
pub mod gitlab;
pub mod repositories;
pub mod types;
pub mod update_token;

use crate::handlers::types::GitAppState as AppState;
use axum::Router;
use std::sync::Arc;

// Re-export the API documentation
pub use base::GitProvidersApiDoc;

/// Configure all routes for git providers including base, GitHub, and GitLab
pub fn configure_routes() -> Router<Arc<AppState>> {
    // Combine all route modules
    base::configure_routes()
        .merge(github::configure_routes())
        .merge(gitlab::configure_routes())
}
