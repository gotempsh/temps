//! HTTP handlers for `/v1/sandboxes/*`. Every handler follows the same
//! shape: `RequireAuth` + `sandbox_permission_guard` + service call + typed DTO.
//! No business logic lives here.

pub mod sandboxes;
pub mod version_header;

use std::sync::Arc;

use axum::{middleware, Router};
use utoipa::OpenApi;

use crate::services::sandbox_service::SandboxService;

/// Shared state for sandbox HTTP handlers. Intentionally minimal — the
/// service already owns db/registry/jobs/config, so handlers need only
/// the service itself.
pub struct SandboxAppState {
    pub sandbox_service: Arc<SandboxService>,
}

/// OpenAPI document for the `/v1/sandboxes/*` surface.
///
/// This is the canonical machine-readable contract for the sandbox API.
/// The doc is merged into the unified Temps OpenAPI at
/// `/api-docs/openapi.json` (and rendered in Swagger UI at `/swagger-ui`)
/// via the plugin system's `openapi_schema()` hook. There is **no**
/// separate `/v1/sandboxes/openapi.json` endpoint — external SDK generators
/// and compatibility tests should fetch the unified doc and filter by the
/// `Sandboxes` tag.
#[derive(OpenApi)]
#[openapi(
    paths(
        sandboxes::create_sandbox,
        sandboxes::list_sandboxes,
        sandboxes::get_sandbox,
        sandboxes::stop_sandbox,
        sandboxes::destroy_sandbox,
        sandboxes::pause_sandbox,
        sandboxes::resume_sandbox,
        sandboxes::restart_sandbox,
        sandboxes::source_sandbox,
        sandboxes::extend_timeout,
        sandboxes::exec,
        sandboxes::exec_detached,
        sandboxes::list_jobs,
        sandboxes::job_status,
        sandboxes::job_logs,
        sandboxes::kill_job,
        sandboxes::cmd,
        sandboxes::get_cmd,
        sandboxes::cmd_logs,
        sandboxes::cmd_kill,
        sandboxes::read_file,
        sandboxes::write_file,
        sandboxes::write_files,
        sandboxes::stat_path,
        sandboxes::mkdir,
        sandboxes::domain,
        sandboxes::set_preview_password,
        sandboxes::clear_preview_password,
        sandboxes::resize_sandbox,
        sandboxes::list_events,
        sandboxes::rootfs_report,
        sandboxes::rootfs_gc,
    ),
    components(schemas(
        sandboxes::CreateSandboxBody,
        sandboxes::SourceBody,
        sandboxes::SandboxResponse,
        sandboxes::ListSandboxesResponse,
        sandboxes::ExtendTimeoutBody,
        sandboxes::ExecBody,
        sandboxes::ExecResponse,
        sandboxes::ExecDetachedResponse,
        sandboxes::JobStatusResponse,
        sandboxes::JobSummaryResponse,
        sandboxes::ListJobsResponse,
        sandboxes::KillJobBody,
        sandboxes::CmdBody,
        sandboxes::CmdInner,
        sandboxes::CmdResponse,
        sandboxes::CmdKillBody,
        sandboxes::WriteFileBody,
        sandboxes::WriteFilesBody,
        sandboxes::WriteFilesResponse,
        sandboxes::ReadFileResponse,
        sandboxes::MkdirBody,
        sandboxes::StatResponse,
        sandboxes::SandboxDomainResponse,
        sandboxes::SetPreviewPasswordBody,
        sandboxes::SetPreviewPasswordResponse,
        sandboxes::ResizeSandboxBody,
        sandboxes::SandboxEvent,
        sandboxes::SandboxEventsResponse,
        temps_agents::sandbox::RootfsReport,
        temps_agents::sandbox::RootfsCacheEntry,
        temps_agents::sandbox::RootfsVmEntry,
        temps_agents::sandbox::RootfsGcReport,
    )),
    tags(
        (name = "Sandboxes", description = "Standalone sandbox API (`/v1/sandboxes/*`) for running isolated containers.")
    )
)]
pub struct SandboxApiDoc;

/// Configure all `/v1/sandboxes/*` routes. Every response is stamped with
/// the `X-Sandbox-API-Version` diagnostic header (see ADR-009).
pub fn configure_routes() -> Router<Arc<SandboxAppState>> {
    sandboxes::routes().layer(middleware::from_fn(version_header::inject_version_header))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The OpenAPI doc must enumerate every `/v1/sandboxes/*` route so external
    /// SDK generators (and the SDK compatibility tests) don't silently drift
    /// from the live router. If you add a route in `sandboxes::routes()`,
    /// update `SandboxApiDoc::paths` to match — this test is the guardrail.
    #[test]
    fn openapi_doc_enumerates_core_sandbox_paths() {
        let api = SandboxApiDoc::openapi();
        let paths = &api.paths.paths;

        for expected in [
            "/v1/sandboxes",
            "/v1/sandboxes/{id}",
            "/v1/sandboxes/{id}/stop",
            "/v1/sandboxes/{id}/destroy",
            "/v1/sandboxes/{id}/pause",
            "/v1/sandboxes/{id}/resume",
            "/v1/sandboxes/{id}/restart",
            "/v1/sandboxes/{id}/source",
            "/v1/sandboxes/{id}/extend-timeout",
            "/v1/sandboxes/{id}/exec",
            "/v1/sandboxes/{id}/exec-detached",
            "/v1/sandboxes/{id}/jobs",
            "/v1/sandboxes/{id}/jobs/{job_id}",
            "/v1/sandboxes/{id}/jobs/{job_id}/logs",
            "/v1/sandboxes/{id}/jobs/{job_id}/kill",
            "/v1/sandboxes/{id}/cmd",
            "/v1/sandboxes/{id}/cmd/{cmd_id}",
            "/v1/sandboxes/{id}/cmd/{cmd_id}/logs",
            "/v1/sandboxes/{id}/{cmd_id}/kill",
            "/v1/sandboxes/{id}/fs/read",
            "/v1/sandboxes/{id}/fs/write",
            "/v1/sandboxes/{id}/fs/write-batch",
            "/v1/sandboxes/{id}/fs/stat",
            "/v1/sandboxes/{id}/fs/mkdir",
            "/v1/sandboxes/{id}/domain",
            "/v1/sandboxes/{id}/preview-password",
            "/v1/sandboxes/{id}/events",
            "/v1/sandboxes/{id}/resize",
            "/v1/sandboxes/rootfs",
            "/v1/sandboxes/rootfs/gc",
        ] {
            assert!(
                paths.contains_key(expected),
                "SandboxApiDoc is missing path {}",
                expected
            );
        }
    }

    #[test]
    fn openapi_doc_exposes_core_schemas() {
        let api = SandboxApiDoc::openapi();
        let components = api.components.expect("components section present");
        for expected in [
            "CreateSandboxBody",
            "ExecBody",
            "SandboxResponse",
            "StatResponse",
        ] {
            assert!(
                components.schemas.contains_key(expected),
                "SandboxApiDoc is missing schema {}",
                expected
            );
        }
    }
}
