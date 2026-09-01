// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared resource limits for static artifact ingestion.
//!
//! These bounds cover both filesystem deployments and Docker archive
//! extraction so neither path can silently accept a shape the other rejects.

/// Maximum number of filesystem/archive entries in one static artifact.
pub const MAX_STATIC_ENTRIES: u32 = 20_000;
/// Maximum bytes in a single static artifact file.
pub const MAX_STATIC_ENTRY_BYTES: u64 = 500 * 1024 * 1024;
/// Maximum bytes across all files in one static artifact.
pub const MAX_STATIC_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
