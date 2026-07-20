//! Error types for email tracking

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EmailTrackingError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Email service error while processing SNS notification: {0}")]
    Email(#[from] temps_email::EmailError),

    #[error("SNS validation failed: {0}")]
    SnsValidation(String),

    #[error("SNS notification for provider message {provider_message_id} matched multiple emails")]
    AmbiguousProviderMessage { provider_message_id: String },

    #[error("SNS recipient {recipient} was not present on email {email_id}")]
    RecipientMismatch { email_id: String, recipient: String },

    #[error("SNS topic {topic_arn} is not bound to email {email_id}'s provider")]
    TopicMismatch { email_id: String, topic_arn: String },

    #[error("HTML rewrite failed for email {email_id}: {reason}")]
    HtmlRewrite { email_id: String, reason: String },

    #[error("HMAC verification failed for email {email_id}")]
    HmacVerification { email_id: String },

    #[error("Invalid event type: {0}")]
    InvalidEventType(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl From<EmailTrackingError> for Problem {
    fn from(error: EmailTrackingError) -> Self {
        match error {
            EmailTrackingError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(error.to_string())
            }
            EmailTrackingError::Email(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Email Processing Error")
                .with_detail(error.to_string()),
            EmailTrackingError::SnsValidation(_) => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("SNS Validation Failed")
                .with_detail(error.to_string()),
            EmailTrackingError::AmbiguousProviderMessage { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Ambiguous Provider Message")
                    .with_detail(error.to_string())
            }
            EmailTrackingError::RecipientMismatch { .. } => {
                problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("SNS Recipient Mismatch")
                    .with_detail(error.to_string())
            }
            EmailTrackingError::TopicMismatch { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("SNS Topic Mismatch")
                .with_detail(error.to_string()),
            EmailTrackingError::HtmlRewrite { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Tracking Rewrite Failed")
                    .with_detail(error.to_string())
            }
            EmailTrackingError::HmacVerification { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Tracking ID")
                    .with_detail(error.to_string())
            }
            EmailTrackingError::InvalidEventType(_) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Event Type")
                .with_detail(error.to_string()),
            EmailTrackingError::Configuration(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Configuration Error")
                    .with_detail(error.to_string())
            }
            EmailTrackingError::Serialization(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Serialization Error")
                    .with_detail(error.to_string())
            }
        }
    }
}
