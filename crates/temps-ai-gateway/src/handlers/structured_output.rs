use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
    Json, Router,
};
use futures_util::{Stream, StreamExt};
use sea_orm::EntityTrait;
use serde::Deserialize;
use serde_json::Value;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::{OpenApi, ToSchema};

use super::types::AiGatewayAppState;
use crate::services::{StructuredOutputError, StructuredOutputEvent, StructuredOutputRequest};

#[derive(Debug, Deserialize, ToSchema)]
pub struct StreamStructuredOutputRequest {
    /// Stable product use-case identifier, for example `api_traffic.summary`.
    pub purpose: String,
    /// The complete, already-authorized context and instructions for the model.
    pub prompt: String,
    /// JSON Schema describing the required response object.
    pub schema: Value,
    /// Optional configured gateway key. Omit to use the active gateway key.
    /// Host subscription CLIs are intentionally not accepted here.
    pub provider_key_id: Option<i32>,
    /// Optional enabled model id for the selected key.
    pub model: Option<String>,
    /// Optional normalized reasoning depth advertised for the selected model.
    pub thinking_level: Option<String>,
    /// Optional screen-owned cache identity. Prompt and schema hashes are also
    /// included automatically, preventing stale output when input data changes.
    pub cache_key: Option<String>,
    /// Backend cache lifetime in seconds (default 60, clamped to 1-300).
    pub cache_ttl_seconds: Option<u64>,
    /// Bypass any matching backend cache entry and replace it with this result.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(OpenApi)]
#[openapi(
    paths(stream_structured_output),
    components(schemas(StreamStructuredOutputRequest)),
    info(
        title = "Structured AI API",
        description = "Provider-neutral, cached streaming JSON generated from a client prompt and schema.",
        version = "1.0.0"
    )
)]
pub struct StructuredOutputApiDoc;

pub fn configure_structured_output_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new().route(
        "/projects/{project_id}/ai/structured-output/stream",
        post(stream_structured_output),
    )
}

