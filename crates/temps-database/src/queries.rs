//! Common database query utilities

use temps_core::PaginationParams;

/// Normalize pagination parameters
pub fn normalize_pagination(params: PaginationParams) -> (u64, u64) {
    params.normalize()
}

/// Placeholder for future query utilities
pub struct QueryUtils;
