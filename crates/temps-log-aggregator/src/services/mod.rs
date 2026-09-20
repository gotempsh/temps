// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service layer for the log aggregator

mod chunk_writer;
mod collector;
mod compactor;
pub(crate) mod global_search;
mod metadata;
mod reindexer;
mod remote_collector;
mod retention;
pub(crate) mod search;
mod tail;

pub use chunk_writer::{ChunkWriterService, ManifestSink};
pub use collector::CollectorService;
pub use compactor::{CompactorService, GC_GRACE};
pub use metadata::{LogEventsQuery, LogMetadataService};
pub use reindexer::{ReindexReport, ReindexService, DEFAULT_REINDEX_BATCH};
pub use remote_collector::{
    RemoteContainerInfo, RemoteContainerLogSource, RemoteLogCollectorService, RemoteLogSourceError,
    RemoteLogStream,
};
pub use retention::RetentionService;
pub use search::LogSearchService;
pub use tail::TailService;
