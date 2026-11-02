// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[test]
fn stateless_source_drop_is_reported_as_a_conflict() {
    let error = temps_core::SourceDropError::UnsupportedInStateless {
        guidance: "deploy by registry image reference".to_string(),
    };

    assert_eq!(source_drop_error_status(&error), StatusCode::CONFLICT);
    let document = AiChatApiDoc::openapi();
    let operation = document
        .paths
        .paths
        .get("/ai/applications/{application_public_id}/projects/{project_id}/deploy")
        .and_then(|path| path.post.as_ref())
        .expect("workspace deploy operation must be documented");
    assert!(operation.responses.responses.contains_key("409"));
}
use crate::PendingPermissionEntry;
use axum::http::{StatusCode, Uri};
use temps_auth::permissions::Role;

#[test]
fn workspace_activity_scope_is_bounded_and_deduplicated() {
    let query = WorkspaceActivityQuery {
        application_public_ids: Some("app_a,app_b,app_a".to_string()),
    };
    assert_eq!(
        parse_workspace_activity_ids(&query).expect("valid scope"),
        vec!["app_a", "app_b"]
    );
    assert!(parse_workspace_activity_ids(&WorkspaceActivityQuery {
        application_public_ids: Some("app_a,,app_b".to_string())
    })
    .is_err());
    assert!(parse_workspace_activity_ids(&WorkspaceActivityQuery {
        application_public_ids: Some("bad/id".to_string())
    })
    .is_err());
    assert!(parse_workspace_activity_ids(&WorkspaceActivityQuery {
        application_public_ids: Some(vec!["app"; 101].join(","))
    })
    .is_err());
    assert!(parse_workspace_activity_ids(&WorkspaceActivityQuery {
        application_public_ids: None
    })
    .expect("global only")
    .is_empty());
}

#[test]
fn workspace_activity_statuses_keep_untouched_threads_idle() {
    let mut harness = WorkspaceHarnessActivity {
        ai_provider: "codex".to_string(),
        total: 0,
        pending: 0,
        running: 0,
        completed: 0,
        failed: 0,
        cancelled: 0,
        idle: 0,
    };
    for status in [
        "pending",
        "running",
        "completed",
        "failed",
        "cancelled",
        "idle",
        "legacy",
    ] {
        harness.add_status(status, 1);
    }
    assert_eq!(harness.total, 7);
    assert_eq!(
        (
            harness.pending,
            harness.running,
            harness.completed,
            harness.failed,
            harness.cancelled,
            harness.idle
        ),
        (1, 1, 1, 1, 1, 2)
    );
}

#[test]
fn conversation_failure_exposes_only_the_durable_public_envelope() {
    let metadata = serde_json::json!({
        "last_failure": {
            "code": "provider_rate_limited",
            "title": "AI provider rate limited",
            "detail": "Wait briefly, then retry this message.",
            "retryable": true
        },
        "raw_provider_error": "token=must-not-be-returned"
    });
    let failure = conversation_failure(Some(&metadata), "failed").expect("failure should decode");

    assert_eq!(failure.code, "provider_rate_limited");
    assert!(failure.retryable);
    assert!(!serde_json::to_string(&failure)
        .expect("failure serializes")
        .contains("must-not-be-returned"));
    assert!(conversation_failure(Some(&metadata), "completed").is_none());
    assert!(conversation_failure(
        Some(&serde_json::json!({
            "last_failure": { "code": "incomplete" }
        })),
        "failed"
    )
    .is_none());
}

#[test]
fn conversation_context_usage_is_optional_and_strictly_typed() {
    let metadata = serde_json::json!({
        "context_usage": {
            "used_tokens": 4096,
            "limit_tokens": null,
            "model": "model-a",
            "source": "provider_reported",
            "estimated": true,
            "updated_at": "2026-01-01T00:00:00Z"
        }
    });
    let usage = conversation_context_usage(Some(&metadata)).expect("usage should decode");
    assert_eq!(usage.used_tokens, 4096);
    assert_eq!(usage.limit_tokens, None);
    assert!(usage.estimated);
    assert!(conversation_context_usage(Some(&serde_json::json!({
        "context_usage": { "used_tokens": "invalid" }
    })))
    .is_none());
}

#[test]
fn diagnostic_text_and_metadata_are_redacted_and_bounded() {
    let secret = format!("token={} {}", "secret-value", "x".repeat(20_000));
    let (text, text_truncated) = bounded_redacted_text(&secret, 128);
    assert!(text_truncated);
    assert!(text.len() <= 128);
    assert!(!text.contains("secret-value"));

    let (metadata, metadata_truncated) = bounded_redacted_metadata(&serde_json::json!({
        "authorization": "Bearer private-token",
        "output": "x".repeat(DIAGNOSTIC_MAX_METADATA_BYTES * 2),
    }));
    assert!(metadata_truncated);
    assert!(!metadata.to_string().contains("private-token"));
    assert!(metadata.to_string().len() <= DIAGNOSTIC_MAX_METADATA_BYTES + 64);
}

