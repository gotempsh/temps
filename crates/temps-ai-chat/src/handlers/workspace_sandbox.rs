// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resolving the sandbox behind a workspace, running commands in it, and
//! mapping sandbox failures to Problem Details.

use super::*;

pub(super) fn application_workspace_failure_detail(
    error: &temps_sandbox::error::SandboxError,
) -> &'static str {
    use temps_sandbox::error::SandboxError;

    match error {
        SandboxError::CreateFailed { reason, .. } => {
            let reason = reason.to_ascii_lowercase();
            if reason.contains("fully subnetted")
                || reason.contains("address pool")
                || reason.contains("non-overlapping ipv4")
            {
                "This Temps host has no private sandbox network capacity available. Contact your Temps administrator, then try again."
            } else if reason.contains("gateway_mode_ipv4")
                || reason.contains("unknown gateway mode")
            {
                "The sandbox runtime on this Temps host is incompatible with secure workspace networking. Contact your Temps administrator, then try again."
            } else {
                "Temps could not start workspace compute. Try again; if it continues, contact your Temps administrator."
            }
        }
        SandboxError::NetworkCapacityExhausted { .. } => {
            "This Temps host has no private sandbox network capacity available. Contact your Temps administrator, then try again."
        }
        SandboxError::Unavailable { .. } => {
            "The sandbox runtime is unavailable on this Temps host. Contact your Temps administrator, then try again."
        }
        SandboxError::ImageUnavailable { .. } => {
            "The workspace image could not be pulled on this Temps host. Contact your Temps administrator to check registry access and that the image tag is published, then try again."
        }
        SandboxError::Timeout { .. } => {
            "The workspace did not become ready before the startup timeout. Try again; if it continues, contact your Temps administrator."
        }
        SandboxError::InvalidState { .. } => {
            "The workspace cannot resume from its current state. Rebuild it, or contact your Temps administrator."
        }
        SandboxError::RuntimeEnvironmentNotFound { .. } => {
            "A linked project has no active environment, so Temps could not prepare workspace services. Activate the environment, then try again."
        }
        SandboxError::RuntimeCredentialsFailed { .. } => {
            "Temps could not prepare credentials for a linked workspace service. Check the service configuration, then try again."
        }
        SandboxError::RuntimeVariableConflict { .. } => {
            "Linked services expose conflicting runtime variable names. Resolve the service configuration, then try again."
        }
        _ => {
            "Temps could not prepare the persistent application workspace. Try again; if it continues, contact your Temps administrator."
        }
    }
}

