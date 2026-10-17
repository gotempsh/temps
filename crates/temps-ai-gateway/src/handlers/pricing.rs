// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::problemdetails::{Problem, ProblemDetails};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AiGatewayAppState;
use crate::services::pricing::{build_pricing, ModelPricing};

// ============================================================================
// OpenAPI schema
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(get_pricing),
    components(schemas(PricingResponse, ModelPricing)),
    info(
        title = "AI Gateway Pricing API",
        description = "Model pricing information for the AI gateway",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Gateway Pricing", description = "Model pricing endpoints")
    )
)]
pub struct AiGatewayPricingApiDoc;

pub fn configure_pricing_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new().route("/ai/pricing", get(get_pricing))
}

// ============================================================================
// DTOs
// ============================================================================

#[derive(Debug, Serialize, ToSchema)]
pub struct PricingResponse {
    pub models: Vec<ModelPricing>,
}

// ============================================================================
// Handlers
// ============================================================================

#[utoipa::path(
    tag = "AI Gateway Pricing",
    get,
    path = "/ai/pricing",
    responses(
        (status = 200, description = "Model pricing information", body = PricingResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_pricing(
    RequireAuth(auth): RequireAuth,
    State(_app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    Ok(Json(PricingResponse {
        models: build_pricing(),
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_response_serialization() {
        let anthropic = build_pricing()
            .into_iter()
            .find(|model| model.provider == "anthropic")
            .expect("catalog lists an anthropic model");
        let model_id = anthropic.model.clone();
        let response = PricingResponse {
            models: vec![anthropic],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains(&model_id));
        assert!(json.contains("input_per_million"));
        assert!(json.contains("output_per_million"));
    }
}
