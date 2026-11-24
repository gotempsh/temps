// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service layer for the log aggregator

mod chunk_writer;
mod collector;
mod compactor;
mod forget_sweeper;
pub(crate) mod global_search;
mod ingest_budget;
pub(crate) mod log_series;
mod metadata;
mod reindexer;
mod remote_collector;
mod retention;
pub(crate) mod saved_search;
pub(crate) mod search;
mod tail;

/// Startup's activate-index-backend transition (`crate::plugin`) reuses the
/// seal path's retry helper — same "transient DB hiccup, not a reason to
/// give up" reasoning, one backoff schedule to keep in sync.
pub(crate) use chunk_writer::retry_with_backoff;
pub use chunk_writer::{ChunkWriterService, CollectionStatus, ManifestSink, RecoveryState};
pub use collector::CollectorService;
pub use compactor::{CompactorService, GC_GRACE};
pub use forget_sweeper::{
    ForgetSweepReport, ForgetSweeper, DEFAULT_FORGET_BATCH, FORGET_SWEEP_BURST_PAUSE,
    FORGET_SWEEP_INTERVAL,
};
pub use ingest_budget::{
    Admission, IngestBudget, IngestDropReason, IngestLimiter, IngestLimits, IngestOverride,
    IngestOverrides, IngestSource, ThrottledLogSource, BURST_SECONDS, STATS_WINDOW,
    THROTTLED_VISIBLE_FOR,
};
pub use log_series::{
    LogSeries, LogSeriesMethod, LogSeriesPoint, LogSeriesService, SeriesRequest, COUNT_PAGE_SIZE,
    MAX_NUMERIC_SERIES_BUCKETS, MAX_SERIES_BUCKETS, SERIES_SCAN_CAP,
};
pub use metadata::{LogEventsQuery, LogMetadataService};
pub use reindexer::{ReindexReport, ReindexService, DEFAULT_REINDEX_BATCH};
pub use remote_collector::{
    RemoteContainerInfo, RemoteContainerLogSource, RemoteLogCollectorService, RemoteLogSourceError,
    RemoteLogStream,
};
pub use retention::RetentionService;
pub use saved_search::{
    decode_query as decode_saved_search_query, LogSavedSearchService, NewSavedSearch,
    SavedSearchQuery, SavedSearchUpdate, MAX_SAVED_SEARCHES_PER_PROJECT, SAVED_SEARCH_TIME_RANGES,
};
pub use search::LogSearchService;
pub use tail::TailService;