pub(super) fn application_workspace_failure_problem(
    error: &temps_sandbox::error::SandboxError,
) -> Problem {
    use temps_sandbox::error::SandboxError;

    let status = match error {
        SandboxError::Unavailable { .. }
        | SandboxError::ImageUnavailable { .. }
        | SandboxError::NetworkCapacityExhausted { .. } => StatusCode::SERVICE_UNAVAILABLE,
        SandboxError::Timeout { .. } => StatusCode::GATEWAY_TIMEOUT,
        SandboxError::InvalidState { .. } | SandboxError::RuntimeVariableConflict { .. } => {
            StatusCode::CONFLICT
        }
        SandboxError::RuntimeEnvironmentNotFound { .. } => StatusCode::NOT_FOUND,
        SandboxError::RuntimeCredentialsFailed { .. } => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    problemdetails::new(status)
        .with_title("Application Workspace Failed")
        .with_detail(application_workspace_failure_detail(error))
}

pub(super) fn application_workspace_provider_problem(
    error: temps_sandbox::error::SandboxError,
    application_id: &str,
    operation: &'static str,
) -> Problem {
    tracing::error!(
        %error,
        application_id,
        operation,
        "application workspace provider operation failed"
    );
    application_workspace_failure_problem(&error)
}

pub(super) fn application_workspace_snapshot_problem(
    error: temps_sandbox::error::SandboxSnapshotError,
    application_id: &str,
    operation: &'static str,
) -> Problem {
    use temps_sandbox::error::SandboxSnapshotError;

    tracing::error!(
        %error,
        application_id,
        operation,
        "application workspace snapshot operation failed"
    );
    let (status, title, detail) = match error {
        SandboxSnapshotError::NotFound { .. } => (
            StatusCode::NOT_FOUND,
            "Snapshot Not Found",
            "The requested workspace snapshot was not found.",
        ),
        SandboxSnapshotError::NotReady { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Snapshot Not Ready",
            "The workspace snapshot is not ready to restore yet. Try again after it finishes.",
        ),
        SandboxSnapshotError::CrossBackendRestore { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Snapshot Restore Not Supported",
            "This snapshot cannot be restored by the workspace's current runtime.",
        ),
        SandboxSnapshotError::NotSupported { .. } => (
            StatusCode::NOT_IMPLEMENTED,
            "Snapshots Not Supported",
            "The configured sandbox runtime does not support workspace snapshots.",
        ),
        SandboxSnapshotError::SnapshotInProgress { .. } => (
            StatusCode::CONFLICT,
            "Snapshot In Progress",
            "Another workspace snapshot is still being created. Wait for it to finish, then try again.",
        ),
        SandboxSnapshotError::QuotaExceeded { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Snapshot Storage Limit Reached",
            "The workspace snapshot storage limit has been reached. Remove an older snapshot, then try again.",
        ),
        SandboxSnapshotError::InvalidState { .. }
        | SandboxSnapshotError::SandboxNotRunning { .. } => (
            StatusCode::CONFLICT,
            "Workspace Not Ready",
            "The workspace must be running before this snapshot operation can continue.",
        ),
        SandboxSnapshotError::SandboxNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "Workspace Compute Not Found",
            "The workspace compute could not be found. Rebuild the workspace, then try again.",
        ),
        SandboxSnapshotError::LegacyArtifactUnsupported { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Snapshot Format Not Supported",
            "This older workspace snapshot cannot be restored by the current runtime.",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Workspace Snapshot Failed",
            "Temps could not complete the workspace snapshot operation. Try again; if it continues, contact your Temps administrator.",
        ),
    };
    problemdetails::new(status)
        .with_title(title)
        .with_detail(detail)
}

pub(super) async fn application_workspace_sandbox(
    state: &AppState,
    auth: &AuthContext,
    application_public_id: &str,
) -> Result<(crate::applications::ApplicationWithProjects, String), Problem> {
    let application = state
        .applications
        .get(auth.user_id(), application_public_id)
        .await?;
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    if let Err(problem) = ensure_application_project_permission(
        auth,
        &state.project_access_checker,
        &project_ids,
        &Permission::SandboxesWrite,
    )
    .await
    {
        quarantine_application_workspace(state, auth.user_id(), application_public_id).await;
        return Err(problem);
    }
    if let Err(problem) = ensure_application_project_permission(
        auth,
        &state.project_access_checker,
        &project_ids,
        &Permission::ProjectsWrite,
    )
    .await
    {
        quarantine_application_workspace(state, auth.user_id(), application_public_id).await;
        return Err(problem);
    }
    let workspace = state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    let desired_workspace = state
        .applications
        .workspace(application.application.id)
        .await?;
    if desired_workspace.desired_state == "quarantined" {
        return Err(problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Application Workspace Quarantined")
            .with_detail("Workspace execution is blocked because access to one or more linked projects could not be verified. Restore access, then explicitly resume the workspace."));
    }
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Application Sandbox Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let sandbox = match sandboxes
        .get_or_create_application_workspace_with_config(
            auth.user_id(),
            &application.application.public_id,
            project_ids.first().copied(),
            workspace.host_work_dir,
            (&desired_workspace).into(),
            &project_ids,
        )
        .await
    {
        Ok(sandbox) => sandbox,
        Err(error) => {
            let detail = application_workspace_failure_detail(&error);
            error!(%error, application_id = application_public_id, "failed to prepare application Git workspace");
            if let Err(persistence_error) = state
                .applications
                .update_workspace_runtime_state(
                    application.application.id,
                    None,
                    Some(detail.to_string()),
                )
                .await
            {
                error!(
                    %persistence_error,
                    application_id = application_public_id,
                    "failed to persist application workspace startup diagnostic"
                );
            }
            return Err(application_workspace_failure_problem(&error));
        }
    };
    state
        .applications
        .record_workspace_sandbox(
            application.application.id,
            Some(sandbox.public_id.clone()),
            None,
            None,
        )
        .await?;
    Ok((application, sandbox.public_id))
}

