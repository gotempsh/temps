// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How [`UserServiceError`] becomes an HTTP problem response for the user and
//! account handlers in [`super`].

use axum::http::StatusCode;
use temps_core::problemdetails::{new as problem_new, Problem};
use tracing::error;

use super::mfa_locked_problem;
use crate::user_service::UserServiceError;

impl From<UserServiceError> for Problem {
    fn from(err: UserServiceError) -> Self {
        match err {
            UserServiceError::DatabaseConnection(msg) => {
                error!("Database connection error: {}", msg);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database connection error")
                    .with_detail(msg)
            }
            UserServiceError::Database { reason } => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database error")
                .with_detail(reason),
            UserServiceError::NotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_title("User not found")
                .with_detail(msg),
            UserServiceError::RoleNotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_title("Role not found")
                .with_detail(msg),
            UserServiceError::Mfa(msg) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("MFA error")
                .with_detail(msg),
            UserServiceError::InvalidMfaCode => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid MFA code")
                .with_detail("The provided MFA code is invalid"),
            UserServiceError::MfaNotSetup(user_id) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("MFA not setup")
                .with_detail(format!("MFA is not setup for user {}", user_id)),
            UserServiceError::MfaAlreadyEnabled(user_id) => problem_new(StatusCode::CONFLICT)
                .with_title("MFA already enabled")
                .with_detail(format!(
                    "MFA is already enabled for user {}. Disable it with current MFA verification before setting it up again.",
                    user_id
                )),
            UserServiceError::AlreadyDeleted(user_id) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("User already deleted")
                .with_detail(format!("User {} is already deleted", user_id)),
            UserServiceError::NotDeleted(user_id) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("User not deleted")
                .with_detail(format!("User {} is not deleted", user_id)),
            UserServiceError::RoleAlreadyAssigned(role, user_id) => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Role already assigned")
                    .with_detail(format!(
                        "Role {} is already assigned to user {}",
                        role, user_id
                    ))
            }
            UserServiceError::RoleNotAssigned(role, user_id) => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Role not assigned")
                    .with_detail(format!("Role {} is not assigned to user {}", role, user_id))
            }
            UserServiceError::Validation(msg) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Validation error")
                .with_detail(msg),
            UserServiceError::EmailTaken { .. } => problem_new(StatusCode::CONFLICT)
                .with_title("Email already in use")
                .with_detail(err.to_string()),
            UserServiceError::Encryption(msg) => {
                error!("Encryption error: {}", msg);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::Io(e) => {
                error!("IO error: {}", e);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::Serialization(e) => {
                error!("Serialization error: {}", e);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::Internal(msg) => {
                error!("Internal error: {}", msg);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail("An unexpected error occurred. Please try again.")
            }
            UserServiceError::CurrentPasswordRequired { .. } => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Current Password Required")
                    .with_detail("Provide your current password to enroll MFA.")
            }
            UserServiceError::InvalidCurrentPassword { .. } => {
                problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Invalid Current Password")
                    .with_detail("The current password you entered is incorrect.")
            }
            UserServiceError::MfaLocked {
                retry_after_seconds,
                ..
            } => mfa_locked_problem(retry_after_seconds),
            e @ UserServiceError::IngressGrantRevocation { .. } => {
                let message = e.to_string();
                error!("{}", message);
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Access Revocation Failed")
                    .with_detail(message)
            }
            UserServiceError::PasswordResetOnDeletedUser { user_id } => {
                problem_new(StatusCode::CONFLICT)
                    .with_title("User Deleted")
                    .with_detail(format!(
                        "User {} is deleted. Restore the user before resetting their password.",
                        user_id
                    ))
            }
            UserServiceError::EmailVerificationOnDeletedUser { user_id } => {
                problem_new(StatusCode::CONFLICT)
                    .with_title("User Deleted")
                    .with_detail(format!(
                        "User {} is deleted. Restore the user before verifying their email address.",
                        user_id
                    ))
            }
            UserServiceError::MfaResetOnDeletedUser { user_id } => {
                problem_new(StatusCode::CONFLICT)
                    .with_title("User Deleted")
                    .with_detail(format!(
                        "User {} is deleted. Restore the user before resetting their MFA.",
                        user_id
                    ))
            }
            UserServiceError::MfaNotEnrolled { user_id } => problem_new(StatusCode::CONFLICT)
                .with_title("MFA Not Enrolled")
                .with_detail(format!(
                    "User {} has no multi-factor authentication enrolled, so there is nothing to reset.",
                    user_id
                )),
        }
    }
}