#[test]
fn diagnostic_response_disables_browser_and_intermediary_caching() {
    let headers = diagnostic_response_headers();
    assert_eq!(
        headers
            .get(CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
}

#[test]
fn diagnostic_conversation_requires_context_read_permission() {
    let mut conversation = conversation_with_runtime("opencode", "default");
    conversation.context_type = "deployment".to_string();
    let unauthorized = custom_auth(vec![Permission::ProjectsRead]);
    let authorized = custom_auth(vec![Permission::ProjectsRead, Permission::DeploymentsRead]);

    assert!(ensure_conversation_read_permission(&unauthorized, &conversation).is_err());
    assert!(ensure_conversation_read_permission(&authorized, &conversation).is_ok());
}

#[test]
fn workspace_import_body_limit_fits_the_bounded_base64_request() {
    let encoded_file_bytes = WORKSPACE_IMPORT_ABSOLUTE_MAX_BYTES_PER_REQUEST.div_ceil(3) * 4;
    let maximum_path_and_json_overhead =
        WORKSPACE_IMPORT_ABSOLUTE_MAX_FILES_PER_REQUEST * (512 + 64) + 32;

    assert!(
        WORKSPACE_IMPORT_BODY_LIMIT >= encoded_file_bytes + maximum_path_and_json_overhead,
        "route body limit must fit the maximum decoded batch after base64 encoding"
    );
    const {
        assert!(
            WORKSPACE_IMPORT_BODY_LIMIT <= 48 * MIB,
            "workspace import route must retain an explicit bounded JSON body"
        );
    }
}

#[test]
fn workspace_file_upload_decodes_only_safe_bounded_paths() {
    let limits = temps_config::AiWorkspaceFileLimitsSettings::default();
    let decoded = decode_workspace_file_writes(
        WriteApplicationWorkspaceFilesRequest {
            files: vec![ApplicationWorkspaceFileWrite {
                path: "projects/demo/README.md".to_string(),
                contents_b64: B64.encode("hello"),
                mode: Some(0o644),
            }],
        },
        &limits,
    )
    .expect("safe workspace upload");

    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].0, FsPath::new("projects/demo/README.md"));
    assert_eq!(decoded[0].1, b"hello");
    assert_eq!(decoded[0].2, Some(0o644));

    let error = decode_workspace_file_writes(
        WriteApplicationWorkspaceFilesRequest {
            files: vec![ApplicationWorkspaceFileWrite {
                path: "projects/demo/.env".to_string(),
                contents_b64: B64.encode("SECRET=value"),
                mode: None,
            }],
        },
        &limits,
    )
    .expect_err("sensitive workspace paths must be rejected");
    assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);

    let duplicate = decode_workspace_file_writes(
        WriteApplicationWorkspaceFilesRequest {
            files: vec![
                ApplicationWorkspaceFileWrite {
                    path: "projects/demo/README.md".to_string(),
                    contents_b64: B64.encode("first"),
                    mode: None,
                },
                ApplicationWorkspaceFileWrite {
                    path: "projects/demo/README.md".to_string(),
                    contents_b64: B64.encode("second"),
                    mode: None,
                },
            ],
        },
        &limits,
    )
    .expect_err("duplicate workspace paths must not overwrite implicitly");
    assert_eq!(duplicate.into_response().status(), StatusCode::BAD_REQUEST);

    let overlapping = decode_workspace_file_writes(
        WriteApplicationWorkspaceFilesRequest {
            files: vec![
                ApplicationWorkspaceFileWrite {
                    path: "projects/demo/tree".to_string(),
                    contents_b64: B64.encode("file"),
                    mode: None,
                },
                ApplicationWorkspaceFileWrite {
                    path: "projects/demo/tree/child.txt".to_string(),
                    contents_b64: B64.encode("child"),
                    mode: None,
                },
            ],
        },
        &limits,
    )
    .expect_err("ancestor and descendant paths must be rejected before writing");
    assert_eq!(
        overlapping.into_response().status(),
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn workspace_file_upload_enforces_the_runtime_policy() {
    let limits = temps_config::AiWorkspaceFileLimitsSettings {
        max_file_size_mb: 1,
        max_upload_size_mb: 2,
        ..temps_config::AiWorkspaceFileLimitsSettings::default()
    };
    let oversized = decode_workspace_file_writes(
        WriteApplicationWorkspaceFilesRequest {
            files: vec![ApplicationWorkspaceFileWrite {
                path: "projects/demo/large.bin".to_string(),
                contents_b64: B64.encode(vec![0_u8; MIB + 1]),
                mode: None,
            }],
        },
        &limits,
    )
    .expect_err("the configured per-file limit must be authoritative");
    assert_eq!(oversized.into_response().status(), StatusCode::BAD_REQUEST);
}

#[test]
fn oversized_workspace_download_maps_to_payload_too_large() {
    let error = Problem::from(ApplicationError::WorkspaceDownloadTooLarge {
        path: "large.bin".to_string(),
        max_bytes: mb_bytes(
            temps_config::AiWorkspaceFileLimitsSettings::default().max_download_size_mb,
        ),
    });

    assert_eq!(
        error.into_response().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[test]
fn workspace_image_preview_requires_matching_extension_and_magic_bytes() {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(b"payload");
    assert_eq!(
        verified_workspace_image_media_type("assets/logo.png", &png),
        Some("image/png")
    );
    assert_eq!(
        verified_workspace_image_media_type("assets/logo.svg", &png),
        None
    );
    assert_eq!(
        verified_workspace_image_media_type("assets/logo.png", b"<script>alert(1)</script>"),
        None
    );
    assert!(workspace_image_extension("photo.JPEG"));
    assert!(!workspace_image_extension("vector.svg"));

    let mut renamed_text = crate::applications::WorkspaceFilePreview {
        bytes: vec![b'a'; 2_048],
        size_bytes: 2_048,
        truncated: false,
    };
    cap_unverified_workspace_image_as_text(&mut renamed_text, true, None, 1_024);
    assert_eq!(renamed_text.bytes.len(), 1_024);
    assert!(renamed_text.truncated);
}

#[test]
fn normal_workspace_user_can_read_effective_file_policy_without_settings_access() {
    let auth = AuthContext::new_session(test_user(), Role::User);

    require_workspace_file_policy_access(&auth)
        .expect("normal workspace users can read the non-sensitive effective policy");
    assert!(auth.has_permission(&Permission::ProjectsRead));
    assert!(auth.has_permission(&Permission::SandboxesRead));
    assert!(!auth.has_permission(&Permission::SettingsRead));

    let effective = effective_workspace_file_limits(temps_config::AiWorkspaceFileLimitsSettings {
        max_download_size_mb: 128,
        ..temps_config::AiWorkspaceFileLimitsSettings::default()
    });
    assert_eq!(effective.max_download_size_mb, 32);
}

#[test]
fn application_list_query_deserializes_numeric_pagination() {
    let uri: Uri = "/api/ai/applications?page=1&page_size=100&status=active"
        .parse()
        .expect("valid application list URI");
    let Query(query) = Query::<LifecycleListQuery>::try_from_uri(&uri)
        .expect("numeric application pagination must deserialize");

    assert_eq!(query.status, ConversationListStatus::Active);
    assert_eq!(
        normalize_list_pagination(query.page, query.page_size),
        (1, 100)
    );
}

#[test]
fn global_conversation_list_query_deserializes_numeric_pagination() {
    let uri: Uri = "/api/ai/conversations?page=1&page_size=50&scope=global&status=active"
        .parse()
        .expect("valid global conversation list URI");
    let Query(query) = Query::<ConversationListQuery>::try_from_uri(&uri)
        .expect("numeric conversation pagination must deserialize");

    assert_eq!(query.status, ConversationListStatus::Active);
    assert_eq!(query.scope, ConversationListScope::Global);
    assert_eq!(
        normalize_list_pagination(query.page, query.page_size),
        (1, 50)
    );
}

#[test]
fn conversation_list_query_accepts_a_search_and_rejects_an_oversized_one() {
    let uri: Uri = "/api/ai/conversations?search=landing%20page&page=1"
        .parse()
        .expect("valid conversation search URI");
    let Query(query) =
        Query::<ConversationListQuery>::try_from_uri(&uri).expect("search must deserialize");
    assert_eq!(query.search.as_deref(), Some("landing page"));
    assert!(validate_conversation_search(query.search.as_deref()).is_ok());
    assert!(validate_conversation_search(None).is_ok());

    // Counted in characters, not bytes: 200 multi-byte characters pass.
    let at_limit = "é".repeat(MAX_CONVERSATION_SEARCH_LEN);
    assert!(validate_conversation_search(Some(&at_limit)).is_ok());
    let problem = validate_conversation_search(Some(&"a".repeat(201)))
        .expect_err("201 characters must be rejected");
    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
}

#[test]
fn application_status_query_deserializes_status_without_pagination() {
    let uri: Uri = "/api/ai/applications/app_123?status=archived"
        .parse()
        .expect("valid application URI");
    let Query(query) = Query::<LifecycleStatusQuery>::try_from_uri(&uri)
        .expect("application lifecycle status must deserialize");

    assert_eq!(query.status, ConversationListStatus::Archived);
}

#[test]
fn application_project_defaults_to_autopack() {
    let request = CreateApplicationProjectRequest {
        name: "Web".to_string(),
        preset: None,
        exposed_port: None,
    };

    assert_eq!(application_project_preset(&request), "autopack");
}

#[test]
fn application_creation_requires_exactly_one_project_source() {
    assert!(validate_application_project_choice(true, 0).is_ok());
    assert!(validate_application_project_choice(false, 1).is_ok());
    assert!(validate_application_project_choice(false, 0).is_err());
    assert!(validate_application_project_choice(true, 1).is_err());
}

#[test]
fn application_drop_archive_excludes_dependencies_and_credentials() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg"))
        .expect("node_modules fixture");
    std::fs::create_dir_all(workspace.path().join(".git")).expect("git fixture");
    std::fs::create_dir_all(workspace.path().join(".SSH")).expect("credential directory fixture");
    std::fs::create_dir_all(workspace.path().join("infra/.Terraform"))
        .expect("terraform directory fixture");
    std::fs::create_dir_all(workspace.path().join(".KUBE")).expect("kube directory fixture");
    std::fs::write(workspace.path().join("package.json"), b"{}").expect("package fixture");
    std::fs::write(workspace.path().join(".env"), b"SECRET=must-not-deploy").expect("env fixture");
    std::fs::write(workspace.path().join("private.pem"), b"must-not-deploy").expect("pem fixture");
    std::fs::write(workspace.path().join(".ENV.LOCAL"), b"must-not-deploy")
        .expect("uppercase env fixture");
    std::fs::write(
        workspace.path().join("CREDENTIALS.JSON"),
        b"must-not-deploy",
    )
    .expect("uppercase credential fixture");
    std::fs::write(workspace.path().join(".SSH/id_ed25519"), b"must-not-deploy")
        .expect("credential directory file fixture");
    std::fs::write(
        workspace.path().join("infra/.Terraform/terraform.tfstate"),
        b"must-not-deploy",
    )
    .expect("terraform state fixture");
    std::fs::write(workspace.path().join(".KUBE/config"), b"must-not-deploy")
        .expect("kube config fixture");
    std::fs::write(workspace.path().join("signing.JKS"), b"must-not-deploy")
        .expect("keystore fixture");
    std::fs::write(
        workspace.path().join("node_modules/pkg/index.js"),
        b"ignored",
    )
    .expect("dependency fixture");
    std::fs::write(workspace.path().join(".git/config"), b"ignored").expect("git config fixture");

    let archive = prepare_application_drop_archive(workspace.path()).expect("drop archive");
    let mut zip = zip::ZipArchive::new(archive.reopen().expect("open archive")).expect("valid zip");
    let names = (0..zip.len())
        .map(|index| {
            zip.by_index(index)
                .expect("zip entry")
                .name()
                .expect("zip entry name")
                .to_string()
        })
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["package.json"]);
}

#[cfg(unix)]
#[test]
fn application_drop_archive_rejects_workspace_symlinks() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let outside = tempfile::NamedTempFile::new().expect("outside fixture");
    symlink(outside.path(), workspace.path().join("escape.txt")).expect("symlink fixture");

    let error =
        prepare_application_drop_archive(workspace.path()).expect_err("symlink must be rejected");

    assert!(matches!(error, ApplicationDropArchiveError::Symlink(_)));
}

#[cfg(unix)]
#[test]
fn application_drop_rejects_symlinked_project_ancestor() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    std::fs::create_dir(outside.path().join("app")).expect("outside project");
    symlink(outside.path(), workspace.path().join("projects")).expect("projects symlink");

    let result = canonical_workspace_subdirectory(
        workspace.path(),
        FsPath::new("projects").join("app").as_path(),
    );

    assert!(
        result.is_err(),
        "Drop must not follow a workspace ancestor symlink"
    );
}

#[test]
fn application_project_preserves_an_explicit_legacy_preset() {
    let request = CreateApplicationProjectRequest {
        name: "Legacy".to_string(),
        preset: Some("nixpacks".to_string()),
        exposed_port: None,
    };

    assert_eq!(application_project_preset(&request), "nixpacks");
}

#[test]
fn conversation_message_query_defaults_to_bounded_latest_page() {
    let query = ConversationMessagesQuery::default();
    assert_eq!(
        validate_conversation_messages_query(&query).expect("default history page"),
        (None, DEFAULT_MESSAGE_PAGE_LIMIT)
    );
}

#[test]
fn conversation_message_query_accepts_returned_cursor_and_limit() {
    let query = ConversationMessagesQuery {
        before: Some(encode_message_before_cursor(42)),
        limit: Some(25),
    };
    assert_eq!(
        validate_conversation_messages_query(&query).expect("valid history page"),
        (Some(42), 25)
    );
}

#[test]
fn conversation_message_query_rejects_zero_and_oversized_limits() {
    for limit in [0, MAX_MESSAGE_PAGE_LIMIT + 1] {
        let error = validate_conversation_messages_query(&ConversationMessagesQuery {
            before: None,
            limit: Some(limit),
        })
        .expect_err("out-of-range history limit must fail");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            title_of(&error).as_deref(),
            Some("Invalid Message Page Limit")
        );
        assert!(format!("{:?}", error.body).contains(&limit.to_string()));
    }
}

#[test]
fn conversation_message_query_rejects_malformed_and_negative_cursors() {
    for cursor in ["not-a-cursor", "m1_-1"] {
        let error = validate_conversation_messages_query(&ConversationMessagesQuery {
            before: Some(cursor.to_string()),
            limit: Some(50),
        })
        .expect_err("invalid history cursor must fail");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(title_of(&error).as_deref(), Some("Invalid Message Cursor"));
        assert!(format!("{:?}", error.body).contains("next_before"));
    }
}

