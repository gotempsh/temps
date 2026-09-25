// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service layer for the log aggregator

mod chunk_writer;
mod collector;
mod compactor;
mod forget_sweeper;
pub(crate) mod global_search;
mod metadata;
mod reindexer;
mod remote_collector;
mod retention;
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
pub use metadata::{LogEventsQuery, LogMetadataService};
pub use reindexer::{ReindexReport, ReindexService, DEFAULT_REINDEX_BATCH};
pub use remote_collector::{
    RemoteContainerInfo, RemoteContainerLogSource, RemoteLogCollectorService, RemoteLogSourceError,
    RemoteLogStream,
};
pub use retention::RetentionService;
pub use search::LogSearchService;
pub use tail::TailService;