#[utoipa::path(
    post,
    tag = "AI Gateway",
    path = "/projects/{project_id}/ai/structured-output/stream",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = StreamStructuredOutputRequest,
    responses(
        (status = 200, description = "SSE events: started, partial, complete, or error", content_type = "text/event-stream"),
        (status = 400, description = "Invalid prompt or JSON Schema"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "No active AI provider")
    ),
    security(("bearer_auth" = []))
)]
pub async fn stream_structured_output(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AiGatewayAppState>>,
    Path(project_id): Path<i32>,
    Json(mut request): Json<StreamStructuredOutputRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Problem> {
    // A completion incurs provider usage/cost, so this is a write capability
    // even though it does not mutate a project row.
    permission_guard!(auth, AiGatewayExecute);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    authorize_structured_purpose(state.db.as_ref(), project_id, &request.purpose).await?;
    apply_summary_preference(&state, &mut request).await?;

    let stream = state
        .structured_output_service
        .stream(StructuredOutputRequest {
            project_id,
            purpose: request.purpose,
            prompt: request.prompt,
            schema: request.schema,
            provider_key_id: request.provider_key_id,
            model: request.model,
            thinking_level: request.thinking_level,
            requester_id: auth.user_id_opt(),
            cache_key: request.cache_key,
            cache_ttl_seconds: request.cache_ttl_seconds,
            refresh: request.refresh,
        })
        .await
        .map_err(structured_output_problem)?;

    let events = stream.map(|event| {
        let name = match &event {
            StructuredOutputEvent::Started { .. } => "started",
            StructuredOutputEvent::Partial { .. } => "partial",
            StructuredOutputEvent::Complete { .. } => "complete",
            StructuredOutputEvent::Error { .. } => "error",
        };
        let data = serde_json::to_string(&event).unwrap_or_else(|error| {
            serde_json::json!({
                "type": "error",
                "code": "serialization_error",
                "message": error.to_string(),
            })
            .to_string()
        });
        Ok::<_, Infallible>(Event::default().event(name).data(data))
    });

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// Browser-authored generic prompts are gateway-only. If the shared summary
/// profile selects a host CLI, return 409 so the screen can use its
/// server-authored summary endpoint instead of exposing an ambient host
/// credential to arbitrary browser prompt text.
async fn apply_summary_preference(
    state: &AiGatewayAppState,
    request: &mut StreamStructuredOutputRequest,
) -> Result<(), Problem> {
    if !request.purpose.ends_with(".summary") || request.provider_key_id.is_some() {
        return Ok(());
    }
    let Some(preference) = state
        .provider_preference_service
        .get("instance")
        .await
        .map_err(Problem::from)?
    else {
        return Ok(());
    };
    let route = preference.summary_provider_id.as_deref().or_else(|| {
        (preference.provider_type == "agent_cli")
            .then_some(preference.agent_cli_provider_id.as_deref())
            .flatten()
    });
    if let Some(route) = route {
        if let Some(key_id) = route.strip_prefix("gateway_key:") {
            request.provider_key_id = Some(key_id.parse::<i32>().map_err(|_| {
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("Invalid AI Summary Provider")
                    .with_detail("The saved AI summary gateway route is invalid; choose it again in AI Gateway settings")
            })?);
        } else if route != "gateway" {
            return Err(problemdetails::new(axum::http::StatusCode::CONFLICT)
                .with_title("Server-authored Summary Required")
                .with_detail(format!(
                    "AI summaries use the host provider '{route}'; use the purpose-specific summary endpoint"
                )));
        }
    }
    if request.model.is_none() {
        request.model = preference.summary_model;
    }
    if request.thinking_level.is_none() {
        request.thinking_level = preference.summary_thinking_level;
    }
    Ok(())
}

/// Structured transport stays reusable, while every product purpose must be
/// registered here with its server-side consent policy before a browser may
/// invoke it. This prevents an arbitrary purpose string from bypassing a
/// feature's opt-in or data-governance controls.
async fn authorize_structured_purpose(
    db: &sea_orm::DatabaseConnection,
    project_id: i32,
    purpose: &str,
) -> Result<(), Problem> {
    match purpose {
        "api_traffic.summary" => {
            let project = temps_entities::projects::Entity::find_by_id(project_id)
                .one(db)
                .await
                .map_err(|error| {
                    tracing::error!(project_id, %error, "Failed to read structured AI consent");
                    problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("AI Consent Check Failed")
                        .with_detail("The project AI consent setting could not be read")
                })?
                .ok_or_else(|| {
                    problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                        .with_title("Project Not Found")
                        .with_detail(format!("Project {project_id} was not found"))
                })?;
            if project.ai_api_traffic_summary_enabled != Some(true) {
                return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                    .with_title("AI Traffic Summary Disabled")
                    .with_detail(
                        "Enable AI traffic summaries in project security settings before requesting a summary",
                    ));
            }
            Ok(())
        }
        _ => Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Unsupported Structured AI Purpose")
            .with_detail(format!(
                "Structured AI purpose '{purpose}' is not registered by the server"
            ))),
    }
}

fn structured_output_problem(error: StructuredOutputError) -> Problem {
    match error {
        StructuredOutputError::InvalidRequest { .. } => {
            problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
                .with_title("Invalid Structured AI Request")
                .with_detail(error.to_string())
        }
        StructuredOutputError::NotAvailable => {
            problemdetails::new(axum::http::StatusCode::CONFLICT)
                .with_title("AI Not Configured")
                .with_detail(error.to_string())
        }
        StructuredOutputError::Provider(_) => {
            tracing::warn!(error = %error, "Structured AI provider failed to start");
            problemdetails::new(axum::http::StatusCode::BAD_GATEWAY)
                .with_title("AI Provider Unavailable")
                .with_detail("The configured AI provider could not start the request")
        }
        StructuredOutputError::Timeout { .. } => {
            problemdetails::new(axum::http::StatusCode::GATEWAY_TIMEOUT)
                .with_title("AI Provider Timed Out")
                .with_detail(error.to_string())
        }
        StructuredOutputError::RateLimited { .. } => {
            problemdetails::new(axum::http::StatusCode::TOO_MANY_REQUESTS)
                .with_title("AI Request Rate Limited")
                .with_detail(error.to_string())
        }
    }
}
