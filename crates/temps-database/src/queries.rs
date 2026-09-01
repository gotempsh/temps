// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Common database query utilities

use temps_core::PaginationParams;

/// Normalize pagination parameters
pub fn normalize_pagination(params: PaginationParams) -> (u64, u64) {
    params.normalize()
}

/// Placeholder for future query utilities
pub struct QueryUtils;
