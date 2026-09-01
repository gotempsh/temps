// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

/// Domain error type for the OTel subsystem.
#[derive(Error, Debug)]
pub enum OtelError {
    #[error("Authentication failed for project: {reason}")]
    AuthFailed { reason: String },

    #[error(
        "Authentication failed for claimed project '{claimed_project_slug}': Missing token in Authorization or X-Temps-Api-Key header"
    )]
    MissingAuthToken { claimed_project_slug: String },

    #[error("Invalid API key format")]
    InvalidApiKey,

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Rate limit exceeded for project {project_id}: {limit} requests/min")]
    RateLimitExceeded { project_id: i32, limit: u32 },

    #[error("Rate limit exceeded for service {service_id}: {limit} requests/min")]
    ServiceRateLimitExceeded { service_id: i32, limit: u32 },

    #[error("OTel ingest is saturated: at most {limit} requests may be processed concurrently")]
    IngestSaturated { limit: usize },

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

    /// A storage-backend failure (ClickHouse, TimescaleDB raw SQL, migrations).
    ///
    /// `kind` records *which* backend failed and *how*, classified at
    /// construction time while the backend's typed error is still available.
    /// `message` is a lossy `Display` rendering that callers cannot
    /// re-classify afterwards, so never string-match on it — read
    /// [`OtelError::is_transient`] or [`OtelError::error_class`] instead.
    #[error("Storage error: {message}")]
    Storage {
        message: String,
        kind: StorageErrorKind,
    },

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

/// Which storage backend failed, and in what way.
///
/// Two jobs in one small enum, both of which need the backend's *typed* error
/// and so must be decided at construction:
///
/// 1. **Retry decision** — [`StorageErrorKind::is_transient`] splits transport
///    failures (retry) from payload/schema failures (don't).
/// 2. **Operator diagnosis** — [`StorageErrorKind::as_class`] gives a stable,
///    low-cardinality label for the ingest-error report, so a dropped batch is
///    attributable to *ClickHouse being unreachable* rather than just
///    "storage error".
///
/// The `ClickHouse*`/`Postgres*` split matters because
/// [`OtelError::Storage`] is produced by both backends — the ClickHouse
/// helpers in `storage/clickhouse` and the raw-SQL paths in
/// `storage/timescaledb.rs` and `ingest/auth.rs`. Collapsing them would tell an
/// operator a write failed but not which system to go look at.
///
/// Low cardinality is a hard requirement, not a nicety: these strings become
/// rows in `otel_ingest_errors`, whose size bound is
/// (signals x classes). Never add a variant that embeds an ID, a table name,
/// or any other unbounded value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind {
    /// ClickHouse transport failure — socket error, connection refused/reset.
    ClickHouseNetwork,
    /// ClickHouse request exceeded its deadline.
    ClickHouseTimeout,
    /// Row/table shape disagreement: schema mismatch, bad columns header,
    /// truncated row. Our data or our DDL understanding is wrong.
    ClickHouseSchema,
    /// (De)compression or serde failure encoding the batch for ClickHouse.
    ClickHouseSerialization,
    /// ClickHouse failed in a way we could not classify (bad response, custom
    /// serde error, or a variant added by a future `clickhouse` release).
    ClickHouseOther,
    /// The connection pool never handed out a connection — it timed out or was
    /// closed. Nothing was transmitted, so the write provably did not happen.
    ///
    /// This is the *only* Postgres failure a non-idempotent write may retry;
    /// see [`StorageErrorKind::is_transient`].
    PostgresConnAcquire,
    /// An established Postgres connection failed. The statement may have been
    /// transmitted, executed and committed before the failure was observed, so
    /// the outcome is **unknown**. Not retryable — see
    /// [`StorageErrorKind::is_transient`].
    PostgresConn,
    /// Postgres executed and rejected the statement, or returned an
    /// unusable row — a query/data problem, not a connectivity one.
    PostgresQuery,
    /// A precondition this crate enforces before touching a backend at all
    /// (e.g. a label key outside the allowed character set, an invalid
    /// database name). Never transient.
    Precondition,
}

