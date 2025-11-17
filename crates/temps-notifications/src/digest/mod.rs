//! Weekly digest module for aggregating and sending weekly summary emails

pub mod digest_data;
pub mod digest_service;
pub mod scheduler;
pub mod templates;

pub use digest_data::*;
pub use digest_service::DigestService;
pub use scheduler::DigestScheduler;
