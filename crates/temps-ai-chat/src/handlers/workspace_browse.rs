// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Browsing a workspace: directories, files, git changes and diffs.

use super::*;

pub(super) fn parse_workspace_status(value: &str) -> Vec<ApplicationWorkspaceFileResponse> {
    let records = value.split('\0').collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index < records.len() && changes.len() < WORKSPACE_MAX_CHANGES {
        let record = records[index];
        index += 1;
        if record.len() < 4 {
            continue;
        }
        let bytes = record.as_bytes();
        let staged_code = bytes[0] as char;
        let unstaged_code = bytes[1] as char;
        let path = record[3..].to_string();
        // In porcelain -z output a rename/copy is followed by the original
        // path as a second NUL-delimited record. The UI identifies the current
        // file, so consume but do not expose that extra record.
        if matches!(staged_code, 'R' | 'C') || matches!(unstaged_code, 'R' | 'C') {
            index += 1;
        }
        if is_sensitive_workspace_path(&path) {
            continue;
        }
        let status = if staged_code == '?' && unstaged_code == '?' {
            "untracked"
        } else if staged_code == 'D' || unstaged_code == 'D' {
            "deleted"
        } else if staged_code == 'R' || unstaged_code == 'R' {
            "renamed"
        } else if staged_code == 'A' {
            "added"
        } else {
            "modified"
        };
        changes.push(ApplicationWorkspaceFileResponse {
            path,
            status: Some(status.to_string()),
            staged: staged_code != ' ' && staged_code != '?',
            unstaged: unstaged_code != ' ',
        });
    }
    changes
}

pub(super) fn truncate_workspace_text(value: String, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value, false);
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

pub(super) fn workspace_file_records(value: &str) -> impl Iterator<Item = &str> {
    value.split('\0').filter(|path| !path.trim().is_empty())
}

pub(super) fn workspace_file_page(
    files: &[String],
    query: &ApplicationWorkspaceChangesQuery,
) -> Result<(Vec<String>, Option<usize>), Problem> {
    let cursor = query.cursor.unwrap_or(0);
    let limit = query.limit.unwrap_or(WORKSPACE_DEFAULT_PAGE_SIZE);
    if cursor > WORKSPACE_MAX_FILES {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Workspace Cursor")
            .with_detail("The workspace file cursor is outside the supported range."));
    }
    if !(1..=WORKSPACE_MAX_PAGE_SIZE).contains(&limit) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Workspace Page Size")
            .with_detail(format!(
                "Workspace file pages must contain between 1 and {WORKSPACE_MAX_PAGE_SIZE} files."
            )));
    }

    let end = cursor.saturating_add(limit).min(files.len());
    let page = files.get(cursor..end).unwrap_or_default().to_vec();
    let next_cursor = (end < files.len()).then_some(end);
    Ok((page, next_cursor))
}

fn workspace_git_command(repository: &str, arguments: &[&str]) -> Vec<String> {
    let mut command = vec!["git".to_string()];
    if !repository.is_empty() {
        command.extend(["-C".to_string(), repository.to_string()]);
    }
    command.extend(arguments.iter().map(|argument| (*argument).to_string()));
    command
}

fn bounded_workspace_git_command(repository: &str, script: &str) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "temps-workspace-git".to_string(),
        if repository.is_empty() {
            ".".to_string()
        } else {
            repository.to_string()
        },
    ]
}

fn workspace_path(repository: &str, path: &str) -> String {
    if repository.is_empty() {
        path.to_string()
    } else {
        format!("{repository}/{path}")
    }
}

fn workspace_entry_kind(kind: WorkspaceEntryKind) -> &'static str {
    match kind {
        WorkspaceEntryKind::Directory => "directory",
        WorkspaceEntryKind::File => "file",
        WorkspaceEntryKind::Symlink => "symlink",
        WorkspaceEntryKind::Other => "other",
    }
}