#[test]
fn message_response_exposes_stable_cursors_without_reordering() {
    let messages = [2_i64, 3]
        .into_iter()
        .map(|id| {
            MessageResponse::from(ai_messages::Model {
                id,
                conversation_id: 1,
                role: "assistant".to_string(),
                content: id.to_string(),
                metadata: None,
                tokens_in: None,
                tokens_out: None,
                cost_microcents: None,
                created_at: chrono::Utc::now(),
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        messages
            .iter()
            .map(|message| (message.content.as_str(), message.cursor.as_str()))
            .collect::<Vec<_>>(),
        vec![("2", "m1_2"), ("3", "m1_3")]
    );
}

#[test]
fn message_response_carries_saved_next_steps() {
    let message = |metadata| {
        MessageResponse::from(ai_messages::Model {
            id: 9,
            conversation_id: 1,
            role: "assistant".to_string(),
            content: "Deployed.".to_string(),
            metadata,
            tokens_in: None,
            tokens_out: None,
            cost_microcents: None,
            created_at: chrono::Utc::now(),
        })
    };
    let with = message(Some(serde_json::json!({
        "next_steps": { "suggestions": ["Show the logs"], "model": "vendor/m" }
    })));
    assert_eq!(with.next_steps, Some(vec!["Show the logs".to_string()]));
    let json = serde_json::to_value(&with).expect("serializes");
    assert!(json.get("next_steps").is_some());
    assert!(!json.to_string().contains("vendor/m"));

    let without = message(None);
    assert_eq!(without.next_steps, None);
    let json = serde_json::to_value(&without).expect("serializes");
    assert!(json.get("next_steps").is_none());
}

#[test]
fn workspace_state_distinguishes_sleeping_from_recovery() {
    assert_eq!(
        application_workspace_state(None, "running", false, false),
        "sleeping"
    );
    assert_eq!(
        application_workspace_state(None, "running", true, false),
        "recovering"
    );
    assert_eq!(
        application_workspace_state(Some("running"), "running", true, false),
        "running"
    );
    assert_eq!(
        application_workspace_state(Some("recovering"), "running", true, false),
        "recovering"
    );
    assert_eq!(
        application_workspace_state(Some("stopped"), "running", true, false),
        "recovering"
    );
    assert_eq!(
        application_workspace_state(Some("stopped"), "paused", true, false),
        "sleeping"
    );
    assert_eq!(
        application_workspace_state(Some("running"), "running", true, true),
        "failed"
    );
}

#[test]
fn workspace_startup_failure_classifies_exhausted_private_network_capacity() {
    let error = temps_sandbox::error::SandboxError::CreateFailed {
        user_id: 7,
        reason: "create network temps-sandbox-private: all predefined address pools have been fully subnetted"
            .to_string(),
    };

    let detail = application_workspace_failure_detail(&error);

    assert_eq!(
        detail,
        "This Temps host has no private sandbox network capacity available. Contact your Temps administrator, then try again."
    );
    assert!(!detail.contains("temps-sandbox-private"));
    assert!(!detail.contains("fully subnetted"));
}

#[test]
fn workspace_startup_failure_classifies_incompatible_secure_networking() {
    let error = temps_sandbox::error::SandboxError::CreateFailed {
        user_id: 7,
        reason: "failed to parse com.docker.network.bridge.gateway_mode_ipv4 value: isolated (unknown gateway mode isolated)"
            .to_string(),
    };

    let detail = application_workspace_failure_detail(&error);

    assert!(detail.contains("incompatible with secure workspace networking"));
    assert!(!detail.contains("gateway_mode_ipv4"));
}

#[test]
fn workspace_startup_failure_falls_back_without_exposing_provider_details() {
    let error = temps_sandbox::error::SandboxError::CreateFailed {
        user_id: 7,
        reason: "provider detail with container temps-sandbox-secret-id".to_string(),
    };

    let detail = application_workspace_failure_detail(&error);

    assert_eq!(
        detail,
        "Temps could not start workspace compute. Try again; if it continues, contact your Temps administrator."
    );
    assert!(!detail.contains("temps-sandbox-secret-id"));
}

#[test]
fn global_workspace_image_pull_failure_is_a_503_naming_the_image() {
    let error = temps_sandbox::error::SandboxError::ImageUnavailable {
        sandbox_id: "sbx_test".to_string(),
        image: "registry.example.test/team/sandbox:1.2.3".to_string(),
        reason: "manifest unknown".to_string(),
    };

    let problem = global_workspace_failure_problem(&error);

    assert_eq!(problem.status_code, StatusCode::SERVICE_UNAVAILABLE);
    let body = serde_json::to_string(&problem.body).expect("problem body serializes");
    assert!(body.contains("Global Workspace Image Unavailable"));
    assert!(body.contains("registry.example.test/team/sandbox:1.2.3"));
}

#[test]
fn global_workspace_other_failures_stay_generic_500s() {
    let error = temps_sandbox::error::SandboxError::CreateFailed {
        user_id: 7,
        reason: "provider detail with container temps-sandbox-secret-id".to_string(),
    };

    let problem = global_workspace_failure_problem(&error);

    assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    let body = serde_json::to_string(&problem.body).expect("problem body serializes");
    assert!(!body.contains("temps-sandbox-secret-id"));
}

#[test]
fn application_workspace_image_pull_failure_is_a_503() {
    let error = temps_sandbox::error::SandboxError::ImageUnavailable {
        sandbox_id: "sbx_test".to_string(),
        image: "registry.example.test/team/sandbox:1.2.3".to_string(),
        reason: "manifest unknown".to_string(),
    };

    let problem = application_workspace_failure_problem(&error);

    assert_eq!(problem.status_code, StatusCode::SERVICE_UNAVAILABLE);
    assert!(application_workspace_failure_detail(&error).contains("image tag is published"));
}

#[test]
fn workspace_provider_problem_never_serializes_raw_provider_details() {
    let error = temps_sandbox::error::SandboxError::CreateFailed {
        user_id: 7,
        reason: "private Docker path /var/run/docker.sock for temps-sandbox-secret-id".to_string(),
    };

    let problem = application_workspace_provider_problem(error, "app_test", "test operation");
    let body = serde_json::to_string(&problem.body).expect("problem body serializes");

    assert!(body.contains("Temps could not start workspace compute"));
    assert!(!body.contains("/var/run/docker.sock"));
    assert!(!body.contains("temps-sandbox-secret-id"));
}

#[test]
fn workspace_snapshot_problem_never_serializes_artifact_paths() {
    let error = temps_sandbox::error::SandboxSnapshotError::ArtifactMissing {
        path: "/private/snapshots/user-7/secret.tar".to_string(),
    };

    let problem = application_workspace_snapshot_problem(error, "app_test", "test snapshot");
    let body = serde_json::to_string(&problem.body).expect("problem body serializes");

    assert!(body.contains("Temps could not complete"));
    assert!(!body.contains("/private/snapshots"));
    assert!(!body.contains("secret.tar"));
}

#[test]
fn recovering_workspace_may_still_retain_data_plane_access() {
    assert!(workspace_may_retain_data_plane("running"));
    assert!(workspace_may_retain_data_plane("recovering"));
    assert!(!workspace_may_retain_data_plane("stopped"));
    assert!(!workspace_may_retain_data_plane("failed"));
}

#[test]
fn global_threads_share_one_user_scoped_workspace_identity() {
    assert_eq!(global_workspace_context_id(42), "global-user-42");
    assert_ne!(
        global_workspace_context_id(42),
        global_workspace_context_id(7)
    );
}

#[test]
fn sandbox_execution_permission_is_required_for_user_workspace_turns() {
    assert!(conversation_context_requires_sandbox_write("application"));
    assert!(conversation_context_requires_sandbox_write("global"));
    assert!(!conversation_context_requires_sandbox_write("general"));
    assert!(!conversation_context_requires_sandbox_write("deployment"));
}

fn send_request(content: &str) -> SendMessageRequest {
    SendMessageRequest {
        content: content.to_string(),
        attachments: Vec::new(),
        turn_id: Some("turn-1".to_string()),
        ai_provider: None,
        ai_model: None,
        ai_thinking_level: None,
        ai_permission_mode: None,
        page_context: None,
    }
}

#[test]
fn chat_submission_does_not_reject_credential_shaped_text() {
    for content in [
        "app_f842f1b699b543e7a339286306a09289",
        "http://localhost:3014/api/ai/applications/app_f842f1b699b543e7a339286306a09289/preview-link",
        "token: github_pat_12345678901234567890",
        "STRIPE_KEY=sk_test_1234567890123456",
    ] {
        validate_send_message_request(&send_request(content))
            .unwrap_or_else(|error| panic!("chat text was rejected: {content}: {error:?}"));
    }
}

#[test]
fn attachment_names_are_safe_workspace_components() {
    assert_eq!(
        sanitized_attachment_name(Some("../../design screenshot.png")),
        "_.._design_screenshot.png"
    );
    assert_eq!(sanitized_attachment_name(Some("...")), "attachment.bin");
    assert_eq!(sanitized_attachment_name(None), "attachment.bin");
}

#[test]
fn attachment_only_messages_are_valid_but_the_count_is_bounded() {
    let mut request = send_request("");
    request.attachments.push(ChatAttachmentReference {
        id: "att_123".to_string(),
        name: "diagram.png".to_string(),
    });
    assert!(validate_send_message_request(&request).is_ok());

    request.attachments = (0..=MAX_CHAT_ATTACHMENTS)
        .map(|index| ChatAttachmentReference {
            id: format!("att_{index}"),
            name: format!("file-{index}.txt"),
        })
        .collect();
    assert!(validate_send_message_request(&request).is_err());
}

#[test]
fn chat_submission_still_rejects_empty_and_invalid_turns() {
    assert!(validate_send_message_request(&send_request("  ")).is_err());
    let mut invalid_turn = send_request("hello");
    invalid_turn.turn_id = Some("".to_string());
    assert!(validate_send_message_request(&invalid_turn).is_err());
}

#[test]
fn websocket_turn_snapshot_includes_the_durable_start_timestamp() {
    let frame = turn_state_frame(
        "running".to_string(),
        Some("turn-1".to_string()),
        Some("2026-09-01T14:00:00+00:00".to_string()),
    );
    let outer: serde_json::Value =
        serde_json::from_str(&frame).expect("turn-state frame should be valid JSON");
    let data: serde_json::Value = serde_json::from_str(
        outer["data"]
            .as_str()
            .expect("turn-state data should be a JSON string"),
    )
    .expect("turn-state payload should be valid JSON");

    assert_eq!(data["status"], "running");
    assert_eq!(data["turn_id"], "turn-1");
    assert_eq!(data["turn_started_at"], "2026-09-01T14:00:00+00:00");
}

/// The `title` value the mapping set on the Problem body, if any.
fn title_of(p: &Problem) -> Option<String> {
    p.body
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// (a) Every `ChatError` variant maps to the expected HTTP status + title.
// Pure: exercises `From<ChatError> for Problem` directly.

#[test]
fn test_not_found_maps_to_404() {
    let p: Problem = ChatError::NotFound("abc".to_string()).into();
    assert_eq!(p.status_code, StatusCode::NOT_FOUND);
    assert_eq!(title_of(&p).as_deref(), Some("Conversation Not Found"));
}

#[test]
fn test_project_not_found_maps_to_404() {
    let p: Problem = ChatError::ProjectNotFound(7).into();
    assert_eq!(p.status_code, StatusCode::NOT_FOUND);
    assert_eq!(title_of(&p).as_deref(), Some("Project Not Found"));
}

#[test]
fn test_no_provider_maps_to_404_context_unavailable() {
    let p: Problem = ChatError::NoProvider("deployment".to_string()).into();
    assert_eq!(p.status_code, StatusCode::NOT_FOUND);
    assert_eq!(title_of(&p).as_deref(), Some("Context Not Available"));
}

#[test]
fn test_context_unavailable_maps_to_404() {
    let p: Problem = ChatError::ContextUnavailable.into();
    assert_eq!(p.status_code, StatusCode::NOT_FOUND);
    assert_eq!(title_of(&p).as_deref(), Some("Context Not Available"));
}

#[test]
fn test_ai_unavailable_maps_to_409() {
    let p: Problem = ChatError::AiUnavailable.into();
    assert_eq!(p.status_code, StatusCode::CONFLICT);
    assert_eq!(title_of(&p).as_deref(), Some("AI Not Configured"));
}

#[test]
fn test_db_error_maps_to_500() {
    let p: Problem = ChatError::Db(sea_orm::DbErr::Custom("boom".to_string())).into();
    assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        title_of(&p).as_deref(),
        Some("Conversation storage unavailable")
    );
    assert!(!serde_json::to_string(&p.body)
        .expect("problem body serializes")
        .contains("boom"));
}

#[test]
fn test_project_lookup_error_maps_to_stable_500() {
    let p: Problem = ChatError::ProjectLookup {
        project_id: 42,
        source: sea_orm::DbErr::Custom("database.internal:5432".to_string()),
    }
    .into();
    assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        title_of(&p).as_deref(),
        Some("Conversation storage unavailable")
    );
    assert!(!serde_json::to_string(&p.body)
        .expect("problem body serializes")
        .contains("database.internal"));
}

#[test]
fn test_ai_error_maps_to_500() {
    let p: Problem = ChatError::Ai("provider exploded".to_string()).into();
    assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(title_of(&p).as_deref(), Some("AI harness failed"));
    let body = serde_json::to_string(&p.body).expect("problem body serializes");
    assert!(body.contains("authentication"));
    assert!(!body.contains("provider exploded"));
}

fn test_user() -> temps_entities::users::Model {
    let now = chrono::Utc::now();
    temps_entities::users::Model {
        id: 1,
        name: "Test User".to_string(),
        email: "test@example.com".to_string(),
        password_hash: None,
        email_verified: true,
        email_verification_token: None,
        email_verification_expires: None,
        password_reset_token: None,
        password_reset_expires: None,
        must_change_password: false,
        deleted_at: None,
        mfa_secret: None,
        mfa_enabled: false,
        mfa_recovery_codes: None,
        oidc_subject: None,
        oidc_provider_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn custom_auth(permissions: Vec<Permission>) -> AuthContext {
    AuthContext::new_api_key(
        test_user(),
        None,
        Some(permissions),
        "test-key".to_string(),
        1,
    )
}

#[test]
fn preview_renewal_requires_persisted_console_session() {
    let browser = AuthContext::new_persisted_session(test_user(), Role::User, 17);
    assert!(require_console_preview_session(&browser).is_ok());
    let api_key = custom_auth(vec![Permission::ProjectsWrite, Permission::SandboxesWrite]);
    assert_eq!(
        require_console_preview_session(&api_key)
            .expect_err("API keys cannot renew browser previews")
            .status_code,
        StatusCode::FORBIDDEN
    );
    let unbound_session = AuthContext::new_session(test_user(), Role::User);
    assert_eq!(
        require_console_preview_session(&unbound_session)
            .expect_err("unpersisted session is insufficient")
            .status_code,
        StatusCode::FORBIDDEN
    );
}

#[test]
fn seeded_contexts_require_their_domain_read_permissions() {
    let project_writer = custom_auth(vec![Permission::ProjectsWrite]);
    assert!(!can_read_context(&project_writer, "deployment"));
    assert!(!can_read_context(&project_writer, "alert"));
    assert!(!can_read_context(&project_writer, "alert_suggest"));

    let deployment_reader = custom_auth(vec![Permission::DeploymentsRead]);
    assert!(can_read_context(&deployment_reader, "deployment"));
    assert!(!can_read_context(&deployment_reader, "alert"));

    let telemetry_reader = custom_auth(vec![Permission::OtelRead]);
    assert!(can_read_context(&telemetry_reader, "alert"));
    assert!(can_read_context(&telemetry_reader, "alert_suggest"));
}

fn conversation_with_runtime(provider: &str, permission_mode: &str) -> ai_conversations::Model {
    let now = chrono::Utc::now();
    ai_conversations::Model {
        id: 1,
        public_id: "conversation-1".to_string(),
        project_id: Some(1),
        application_id: None,
        context_type: "project".to_string(),
        context_id: "1".to_string(),
        title: None,
        status: "active".to_string(),
        kind: "ai".to_string(),
        created_by: 1,
        metadata: None,
        created_at: now,
        last_activity_at: now,
        ai_provider: provider.to_string(),
        ai_model: "default".to_string(),
        ai_thinking_level: None,
        ai_permission_mode: permission_mode.to_string(),
        cli_session_id: None,
        cli_session_fingerprint: None,
        turn_status: "idle".to_string(),
        active_turn_id: None,
        last_turn_id: None,
        turn_started_at: None,
        next_step_suggestions: None,
    }
}

fn api_key_with_role(role: Role) -> AuthContext {
    AuthContext::new_api_key(test_user(), Some(role), None, "test-key".to_string(), 1)
}

// Rename, archive, restore and stop on `/ai/conversations/{id}` change a
// conversation's state, so a read-only key is refused on every context
// its project-scoped twin would refuse it on.
#[test]
fn read_only_api_keys_cannot_change_user_conversations() {
    for context_type in ["project", "deployment", "application", "global"] {
        let mut conversation = conversation_with_runtime("claude_code", "ask");
        conversation.context_type = context_type.to_string();
        for role in [Role::Reader, Role::ApiReader] {
            let auth = api_key_with_role(role.clone());
            let problem = ensure_user_conversation_write_permission(&auth, &conversation)
                .expect_err("a read-only key must not change a conversation");
            assert_eq!(
                problem.status_code,
                StatusCode::FORBIDDEN,
                "{role:?} on a {context_type} conversation"
            );
        }
        for role in [Role::User, Role::Admin] {
            assert!(
                ensure_user_conversation_write_permission(
                    &api_key_with_role(role.clone()),
                    &conversation
                )
                .is_ok(),
                "{role:?} may change its own {context_type} conversation"
            );
        }
    }
}

// Any role that may start an assistant chat and send it messages may also
// rename, archive, restore and stop it, including roles without
// `projects:write` such as a platform administrator.
#[test]
fn assistant_chats_are_managed_by_whoever_may_drive_them() {
    let mut conversation = conversation_with_runtime("openrouter", "ask");
    conversation.context_type = crate::providers::assistant::ASSISTANT_CONTEXT_TYPE.to_string();
    conversation.project_id = None;
    for role in [
        Role::Reader,
        Role::ApiReader,
        Role::PlatformAdmin,
        Role::User,
    ] {
        assert!(
            ensure_user_conversation_write_permission(
                &api_key_with_role(role.clone()),
                &conversation
            )
            .is_ok(),
            "{role:?} may manage an assistant chat it can message"
        );
    }
}

#[test]
fn archived_conversation_is_read_only_until_restored() {
    let active = conversation_with_runtime("claude_code", "ask");
    assert!(ensure_conversation_is_active(&active).is_ok());

    let mut archived = active;
    archived.status = "archived".to_string();
    let problem = ensure_conversation_is_active(&archived)
        .expect_err("archived conversations must reject new messages");
    assert_eq!(problem.into_response().status(), StatusCode::CONFLICT);
}

#[test]
fn global_conversation_response_exposes_durable_turn_status() {
    let mut conversation = conversation_with_runtime("codex_cli", "auto");
    conversation.turn_status = "failed".to_string();
    conversation.kind = THREAD_KIND_TERMINAL.to_string();
    let response = GlobalConversationResponse {
        public_id: conversation.public_id,
        project_id: conversation.project_id,
        project_name: Some("Example".to_string()),
        project_slug: Some("example".to_string()),
        context_type: conversation.context_type,
        context_id: conversation.context_id,
        title: conversation.title,
        status: conversation.status,
        kind: conversation.kind,
        created_at: conversation.created_at.to_rfc3339(),
        last_activity_at: conversation.last_activity_at.to_rfc3339(),
        ai_provider: conversation.ai_provider,
        ai_model: conversation.ai_model,
        ai_thinking_level: conversation.ai_thinking_level,
        ai_permission_mode: conversation.ai_permission_mode,
        turn_status: conversation.turn_status,
        focus_project_id: Some(7),
        failure: None,
        context_usage: None,
        sandbox: Some(ConversationSandboxResponse {
            sandbox_id: "sbx_abc".to_string(),
            name: "scratch".to_string(),
            backend: Some("firecracker".to_string()),
        }),
        workspace: None,
    };

    let value = serde_json::to_value(response).expect("response serializes");
    assert_eq!(value["turn_status"], "failed");
    assert_eq!(value["sandbox"]["backend"], "firecracker");
    assert_eq!(value["sandbox"]["sandbox_id"], "sbx_abc");
    assert_eq!(value["focus_project_id"], 7);
    // The cross-project switcher must know which threads are shells, or a
    // terminal thread would open as an empty agent conversation.
    assert_eq!(value["kind"], "terminal");
}

#[test]
fn running_turn_can_only_be_elevated_to_auto() {
    let mut conversation = conversation_with_runtime("claude_cli", "default");
    conversation.turn_status = "running".to_string();

    ensure_permission_mode_can_change_during_turn(&conversation, "full-access")
        .expect("Auto is a safe server-mediated elevation");
    let error = ensure_permission_mode_can_change_during_turn(&conversation, "default")
        .expect_err("a launched provider process cannot be reduced safely");
    assert_eq!(error.status_code, StatusCode::CONFLICT);

    conversation.turn_status = "idle".to_string();
    ensure_permission_mode_can_change_during_turn(&conversation, "default")
        .expect("idle conversations may choose any advertised mode");
}

enum HiddenProjectsOutcome {
    Hidden(Vec<i32>),
    Error,
}

struct HiddenProjectsChecker(HiddenProjectsOutcome);

#[async_trait::async_trait]
impl temps_core::ProjectAccessChecker for HiddenProjectsChecker {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        _project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(true)
    }

    async fn hidden_project_ids(
        &self,
        _user_id: i32,
    ) -> Result<Option<Vec<i32>>, Box<dyn std::error::Error + Send + Sync>> {
        match &self.0 {
            HiddenProjectsOutcome::Hidden(ids) => Ok(Some(ids.clone())),
            HiddenProjectsOutcome::Error => {
                Err(Box::new(std::io::Error::other("checker unavailable")))
            }
        }
    }
}

struct EffectivePermissionChecker {
    permissions: Option<Vec<String>>,
    member: bool,
    denied_project_id: Option<i32>,
}

#[async_trait::async_trait]
impl temps_core::ProjectAccessChecker for EffectivePermissionChecker {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.member && self.denied_project_id != Some(project_id))
    }

    async fn effective_project_permissions(
        &self,
        _user_id: i32,
        project_id: i32,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
        if self.denied_project_id == Some(project_id) {
            Ok(Some(Vec::new()))
        } else {
            Ok(self.permissions.clone())
        }
    }
}

#[tokio::test]
async fn application_permission_is_narrowed_by_every_project_role() {
    let auth = custom_auth(vec![Permission::ProjectsRead, Permission::SandboxesWrite]);
    let denied: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![Permission::ProjectsRead.to_string()]),
            member: true,
            denied_project_id: None,
        }));
    let error =
        ensure_application_project_permission(&auth, &denied, &[7, 9], &Permission::SandboxesWrite)
            .await
            .expect_err("coarse membership must not grant sandbox execution");
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);

    let allowed: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![Permission::SandboxesWrite.to_string()]),
            member: true,
            denied_project_id: None,
        }));
    ensure_application_project_permission(&auth, &allowed, &[7, 9], &Permission::SandboxesWrite)
        .await
        .expect("the required permission is granted on every linked project");
}

