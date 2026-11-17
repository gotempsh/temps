// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversions from the domain errors into Problem Details responses.

use super::*;

impl From<ChatError> for Problem {
    fn from(e: ChatError) -> Self {
        match e {
            ChatError::NotFound { .. } => problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                .with_title("Conversation Not Found")
                .with_detail(e.to_string()),
            ChatError::ProjectNotFound(_) => problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(e.to_string()),
            ChatError::NoProvider { .. } | ChatError::ContextUnavailable => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Context Not Available")
                    .with_detail(e.to_string())
            }
            ChatError::AiUnavailable => problemdetails::new(axum::http::StatusCode::CONFLICT)
                .with_title("AI Not Configured")
                .with_detail(e.to_string()),
            ChatError::ProposalNotStaged => {
                let failure = e.public_failure();
                problemdetails::new(axum::http::StatusCode::BAD_GATEWAY)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            ChatError::TurnInProgress { .. } | ChatError::DuplicateTurn { .. } => {
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("AI Turn Already Running")
                    .with_detail(e.to_string())
            }
            ChatError::WorkspaceRuntimeBusy { .. }
            | ChatError::WorkspaceRuntimeUpdateRequired { .. } => {
                let failure = e.public_failure();
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            chat_error @ ChatError::AuthorizationRefresh(_) => {
                let failure = chat_error.public_failure();
                error!(
                    failure_code = failure.code,
                    "AI chat authorization refresh failed: {chat_error}"
                );
                problemdetails::new(axum::http::StatusCode::UNAUTHORIZED)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            chat_error @ (ChatError::ProjectLookup { .. } | ChatError::Db(_)) => {
                let failure = chat_error.public_failure();
                error!(
                    failure_code = failure.code,
                    "AI chat storage operation failed: {chat_error}"
                );
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            chat_error @ ChatError::ApplicationWorkspace(_) => {
                let failure = chat_error.public_failure();
                error!(
                    failure_code = failure.code,
                    "AI chat workspace preparation failed: {chat_error}"
                );
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            ChatError::Ai(reason) => {
                let error = ChatError::Ai(reason);
                let failure = error.public_failure();
                error!(
                    failure_code = failure.code,
                    "AI chat provider operation failed: {error}"
                );
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            error @ ChatError::RetainedHarnessDiagnostic { .. } => {
                let failure = error.public_failure();
                tracing::error!(
                    failure_code = failure.code,
                    "retained AI harness failed: {error}"
                );
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            error @ ChatError::StreamInterrupted { .. } => {
                let failure = error.public_failure();
                error!(
                    failure_code = failure.code,
                    "AI provider stream ended before a terminal chunk: {error}"
                );
                problemdetails::new(axum::http::StatusCode::BAD_GATEWAY)
                    .with_title(failure.title)
                    .with_detail(failure.detail)
            }
            ChatError::PermissionKindMismatch { .. } => {
                problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
                    .with_title("Permission Decision Mismatch")
                    .with_detail(e.to_string())
            }
        }
    }
}

impl From<crate::attachments::AttachmentError> for Problem {
    fn from(error: crate::attachments::AttachmentError) -> Self {
        use crate::attachments::AttachmentError;
        match error {
            AttachmentError::Empty { .. }
            | AttachmentError::TooMany { .. }
            | AttachmentError::Duplicate { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Attachment")
                .with_detail(error.to_string()),
            AttachmentError::TooLarge { .. } | AttachmentError::TooMuchText { .. } => {
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Attachment Too Large")
                    .with_detail(error.to_string())
            }
            AttachmentError::QuotaExceeded { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Attachment Quota Reached")
                .with_detail(error.to_string()),
            AttachmentError::ModelCannotSeeImages { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Model Cannot Read Images")
                    .with_detail(error.to_string())
            }
            AttachmentError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Attachment Not Found")
                .with_detail(error.to_string()),
            AttachmentError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

impl From<ApplicationError> for Problem {
    fn from(error: ApplicationError) -> Self {
        match error {
            ApplicationError::NotFound { .. }
            | ApplicationError::ProjectNotFound(_)
            | ApplicationError::ConversationNotFound { .. }
            | ApplicationError::ProjectNotLinked { .. }
            | ApplicationError::WorkspacePathNotFound { .. }
            | ApplicationError::AttachmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("AI Application Resource Not Found")
                    .with_detail(error.to_string())
            }
            ApplicationError::InvalidName
            | ApplicationError::InvalidProjects
            | ApplicationError::InvalidWorkspaceSetting { .. }
            | ApplicationError::WorkspaceQuota { .. }
            | ApplicationError::InvalidArtifactKind { .. }
            | ApplicationError::SecretValue { .. }
            | ApplicationError::InvalidWorkspaceIdentifier { .. }
            | ApplicationError::InvalidWorkspacePath { .. }
            | ApplicationError::WorkspacePathNotFile { .. }
            | ApplicationError::InvalidAttachment { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid AI Application Request")
                    .with_detail(error.to_string())
            }
            ApplicationError::WorkspaceDownloadTooLarge { .. } => {
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Workspace File Too Large")
                    .with_detail(error.to_string())
            }
            ApplicationError::ProjectAlreadyLinked { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Invalid Application Topology Change")
                    .with_detail(error.to_string())
            }
            ApplicationError::Workspace { .. } | ApplicationError::Database(_) => {
                error!(error = %error, "AI application operation failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(
                        "An internal operation failed while handling the AI application request.",
                    )
            }
        }
    }
}

impl From<PendingActionError> for Problem {
    fn from(e: PendingActionError) -> Self {
        match e {
            PendingActionError::NotFound { .. } => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Pending Action Not Found")
                    .with_detail(e.to_string())
            }
            PendingActionError::InvalidState { .. } => {
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("Invalid Action State")
                    .with_detail(e.to_string())
            }
            PendingActionError::InlineApprovalRequired { .. } => {
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("Inline Approval Required")
                    .with_detail(e.to_string())
            }
            PendingActionError::StepBlocked { .. } => {
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("Plan Step Not Ready")
                    .with_detail(e.to_string())
            }
            PendingActionError::PermissionDenied { .. } => {
                problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                    .with_title("Permission Denied")
                    .with_detail(e.to_string())
            }
            PendingActionError::Unavailable => {
                problemdetails::new(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Write Actions Unavailable")
                    .with_detail(e.to_string())
            }
            PendingActionError::Execution { .. } => {
                problemdetails::new(axum::http::StatusCode::BAD_GATEWAY)
                    .with_title("Execution Failed")
                    .with_detail(e.to_string())
            }
            PendingActionError::Database(_) => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(e.to_string())
            }
            PendingActionError::Encryption { .. } => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Protected Action Data Error")
                    .with_detail(e.to_string())
            }
        }
    }
}

impl From<ProjectPermissionError> for Problem {
    fn from(error: ProjectPermissionError) -> Self {
        match error {
            ProjectPermissionError::MissingInstancePermission { .. } => {
                problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("Insufficient Permissions")
                    .with_detail(error.to_string())
            }
            ProjectPermissionError::NoUserIdentity => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Project Permission Denied")
                .with_detail(error.to_string()),
            ProjectPermissionError::PermissionCheck { .. } => {
                error!(error = %error, "failed to check AI application project permissions");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Project Permission Check Failed")
                    .with_detail("Could not verify project permissions; please try again")
            }
            ProjectPermissionError::MembershipCheck { .. } => {
                error!(error = %error, "failed to check AI application project membership");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Project Access Check Failed")
                    .with_detail("Could not verify project access; please try again")
            }
            ProjectPermissionError::Denied { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Project Permission Denied")
                .with_detail(error.to_string()),
        }
    }
}

impl From<LinkProjectError> for Problem {
    fn from(error: LinkProjectError) -> Self {
        match error {
            LinkProjectError::Permission(error) => error.into(),
            LinkProjectError::RuntimeLock(error) => ChatError::from(error).into(),
            LinkProjectError::Application(error) => error.into(),
            LinkProjectError::ProjectPermissions { source, .. } => (*source).into(),
            LinkProjectError::Network(error) => error.into(),
        }
    }
}

impl From<NetworkSyncError> for Problem {
    fn from(error: NetworkSyncError) -> Self {
        application_workspace_provider_problem(
            *error.source,
            &error.application_id,
            error.operation,
        )
    }
}

#[cfg(test)]
mod attachment_problem_tests {
    use super::*;
    use crate::attachments::AttachmentError;
    use crate::ToolAuthorizationRefreshError;

    #[test]
    fn a_full_conversation_is_a_conflict_titled_as_the_quota() {
        let error = AttachmentError::QuotaExceeded {
            conversation_id: 42,
            files: 20,
            bytes: 2048,
            max_files: 20,
            max_bytes: 4096,
        };
        let detail = error.to_string();
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body["title"].as_str(),
            Some("Attachment Quota Reached")
        );
        assert_eq!(problem.body["detail"].as_str(), Some(detail.as_str()));
        assert!(detail.contains("Conversation 42"));
    }

    #[test]
    fn an_image_for_a_text_model_is_a_conflict_titled_as_the_model() {
        let error = AttachmentError::ModelCannotSeeImages {
            model: "text-only-model".to_string(),
            name: "diagram.png".to_string(),
        };
        let detail = error.to_string();
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body["title"].as_str(),
            Some("Model Cannot Read Images")
        );
        assert_eq!(problem.body["detail"].as_str(), Some(detail.as_str()));
        assert!(detail.contains("text-only-model"));
    }

    #[test]
    fn missing_conversations_and_context_providers_answer_404_with_the_id() {
        for (error, title, detail) in [
            (
                ChatError::NotFound {
                    conversation_id: "conv-1".to_string(),
                },
                "Conversation Not Found",
                "conversation 'conv-1' not found",
            ),
            (
                ChatError::NoProvider {
                    context_type: "deployment".to_string(),
                },
                "Context Not Available",
                "no AI context provider registered for type 'deployment'",
            ),
        ] {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
            assert_eq!(problem.body["title"].as_str(), Some(title));
            assert_eq!(problem.body["detail"].as_str(), Some(detail));
        }
    }

    #[test]
    fn a_stale_api_key_role_is_a_401_that_does_not_echo_the_role() {
        let error = ChatError::AuthorizationRefresh(ToolAuthorizationRefreshError::InvalidRole {
            role: "retired-role".to_string(),
        });
        assert!(error.to_string().contains("retired-role"));
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);
        assert!(!problem.body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("retired-role")));
    }

    #[test]
    fn application_errors_keep_their_status_and_name_what_they_refer_to() {
        let text = |value: &str| value.to_string();
        for (error, status, detail) in [
            (
                ApplicationError::NotFound {
                    application_id: text("app-1"),
                },
                StatusCode::NOT_FOUND,
                "application 'app-1' not found",
            ),
            (
                ApplicationError::ConversationNotFound {
                    conversation_id: text("conv-1"),
                },
                StatusCode::NOT_FOUND,
                "conversation 'conv-1' does not belong to this application",
            ),
            (
                ApplicationError::WorkspacePathNotFound {
                    path: text("a.txt"),
                },
                StatusCode::NOT_FOUND,
                "workspace path 'a.txt' was not found",
            ),
            (
                ApplicationError::AttachmentNotFound {
                    attachment_id: text("file-1"),
                },
                StatusCode::NOT_FOUND,
                "chat attachment 'file-1' was not found in this workspace",
            ),
            (
                ApplicationError::InvalidWorkspaceSetting {
                    reason: text("bad"),
                },
                StatusCode::BAD_REQUEST,
                "invalid workspace setting: bad",
            ),
            (
                ApplicationError::WorkspaceQuota {
                    reason: text("full"),
                },
                StatusCode::BAD_REQUEST,
                "application workspace quota exceeded: full",
            ),
            (
                ApplicationError::InvalidArtifactKind { kind: text("blob") },
                StatusCode::BAD_REQUEST,
                "artifact kind 'blob' is not supported",
            ),
            (
                ApplicationError::InvalidWorkspaceIdentifier {
                    identifier: text("../x"),
                },
                StatusCode::BAD_REQUEST,
                "application workspace identifier '../x' is invalid",
            ),
            (
                ApplicationError::InvalidWorkspacePath { path: text("../y") },
                StatusCode::BAD_REQUEST,
                "workspace path '../y' is invalid",
            ),
            (
                ApplicationError::WorkspacePathNotFile { path: text("dir") },
                StatusCode::BAD_REQUEST,
                "workspace path 'dir' is not a regular file",
            ),
            (
                ApplicationError::InvalidAttachment {
                    reason: text("empty"),
                },
                StatusCode::BAD_REQUEST,
                "chat attachment is invalid: empty",
            ),
        ] {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status, "{detail}");
            assert_eq!(problem.body["detail"].as_str(), Some(detail));
        }

        let secret_path = ApplicationError::SecretValue {
            path: text("config.token"),
        };
        let detail = secret_path.to_string();
        assert!(detail.contains("at 'config.token'"));
        let problem = Problem::from(secret_path);
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(problem.body["detail"].as_str(), Some(detail.as_str()));
    }
}