async fn workspace_directory_response(
    state: &AppState,
    workspace_id: &str,
    query: ApplicationWorkspaceDirectoryQuery,
) -> Result<ApplicationWorkspaceDirectoryResponse, Problem> {
    let path = query.path.unwrap_or_default();
    let page = state
        .application_workspaces
        .list_directory(
            workspace_id,
            &path,
            query.cursor.unwrap_or(0),
            query.limit.unwrap_or(100),
        )
        .await?;
    Ok(ApplicationWorkspaceDirectoryResponse {
        path,
        entries: page
            .entries
            .into_iter()
            .map(|entry| ApplicationWorkspaceDirectoryEntryResponse {
                name: entry.name,
                path: entry.path,
                kind: workspace_entry_kind(entry.kind).to_string(),
                size_bytes: entry.size_bytes,
            })
            .collect(),
        next_cursor: page.next_cursor,
        truncated: page.truncated,
    })
}

pub(super) fn verified_workspace_image_media_type(
    path: &str,
    bytes: &[u8],
) -> Option<&'static str> {
    let extension = FsPath::new(path)
        .extension()
        .and_then(|value| value.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some("image/png"),
        "jpg" | "jpeg" if bytes.starts_with(&[0xff, 0xd8, 0xff]) => Some("image/jpeg"),
        "gif" if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => Some("image/gif"),
        "webp"
            if bytes.len() >= 12
                && bytes.starts_with(b"RIFF")
                && bytes.get(8..12) == Some(b"WEBP") =>
        {
            Some("image/webp")
        }
        _ => None,
    }
}

pub(super) fn workspace_image_extension(path: &str) -> bool {
    FsPath::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp"
            )
        })
}

pub(super) fn cap_unverified_workspace_image_as_text(
    preview: &mut crate::applications::WorkspaceFilePreview,
    image_candidate: bool,
    verified_media_type: Option<&str>,
    text_max_bytes: usize,
) {
    if image_candidate && verified_media_type.is_none() && preview.bytes.len() > text_max_bytes {
        preview.bytes.truncate(text_max_bytes);
        preview.truncated = true;
    }
}

async fn workspace_file_response(
    state: &AppState,
    workspace_id: &str,
    path: String,
) -> Result<ApplicationWorkspaceFileContentResponse, Problem> {
    let limits = workspace_file_limits(state).await?;
    let image_candidate = workspace_image_extension(&path);
    let text_max_bytes = usize::try_from(limits.max_text_preview_kb)
        .unwrap_or(1_024)
        .saturating_mul(1024)
        .min(WORKSPACE_ABSOLUTE_MAX_TEXT_PREVIEW_BYTES);
    let max_bytes = if image_candidate {
        mb_bytes(limits.max_image_preview_size_mb).min(WORKSPACE_ABSOLUTE_MAX_IMAGE_PREVIEW_BYTES)
    } else {
        text_max_bytes
    };
    let mut preview = state
        .application_workspaces
        .read_file_preview_with_limit(workspace_id, &path, max_bytes)
        .await?;
    let media_type = verified_workspace_image_media_type(&path, &preview.bytes);
    cap_unverified_workspace_image_as_text(
        &mut preview,
        image_candidate,
        media_type,
        text_max_bytes,
    );
    let content_b64 = media_type
        .filter(|_| !preview.truncated)
        .map(|_| B64.encode(&preview.bytes));
    let content = if media_type.is_some() {
        None
    } else {
        String::from_utf8(preview.bytes).ok()
    };
    Ok(ApplicationWorkspaceFileContentResponse {
        path,
        binary: media_type.is_some() || content.is_none(),
        content,
        content_b64,
        media_type: media_type.map(str::to_string),
        size_bytes: preview.size_bytes,
        truncated: preview.truncated,
    })
}

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications/{application_public_id}/workspace/directory",
    params(
        ("application_public_id" = String, Path,),
        ("path" = Option<String>, Query, description = "Workspace-relative directory; omit for the root"),
        ("cursor" = Option<usize>, Query, description = "Position returned by the previous page"),
        ("limit" = Option<usize>, Query, description = "Entries per page (1-100, default 100)"),
    ),
    responses((status = 200, body = ApplicationWorkspaceDirectoryResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn get_application_workspace_directory(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
    Query(query): Query<ApplicationWorkspaceDirectoryQuery>,
) -> Result<Json<ApplicationWorkspaceDirectoryResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &application_public_id,
        Some("active"),
        &[Permission::ProjectsRead, Permission::SandboxesRead],
    )
    .await?;
    state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    Ok(Json(
        workspace_directory_response(&state, &application.application.public_id, query).await?,
    ))
}

