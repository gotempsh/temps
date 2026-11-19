// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use temps_sandbox_providers::{
    remote::RemoteServiceAccessRequest,
    service_access::{ServiceAccessCommand, ServiceAccessReply},
};

pub const PATH: &str = "/agent/sandboxes/service-access";
pub const BODY_LIMIT: usize = 8 * 1024 * 1024;

/// Runs after authentication but before sealed-body decoding. At most two
/// deliveries may allocate decrypted policy bodies at a time; excess work is
/// rejected immediately so a slow worker cannot accumulate an input queue.
pub async fn limit(State(state): HostState, request: Request, next: Next) -> Response {
    if request.uri().path() != PATH {
        return next.run(request).await;
    }
    let Ok(_permit) = Arc::clone(&state.access_permits).try_acquire_owned() else {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "worker service-access delivery capacity is full",
        )
        .into_response();
    };
    match tokio::time::timeout(Duration::from_secs(30), next.run(request)).await {
        Ok(response) => response,
        Err(_) => err(
            StatusCode::GATEWAY_TIMEOUT,
            "worker service-access request exceeded its receive and delivery deadline",
        )
        .into_response(),
    }
}

#[utoipa::path(
    tag = "Sandboxes", post, path = "/agent/sandboxes/service-access",
    request_body = RemoteServiceAccessRequest,
    responses(
        (status = 200, description = "Current host identity or enforced command receipt", body = ServiceAccessReply),
        (status = 400, description = "Invalid sandbox handle", body = RemoteErrorBody),
        (status = 401, description = "Unauthorized control plane"),
        (status = 404, description = "Sandbox missing", body = RemoteErrorBody),
        (status = 413, description = "Policy body too large"),
        (status = 429, description = "Delivery capacity exhausted", body = RemoteErrorBody),
        (status = 500, description = "Policy rejected or enforcement failed", body = RemoteErrorBody),
        (status = 503, description = "Sandbox host unavailable", body = RemoteErrorBody),
        (status = 504, description = "Delivery deadline exceeded", body = RemoteErrorBody)
    ), security(("bearer_auth" = []))
)]
pub async fn command(
    State(state): HostState,
    Json(request): Json<RemoteServiceAccessRequest>,
) -> ApiResult<ServiceAccessReply> {
    let host = host(&state)?;
    // This worker endpoint uses AgentAuth + sealed requests, not console user
    // tokens. Project authorization happens before the control plane sends it.
    if !is_sandbox_handle(&request.handle) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "service access requires a managed sandbox handle",
        ));
    }
    let sandbox = request.handle.sandbox_name.clone();
    tokio::time::timeout(Duration::from_secs(25), async {
        let handle = if matches!(request.command, ServiceAccessCommand::Retire { .. }) {
            // Retirement must work after Docker deletion or during an outage.
            // The provider fences the exact host-assigned incarnation itself.
            request.handle
        } else {
            require_handle(host, request.handle).await?
        };
        host.provider
            .service_access(&handle, request.command)
            .await
            .map(Json)
            .map_err(provider_err)
    })
    .await
    .map_err(|_| {
        err(
            StatusCode::GATEWAY_TIMEOUT,
            format!("service access for sandbox '{sandbox}' exceeded its delivery deadline"),
        )
    })?
}