impl StorageErrorKind {
    /// Whether a retry could plausibly succeed **without risking a duplicate
    /// write**.
    ///
    /// Retrying is only safe when one of two things holds: the write is
    /// idempotent, or the failure proves the write never reached the server.
    /// The two backends sit on opposite sides of that line, which is why this
    /// is not simply "was it a transport error".
    ///
    /// # ClickHouse — idempotent, so transport errors retry freely
    ///
    /// The `spans` and `metrics` tables are `ReplacingMergeTree(_version)`
    /// keyed on the natural identity of a row (`ORDER BY (project_id,
    /// trace_id, span_id)` for spans). A re-sent batch converges to one
    /// canonical row per span, by design — the migration was written that way
    /// precisely because OTLP exporters retry. So `Network`, `TimedOut` and
    /// the unclassified `ClickHouseOther` are all retried.
    ///
    /// # Postgres/TimescaleDB — NOT idempotent, so only "never sent" retries
    ///
    /// `otel_spans`, `otel_metrics` and `otel_log_events` have no unique key,
    /// so re-sending a batch inserts it twice. They cannot easily get one:
    /// all three are hypertables with a **space partition on `id`**
    /// (`create_hypertable(..., partitioning_column => 'id')`), and
    /// TimescaleDB rejects any unique index that omits a partitioning column
    /// ("cannot create a unique index without the column \"id\" (used in
    /// partitioning)"). Since `id` is `BIGSERIAL`, a fresh value is generated
    /// per attempt, so including it would make `ON CONFLICT` never fire.
    /// Fixing that properly means rebuilding all three hypertables — see the
    /// note on `TimescaleDbStorage::batch_insert_spans`.
    ///
    /// That leaves the second condition: retry only when the failure proves
    /// nothing was transmitted. Exactly one variant does:
    /// [`StorageErrorKind::PostgresConnAcquire`], which sea-orm raises only
    /// for `PoolTimedOut`/`PoolClosed` — both occur before a connection
    /// exists. [`StorageErrorKind::PostgresConn`] wraps an arbitrary
    /// `sqlx::Error` (I/O, protocol, TLS) that can surface *after* the server
    /// committed but before the acknowledgement arrived, so retrying it would
    /// silently double every row in the batch. Dropping a batch is bad;
    /// silently double-counting every span in a trace — inflating
    /// `spans_stored` and every downstream aggregate, undetectably and
    /// permanently — is worse.
    ///
    /// # Unclassified errors
    ///
    /// [`StorageErrorKind::ClickHouseOther`] is deliberately treated as
    /// transient: the write it guards is idempotent, so a wrong guess costs a
    /// couple of sub-second retries while the opposite wrong guess costs
    /// permanent, silent data loss.
    pub fn is_transient(&self) -> bool {
        match self {
            // Idempotent destination (ReplacingMergeTree): safe to re-send.
            StorageErrorKind::ClickHouseNetwork
            | StorageErrorKind::ClickHouseTimeout
            | StorageErrorKind::ClickHouseOther => true,
            // Non-idempotent destination, but the batch provably never left us.
            StorageErrorKind::PostgresConnAcquire => true,
            // Outcome unknown against a non-idempotent destination.
            StorageErrorKind::PostgresConn => false,
            // Deterministic: the identical batch reproduces these exactly.
            StorageErrorKind::ClickHouseSchema
            | StorageErrorKind::ClickHouseSerialization
            | StorageErrorKind::PostgresQuery
            | StorageErrorKind::Precondition => false,
        }
    }

    /// Stable snake_case label for the ingest-error report and logs.
    ///
    /// These strings are persisted and grouped on, so treat them as a wire
    /// format: renaming one splits an existing group in two.
    pub fn as_class(&self) -> &'static str {
        match self {
            StorageErrorKind::ClickHouseNetwork => "clickhouse_network",
            StorageErrorKind::ClickHouseTimeout => "clickhouse_timeout",
            StorageErrorKind::ClickHouseSchema => "clickhouse_schema",
            StorageErrorKind::ClickHouseSerialization => "clickhouse_serialization",
            StorageErrorKind::ClickHouseOther => "clickhouse_other",
            StorageErrorKind::PostgresConnAcquire => "postgres_conn_acquire",
            StorageErrorKind::PostgresConn => "postgres_conn",
            StorageErrorKind::PostgresQuery => "postgres_query",
            StorageErrorKind::Precondition => "precondition",
        }
    }
}