#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/workspace/directory",
    operation_id = "get_global_workspace_directory",
    params(
        ("path" = Option<String>, Query, description = "Workspace-relative directory; omit for the root"),
        ("cursor" = Option<usize>, Query, description = "Position returned by the previous page"),
        ("limit" = Option<usize>, Query, description = "Entries per page (1-100, default 100)"),
    ),
    responses((status = 200, body = ApplicationWorkspaceDirectoryResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn get_global_workspace_directory(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ApplicationWorkspaceDirectoryQuery>,
) -> Result<Json<ApplicationWorkspaceDirectoryResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let workspace_id = global_workspace_context_id(auth.user_id());
    state
        .application_workspaces
        .ensure(&workspace_id, &[])
        .await?;
    Ok(Json(
        workspace_directory_response(&state, &workspace_id, query).await?,
    ))
}

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications/{application_public_id}/workspace/file",
    params(("application_public_id" = String, Path,), ("path" = String, Query,)),
    responses((status = 200, body = ApplicationWorkspaceFileContentResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn get_application_workspace_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
    Query(query): Query<ApplicationWorkspaceFileQuery>,
) -> Result<Json<ApplicationWorkspaceFileContentResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let application = authorized_application_for_permissions(
        &state,
        &auth,
        &application_public_id,
        Some("active"),
        &[Permission::ProjectsRead, Permission::SandboxesRead],
    )
    .await?;
    state
        .application_workspaces
        .ensure(&application.application.public_id, &application.projects)
        .await?;
    Ok(Json(
        workspace_file_response(&state, &application.application.public_id, query.path).await?,
    ))
}

#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/workspace/file",
    operation_id = "get_global_workspace_file",
    params(("path" = String, Query,)),
    responses((status = 200, body = ApplicationWorkspaceFileContentResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 500)),
    security(("bearer_auth" = []))
)]
pub async fn get_global_workspace_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ApplicationWorkspaceFileQuery>,
) -> Result<Json<ApplicationWorkspaceFileContentResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesRead);
    deny_deployment_token!(auth);
    let workspace_id = global_workspace_context_id(auth.user_id());
    state
        .application_workspaces
        .ensure(&workspace_id, &[])
        .await?;
    Ok(Json(
        workspace_file_response(&state, &workspace_id, query.path).await?,
    ))
}

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications/{application_public_id}/workspace/changes",
    params(
        ("application_public_id" = String, Path,),
        ("cursor" = Option<usize>, Query, description = "Position returned by the previous page"),
        ("limit" = Option<usize>, Query, description = "Files per page (1-200, default 100)"),
    ),
    responses((status = 200, body = ApplicationWorkspaceChangesResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 503), (status = 504)),
    security(("bearer_auth" = []))
)]
pub async fn get_application_workspace_changes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
    Query(query): Query<ApplicationWorkspaceChangesQuery>,
) -> Result<Json<ApplicationWorkspaceChangesResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let response = tokio::time::timeout(
        WORKSPACE_REQUEST_TIMEOUT,
        collect_application_workspace_changes(&state, &auth, &application_public_id, &query),
    )
    .await
    .map_err(|_| {
        error!(
            application_id = application_public_id,
            timeout_seconds = WORKSPACE_REQUEST_TIMEOUT.as_secs(),
            "application workspace inspection request timed out"
        );
        problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
            .with_title("Application Workspace Timed Out")
            .with_detail(format!(
                "Workspace inspection exceeded the {} second safety limit. Narrow the workspace or try again.",
                WORKSPACE_REQUEST_TIMEOUT.as_secs()
            ))
    })??;
    Ok(Json(response))
}

