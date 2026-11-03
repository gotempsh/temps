// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Configuration management utilities

use serde::{Deserialize, Serialize};
use utoipa::IntoParams;

/// Database configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
}

/// Common pagination parameters
#[derive(Debug, Clone, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PaginationParams {
    /// Page number (1-indexed)
    #[param(example = 1)]
    pub page: Option<u64>,
    /// Number of items per page (max 100)
    #[param(example = 20)]
    pub page_size: Option<u64>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}

impl Default for PaginationParams {
    fn default() -> Self {
        Self {
            page: Some(1),
            page_size: Some(20),
            sort_by: Some("created_at".to_string()),
            sort_order: Some("desc".to_string()),
        }
    }
}

impl PaginationParams {
    pub fn normalize(self) -> (u64, u64) {
        let page = self.page.unwrap_or(1).clamp(1, MAX_PAGE);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 100);
        (page, page_size)
    }
}

/// Largest page a list can be asked for. Beyond this a request gets the same
/// empty page it would have got anyway. Without a ceiling the offset is computed
/// as an unchecked `u64` product, which either overflows or fails to bind as
/// Postgres's `i64`, and both panic the handler.
pub const MAX_PAGE: u64 = i32::MAX as u64;

/// Zero-based page index for a caller-supplied 1-based `page`, safe for
/// `Paginator::fetch_page`: `page=0` is the first page, not an underflow.
pub fn page_index(page: u64) -> u64 {
    page.clamp(1, MAX_PAGE) - 1
}

/// `OFFSET` for a caller-supplied 1-based `page`, guaranteed to bind as `i64`.
pub fn page_offset(page: u64, page_size: u64) -> u64 {
    page_index(page)
        .saturating_mul(page_size)
        .min(i64::MAX as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_zero_is_the_first_page_rather_than_an_underflow() {
        assert_eq!(page_index(0), 0);
        assert_eq!(page_index(1), 0);
        assert_eq!(page_index(2), 1);
        assert_eq!(page_offset(0, 20), 0);
    }

    #[test]
    fn a_huge_page_keeps_the_offset_within_postgres_bigint() {
        for page in [MAX_PAGE, MAX_PAGE + 1, u64::MAX / 2, u64::MAX] {
            assert_eq!(page_index(page), MAX_PAGE - 1);
            // The largest page size any list accepts, and one far past it.
            for page_size in [100, u64::MAX] {
                assert!(i64::try_from(page_offset(page, page_size)).is_ok());
            }
            // What `Paginator::fetch_page` multiplies, unchecked.
            assert!(page_index(page).checked_mul(100).is_some());
        }
    }

    #[test]
    fn normalize_applies_the_floor_and_the_ceiling() {
        let normalized = |page, page_size| {
            PaginationParams {
                page,
                page_size,
                sort_by: None,
                sort_order: None,
            }
            .normalize()
        };
        assert_eq!(normalized(None, None), (1, 20));
        assert_eq!(normalized(Some(0), Some(0)), (1, 1));
        assert_eq!(normalized(Some(u64::MAX), Some(u64::MAX)), (MAX_PAGE, 100));
    }
}
