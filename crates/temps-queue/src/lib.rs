//! Implementation of job queue using tokio channels
//! This crate implements the JobQueue trait from temps-core using tokio's
//! broadcast and mpsc channels.

pub mod jobs;
pub mod plugin;
pub mod queue;
pub mod subscriber;

pub use jobs::*;
pub use plugin::QueuePlugin;
pub use queue::*;
pub use subscriber::*;

// Re-export core traits for convenience
pub use temps_core::{JobQueue, JobReceiver, QueueError};