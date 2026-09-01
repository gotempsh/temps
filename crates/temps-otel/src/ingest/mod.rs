// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OTLP ingest pipeline.
//!
//! Handles protobuf decoding, decompression, auth, rate limiting,
//! and routing to the appropriate storage path.

pub mod auth;
pub mod decode;
pub mod quota_cache;
pub mod rate_limit;
pub mod sampler;
