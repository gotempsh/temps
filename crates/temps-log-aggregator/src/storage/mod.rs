// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pluggable storage backends for log chunks
//!
//! Both backends implement the same `LogStorage` trait: `write_chunk`, `read_chunk`,
//! `list_chunks`, `delete_chunk`. Switching backends requires only a config change.

mod filesystem;
mod s3;
pub(crate) mod traits;

pub use filesystem::FilesystemStorage;
pub use s3::S3Storage;
pub use traits::LogStorage;
