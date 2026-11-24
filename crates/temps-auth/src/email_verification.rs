// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email verification: what `users.email_verified` means and how the
//! verification credential is stored.
//!
//! `email_verified` records that something trusted vouched for the address:
//! an administrator (creating the user, changing their email, or using
//! "Mark email as verified"), the initial admin bootstrap, an identity
//! provider asserting `email_verified=true`, or the user clicking a link sent
//! to that address. It deliberately does not depend on email delivery, because
//! many self-hosted instances have no email provider. Self-registration and a
//! user changing their own email leave the address unverified.
//!
//! SSO account linking relies on it: an identity provider login is only linked
//! onto an existing local account whose address is verified (see
//! `OidcService::resolve_user`).

use axum::http::StatusCode;
use temps_core::problemdetails::Problem;
use thiserror::Error;

/// How long an emailed verification link stays valid.
pub(crate) const VERIFICATION_TOKEN_TTL_HOURS: i64 = 24;

/// Minimum time between two verification emails requested by the same user.
/// Bounds how often a user can make the instance mail an address, on top of
/// the per-IP limiter on the route.
pub(crate) const RESEND_COOLDOWN_SECONDS: i64 = 60;

/// The value persisted in `users.email_verification_token` for `token`.
///
/// Only the SHA-256 digest is stored, so a database read (a backup, a log of
/// a failed query) does not yield a usable verification link. The raw token
/// is a random UUID with 122 bits of entropy, so an unsalted fast hash is
/// sufficient: there is nothing to brute-force.
pub(crate) fn hash_verification_token(token: &str) -> String {
    temps_core::crypto::sha256_hex(token)
}

#[derive(Debug, Error)]
pub enum EmailVerificationError {
    #[error(
        "No email provider is configured, so a verification email cannot be sent to user {user_id}"
    )]
    EmailProviderNotConfigured { user_id: i32 },

    #[error("The email address {email} of user {user_id} is already verified")]
    AlreadyVerified { user_id: i32, email: String },

    #[error(
        "A verification email was sent to user {user_id} less than {cooldown_seconds}s ago; retry in {retry_after_seconds}s"
    )]
    RecentlySent {
        user_id: i32,
        cooldown_seconds: i64,
        retry_after_seconds: i64,
    },

    #[error("User {user_id} not found")]
    UserNotFound { user_id: i32 },

    #[error("Failed to send the verification email to {email} for user {user_id}: {reason}")]
    DeliveryFailed {
        user_id: i32,
        email: String,
        reason: String,
    },

    #[error("Database error while issuing an email verification for user {user_id}: {source}")]
    Database {
        user_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl From<EmailVerificationError> for Problem {
    fn from(error: EmailVerificationError) -> Self {
        match error {
            EmailVerificationError::EmailProviderNotConfigured { .. } => {
                temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                    .title("No Email Provider Configured")
                    .detail(
                        "No email provider is configured on this Temps instance, so a \
                         verification email cannot be sent. Ask an administrator to verify \
                         your email address.",
                    )
                    .value("error_code", "EMAIL_PROVIDER_NOT_CONFIGURED")
                    .build()
            }
            EmailVerificationError::AlreadyVerified { email, .. } => {
                temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                    .title("Email Already Verified")
                    .detail(format!("The email address {email} is already verified."))
                    .value("error_code", "EMAIL_ALREADY_VERIFIED")
                    .build()
            }
            EmailVerificationError::RecentlySent {
                retry_after_seconds,
                ..
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
                .title("Verification Email Recently Sent")
                .detail(format!(
                    "A verification email was sent recently. Check your inbox, or try again in {retry_after_seconds}s."
                ))
                .value("error_code", "EMAIL_VERIFICATION_RECENTLY_SENT")
                .value("retry_after_seconds", retry_after_seconds)
                .build(),
            EmailVerificationError::UserNotFound { user_id } => {
                temps_core::error_builder::not_found()
                    .title("User Not Found")
                    .detail(format!("User {user_id} was not found"))
                    .build()
            }
            EmailVerificationError::DeliveryFailed {
                user_id,
                ref reason,
                ..
            } => {
                tracing::warn!(user_id, %reason, "Verification email delivery failed");
                temps_core::error_builder::ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                    .title("Verification Email Not Sent")
                    .detail(
                        "The email provider could not deliver the verification email. Try \
                         again later, or ask an administrator to verify your email address.",
                    )
                    .value("error_code", "EMAIL_VERIFICATION_DELIVERY_FAILED")
                    .build()
            }
            EmailVerificationError::Database { user_id, ref source } => {
                tracing::error!(user_id, %source, "Email verification database error");
                temps_core::error_builder::internal_server_error()
                    .title("Internal Server Error")
                    .detail(
                        "A database error occurred while issuing the verification email. Check \
                         the server logs for details.",
                    )
                    .build()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_token_is_a_digest_not_the_token() {
        let token = "0f5a7c1e-3b2d-4c8e-9a6f-1d2e3f4a5b6c";
        let stored = hash_verification_token(token);
        assert_ne!(stored, token);
        assert_eq!(stored.len(), 64, "hex-encoded SHA-256");
        assert_eq!(stored, hash_verification_token(token), "deterministic");
        assert_ne!(
            hash_verification_token(&stored),
            stored,
            "presenting the stored digest must not match it"
        );
    }

    #[test]
    fn no_email_provider_is_a_conflict_that_says_to_ask_an_admin() {
        let problem: Problem =
            EmailVerificationError::EmailProviderNotConfigured { user_id: 7 }.into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let body = serde_json::to_string(&problem.body).unwrap_or_default();
        assert!(body.contains("EMAIL_PROVIDER_NOT_CONFIGURED"), "{body}");
        assert!(body.contains("Ask an administrator"), "{body}");
    }

    #[test]
    fn each_variant_maps_to_its_status() {
        let cases: Vec<(EmailVerificationError, StatusCode)> = vec![
            (
                EmailVerificationError::AlreadyVerified {
                    user_id: 1,
                    email: "user@example.com".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                EmailVerificationError::RecentlySent {
                    user_id: 1,
                    cooldown_seconds: RESEND_COOLDOWN_SECONDS,
                    retry_after_seconds: 30,
                },
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (
                EmailVerificationError::UserNotFound { user_id: 1 },
                StatusCode::NOT_FOUND,
            ),
            (
                EmailVerificationError::DeliveryFailed {
                    user_id: 1,
                    email: "user@example.com".into(),
                    reason: "smtp refused".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                EmailVerificationError::Database {
                    user_id: 1,
                    source: sea_orm::DbErr::Custom("boom".into()),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            let label = error.to_string();
            let problem: Problem = error.into();
            assert_eq!(problem.status_code, status, "{label}");
        }
    }

    #[test]
    fn delivery_and_database_details_are_not_reflected() {
        let problem: Problem = EmailVerificationError::DeliveryFailed {
            user_id: 1,
            email: "user@example.com".into(),
            reason: "550 relay denied by smtp.internal".into(),
        }
        .into();
        let body = serde_json::to_string(&problem.body).unwrap_or_default();
        assert!(!body.contains("relay denied"), "{body}");

        let problem: Problem = EmailVerificationError::Database {
            user_id: 1,
            source: sea_orm::DbErr::Custom("relation \"users\" missing".into()),
        }
        .into();
        let body = serde_json::to_string(&problem.body).unwrap_or_default();
        assert!(!body.contains("relation"), "{body}");
    }
}
