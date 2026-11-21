// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Install an operator-selected executable. Multipart filenames are never used.
use super::*;
use crate::install::{self, LocalFileReceipt, LocalFileSource, MAX_BINARY_BYTES};
use axum::extract::{Multipart, Query};
use axum::http::HeaderMap;
use tokio::io::AsyncWriteExt;

#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(super) struct FileIdentity {
    name: String,
    version: String,
}

#[derive(ToSchema)]
pub(super) struct FileUpload {
    #[allow(
        dead_code,
        reason = "Describes the streamed multipart field in OpenAPI"
    )]
    #[schema(value_type = String, format = Binary)]
    file: String,
}

fn upload_problem(status: StatusCode, detail: &str) -> Problem {
    temps_core::problemdetails::new(status)
        .with_title("Plugin file installation")
        .with_detail(detail)
}

async fn receive_file(mut multipart: Multipart) -> Result<tempfile::TempPath, Problem> {
    let malformed = |err: axum::extract::multipart::MultipartError| {
        upload_problem(err.status(), "Invalid or oversized multipart upload")
    };
    let mut field = multipart
        .next_field()
        .await
        .map_err(malformed)?
        .ok_or_else(|| upload_problem(StatusCode::BAD_REQUEST, "Choose a plugin executable"))?;
    if field.name() != Some("file") {
        return Err(upload_problem(
            StatusCode::BAD_REQUEST,
            "Expected a file field",
        ));
    }
    let disk = |err: std::io::Error| {
        tracing::error!(error = %err, "Could not stage plugin upload");
        upload_problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to stage the plugin upload",
        )
    };
    let (file, path) = tempfile::NamedTempFile::new().map_err(disk)?.into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let mut size = 0u64;
    while let Some(chunk) = field.chunk().await.map_err(malformed)? {
        size += chunk.len() as u64;
        if size > MAX_BINARY_BYTES {
            return Err(upload_problem(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Plugin exceeds 256 MiB",
            ));
        }
        file.write_all(&chunk).await.map_err(disk)?;
    }
    drop(field);
    if size == 0 || multipart.next_field().await.map_err(malformed)?.is_some() {
        return Err(upload_problem(
            StatusCode::BAD_REQUEST,
            "Upload exactly one non-empty executable",
        ));
    }
    file.flush().await.map_err(disk)?;
    Ok(path)
}

#[utoipa::path(post, path = "/x/plugins/install/file", tag = "External Plugins",
    params(FileIdentity, ("X-Temps-Trust-Sha256" = String, Header, description = "Operator approval to execute the file with this SHA-256")),
    request_body(content = FileUpload, content_type = "multipart/form-data"),
    responses((status = 200, body = InstallPluginResponse), (status = 400), (status = 401), (status = 403), (status = 408), (status = 409), (status = 413), (status = 428), (status = 429), (status = 500), (status = 502), (status = 503)),
    security(("bearer_auth" = [])))]
pub(super) async fn install_local_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Query(identity): Query<FileIdentity>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<InstallPluginResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    install::validate_plugin_name(&identity.name)
        .and_then(|()| install::validate_version(&identity.name, &identity.version))
        .map_err(|err| service_problem(&err.into()))?;
    let approved = headers
        .get("X-Temps-Trust-Sha256")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let sha256 =
        install::normalize_digest(&identity.name, &identity.version, approved).map_err(|_| {
            upload_problem(
                StatusCode::BAD_REQUEST,
                "Approve execution by supplying the executable's SHA-256 digest",
            )
        })?;
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::InstallExternalPlugin {
            name: identity.name.clone(),
        },
    )
    .await?;
    if !state.service.manager().config().persistent_installations {
        return Err(service_problem(
            &ExternalPluginsError::PersistentStorageRequired,
        ));
    }
    // Bound temporary disk use and release the slot when the transfer disconnects.
    static UPLOAD: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    let permit = UPLOAD.try_acquire().map_err(|_| {
        upload_problem(
            StatusCode::TOO_MANY_REQUESTS,
            "Another plugin file installation is in progress",
        )
    })?;
    let receipt = LocalFileReceipt {
        source: LocalFileSource::File,
        plugin_name: identity.name,
        version: identity.version,
        platform: install::platform_target().map_err(|err| service_problem(&err.into()))?,
        sha256,
    };
    let audit = ExternalPluginWriteAudit {
        context: audit_context(&auth, &metadata),
        operation: "EXTERNAL_PLUGIN_FILE_APPROVED".into(),
        plugin_name: Some(receipt.plugin_name.clone()),
        version: Some(receipt.version.clone()),
        platform: Some(receipt.platform.clone()),
        sha256: Some(receipt.sha256.clone()),
        signer_key_id: None,
        registry_source: Some("local-file".into()),
        failure: None,
    };
    record_required_audit(&state, &audit).await?;
    let upload = tokio::time::timeout(std::time::Duration::from_secs(300), receive_file(multipart))
        .await
        .unwrap_or_else(|_| {
            Err(upload_problem(
                StatusCode::REQUEST_TIMEOUT,
                "Plugin upload timed out",
            ))
        });
    let path = match upload {
        Ok(path) => path,
        Err(problem) => {
            record_audit(
                &state,
                &ExternalPluginWriteAudit {
                    operation: "EXTERNAL_PLUGIN_INSTALL_FAILED".into(),
                    failure: Some("File upload failed".into()),
                    ..audit
                },
            )
            .await;
            return Err(problem);
        }
    };
    // Once accepted, finish promotion/rollback and audit even if the caller disconnects.
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(err) = state.service.install_file(&receipt, &path).await {
            record_audit(
                &state,
                &ExternalPluginWriteAudit {
                    operation: "EXTERNAL_PLUGIN_INSTALL_FAILED".into(),
                    failure: Some(public_error_detail(&err)),
                    ..audit
                },
            )
            .await;
            return Err(service_problem(&err));
        }
        record_audit(
            &state,
            &ExternalPluginWriteAudit {
                operation: "EXTERNAL_PLUGIN_INSTALLED".into(),
                ..audit
            },
        )
        .await;
        Ok(Json(InstallPluginResponse {
            name: receipt.plugin_name,
            version: receipt.version,
            platform: receipt.platform,
            sha256: receipt.sha256,
            message: "Plugin verified, installed, and started".into(),
        }))
    })
    .await
    .map_err(|_| {
        upload_problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Installation interrupted. Check plugin status before retrying.",
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::FromRequest, http::Request};

    #[tokio::test]
    async fn receive_one_complete_file_without_interpreting_its_filename() {
        let request = Request::builder().header("content-type", "multipart/form-data; boundary=test")
            .body(Body::from("--test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"/unused/path\"\r\n\r\nfixture bytes\r\n--test--\r\n")).expect("request");
        let multipart = Multipart::from_request(request, &())
            .await
            .expect("multipart");
        let path = receive_file(multipart).await.expect("one file");
        assert_eq!(std::fs::read(path).expect("staged bytes"), b"fixture bytes");
    }
}
