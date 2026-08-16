//! HTTP handlers for `/v1/sandboxes/*`. Every handler follows the same
//! shape: `RequireAuth` + `sandbox_permission_guard` + service call + typed DTO.
//! No business logic lives here.

pub mod sandboxes;
pub mod snapshots;
pub mod terminal;
pub mod version_header;

use std::sync::Arc;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use utoipa::OpenApi;

use crate::services::sandbox_service::SandboxService;
use crate::services::snapshot_service::SnapshotService;

/// Shared state for sandbox HTTP handlers. Intentionally minimal — the
/// service already owns db/registry/jobs/config, so handlers need only
/// the service itself plus the project access checker.
pub struct SandboxAppState {
    pub sandbox_service: Arc<SandboxService>,
    /// Snapshot service (ADR-037). `None` when the sandbox plugin is loaded
    /// without a Docker provider (e.g. Firecracker-only builds).
    pub snapshot_service: Option<Arc<SnapshotService>>,
    /// Team-membership gate for `project_id`-scoped creates (ADR-028).
    /// `None` when no checker is registered (OSS builds without the
    /// teams plugin), in which case `project_access_guard!` is a no-op
    /// and ownership/scope checks alone apply.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// Audit logger for write operations. `None` when no audit plugin is
    /// registered (e.g. some test builds). Audit failures must not fail
    /// the primary request — log the error and continue.
    pub audit_service: Option<Arc<dyn temps_core::AuditLogger>>,
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
        sandboxes::create_preview_link,
        sandboxes::clear_preview_password,
        sandboxes::resize_sandbox,
        sandboxes::list_events,
        sandboxes::rootfs_report,
        sandboxes::rootfs_gc,
        terminal::terminal,
        // Snapshot API (ADR-037)
        snapshots::create_snapshot,
        snapshots::list_snapshots,
        snapshots::get_snapshot,
        snapshots::delete_snapshot,
        snapshots::storage_summary,
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
        sandboxes::PreviewShareLinkBody,
        sandboxes::PreviewShareLinkResponse,
        sandboxes::SetPreviewPasswordResponse,
        sandboxes::ResizeSandboxBody,
        sandboxes::SandboxEvent,
        sandboxes::SandboxEventsResponse,
        temps_agents::sandbox::RootfsReport,
        temps_agents::sandbox::RootfsCacheEntry,
        temps_agents::sandbox::RootfsVmEntry,
        temps_agents::sandbox::RootfsGcReport,
        // Snapshot schemas (ADR-037)
        snapshots::CreateSnapshotBody,
        snapshots::SnapshotResponse,
        snapshots::ListSnapshotsResponse,
        crate::services::snapshot_service::StorageSummary,
    )),
    tags(
        (name = "Sandboxes", description = "Standalone sandbox API (`/v1/sandboxes/*`) for running isolated containers."),
        (name = "Snapshots", description = "Sandbox snapshot API (ADR-037): create, list, get, and delete snapshots."),
    )
)]
pub struct SandboxApiDoc;

/// Configure all `/v1/sandboxes/*` and `/v1/sandbox-snapshots/*` routes.
/// Every response is stamped with the `X-Sandbox-API-Version` diagnostic
/// header (see ADR-009).
pub fn configure_routes() -> Router<Arc<SandboxAppState>> {
    // Snapshot collection routes — static `/storage-summary` registered
    // before the `{snap_id}` capture so Axum doesn't swallow it.
    let snapshot_routes = Router::new()
        .route(
            "/v1/sandbox-snapshots/storage-summary",
            get(snapshots::storage_summary),
        )
        .route("/v1/sandbox-snapshots", get(snapshots::list_snapshots))
        .route(
            "/v1/sandbox-snapshots/{snap_id}",
            get(snapshots::get_snapshot).delete(snapshots::delete_snapshot),
        )
        // Per-sandbox snapshot creation is nested under the sandbox routes.
        .route(
            "/v1/sandboxes/{id}/snapshots",
            post(snapshots::create_snapshot),
        );

    sandboxes::routes()
        .merge(snapshot_routes)
        .layer(middleware::from_fn(version_header::inject_version_header))
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
            "/v1/sandboxes/{id}/preview-link",
            "/v1/sandboxes/{id}/events",
            "/v1/sandboxes/{id}/resize",
            "/v1/sandboxes/{id}/terminal",
            "/v1/sandboxes/rootfs",
            "/v1/sandboxes/rootfs/gc",
            // Snapshot API (ADR-037)
            "/v1/sandboxes/{id}/snapshots",
            "/v1/sandbox-snapshots",
            "/v1/sandbox-snapshots/storage-summary",
            "/v1/sandbox-snapshots/{snap_id}",
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

    #[test]
    fn preview_link_openapi_contract_is_domain_prefixed_and_documents_conflict() {
        let api = serde_json::to_value(SandboxApiDoc::openapi()).expect("serialize OpenAPI");
        let operation = &api["paths"]["/v1/sandboxes/{id}/preview-link"]["post"];
        assert_eq!(
            operation["operationId"],
            serde_json::json!("sandbox_create_preview_link")
        );
        assert!(operation["responses"].get("409").is_some());
        assert!(operation["responses"].get("500").is_some());
    }
}
