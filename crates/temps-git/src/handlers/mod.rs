pub mod audit;
pub mod base;
pub mod bitbucket;
pub mod generic;
pub mod gitea;
pub mod github;
pub mod gitlab;
pub mod public;
pub mod repositories;
pub mod types;
pub mod update_token;

use crate::handlers::types::GitAppState as AppState;
use axum::Router;
use std::sync::Arc;

// Re-export the API documentation
pub use base::GitProvidersApiDoc;
pub use public::PublicRepositoriesApiDoc;

/// Configure all routes for git providers including base, GitHub, GitLab, Gitea, Bitbucket,
/// Generic/Manual, and public repos.
pub fn configure_routes() -> Router<Arc<AppState>> {
    // Combine all route modules
    base::configure_routes()
        .merge(github::configure_routes())
        .merge(gitlab::configure_routes())
        .merge(gitea::configure_routes())
        .merge(bitbucket::configure_routes())
        .merge(generic::configure_routes())
        .merge(public::configure_routes())
}
