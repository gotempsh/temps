//! Service layer for the log aggregator

mod chunk_writer;
mod collector;
mod metadata;
mod remote_collector;
mod retention;
mod search;
mod tail;

pub use chunk_writer::{ChunkWriterService, FlushResult};
pub use collector::CollectorService;
pub use metadata::{LogEventsQuery, LogMetadataService};
pub use remote_collector::{
    RemoteContainerInfo, RemoteContainerLogSource, RemoteLogCollectorService, RemoteLogSourceError,
    RemoteLogStream,
};
pub use retention::RetentionService;
pub use search::LogSearchService;
pub use tail::TailService;
