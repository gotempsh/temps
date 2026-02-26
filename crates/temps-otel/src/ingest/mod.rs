//! OTLP ingest pipeline.
//!
//! Handles protobuf decoding, decompression, auth, rate limiting,
//! and routing to the appropriate storage path.

pub mod auth;
pub mod decode;
pub mod rate_limit;
pub mod sampler;