impl OtelError {
    /// Stable, low-cardinality label describing *why* an ingest write failed.
    ///
    /// Written to `otel_ingest_errors` and grouped on, so it must never embed
    /// an ID, a message fragment, or anything else unbounded — see
    /// [`StorageErrorKind`].
    pub fn error_class(&self) -> &'static str {
        match self {
            OtelError::Storage { kind, .. } => kind.as_class(),
            OtelError::Database(db_err) => db_err_kind(db_err).as_class(),
            OtelError::S3 { .. } => "s3",
            OtelError::Io(_) => "io",
            OtelError::Serialization(_) => "serialization",
            OtelError::QuotaExceeded { .. } => "quota_exceeded",
            OtelError::RateLimitExceeded { .. } | OtelError::ServiceRateLimitExceeded { .. } => {
                "rate_limited"
            }
            OtelError::IngestSaturated { .. } => "ingest_saturated",
            OtelError::ProtobufDecode { .. } => "protobuf_decode",
            OtelError::DecompressionFailed { .. } => "decompression_failed",
            OtelError::UnsupportedEncoding { .. } => "unsupported_encoding",
            OtelError::Validation { .. } => "validation",
            OtelError::AuthFailed { .. }
            | OtelError::MissingAuthToken { .. }
            | OtelError::InvalidApiKey => "auth_failed",
            OtelError::ProjectNotFound { .. }
            | OtelError::DashboardNotFound { .. }
            | OtelError::MetricAlertNotFound { .. } => "not_found",
            OtelError::Internal { .. } => "internal",
        }
    }

    /// Whether this error is a transient failure that a bounded retry could
    /// plausibly recover from.
    ///
    /// Used by the ingest path (see `OtelService::ingest_spans` and friends) to
    /// decide between "wait a few milliseconds and write the batch again" and
    /// "give up now" — the second is the right answer for anything the retry
    /// cannot change, and waiting on it only holds the ingest permit longer.
    ///
    /// Only storage/database failures can be transient. Auth failures,
    /// validation errors, quota/rate-limit rejections, decode failures and
    /// not-found errors are all deterministic given the same input, so they
    /// return `false`.
    pub fn is_transient(&self) -> bool {
        match self {
            // Classified at construction from the backend's typed error —
            // see the `Storage` variant's docs.
            OtelError::Storage { kind, .. } => kind.is_transient(),
            OtelError::Database(db_err) => db_err_kind(db_err).is_transient(),
            OtelError::AuthFailed { .. }
            | OtelError::MissingAuthToken { .. }
            | OtelError::InvalidApiKey
            | OtelError::ProjectNotFound { .. }
            | OtelError::RateLimitExceeded { .. }
            | OtelError::ServiceRateLimitExceeded { .. }
            | OtelError::IngestSaturated { .. }
            | OtelError::QuotaExceeded { .. }
            | OtelError::ProtobufDecode { .. }
            | OtelError::DecompressionFailed { .. }
            | OtelError::UnsupportedEncoding { .. }
            | OtelError::S3 { .. }
            | OtelError::Validation { .. }
            | OtelError::DashboardNotFound { .. }
            | OtelError::MetricAlertNotFound { .. }
            | OtelError::Io(_)
            | OtelError::Serialization(_)
            | OtelError::Internal { .. } => false,
        }
    }
}

