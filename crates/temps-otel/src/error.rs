use thiserror::Error;

/// Domain error type for the OTel subsystem.
#[derive(Error, Debug)]
pub enum OtelError {
    #[error("Authentication failed for project: {reason}")]
    AuthFailed { reason: String },

    #[error("Invalid API key format")]
    InvalidApiKey,

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Rate limit exceeded for project {project_id}: {limit} requests/min")]
    RateLimitExceeded { project_id: i32, limit: u32 },

    #[error("Rate limit exceeded for service {service_id}: {limit} requests/min")]
    ServiceRateLimitExceeded { service_id: i32, limit: u32 },

    #[error(
        "Storage quota exceeded for project {project_id}: used {used_bytes} of {limit_bytes} bytes"
    )]
    QuotaExceeded {
        project_id: i32,
        used_bytes: u64,
        limit_bytes: u64,
    },

    #[error("Failed to decode protobuf payload: {reason}")]
    ProtobufDecode { reason: String },

    #[error("Failed to decompress request body ({encoding}): {reason}")]
    DecompressionFailed { encoding: String, reason: String },

    #[error("Unsupported content encoding: {encoding}")]
    UnsupportedEncoding { encoding: String },

    #[error("Storage error: {message}")]
    Storage { message: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("S3 error for project {project_id}: {reason}")]
    S3 { project_id: i32, reason: String },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Metric dashboard {dashboard_id} not found")]
    DashboardNotFound { dashboard_id: i32 },

    #[error("Metric alert rule {rule_id} not found")]
    MetricAlertNotFound { rule_id: i32 },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Internal error: {message}")]
    Internal { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_auth_failed() {
        let err = OtelError::AuthFailed {
            reason: "invalid token".into(),
        };
        assert_eq!(
            err.to_string(),
            "Authentication failed for project: invalid token"
        );
    }

    #[test]
    fn test_display_invalid_api_key() {
        let err = OtelError::InvalidApiKey;
        assert_eq!(err.to_string(), "Invalid API key format");
    }

    #[test]
    fn test_display_project_not_found() {
        let err = OtelError::ProjectNotFound { project_id: 42 };
        assert_eq!(err.to_string(), "Project 42 not found");
    }

    #[test]
    fn test_display_rate_limit_exceeded() {
        let err = OtelError::RateLimitExceeded {
            project_id: 7,
            limit: 500,
        };
        assert_eq!(
            err.to_string(),
            "Rate limit exceeded for project 7: 500 requests/min"
        );
    }

    #[test]
    fn test_display_quota_exceeded() {
        let err = OtelError::QuotaExceeded {
            project_id: 1,
            used_bytes: 1000,
            limit_bytes: 500,
        };
        assert_eq!(
            err.to_string(),
            "Storage quota exceeded for project 1: used 1000 of 500 bytes"
        );
    }

    #[test]
    fn test_display_protobuf_decode() {
        let err = OtelError::ProtobufDecode {
            reason: "truncated message".into(),
        };
        assert_eq!(
            err.to_string(),
            "Failed to decode protobuf payload: truncated message"
        );
    }

    #[test]
    fn test_display_decompression_failed() {
        let err = OtelError::DecompressionFailed {
            encoding: "gzip".into(),
            reason: "corrupt header".into(),
        };
        assert_eq!(
            err.to_string(),
            "Failed to decompress request body (gzip): corrupt header"
        );
    }

    #[test]
    fn test_display_unsupported_encoding() {
        let err = OtelError::UnsupportedEncoding {
            encoding: "brotli".into(),
        };
        assert_eq!(err.to_string(), "Unsupported content encoding: brotli");
    }

    #[test]
    fn test_display_storage() {
        let err = OtelError::Storage {
            message: "disk full".into(),
        };
        assert_eq!(err.to_string(), "Storage error: disk full");
    }

    #[test]
    fn test_display_s3() {
        let err = OtelError::S3 {
            project_id: 3,
            reason: "timeout".into(),
        };
        assert_eq!(err.to_string(), "S3 error for project 3: timeout");
    }

    #[test]
    fn test_display_validation() {
        let err = OtelError::Validation {
            message: "empty name".into(),
        };
        assert_eq!(err.to_string(), "Validation error: empty name");
    }

    #[test]
    fn test_display_internal() {
        let err = OtelError::Internal {
            message: "unexpected state".into(),
        };
        assert_eq!(err.to_string(), "Internal error: unexpected state");
    }

    #[test]
    fn test_from_db_err() {
        let db_err = sea_orm::DbErr::Custom("connection refused".into());
        let otel_err: OtelError = db_err.into();
        assert!(otel_err.to_string().contains("connection refused"));
    }

    #[test]
    fn test_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let otel_err: OtelError = io_err.into();
        assert!(otel_err.to_string().contains("file missing"));
    }

    #[test]
    fn test_from_serde_error() {
        let serde_err = serde_json::from_str::<String>("invalid").unwrap_err();
        let otel_err: OtelError = serde_err.into();
        assert!(otel_err.to_string().contains("Serialization error"));
    }
}