#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/workspace/changes",
    operation_id = "get_global_workspace_changes",
    params(
        ("cursor" = Option<usize>, Query, description = "Position returned by the previous page"),
        ("limit" = Option<usize>, Query, description = "Files per page (1-200, default 100)"),
    ),
    responses((status = 200, body = ApplicationWorkspaceChangesResponse), (status = 400), (status = 401), (status = 403), (status = 503), (status = 504)),
    security(("bearer_auth" = []))
)]
pub async fn get_global_workspace_changes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ApplicationWorkspaceChangesQuery>,
) -> Result<Json<ApplicationWorkspaceChangesResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    let response = tokio::time::timeout(WORKSPACE_REQUEST_TIMEOUT, async {
        let (_, sandbox_public_id) = global_workspace_sandbox(&state, &auth).await?;
        collect_workspace_changes(&state, &auth, &sandbox_public_id, &query).await
    })
    .await
    .map_err(|_| {
        error!(
            user_id = auth.user_id(),
            timeout_seconds = WORKSPACE_REQUEST_TIMEOUT.as_secs(),
            "global workspace inspection request timed out"
        );
        problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
            .with_title("Global Workspace Timed Out")
            .with_detail(format!(
                "Workspace inspection exceeded the {} second safety limit. Narrow the workspace or try again.",
                WORKSPACE_REQUEST_TIMEOUT.as_secs()
            ))
    })??;
    Ok(Json(response))
}

async fn collect_application_workspace_changes(
    state: &AppState,
    auth: &AuthContext,
    application_public_id: &str,
    query: &ApplicationWorkspaceChangesQuery,
) -> Result<ApplicationWorkspaceChangesResponse, Problem> {
    let (_application, sandbox_public_id) =
        application_workspace_sandbox(state, auth, application_public_id).await?;

    collect_workspace_changes(state, auth, &sandbox_public_id, query).await
}