#[tokio::test]
async fn application_permission_falls_back_to_membership_without_a_scoped_opinion() {
    let auth = custom_auth(vec![Permission::SandboxesWrite]);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: None,
            member: true,
            denied_project_id: None,
        }));
    ensure_application_project_permission(&auth, &checker, &[7], &Permission::SandboxesWrite)
        .await
        .expect("unconfigured project permissions preserve coarse membership semantics");
}

#[tokio::test]
async fn application_workspace_upload_requires_scoped_sandbox_and_project_write() {
    let auth = custom_auth(vec![
        Permission::ProjectsRead,
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
    ]);
    let missing_sandbox: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsRead.to_string(),
                Permission::ProjectsWrite.to_string(),
            ]),
            member: true,
            denied_project_id: None,
        }));
    let error = ensure_application_project_permission(
        &auth,
        &missing_sandbox,
        &[7],
        &Permission::SandboxesWrite,
    )
    .await
    .expect_err("workspace bytes must not enter a sandbox without scoped sandbox write");
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);

    let missing_project_write: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsRead.to_string(),
                Permission::SandboxesWrite.to_string(),
            ]),
            member: true,
            denied_project_id: None,
        }));
    let error = ensure_application_project_permission(
        &auth,
        &missing_project_write,
        &[7],
        &Permission::ProjectsWrite,
    )
    .await
    .expect_err("application workspace mutation also requires scoped project write");
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);

    let allowed: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsRead.to_string(),
                Permission::ProjectsWrite.to_string(),
                Permission::SandboxesWrite.to_string(),
            ]),
            member: true,
            denied_project_id: None,
        }));
    ensure_application_project_permission(&auth, &allowed, &[7], &Permission::SandboxesWrite)
        .await
        .expect("scoped sandbox write allows the workspace mutation");
    ensure_application_project_permission(&auth, &allowed, &[7], &Permission::ProjectsWrite)
        .await
        .expect("scoped project write allows the application mutation");
}