pub(super) async fn global_workspace_sandbox(
    state: &AppState,
    auth: &AuthContext,
) -> Result<(String, String), Problem> {
    let workspace_id = global_workspace_context_id(auth.user_id());
    let workspace = state
        .application_workspaces
        .ensure(&workspace_id, &[])
        .await?;
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Global Workspace Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let sandbox = sandboxes
        .get_or_create_application_workspace(
            auth.user_id(),
            &workspace_id,
            None,
            workspace.host_work_dir,
        )
        .await
        .map_err(|error| {
            error!(
                %error,
                workspace_id,
                "failed to prepare global AI workspace sandbox"
            );
            global_workspace_failure_problem(&error)
        })?;
    Ok((workspace_id, sandbox.public_id))
}

/// Problem returned when the persistent global workspace cannot be prepared.
/// A sandbox image that cannot be pulled is reported as such (503, naming the
/// image) instead of the generic 500, so the operator learns what to fix.
pub(super) fn global_workspace_failure_problem(
    error: &temps_sandbox::error::SandboxError,
) -> Problem {
    use temps_sandbox::error::SandboxError;

    // Only the image-pull failure gets its own response here. `SandboxError`
    // is `#[non_exhaustive]` and owned by `temps-sandbox`, which maps every
    // variant explicitly in its own `From<SandboxError> for Problem`; this
    // workspace-preparation path deliberately returns one generic 500 for the
    // rest so no sandbox internals reach the response.
    if let SandboxError::ImageUnavailable { image, .. } = error {
        return problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_type("https://temps.sh/probs/sandbox-image-unavailable")
            .with_title("Global Workspace Image Unavailable")
            .with_detail(format!(
                "Temps could not pull the sandbox image '{image}' needed for the persistent \
                 global workspace. Check that this host can reach the image registry and \
                 that the image tag is published, then try again."
            ));
    }
    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_title("Global Workspace Failed")
        .with_detail("Temps could not prepare the persistent global workspace.")
}

pub(super) async fn run_workspace_command(
    state: &AppState,
    auth: &AuthContext,
    sandbox_public_id: &str,
    cmd: Vec<String>,
) -> Result<temps_sandbox::services::ExecResult, Problem> {
    let sandboxes = state.application_sandboxes.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Application Sandbox Unavailable")
            .with_detail("The instance sandbox service is not configured.")
    })?;
    let execution = sandboxes.exec(
        sandbox_public_id,
        auth.user_id(),
        temps_sandbox::services::ExecOptions {
            cmd,
            ..Default::default()
        },
    );
    match tokio::time::timeout(WORKSPACE_COMMAND_TIMEOUT, execution).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            error!(%error, sandbox_id = sandbox_public_id, "application workspace command failed");
            Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Application Workspace Failed")
                .with_detail("Temps could not inspect the persistent application workspace."))
        }
        Err(_) => {
            error!(
                sandbox_id = sandbox_public_id,
                timeout_seconds = WORKSPACE_COMMAND_TIMEOUT.as_secs(),
                "application workspace command timed out"
            );
            Err(problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                .with_title("Application Workspace Timed Out")
                .with_detail(format!(
                    "Git inspection exceeded the {} second safety limit. Narrow the workspace or try again.",
                    WORKSPACE_COMMAND_TIMEOUT.as_secs()
                )))
        }
    }
}

pub(super) async fn application_workspace_repositories(
    state: &AppState,
    auth: &AuthContext,
    sandbox_public_id: &str,
) -> Result<Vec<String>, Problem> {
    let result = run_workspace_command(
        state,
        auth,
        sandbox_public_id,
        vec![
            "sh".to_string(),
            "-lc".to_string(),
            "find projects -mindepth 2 -maxdepth 2 -type d -name .git -print0 2>/dev/null | head -c 65536"
                .to_string(),
        ],
    )
    .await?;
    let mut repositories = vec![String::new()];
    repositories.extend(
        result
            .stdout
            .split('\0')
            .filter_map(|path| path.strip_suffix("/.git"))
            .filter(|path| !path.is_empty() && !is_sensitive_workspace_path(path))
            .take(20)
            .map(str::to_string),
    );
    Ok(repositories)
}
