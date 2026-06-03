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

    /// A ClickHouse operation (DDL, insert, or query) failed.
    ///
    /// `operation` identifies the call site (e.g. `"write_batch"`,
    /// `"query_range"`) so the failure is greppable; `reason` carries the
    /// underlying `clickhouse::error::Error` message. The ClickHouse client
    /// error type is foreign and not `#[from]`-able onto this enum, so the CH
    /// store maps it explicitly via `map_err`.
    #[error("ClickHouse error during {operation}: {reason}")]
    ClickHouse { operation: String, reason: String },

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