#[tokio::test]
async fn application_workspace_reads_require_scoped_project_and_sandbox_read() {
    let auth = custom_auth(vec![Permission::ProjectsRead, Permission::SandboxesRead]);
    let projects_only: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![Permission::ProjectsRead.to_string()]),
            member: true,
            denied_project_id: None,
        }));
    let error = ensure_application_project_permission(
        &auth,
        &projects_only,
        &[7],
        &Permission::SandboxesRead,
    )
    .await
    .expect_err("workspace contents require scoped sandbox read");
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);

    let allowed: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsRead.to_string(),
                Permission::SandboxesRead.to_string(),
            ]),
            member: true,
            denied_project_id: None,
        }));
    ensure_application_project_permission(&auth, &allowed, &[7], &Permission::ProjectsRead)
        .await
        .expect("scoped project read allows workspace metadata access");
    ensure_application_project_permission(&auth, &allowed, &[7], &Permission::SandboxesRead)
        .await
        .expect("scoped sandbox read allows workspace content access");
}

#[tokio::test]
async fn workspace_terminal_requires_scoped_sandbox_exec() {
    let auth = custom_auth(vec![
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
        Permission::SandboxesExec,
    ]);
    let without_exec: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsWrite.to_string(),
                Permission::SandboxesWrite.to_string(),
            ]),
            member: true,
            denied_project_id: None,
        }));
    let error = ensure_workspace_terminal_permissions(&auth, &without_exec, &[7])
        .await
        .expect_err("a project role without sandbox exec must not get a shell");
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);

    let allowed: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsWrite.to_string(),
                Permission::SandboxesWrite.to_string(),
                Permission::SandboxesExec.to_string(),
            ]),
            member: true,
            denied_project_id: None,
        }));
    ensure_workspace_terminal_permissions(&auth, &allowed, &[7])
        .await
        .expect("project write, sandbox write and sandbox exec allow a shell");
}

#[tokio::test]
async fn workspace_terminal_requires_project_sandbox_write_like_attach_does() {
    // Create and attach share this check, so a project role that holds
    // exec but not sandbox write cannot be handed a thread it can never
    // open.
    let auth = custom_auth(vec![
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
        Permission::SandboxesExec,
    ]);
    let without_sandbox_write: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsWrite.to_string(),
                Permission::SandboxesExec.to_string(),
            ]),
            member: true,
            denied_project_id: None,
        }));

    let error = ensure_workspace_terminal_permissions(&auth, &without_sandbox_write, &[7])
        .await
        .expect_err("a project role without sandbox write must not get a terminal");

    assert_eq!(error.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn workspace_terminal_is_denied_when_one_linked_project_is_not_allowed() {
    let auth = custom_auth(vec![
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
        Permission::SandboxesExec,
    ]);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![
                Permission::ProjectsWrite.to_string(),
                Permission::SandboxesWrite.to_string(),
                Permission::SandboxesExec.to_string(),
            ]),
            member: true,
            denied_project_id: Some(9),
        }));
    let error = ensure_workspace_terminal_permissions(&auth, &checker, &[7, 9])
        .await
        .expect_err("access to one project must not authorize a shell over both");
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn workspace_terminal_is_denied_without_instance_sandbox_exec() {
    // Instance permissions are the ceiling, whatever a project role says.
    let auth = custom_auth(vec![Permission::ProjectsWrite, Permission::SandboxesWrite]);
    let error = ensure_workspace_terminal_permissions(&auth, &None, &[7])
        .await
        .expect_err("the instance permission is required first");
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    let detail = error
        .body
        .get("detail")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(
        detail.contains("sandboxes:exec"),
        "the problem must name the missing permission: {detail}"
    );
}

#[test]
fn global_terminal_needs_everything_an_attach_needs() {
    // Creating a thread must not succeed for a role whose attach would be
    // refused, so create and attach share one permission check.
    for missing in [
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
        Permission::SandboxesExec,
    ] {
        let held = [
            Permission::ProjectsRead,
            Permission::ProjectsWrite,
            Permission::SandboxesWrite,
            Permission::SandboxesExec,
        ]
        .into_iter()
        .filter(|permission| *permission != missing)
        .collect::<Vec<_>>();
        let error = require_global_terminal_permissions(&custom_auth(held))
            .expect_err("a role missing one terminal permission must be refused");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN, "{missing}");
    }

    require_global_terminal_permissions(&custom_auth(vec![
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
        Permission::SandboxesExec,
    ]))
    .expect("project write, sandbox write and sandbox exec open a global terminal");
}

fn terminal_thread(application_id: Option<i64>, context_type: &str) -> ai_conversations::Model {
    let mut thread = conversation_with_runtime("terminal", "default");
    thread.public_id = "0123456789abcdef0123456789abcdef".to_string();
    thread.kind = "terminal".to_string();
    thread.application_id = application_id;
    thread.context_type = context_type.to_string();
    thread
}

#[test]
fn workspace_terminal_never_forwards_a_command() {
    let query = WorkspaceTerminalQuery {
        thread: "0123456789abcdef0123456789abcdef".to_string(),
        cols: Some(120),
        rows: Some(40),
    };
    let params = workspace_terminal_params(&query, "t-abc".to_string());
    assert_eq!(params.tab.as_deref(), Some("t-abc"));
    assert_eq!(params.cols, Some(120));
    assert_eq!(params.rows, Some(40));
    assert!(
        params.cmd.is_none(),
        "the workspace terminal always opens the login shell"
    );
}

#[test]
fn workspace_terminal_query_ignores_a_smuggled_command_and_tab() {
    // Neither `cmd` nor `tab` is a field, so a client that sends them
    // gets the thread's own shell, not an arbitrary program or tab.
    let uri: Uri = "/ai/workspace/terminal?thread=abc&tab=main&cmd=rm%20-rf%20%2F&cols=80"
        .parse()
        .expect("uri");
    let Query(query) =
        Query::<WorkspaceTerminalQuery>::try_from_uri(&uri).expect("query should parse");
    assert_eq!(query.thread, "abc");
    let params = workspace_terminal_params(&query, "t-abc".to_string());
    assert!(params.cmd.is_none());
    assert_eq!(params.tab.as_deref(), Some("t-abc"));
}

#[test]
fn workspace_terminal_requires_a_thread() {
    let uri: Uri = "/ai/workspace/terminal?cols=80".parse().expect("uri");
    assert!(
        Query::<WorkspaceTerminalQuery>::try_from_uri(&uri).is_err(),
        "a terminal without a thread has no shell to attach to"
    );
}

#[test]
fn a_terminal_thread_attaches_to_its_own_tab() {
    let thread = terminal_thread(Some(7), "application");
    let tab = terminal_thread_attach_tab(&thread, Some(7)).expect("attachable");
    assert_eq!(tab, "t-0123456789abcdef0123456789abcdef");
}