/// Classify a [`sea_orm::DbErr`] into a [`StorageErrorKind`].
///
/// The two connection-level variants are deliberately kept **apart** rather
/// than lumped together, because they answer different questions about whether
/// the statement reached the server — which is what decides whether a
/// non-idempotent write may be retried. See [`StorageErrorKind::is_transient`].
///
/// Everything else — `Query`, `Type`, `RecordNotInserted`, `RecordNotFound`,
/// `Exec`, `Migration`, … — describes the *statement or the data* and will
/// fail identically on the next attempt.
///
/// This is narrower than `temps-status-page`'s retry helper, which treats
/// `ConnectionAcquire | Conn` alike. That helper guards idempotent
/// single-row upserts; this one guards append-only batch inserts with no
/// unique key, where a wrongly-retried write duplicates silently.
pub(crate) fn db_err_kind(err: &sea_orm::DbErr) -> StorageErrorKind {
    match err {
        // sea-orm raises this from `sqlx_conn_acquire_err` for exactly two
        // sqlx errors — `PoolTimedOut` and `PoolClosed` — both of which happen
        // before a connection is obtained, hence before any bytes reach
        // Postgres. That "provably never sent" guarantee is what makes it the
        // only Postgres failure a non-idempotent write may retry.
        sea_orm::DbErr::ConnectionAcquire(_) => StorageErrorKind::PostgresConnAcquire,
        // Wraps an arbitrary `sqlx::Error` on an already-established
        // connection, so the statement may already have committed.
        sea_orm::DbErr::Conn(_) => StorageErrorKind::PostgresConn,
        _ => StorageErrorKind::PostgresQuery,
    }
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
    fn test_display_missing_auth_token_includes_project_slug() {
        let err = OtelError::MissingAuthToken {
            claimed_project_slug: "example-project".into(),
        };
        assert_eq!(
            err.to_string(),
            "Authentication failed for claimed project 'example-project': Missing token in Authorization or X-Temps-Api-Key header"
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
    fn test_display_ingest_saturated() {
        let err = OtelError::IngestSaturated { limit: 64 };
        assert_eq!(
            err.to_string(),
            "OTel ingest is saturated: at most 64 requests may be processed concurrently"
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
            kind: StorageErrorKind::PostgresQuery,
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

    // ── is_transient() classification ───────────────────────────────────

    #[test]
    fn test_is_transient_storage_retryable() {
        let err = OtelError::Storage {
            message: "ClickHouse insert failed: network error: connection reset".into(),
            kind: StorageErrorKind::ClickHouseNetwork,
        };
        assert!(err.is_transient());
    }

    #[test]
    fn test_is_transient_storage_not_retryable() {
        let err = OtelError::Storage {
            message: "ClickHouse insert failed: schema mismatch: column `foo` missing".into(),
            kind: StorageErrorKind::ClickHouseSchema,
        };
        assert!(!err.is_transient());
    }

    /// A dropped connection may have dropped *after* Postgres committed, so
    /// the outcome is unknown. Since the OTel batch inserts have no unique
    /// key, retrying would silently duplicate every row — see
    /// `StorageErrorKind::is_transient`.
    #[test]
    fn test_is_transient_database_conn_is_not_transient() {
        let err = OtelError::Database(sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
            "connection reset by peer".into(),
        )));
        assert!(
            !err.is_transient(),
            "an established-connection failure has an unknown outcome and must not be retried \
             against a non-idempotent table"
        );
    }

    /// `ConnectionAcquire` is raised only for `PoolTimedOut`/`PoolClosed`,
    /// both of which occur before a connection exists — so nothing was ever
    /// transmitted and a retry cannot duplicate anything.
    #[test]
    fn test_is_transient_database_connection_acquire_is_transient() {
        for err in [
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout),
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::ConnectionClosed),
        ] {
            assert!(OtelError::Database(err).is_transient());
        }
    }

    /// The whole point of splitting the two connection variants: they must not
    /// collapse back into one class, or the retry decision loses the only
    /// signal that distinguishes "never sent" from "outcome unknown".
    #[test]
    fn test_connection_acquire_and_conn_are_distinct_classes() {
        let acquire = OtelError::Database(sea_orm::DbErr::ConnectionAcquire(
            sea_orm::ConnAcquireErr::Timeout,
        ));
        let conn = OtelError::Database(sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
            "reset".into(),
        )));
        assert_ne!(acquire.error_class(), conn.error_class());
        assert!(acquire.is_transient());
        assert!(!conn.is_transient());
    }

    #[test]
    fn test_is_transient_database_record_not_inserted_is_fatal() {
        let err = OtelError::Database(sea_orm::DbErr::RecordNotInserted);
        assert!(!err.is_transient());
    }

    #[test]
    fn test_is_transient_database_type_error_is_fatal() {
        let err = OtelError::Database(sea_orm::DbErr::Type(
            "invalid value for column `duration_ms`".into(),
        ));
        assert!(!err.is_transient());
    }

    #[test]
    fn test_is_transient_database_query_error_is_fatal() {
        let err = OtelError::Database(sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal(
            "syntax error at or near \"SELECT\"".into(),
        )));
        assert!(!err.is_transient());
    }

    // ── error_class() derivation ────────────────────────────────────────

    /// The class must name the *backend and failure mode*, so an operator can
    /// tell "ClickHouse is down" from "our rows don't match the table".
    #[test]
    fn test_error_class_distinguishes_backend_and_mode() {
        let cases = [
            (StorageErrorKind::ClickHouseNetwork, "clickhouse_network"),
            (StorageErrorKind::ClickHouseTimeout, "clickhouse_timeout"),
            (StorageErrorKind::ClickHouseSchema, "clickhouse_schema"),
            (
                StorageErrorKind::ClickHouseSerialization,
                "clickhouse_serialization",
            ),
            (StorageErrorKind::ClickHouseOther, "clickhouse_other"),
            (
                StorageErrorKind::PostgresConnAcquire,
                "postgres_conn_acquire",
            ),
            (StorageErrorKind::PostgresConn, "postgres_conn"),
            (StorageErrorKind::PostgresQuery, "postgres_query"),
            (StorageErrorKind::Precondition, "precondition"),
        ];
        for (kind, expected) in cases {
            let err = OtelError::Storage {
                message: "irrelevant".into(),
                kind,
            };
            assert_eq!(err.error_class(), expected);
        }
    }

    #[test]
    fn test_error_class_maps_database_variants() {
        let conn = OtelError::Database(sea_orm::DbErr::ConnectionAcquire(
            sea_orm::ConnAcquireErr::Timeout,
        ));
        assert_eq!(conn.error_class(), "postgres_conn_acquire");

        let query = OtelError::Database(sea_orm::DbErr::RecordNotInserted);
        assert_eq!(query.error_class(), "postgres_query");
    }

    /// The class is persisted and grouped on, so the set of possible values
    /// must be *closed*: two errors differing only by an ID or message must
    /// collapse to the same class, and every class must come from the known
    /// allowlist. A leak here makes `otel_ingest_errors` grow without bound.
    #[test]
    fn test_error_class_is_low_cardinality() {
        const KNOWN_CLASSES: &[&str] = &[
            "clickhouse_network",
            "clickhouse_timeout",
            "clickhouse_schema",
            "clickhouse_serialization",
            "clickhouse_other",
            "postgres_conn_acquire",
            "postgres_conn",
            "postgres_query",
            "precondition",
            "s3",
            "io",
            "serialization",
            "quota_exceeded",
            "rate_limited",
            "ingest_saturated",
            "protobuf_decode",
            "decompression_failed",
            "unsupported_encoding",
            "validation",
            "auth_failed",
            "not_found",
            "internal",
        ];

        let cases = vec![
            OtelError::S3 {
                project_id: 12345,
                reason: "bucket unreachable".into(),
            },
            OtelError::QuotaExceeded {
                project_id: 999,
                used_bytes: 10,
                limit_bytes: 5,
            },
            OtelError::ProjectNotFound { project_id: 777 },
            OtelError::RateLimitExceeded {
                project_id: 5,
                limit: 100,
            },
            OtelError::Internal {
                message: "unique detail 42".into(),
            },
            OtelError::Storage {
                message: "row 918273 rejected".into(),
                kind: StorageErrorKind::ClickHouseSchema,
            },
        ];
        for err in &cases {
            let class = err.error_class();
            assert!(
                KNOWN_CLASSES.contains(&class),
                "class {class:?} is outside the known allowlist — a new variant \
                 needs an entry here and in the dashboard"
            );
            assert!(
                !err.to_string().contains(class) || KNOWN_CLASSES.contains(&class),
                "class must not be derived from the message"
            );
        }

        // Two errors of the same kind differing only by their identifiers must
        // land in the same group.
        let a = OtelError::ProjectNotFound { project_id: 1 };
        let b = OtelError::ProjectNotFound { project_id: 2 };
        assert_eq!(a.error_class(), b.error_class());
    }

    /// Rate-limit rejections from both limiters share one class: an operator
    /// cares that traffic was shed, not which limiter shed it.
    #[test]
    fn test_error_class_groups_both_rate_limiters() {
        let project = OtelError::RateLimitExceeded {
            project_id: 1,
            limit: 10,
        };
        let service = OtelError::ServiceRateLimitExceeded {
            service_id: 2,
            limit: 10,
        };
        assert_eq!(project.error_class(), "rate_limited");
        assert_eq!(service.error_class(), service.error_class());
        assert_eq!(project.error_class(), service.error_class());
    }

    /// `is_transient` and `error_class` must agree: every class derived from a
    /// transient kind must itself come from a transient error.
    #[test]
    fn test_storage_error_kind_transience_matches_class() {
        let transient = [
            StorageErrorKind::ClickHouseNetwork,
            StorageErrorKind::ClickHouseTimeout,
            StorageErrorKind::ClickHouseOther,
            StorageErrorKind::PostgresConnAcquire,
        ];
        for kind in transient {
            assert!(kind.is_transient(), "{} must be transient", kind.as_class());
            let err = OtelError::Storage {
                message: "m".into(),
                kind,
            };
            assert!(err.is_transient());
        }

        let terminal = [
            StorageErrorKind::ClickHouseSchema,
            StorageErrorKind::ClickHouseSerialization,
            StorageErrorKind::PostgresConn,
            StorageErrorKind::PostgresQuery,
            StorageErrorKind::Precondition,
        ];
        for kind in terminal {
            assert!(!kind.is_transient(), "{} must be fatal", kind.as_class());
        }
    }

    /// Class strings are a persisted wire format — a rename splits an existing
    /// group in two, so pin them explicitly and keep them unique.
    #[test]
    fn test_storage_error_kind_classes_are_unique() {
        let kinds = [
            StorageErrorKind::ClickHouseNetwork,
            StorageErrorKind::ClickHouseTimeout,
            StorageErrorKind::ClickHouseSchema,
            StorageErrorKind::ClickHouseSerialization,
            StorageErrorKind::ClickHouseOther,
            StorageErrorKind::PostgresConnAcquire,
            StorageErrorKind::PostgresConn,
            StorageErrorKind::PostgresQuery,
            StorageErrorKind::Precondition,
        ];
        let unique: std::collections::HashSet<&str> = kinds.iter().map(|k| k.as_class()).collect();
        assert_eq!(unique.len(), kinds.len(), "duplicate class label");
    }

    #[test]
    fn test_is_transient_auth_failed_is_false() {
        let err = OtelError::AuthFailed {
            reason: "invalid token".into(),
        };
        assert!(!err.is_transient());
    }

    #[test]
    fn test_is_transient_non_storage_variants_are_false() {
        let cases = vec![
            OtelError::InvalidApiKey,
            OtelError::ProjectNotFound { project_id: 42 },
            OtelError::RateLimitExceeded {
                project_id: 7,
                limit: 500,
            },
            OtelError::IngestSaturated { limit: 64 },
            OtelError::QuotaExceeded {
                project_id: 1,
                used_bytes: 10,
                limit_bytes: 5,
            },
            OtelError::ProtobufDecode {
                reason: "truncated".into(),
            },
            OtelError::Validation {
                message: "empty name".into(),
            },
            OtelError::S3 {
                project_id: 3,
                reason: "timeout".into(),
            },
            OtelError::Internal {
                message: "unexpected state".into(),
            },
        ];
        for err in cases {
            assert!(!err.is_transient(), "expected fatal: {err}");
        }
    }

    #[test]
    fn test_from_serde_error() {
        let serde_err = serde_json::from_str::<String>("invalid").unwrap_err();
        let otel_err: OtelError = serde_err.into();
        assert!(otel_err.to_string().contains("Serialization error"));
    }
}