async fn collect_workspace_changes(
    state: &AppState,
    auth: &AuthContext,
    sandbox_public_id: &str,
    query: &ApplicationWorkspaceChangesQuery,
) -> Result<ApplicationWorkspaceChangesResponse, Problem> {
    let repositories = application_workspace_repositories(state, auth, sandbox_public_id).await?;
    let nested_repositories = repositories
        .iter()
        .filter(|repository| !repository.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut changes = Vec::new();
    let mut branch = None;
    let mut head = None;
    let mut files_truncated = false;
    let mut changes_truncated = false;

    for repository in repositories {
        let files_result = run_workspace_command(
            state,
            auth,
            sandbox_public_id,
            bounded_workspace_git_command(
                &repository,
                "git -C \"$1\" ls-files -co --exclude-standard -z | head -c 262144",
            ),
        )
        .await?;
        let status_result = run_workspace_command(
            state,
            auth,
            sandbox_public_id,
            bounded_workspace_git_command(
                &repository,
                "git -C \"$1\" status --porcelain=v1 -z --untracked-files=all | head -c 262144",
            ),
        )
        .await?;
        if files_result.exit_code != 0 || status_result.exit_code != 0 {
            return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Git Workspace Inspection Failed")
                .with_detail("Git could not inspect this persistent workspace."));
        }
        let branch_result = run_workspace_command(
            state,
            auth,
            sandbox_public_id,
            workspace_git_command(&repository, &["branch", "--show-current"]),
        )
        .await?;
        let head_result = run_workspace_command(
            state,
            auth,
            sandbox_public_id,
            workspace_git_command(&repository, &["rev-parse", "--short=12", "HEAD"]),
        )
        .await?;

        let is_nested_boundary = |path: &str| {
            repository.is_empty()
                && nested_repositories.iter().any(|nested| {
                    path == nested
                        || path == format!("{nested}/")
                        || path.starts_with(&format!("{nested}/"))
                })
        };
        for path in workspace_file_records(&files_result.stdout) {
            if files.len() >= WORKSPACE_MAX_FILES {
                files_truncated = true;
                break;
            }
            if is_sensitive_workspace_path(path) || is_nested_boundary(path) {
                continue;
            }
            files.push(workspace_path(&repository, path));
        }
        for mut change in parse_workspace_status(&status_result.stdout) {
            if changes.len() >= WORKSPACE_MAX_CHANGES {
                changes_truncated = true;
                break;
            }
            if is_nested_boundary(&change.path) {
                continue;
            }
            change.path = workspace_path(&repository, &change.path);
            changes.push(change);
        }
        if head.is_none() && head_result.exit_code == 0 {
            head = Some(head_result.stdout.trim().to_string()).filter(|value| !value.is_empty());
            branch = (branch_result.exit_code == 0)
                .then(|| branch_result.stdout.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        files_truncated |= files_result.stdout.len() >= WORKSPACE_LIST_LIMIT_BYTES;
        changes_truncated |= status_result.stdout.len() >= WORKSPACE_LIST_LIMIT_BYTES;
    }
    files.sort();
    files.dedup();
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    let listed_file_count = files.len();
    let (files, next_cursor) = workspace_file_page(&files, query)?;

    Ok(ApplicationWorkspaceChangesResponse {
        branch,
        head,
        clean: changes.is_empty(),
        truncated: files_truncated || changes_truncated,
        files_truncated,
        changes_truncated,
        listed_file_count,
        next_cursor,
        files,
        changes,
    })
}

#[utoipa::path(
    get, tag = "AI Applications", path = "/ai/applications/{application_public_id}/workspace/diff",
    params(("application_public_id" = String, Path,), ("path" = String, Query,)),
    responses((status = 200, body = ApplicationWorkspaceDiffResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 413), (status = 503), (status = 504)),
    security(("bearer_auth" = []))
)]
pub async fn get_application_workspace_diff(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
    Query(query): Query<ApplicationWorkspaceDiffQuery>,
) -> Result<Json<ApplicationWorkspaceDiffResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    if query.path.is_empty() || is_sensitive_workspace_path(&query.path) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Workspace Path")
            .with_detail("That workspace path cannot be displayed."));
    }
    let response = tokio::time::timeout(
        WORKSPACE_REQUEST_TIMEOUT,
        collect_application_workspace_diff(&state, &auth, &application_public_id, &query),
    )
    .await
    .map_err(|_| {
        error!(
            application_id = application_public_id,
            timeout_seconds = WORKSPACE_REQUEST_TIMEOUT.as_secs(),
            "application workspace diff request timed out"
        );
        problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
            .with_title("Application Workspace Timed Out")
            .with_detail(format!(
                "Workspace diff inspection exceeded the {} second safety limit. Try again or inspect the file in the sandbox.",
                WORKSPACE_REQUEST_TIMEOUT.as_secs()
            ))
    })??;
    Ok(Json(response))
}

#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/workspace/diff",
    operation_id = "get_global_workspace_diff",
    params(("path" = String, Query,)),
    responses((status = 200, body = ApplicationWorkspaceDiffResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 413), (status = 503), (status = 504)),
    security(("bearer_auth" = []))
)]
pub async fn get_global_workspace_diff(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ApplicationWorkspaceDiffQuery>,
) -> Result<Json<ApplicationWorkspaceDiffResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    if query.path.is_empty() || is_sensitive_workspace_path(&query.path) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Workspace Path")
            .with_detail("That workspace path cannot be displayed."));
    }
    let response = tokio::time::timeout(WORKSPACE_REQUEST_TIMEOUT, async {
        let (_, sandbox_public_id) = global_workspace_sandbox(&state, &auth).await?;
        collect_workspace_diff(&state, &auth, &sandbox_public_id, &query).await
    })
    .await
    .map_err(|_| {
        error!(
            user_id = auth.user_id(),
            timeout_seconds = WORKSPACE_REQUEST_TIMEOUT.as_secs(),
            "global workspace diff request timed out"
        );
        problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
            .with_title("Global Workspace Timed Out")
            .with_detail(format!(
                "Workspace diff inspection exceeded the {} second safety limit. Try again or inspect the file in the sandbox.",
                WORKSPACE_REQUEST_TIMEOUT.as_secs()
            ))
    })??;
    Ok(Json(response))
}