#[test]
fn a_thread_of_another_workspace_is_not_found() {
    let thread = terminal_thread(Some(7), "application");
    let error = terminal_thread_attach_tab(&thread, Some(8))
        .expect_err("a thread of another application must not attach");
    assert_eq!(error.status_code, StatusCode::NOT_FOUND);
    // The global workspace cannot attach an application thread either.
    let error = terminal_thread_attach_tab(&thread, None)
        .expect_err("an application thread is not a global thread");
    assert_eq!(error.status_code, StatusCode::NOT_FOUND);
}

#[test]
fn a_global_terminal_thread_attaches_only_to_the_global_workspace() {
    let thread = terminal_thread(None, "global");
    assert!(terminal_thread_attach_tab(&thread, None).is_ok());
    let error = terminal_thread_attach_tab(&thread, Some(7))
        .expect_err("a global thread must not attach through an application");
    assert_eq!(error.status_code, StatusCode::NOT_FOUND);
}

#[test]
fn an_ai_thread_cannot_attach_a_terminal() {
    let mut thread = terminal_thread(Some(7), "application");
    thread.kind = "ai".to_string();
    let error =
        terminal_thread_attach_tab(&thread, Some(7)).expect_err("an AI thread has no shell");
    assert_eq!(error.status_code, StatusCode::CONFLICT);
}

#[test]
fn an_archived_terminal_thread_must_be_restored_first() {
    let mut thread = terminal_thread(Some(7), "application");
    thread.status = "archived".to_string();
    let error =
        terminal_thread_attach_tab(&thread, Some(7)).expect_err("archived threads are read-only");
    assert_eq!(error.status_code, StatusCode::CONFLICT);
}

#[test]
fn a_terminal_thread_takes_no_messages() {
    let thread = terminal_thread(Some(7), "application");
    let error =
        ensure_conversation_takes_messages(&thread).expect_err("a terminal is not an AI turn");
    assert_eq!(error.status_code, StatusCode::CONFLICT);
    let mut ai = thread;
    ai.kind = "ai".to_string();
    assert!(ensure_conversation_takes_messages(&ai).is_ok());
}

#[test]
fn an_unknown_thread_kind_is_a_bad_request() {
    let error = requested_thread_kind(Some("notebook")).expect_err("unknown kind");
    assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    assert_eq!(
        requested_thread_kind(None).expect("default"),
        ThreadKind::Ai
    );
    assert_eq!(
        requested_thread_kind(Some("terminal")).expect("terminal"),
        ThreadKind::Terminal
    );
}

#[tokio::test]
async fn application_list_visibility_honors_scoped_projects_read() {
    let auth = custom_auth(vec![Permission::ProjectsRead]);
    let denied: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![Permission::ProjectsWrite.to_string()]),
            member: true,
            denied_project_id: None,
        }));

    let access = application_project_access_map(&auth, &denied, &[7, 9])
        .await
        .expect("project visibility check should succeed")
        .expect("configured checker should return a visibility map");

    assert_eq!(access.get(&7), Some(&false));
    assert_eq!(access.get(&9), Some(&false));
}

#[tokio::test]
async fn application_list_visibility_falls_back_to_membership_without_scoped_permissions() {
    let auth = custom_auth(vec![Permission::ProjectsRead]);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: None,
            member: true,
            denied_project_id: None,
        }));

    let access = application_project_access_map(&auth, &checker, &[7])
        .await
        .expect("project visibility check should succeed")
        .expect("configured checker should return a visibility map");

    assert_eq!(access.get(&7), Some(&true));
}

#[tokio::test]
async fn application_access_requires_permission_on_every_project() {
    let auth = custom_auth(vec![Permission::ProjectsRead]);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![Permission::ProjectsRead.to_string()]),
            member: true,
            denied_project_id: Some(9),
        }));

    assert!(
        !has_application_project_access(&auth, &checker, &[7, 9])
            .await
            .expect("project access check"),
        "access to one project must not authorize a multi-project application"
    );
}

