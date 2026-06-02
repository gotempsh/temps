use thiserror::Error;

#[derive(Error, Debug)]
pub enum MetricsError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Serialization error: failed to serialize metric labels")]
    SerializationError,

    #[error("Metric not found")]
    NotFound,

    #[error("Not implemented: this metrics store backend does not support this operation")]
    NotImplemented,

    #[error("Collector connection failed for source_id={source_id} engine={engine}: {reason}")]
    CollectorConnectionFailed {
        source_id: i32,
        engine: String,
        reason: String,
    },

    #[error(
        "Collector query timed out for source_id={source_id} engine={engine} after {timeout_secs}s"
    )]
    CollectorTimeout {
        source_id: i32,
        engine: String,
        timeout_secs: u64,
    },

    #[error("Collector query failed for source_id={source_id} engine={engine}: {reason}")]
    CollectorQueryFailed {
        source_id: i32,
        engine: String,
        reason: String,
    },
}