async fn collect_application_workspace_diff(
    state: &AppState,
    auth: &AuthContext,
    application_public_id: &str,
    query: &ApplicationWorkspaceDiffQuery,
) -> Result<ApplicationWorkspaceDiffResponse, Problem> {
    let (_application, sandbox_public_id) =
        application_workspace_sandbox(state, auth, application_public_id).await?;

    collect_workspace_diff(state, auth, &sandbox_public_id, query).await
}

async fn collect_workspace_diff(
    state: &AppState,
    auth: &AuthContext,
    sandbox_public_id: &str,
    query: &ApplicationWorkspaceDiffQuery,
) -> Result<ApplicationWorkspaceDiffResponse, Problem> {
    let repositories = application_workspace_repositories(state, auth, sandbox_public_id).await?;
    let mut owner = None;
    for repository in repositories {
        let status_result = run_workspace_command(
            state,
            auth,
            sandbox_public_id,
            bounded_workspace_git_command(
                &repository,
                "git -C \"$1\" status --porcelain=v1 -z --untracked-files=all | head -c 262144",
            ),
        )
        .await?;
        if let Some(change) = parse_workspace_status(&status_result.stdout)
            .into_iter()
            .find(|change| workspace_path(&repository, &change.path) == query.path)
        {
            owner = Some((repository, change));
            break;
        }
    }
    let (repository, change) = owner.ok_or_else(|| {
        problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Workspace Change Not Found")
            .with_detail("That file is not currently changed in this persistent workspace.")
    })?;
    let relative_path = query
        .path
        .strip_prefix(&format!("{repository}/"))
        .unwrap_or(&query.path)
        .to_string();

    let current_size = run_workspace_command(
        state,
        auth,
        sandbox_public_id,
        vec![
            "stat".into(),
            "-c".into(),
            "%s".into(),
            "--".into(),
            query.path.clone(),
        ],
    )
    .await?;
    let head_object = format!("HEAD:{relative_path}");
    let head_size = run_workspace_command(
        state,
        auth,
        sandbox_public_id,
        workspace_git_command(&repository, &["cat-file", "-s", &head_object]),
    )
    .await?;
    let oversized = [&current_size, &head_size].into_iter().any(|result| {
        result.exit_code == 0
            && result
                .stdout
                .trim()
                .parse::<u64>()
                .is_ok_and(|size| size > WORKSPACE_MAX_DIFF_FILE_BYTES)
    });
    if oversized {
        return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
            .with_title("Workspace Diff Too Large")
            .with_detail(
                "This file is too large to render safely. Inspect it in the sandbox terminal.",
            ));
    }

    let mut diff = String::new();
    for cached in [true, false] {
        let mut arguments = vec![
            "--no-pager",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--unified=3",
        ];
        if cached {
            arguments.push("--cached");
        }
        arguments.extend(["--", relative_path.as_str()]);
        let cmd = workspace_git_command(&repository, &arguments);
        let result = run_workspace_command(state, auth, sandbox_public_id, cmd).await?;
        if result.exit_code == 0 && !result.stdout.is_empty() {
            diff.push_str(&result.stdout);
        }
    }
    if change.status.as_deref() == Some("untracked") {
        let result = run_workspace_command(
            state,
            auth,
            sandbox_public_id,
            vec![
                "git".into(),
                "-C".into(),
                if repository.is_empty() {
                    ".".into()
                } else {
                    repository.clone()
                },
                "--no-pager".into(),
                "diff".into(),
                "--no-index".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--".into(),
                "/dev/null".into(),
                relative_path.clone(),
            ],
        )
        .await?;
        if matches!(result.exit_code, 0 | 1) {
            diff = result.stdout;
        }
    }
    let (diff, truncated) =
        truncate_workspace_text(redact_workspace_diff(&diff), WORKSPACE_DIFF_LIMIT_BYTES);
    Ok(ApplicationWorkspaceDiffResponse {
        path: query.path.clone(),
        diff,
        truncated,
    })
}
