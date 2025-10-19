use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Redirect, Router,
};
use std::collections::HashMap;
use std::sync::Arc;
use temps_auth::{permission_check, Permission, RequireAuth};
use temps_core::problemdetails::{new as problem_new, Problem};
use tracing::info;

use super::types::GitAppState as AppState;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // GitLab OAuth callback endpoint
        .route(
            "/webhook/git/gitlab/auth",
            axum::routing::get(gitlab_oauth_callback),
        )
}

/// Handle GitLab OAuth callback
async fn gitlab_oauth_callback(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, Problem> {
    permission_check!(auth, Permission::GitConnectionsCreate);

    // Extract OAuth parameters
    let code = params
        .get("code")
        .ok_or_else(|| {
            problem_new(StatusCode::BAD_REQUEST)
                .with_title("Missing Authorization Code")
                .with_detail("The 'code' parameter is required for GitLab OAuth callback")
        })?
        .clone();

    let state_param = params.get("state").cloned();

    info!(
        "GitLab OAuth callback received - code: {}, state: {:?}",
        code, state_param
    );

    // Get the GitLab provider ID from the state or from a default configuration
    // For now, we'll need to determine which GitLab provider this is for
    // In a real implementation, you might encode the provider_id in the state parameter

    // Find the GitLab provider
    // This is a simplified approach - in production, you'd want to encode the provider_id in the state
    let providers = state
        .git_provider_manager
        .list_providers()
        .await
        .map_err(|e| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to list providers")
                .with_detail(&format!("Error: {}", e))
        })?;

    let gitlab_provider = providers
        .into_iter()
        .find(|p| p.provider_type == "gitlab")
        .ok_or_else(|| {
            problem_new(StatusCode::NOT_FOUND)
                .with_title("GitLab Provider Not Found")
                .with_detail("No GitLab provider configured in the system")
        })?;

    // For now, we'll use a placeholder user_id (1) - this should be replaced with proper user identification
    let user_id = auth.user.id;

    // Handle the OAuth callback
    let connection = state
        .git_provider_manager
        .handle_oauth_callback(
            gitlab_provider.id,
            code,
            state_param.unwrap_or_default(),
            user_id,
            None, // host_override - not needed as we use external_url from config
        )
        .await
        .map_err(|e| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("OAuth Callback Failed")
                .with_detail(&format!("Failed to handle GitLab OAuth callback: {}", e))
        })?;

    info!(
        "Successfully created GitLab connection for user {} with account {}",
        user_id, connection.account_name
    );

    // Get external URL from config for redirect
    let external_url = state
        .config_service
        .get_setting("external_url")
        .await
        .unwrap_or(None)
        .unwrap_or_else(|| "http://localhost:3000".to_string());

    // Redirect to the git provider page with success status
    let redirect_url = format!(
        "{}/git-providers/{}?status=connected",
        external_url, gitlab_provider.id
    );

    Ok(Redirect::to(&redirect_url))
}