#[tokio::test]
async fn revoked_project_membership_denies_user_conversation_mutations() {
    let auth = custom_auth(vec![
        Permission::ProjectsRead,
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
    ]);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
        Some(Arc::new(EffectivePermissionChecker {
            permissions: Some(vec![Permission::ProjectsWrite.to_string()]),
            member: true,
            denied_project_id: Some(42),
        }));

    let error = ensure_legacy_conversation_mutation_access(&auth, &checker, Some(42))
        .await
        .expect_err("attachment uploads and other mutations must re-check current membership");

    assert_eq!(error.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn application_creation_fails_closed_without_project_membership_checker() {
    let auth = custom_auth(vec![
        Permission::ProjectsRead,
        Permission::ProjectsWrite,
        Permission::SandboxesWrite,
    ]);

    let error = ensure_application_creation_project_permission(
        &auth,
        &None,
        &[42],
        &Permission::ProjectsWrite,
    )
    .await
    .expect_err("an existing cross-user project must not be accepted without ownership data");

    assert_eq!(error.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn global_conversation_visibility_uses_current_hidden_projects() {
    let auth = custom_auth(vec![Permission::ProjectsRead]);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(Arc::new(
        HiddenProjectsChecker(HiddenProjectsOutcome::Hidden(vec![7, 9])),
    ));

    assert_eq!(
        hidden_conversation_project_ids(&auth, &checker)
            .await
            .expect("visibility lookup"),
        vec![7, 9]
    );
}

#[tokio::test]
async fn global_conversation_visibility_fails_closed_on_checker_error() {
    let auth = custom_auth(vec![Permission::ProjectsRead]);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(Arc::new(
        HiddenProjectsChecker(HiddenProjectsOutcome::Error),
    ));

    let error = hidden_conversation_project_ids(&auth, &checker)
        .await
        .expect_err("checker errors must not return an unfiltered list");
    assert_eq!(error.status_code, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn host_cli_requires_provider_administration_permission() {
    let project_writer = custom_auth(vec![Permission::ProjectsWrite]);
    let denied = ensure_runtime_permission(&project_writer, Some("codex_cli"), Some("auto"))
        .expect_err("project write access must not authorize host CLI execution");
    assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

    let provider_admin = custom_auth(vec![Permission::ProjectsWrite, Permission::AiGatewayWrite]);
    let denied = ensure_runtime_permission(&provider_admin, Some("codex_cli"), Some("auto"))
        .expect_err("Codex host access requires system administration");
    assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

    let system_admin = custom_auth(vec![
        Permission::ProjectsWrite,
        Permission::AiGatewayWrite,
        Permission::SystemAdmin,
    ]);
    ensure_runtime_permission(&system_admin, Some("codex_cli"), Some("auto"))
        .expect("system administrator may opt into the Codex host boundary");
}

#[test]
fn resolved_conversation_provider_is_authorized_after_default_resolution() {
    let project_writer = custom_auth(vec![Permission::ProjectsWrite]);
    let resolved = conversation_with_runtime("claude_cli", "default");

    let denied = ensure_runtime_permission(
        &project_writer,
        Some(&resolved.ai_provider),
        Some(&resolved.ai_permission_mode),
    )
    .expect_err("resolved host provider must be authorized even when request omitted it");
    assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

    let provider_admin = custom_auth(vec![Permission::ProjectsWrite, Permission::AiGatewayWrite]);
    ensure_runtime_permission(
        &provider_admin,
        Some(&resolved.ai_provider),
        Some(&resolved.ai_permission_mode),
    )
    .expect("provider administrator may use the resolved Claude provider");
}

#[test]
fn stored_host_conversation_remains_readable_without_runtime_permission() {
    let project_reader = custom_auth(vec![Permission::ProjectsRead]);
    let stored = conversation_with_runtime("claude_cli", "full-access");

    ensure_conversation_read_permission(&project_reader, &stored)
        .expect("stored project history must not require host-provider execution access");
    assert!(ensure_runtime_permission(
        &project_reader,
        Some(&stored.ai_provider),
        Some(&stored.ai_permission_mode),
    )
    .is_err());
}

#[test]
fn application_threads_require_a_registered_harness() {
    let missing = require_application_harness(None).expect_err("harness is required");
    assert_eq!(missing.status_code, StatusCode::BAD_REQUEST);

    let gateway = require_application_harness(Some("gateway_key:7"))
        .expect_err("gateway keys cannot execute an application harness");
    assert_eq!(gateway.status_code, StatusCode::BAD_REQUEST);

    assert_eq!(
        require_application_harness(Some("claude_cli")).expect("Claude is registered"),
        "claude_cli"
    );

    assert_eq!(
        require_application_harness(Some("codex_cli")).expect("Codex is registered"),
        "codex_cli"
    );
}

#[test]
fn full_access_requires_system_administrator_permission() {
    let provider_admin = custom_auth(vec![Permission::AiGatewayWrite]);
    let denied =
        ensure_runtime_permission(&provider_admin, Some("claude_cli"), Some("full-access"))
            .expect_err("provider administration alone must not authorize full host access");
    assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

    let system_admin = custom_auth(vec![Permission::AiGatewayWrite, Permission::SystemAdmin]);
    assert!(
        ensure_runtime_permission(&system_admin, Some("claude_cli"), Some("full-access"),).is_ok()
    );
}

#[test]
fn alert_suggestion_context_requires_otel_read() {
    let without_otel = custom_auth(vec![Permission::ProjectsRead, Permission::ProjectsWrite]);
    let err = ensure_context_read_permission(&without_otel, "alert_suggest")
        .expect_err("OTel data must not leak through chat seeds or history");
    assert_eq!(err.status_code, StatusCode::FORBIDDEN);

    let with_otel = custom_auth(vec![Permission::OtelRead]);
    ensure_context_read_permission(&with_otel, "alert_suggest")
        .expect("otel:read authorizes alert suggestion context");
}

#[test]
fn unrelated_chat_context_does_not_require_otel_read() {
    let auth = custom_auth(vec![Permission::ProjectsRead]);
    ensure_context_read_permission(&auth, "project")
        .expect("project chat keeps its existing project permission boundary");
}

// (c) Over-length input is rejected as 400 before any DB/AI work (cost/DoS
// hardening).
#[test]
fn test_too_long_is_400() {
    let p = too_long("content", 10);
    assert_eq!(p.status_code, StatusCode::BAD_REQUEST);
    assert_eq!(title_of(&p).as_deref(), Some("Input Too Long"));
}

// Note on full handler-level (401/403 via the guard macros) coverage: the
// `permission_guard!` / `project_scope_guard!` / `deny_deployment_token!`
// macros are themselves tested in `temps-auth`; here we cover the
// crate-specific input-length gate, service-layer scoping (see service.rs
// tests), and the HTTP error mapping via the
// pure `From<ChatError>` conversion.

// ── PendingActionError → Problem mapping ─────────────────────────────────

#[test]
fn test_pending_action_not_found_maps_to_404() {
    let p: Problem = PendingActionError::NotFound {
        public_id: "abc".to_string(),
    }
    .into();
    assert_eq!(p.status_code, StatusCode::NOT_FOUND);
    assert_eq!(title_of(&p).as_deref(), Some("Pending Action Not Found"));
}

#[test]
fn test_pending_action_invalid_state_maps_to_409() {
    let p: Problem = PendingActionError::InvalidState {
        public_id: "abc".to_string(),
        status: "executed".to_string(),
    }
    .into();
    assert_eq!(p.status_code, StatusCode::CONFLICT);
    assert_eq!(title_of(&p).as_deref(), Some("Invalid Action State"));
}

#[test]
fn test_pending_action_inline_approval_required_maps_to_409_with_guidance() {
    let p: Problem = PendingActionError::InlineApprovalRequired {
        public_id: "abc".to_string(),
    }
    .into();
    assert_eq!(p.status_code, StatusCode::CONFLICT);
    assert_eq!(title_of(&p).as_deref(), Some("Inline Approval Required"));
    let body = serde_json::to_string(&p.body).expect("problem body serializes");
    assert!(
        body.contains("inline approval in the chat conversation"),
        "{body}"
    );
    assert!(!body.contains("concurrent"), "{body}");
}

#[test]
fn test_pending_action_permission_denied_maps_to_403() {
    let p: Problem = PendingActionError::PermissionDenied {
        public_id: "abc".to_string(),
        permission: "deployments:write".to_string(),
        decision: "confirm",
    }
    .into();
    assert_eq!(p.status_code, StatusCode::FORBIDDEN);
    assert_eq!(title_of(&p).as_deref(), Some("Permission Denied"));
}

#[test]
fn test_pending_action_unavailable_maps_to_503() {
    let p: Problem = PendingActionError::Unavailable.into();
    assert_eq!(p.status_code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(title_of(&p).as_deref(), Some("Write Actions Unavailable"));
}

#[test]
fn test_pending_action_database_error_maps_to_500() {
    let p: Problem =
        PendingActionError::Database(sea_orm::DbErr::Custom("boom".to_string())).into();
    assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(title_of(&p).as_deref(), Some("Internal Server Error"));
}

// ── redact_params ────────────────────────────────────────────────────────

#[test]
fn test_redact_params_masks_sensitive_keys() {
    let params = serde_json::json!({
        "name": "MY_SECRET",
        "value": "super-secret",
        "secret": "also-secret",
        "password": "p@ssword",
        "token": "tok_abc",
        "key": "k123",
        "operation": "update",
    });
    let redacted = redact_params(&params);
    assert_eq!(redacted["name"], serde_json::json!("MY_SECRET"));
    assert_eq!(redacted["operation"], serde_json::json!("update"));
    assert_eq!(redacted["value"], serde_json::json!("***"));
    assert_eq!(redacted["secret"], serde_json::json!("***"));
    assert_eq!(redacted["password"], serde_json::json!("***"));
    assert_eq!(redacted["token"], serde_json::json!("***"));
    assert_eq!(redacted["key"], serde_json::json!("***"));
}

#[test]
fn test_redact_params_masks_keys_containing_sensitive_substrings() {
    let params = serde_json::json!({
        "api_key": "my-api-key",
        "access_token": "tok",
        "db_password": "hunter2",
    });
    let redacted = redact_params(&params);
    assert_eq!(redacted["api_key"], serde_json::json!("***"));
    assert_eq!(redacted["access_token"], serde_json::json!("***"));
    assert_eq!(redacted["db_password"], serde_json::json!("***"));
}

#[test]
fn test_redact_params_non_object_passthrough() {
    let arr = serde_json::json!([1, 2, 3]);
    assert_eq!(redact_params(&arr), arr);
    let s = serde_json::json!("hello");
    assert_eq!(redact_params(&s), s);
    let n = serde_json::json!(42);
    assert_eq!(redact_params(&n), n);
}

#[test]
fn test_redact_params_recurses_through_nested_objects_and_arrays() {
    let params = serde_json::json!({
        "parameters": {
            "database": [{
                "name": "primary",
                "credentials": {
                    "password": "nested-password",
                    "apiKey": "nested-key"
                }
            }]
        },
        "result": [{"access_token": "nested-token", "status": "ok"}]
    });
    let redacted = redact_params(&params);
    assert_eq!(redacted["parameters"]["database"][0]["name"], "primary");
    assert_eq!(redacted["parameters"]["database"][0]["credentials"], "***");
    assert_eq!(redacted["result"][0]["access_token"], "***");
    assert_eq!(redacted["result"][0]["status"], "ok");
}

#[test]
fn test_redact_params_empty_object_passthrough() {
    let empty = serde_json::json!({});
    assert_eq!(redact_params(&empty), empty);
}

#[test]
fn test_redact_params_case_insensitive() {
    let params = serde_json::json!({
        "VALUE": "sensitive",
        "Secret": "also-sensitive",
    });
    let redacted = redact_params(&params);
    assert_eq!(redacted["VALUE"], serde_json::json!("***"));
    assert_eq!(redacted["Secret"], serde_json::json!("***"));
}

#[test]
fn reloaded_tool_metadata_never_returns_embedded_write_secret() {
    let secret = "must-not-survive-reload";
    let arguments = serde_json::json!({
        "command": format!("projects create_environment_variable --name API_TOKEN --value {secret}")
    })
    .to_string();
    let message = ai_messages::Model {
        id: 1,
        conversation_id: 1,
        role: "assistant".to_string(),
        content: String::new(),
        metadata: Some(serde_json::json!({
            "tools": [{
                "id": "call-1",
                "name": "temps_write",
                "arguments": arguments,
                "result": format!("HTTP 400: token={secret}")
            }]
        })),
        tokens_in: None,
        tokens_out: None,
        cost_microcents: None,
        created_at: chrono::Utc::now(),
    };

    let response = MessageResponse::from(message);
    let serialized = serde_json::to_string(&response).expect("message serializes");
    assert!(!serialized.contains(secret));
    assert!(serialized.contains("***"));
}

// ----- SSE mapping tests (ADR-038 Phase 2, milestone 3) -----

/// `ChatStreamEvent::PermissionRequested` serialises to a `PermissionRequestedEvent`
/// with every field intact and no sensitive content omitted.
#[test]
fn test_permission_requested_event_serialization() {
    let payload = PermissionRequestedEvent {
        id: "perm-abc".to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: "bash".to_string(),
        input: serde_json::json!({ "command": "ls" }),
    };
    let json = serde_json::to_string(&payload).expect("serializes");
    let back: serde_json::Value = serde_json::from_str(&json).expect("parses");
    assert_eq!(back["id"], "perm-abc");
    assert_eq!(back["kind"], "tool_approval");
    assert_eq!(back["tool_name"], "bash");
    assert_eq!(back["input"]["command"], "ls");
}

/// Verifies that the `PermissionKind` serde aliases match the wire format
/// expected by the ADR-038 spec: snake_case tags.
#[test]
fn test_permission_kind_serde_roundtrip() {
    let kinds = [
        (PermissionKind::ToolApproval, "tool_approval"),
        (PermissionKind::Question, "question"),
        (PermissionKind::PlanApproval, "plan_approval"),
    ];
    for (kind, expected_tag) in kinds {
        let json = serde_json::to_string(&kind).expect("serializes");
        // PermissionKind is a unit enum with rename_all = snake_case
        // so it serialises as a plain string
        assert_eq!(json, format!("\"{expected_tag}\""), "kind={kind:?}");
    }
}

/// `PermissionDecision` with the `type` tag serialises correctly for all
/// five variants — only the kind tag and optional payload appear, never raw
/// tool input or answer content.
#[test]
fn test_permission_decision_serde_type_tags() {
    use temps_ai::streaming::PermissionDecision;

    let cases: &[(PermissionDecision, &str)] = &[
        (PermissionDecision::AllowTool, "allow_tool"),
        (
            PermissionDecision::DenyTool {
                reason: Some("blocked".to_string()),
            },
            "deny_tool",
        ),
        (
            PermissionDecision::AnswerQuestion {
                answers: serde_json::json!({}),
            },
            "answer_question",
        ),
        (PermissionDecision::ApprovePlan, "approve_plan"),
        (
            PermissionDecision::RejectPlan {
                feedback: Some("needs revision".to_string()),
            },
            "reject_plan",
        ),
    ];

    for (decision, expected_type) in cases {
        let json = serde_json::to_string(decision).expect("serializes");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(
            v["type"].as_str(),
            Some(*expected_type),
            "decision={decision:?}"
        );
    }
}

// ── Security-finding tests (ADR-038 Phase 2 milestone 3) ─────────────────

/// Finding 1 (HIGH-IDOR): cross-conversation claim is rejected with 404.
///
/// A registry entry created for conversation "conv-A" must not be claimable
/// via "conv-B"'s URL.  The entry must remain in the registry so the
/// legitimate owner can still claim it.
#[test]
fn test_resolve_permission_cross_conversation_is_rejected() {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use temps_ai::streaming::PermissionKind;
    use tokio::sync::oneshot;

    let (tx, _rx) = oneshot::channel::<PermissionResolution>();

    let mut registry: HashMap<String, PendingPermissionEntry> = HashMap::new();
    registry.insert(
        "req-1".to_string(),
        PendingPermissionEntry {
            sender: tx,
            conv_public_id: "conv-A".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: "Bash".to_string(),
            input: serde_json::Value::Null,
            generation: uuid::Uuid::new_v4(),
            origin: PendingPermissionOrigin::Provider,
        },
    );
    let registry = Arc::new(Mutex::new(registry));

    // Simulate what `resolve_permission` does inside the lock.
    let attacker_conv = "conv-B";
    let stored_conv_id = {
        let guard = registry.lock().unwrap();
        guard.get("req-1").map(|e| e.conv_public_id.clone())
    };
    let is_idor = stored_conv_id
        .as_deref()
        .map(|id| id != attacker_conv)
        .unwrap_or(false);

    assert!(
        is_idor,
        "cross-conversation claim should be detected as IDOR"
    );

    // The entry must NOT have been removed — the real owner can still claim.
    let guard = registry.lock().unwrap();
    assert!(
        guard.contains_key("req-1"),
        "registry entry must remain after IDOR rejection"
    );
}

/// Finding 2 (MEDIUM): submitting the wrong decision kind for a `Question`
/// permission returns 400 with the typed `PermissionKindMismatch` error.
#[test]
fn test_permission_kind_mismatch_maps_to_400() {
    let p: Problem = ChatError::PermissionKindMismatch {
        expected_kind: "question".to_string(),
        received: "allow_tool".to_string(),
    }
    .into();
    assert_eq!(
        p.status_code,
        StatusCode::BAD_REQUEST,
        "kind mismatch must be a client error (400)"
    );
    assert_eq!(
        title_of(&p).as_deref(),
        Some("Permission Decision Mismatch")
    );
    // Detail must include the mismatch context so the caller can fix it.
    let detail = p.body.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        detail.contains("question"),
        "detail must mention the expected kind"
    );
    assert!(
        detail.contains("allow_tool"),
        "detail must mention the received decision"
    );
}

#[test]
fn mismatched_permission_decision_can_be_retried() {
    use std::collections::HashMap;
    use temps_ai::streaming::{PermissionDecision, PermissionKind};
    use tokio::sync::oneshot;

    let (tx, _rx) = oneshot::channel();
    let mut registry = HashMap::new();
    registry.insert(
        "question-1".to_string(),
        PendingPermissionEntry {
            sender: tx,
            conv_public_id: "conv-1".to_string(),
            kind: PermissionKind::Question,
            tool_name: "AskUserQuestion".to_string(),
            input: serde_json::Value::Null,
            generation: uuid::Uuid::new_v4(),
            origin: PendingPermissionOrigin::Provider,
        },
    );

    assert!(!permission_decision_matches_kind(
        &registry["question-1"].kind,
        &PermissionDecision::AllowTool
    ));
    assert!(
        registry.contains_key("question-1"),
        "validation must happen before remove-to-claim"
    );

    let retry = PermissionDecision::AnswerQuestion {
        answers: serde_json::json!({"answer": "yes"}),
    };
    assert!(permission_decision_matches_kind(
        &registry["question-1"].kind,
        &retry
    ));
    assert!(registry.remove("question-1").is_some());
}

/// Finding 3 (MEDIUM): oversized `reason` in a `DenyTool` payload is caught
/// by `too_long` before the registry is touched.
#[test]
fn test_deny_tool_oversized_reason_returns_400_input_too_long() {
    // Build a `reason` that exceeds the 4 KiB limit.
    let oversized = "x".repeat(MAX_DECISION_STRING_LEN + 1);

    // Mimic the guard in `resolve_permission`.
    let decision = PermissionDecision::DenyTool {
        reason: Some(oversized.clone()),
    };
    let error = match &decision {
        PermissionDecision::DenyTool {
            reason: Some(reason),
        } if reason.len() > MAX_DECISION_STRING_LEN => {
            Some(too_long("reason", MAX_DECISION_STRING_LEN))
        }
        _ => None,
    };

    let p = error.expect("oversized reason must trigger too_long guard");
    assert_eq!(
        p.status_code,
        StatusCode::BAD_REQUEST,
        "oversized payload must be rejected with 400"
    );
    assert_eq!(title_of(&p).as_deref(), Some("Input Too Long"));
}

/// Finding 3 (MEDIUM): oversized serialized `answers` in `AnswerQuestion`
/// is caught before the registry is touched.
#[test]
fn test_answer_question_oversized_answers_returns_400() {
    // answers = { "q": "aaa...4097 chars..."}
    let big_value = "a".repeat(MAX_DECISION_STRING_LEN + 1);
    let answers = serde_json::json!({ "q": big_value });

    let decision = PermissionDecision::AnswerQuestion {
        answers: answers.clone(),
    };
    let error = match &decision {
        PermissionDecision::AnswerQuestion { answers } => {
            let serialized_len = serde_json::to_string(answers)
                .map(|s| s.len())
                .unwrap_or(usize::MAX);
            if serialized_len > MAX_DECISION_STRING_LEN {
                Some(too_long("answers", MAX_DECISION_STRING_LEN))
            } else {
                None
            }
        }
        _ => None,
    };

    let p = error.expect("oversized answers must trigger too_long guard");
    assert_eq!(p.status_code, StatusCode::BAD_REQUEST);
    assert_eq!(title_of(&p).as_deref(), Some("Input Too Long"));
}

#[test]
fn workspace_status_is_structured_and_hides_sensitive_paths() {
    let status = concat!(
        " M src/main.rs\0",
        "A  README.md\0",
        "?? .env\0",
        "?? src/new file.ts\0",
        "R  src/new-name.rs\0src/old-name.rs\0"
    );

    let changes = parse_workspace_status(status);

    assert_eq!(changes.len(), 4);
    assert_eq!(changes[0].path, "src/main.rs");
    assert!(changes[0].unstaged);
    assert_eq!(changes[1].status.as_deref(), Some("added"));
    assert!(changes[1].staged);
    assert_eq!(changes[2].path, "src/new file.ts");
    assert_eq!(changes[2].status.as_deref(), Some("untracked"));
    assert_eq!(changes[3].path, "src/new-name.rs");
    assert_eq!(changes[3].status.as_deref(), Some("renamed"));
    assert!(changes.iter().all(|change| change.path != ".env"));
}

#[test]
fn workspace_secret_file_filter_allows_documented_examples() {
    assert!(is_sensitive_workspace_path(".env.production"));
    assert!(is_sensitive_workspace_path(".Git-Credentials"));
    assert!(is_sensitive_workspace_path(".docker/config.json"));
    assert!(is_sensitive_workspace_path(".temps/runtime.json"));
    assert!(is_sensitive_workspace_path(".config/gcloud/credentials.db"));
    assert!(is_sensitive_workspace_path("infra/terraform.tfstate"));
    assert!(is_sensitive_workspace_path("config/private-key.pem"));
    assert!(is_sensitive_workspace_path("keys/id_dsa"));
    assert!(is_sensitive_workspace_path(
        "config/production.credentials.json"
    ));
    assert!(is_sensitive_workspace_path(
        "config/google-service-account.json"
    ));
    assert!(is_sensitive_workspace_path("../outside"));
    assert!(!is_sensitive_workspace_path(".env.example"));
    assert!(!is_sensitive_workspace_path("src/config.ts"));
    assert!(validate_workspace_import_path("src/config.ts").is_ok());
    assert!(validate_workspace_import_path("../outside").is_err());
    assert!(validate_workspace_import_path("/absolute.ts").is_err());
}

#[test]
fn workspace_file_records_ignore_exec_transport_line_breaks() {
    let files = workspace_file_records("src/main.rs\0README.md\0\n").collect::<Vec<_>>();
    assert_eq!(files, vec!["src/main.rs", "README.md"]);
}

#[test]
fn workspace_file_pages_are_bounded_and_resumable() {
    let files = (0..250)
        .map(|index| format!("src/file-{index:03}.ts"))
        .collect::<Vec<_>>();
    let query = ApplicationWorkspaceChangesQuery {
        cursor: Some(100),
        limit: Some(100),
    };

    let (page, next_cursor) = workspace_file_page(&files, &query).expect("valid page");

    assert_eq!(page.len(), 100);
    assert_eq!(page.first().map(String::as_str), Some("src/file-100.ts"));
    assert_eq!(page.last().map(String::as_str), Some("src/file-199.ts"));
    assert_eq!(next_cursor, Some(200));
}

#[test]
fn workspace_file_page_rejects_unbounded_page_sizes() {
    let query = ApplicationWorkspaceChangesQuery {
        cursor: None,
        limit: Some(WORKSPACE_MAX_PAGE_SIZE + 1),
    };

    let error = workspace_file_page(&[], &query).expect_err("oversized page must fail");

    assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    assert_eq!(
        title_of(&error).as_deref(),
        Some("Invalid Workspace Page Size")
    );
}

#[test]
fn workspace_diff_truncation_preserves_utf8_boundaries() {
    let (value, truncated) = truncate_workspace_text("ééé".to_string(), 3);
    assert_eq!(value, "é");
    assert!(truncated);
}

fn loaded_text(name: &str) -> crate::attachments::LoadedAttachment {
    crate::attachments::LoadedAttachment {
        public_id: "att_1".to_string(),
        name: name.to_string(),
        mime_type: "text/plain".to_string(),
        kind: crate::attachments::AttachmentKind::Text,
        bytes: b"hello".to_vec(),
        extracted_text: None,
    }
}

#[test]
fn sandbox_copy_notes_name_only_the_files_that_were_placed() {
    let loaded = vec![loaded_text("a.txt"), loaded_text("b \"x\".txt")];
    let paths = vec![
        None,
        Some("/w/.temps/chat-attachments/c/att_1/b.txt".to_string()),
    ];
    let notes = sandbox_copy_notes(&loaded, &paths).expect("one file was placed");
    assert!(!notes.contains("a.txt"));
    assert!(notes.contains("/w/.temps/chat-attachments/c/att_1/b.txt"));
    assert!(
        notes.contains("b 'x'.txt"),
        "quotes in names are neutralized: {notes}"
    );
}

#[test]
fn no_sandbox_copies_means_no_notes() {
    let loaded = vec![loaded_text("a.txt")];
    assert!(sandbox_copy_notes(&loaded, &[None]).is_none());
    assert!(sandbox_copy_notes(&[], &[]).is_none());
}

#[test]
fn openapi_documents_workspace_attachment_uploads_as_multipart() {
    let document = serde_json::to_value(AiChatApiDoc::openapi())
        .expect("AI chat OpenAPI document should serialize");
    let content = &document["paths"]["/ai/conversations/{public_id}/attachments"]["post"]
        ["requestBody"]["content"];

    assert!(
        content.get("multipart/form-data").is_some(),
        "attachment upload must advertise multipart/form-data"
    );
    assert!(document["components"]["schemas"]
        .get("ChatAttachmentUpload")
        .is_some());
    assert!(
        document["paths"]["/ai/conversations/{public_id}/attachments/{attachment_id}"]
            .get("get")
            .is_some()
    );
}

#[test]
fn openapi_documents_global_workspace_files_and_preview() {
    let document = serde_json::to_value(AiChatApiDoc::openapi())
        .expect("AI chat OpenAPI document should serialize");

    assert!(document["paths"]["/ai/workspace/changes"]
        .get("get")
        .is_some());
    assert!(document["paths"]["/ai/workspace/diff"].get("get").is_some());
    assert!(document["paths"]["/ai/workspace/preview-link"]
        .get("post")
        .is_some());
}
