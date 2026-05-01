use thiserror::Error;

/// Top-level error type for the observability crate. Per CLAUDE.md:
/// structured fields, contextual messages, automatic conversion from
/// `sea_orm::DbErr`. The `From<ObservabilityError> for Problem` impl lives
/// in the handlers module so the HTTP layer owns the response mapping.
#[derive(Debug, Error)]
pub enum ObservabilityError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Event {event_id} of kind {kind} not found in project {project_id}")]
    EventNotFound {
        project_id: i32,
        kind: String,
        event_id: String,
    },

    #[error("Invalid kinds filter: '{value}'. Valid kinds: log, request, span, error, revenue")]
    InvalidKindsFilter { value: String },

    #[error("Invalid cursor: {reason}")]
    InvalidCursor { reason: String },

    #[error("Time range invalid: from={from} is after to={to}; the merge query needs from <= to")]
    InvalidTimeRange { from: String, to: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_err_converts_via_from() {
        let db_err = sea_orm::DbErr::Custom("boom".into());
        let err: ObservabilityError = db_err.into();
        assert!(matches!(err, ObservabilityError::Database(_)));
    }

    #[test]
    fn error_messages_carry_context() {
        let err = ObservabilityError::EventNotFound {
            project_id: 7,
            kind: "request".into(),
            event_id: "42".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("42"));
        assert!(msg.contains("request"));
        assert!(msg.contains("7"));
    }

    #[test]
    fn invalid_kinds_filter_lists_valid_options() {
        let err = ObservabilityError::InvalidKindsFilter {
            value: "blob".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("blob"));
        assert!(msg.contains("log"));
        assert!(msg.contains("revenue"));
    }
}
