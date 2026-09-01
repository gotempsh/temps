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
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 100);
        (page, page_size)
    }
}
