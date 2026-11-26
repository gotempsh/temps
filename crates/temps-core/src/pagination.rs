// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Page-number pagination shared by list endpoints.
//!
//! Every growing list takes `page` (1-based, default 1) and `page_size`
//! (default [`DEFAULT_PAGE_SIZE`], at most [`MAX_PAGE_SIZE`]) and answers with
//! an `{ items, total, page, page_size }` envelope declared with
//! [`paginated_response!`](crate::paginated_response), newest first unless
//! the endpoint documents another order.

use serde::{Deserialize, Serialize};
use utoipa::IntoParams;

use crate::config::MAX_PAGE;

/// Items per page when the request does not say.
pub const DEFAULT_PAGE_SIZE: u64 = 20;
/// Largest `page_size` a list serves; larger requests are clamped to it.
pub const MAX_PAGE_SIZE: u64 = 100;

/// `page` and `page_size` query parameters of a paginated list.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    /// Page number, starting at 1 (default 1).
    #[param(example = 1, minimum = 1)]
    pub page: Option<u64>,
    /// Items per page (default 20, max 100).
    #[param(example = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u64>,
}

impl PageQuery {
    /// The page and page size actually served: `page` clamped to
    /// `1..=MAX_PAGE`, `page_size` to `1..=MAX_PAGE_SIZE`, defaults applied.
    /// Handlers echo these values, so `page_size` in a response is what was
    /// served, not what was asked for.
    pub fn normalize(self) -> (u64, u64) {
        let page = self.page.unwrap_or(1).clamp(1, MAX_PAGE);
        let page_size = self
            .page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        (page, page_size)
    }

    /// Zero-based index of the page, for `Paginator::fetch_page`.
    pub fn page_index(self) -> u64 {
        self.normalize().0 - 1
    }

    /// Row offset of the page, for `OFFSET` queries. Cannot overflow: the
    /// page is bounded by `MAX_PAGE` and the size by `MAX_PAGE_SIZE`.
    pub fn offset(self) -> u64 {
        let (page, page_size) = self.normalize();
        (page - 1) * page_size
    }

    /// One page of an already-ordered, bounded in-memory list, with the
    /// list's total. For lists assembled in memory (merged or computed
    /// sources); database-backed lists page in the query instead.
    pub fn slice<T>(self, items: Vec<T>) -> (Vec<T>, u64) {
        let total = items.len() as u64;
        let (_, page_size) = self.normalize();
        let start = usize::try_from(self.offset()).unwrap_or(usize::MAX);
        let page = items
            .into_iter()
            .skip(start)
            .take(page_size as usize)
            .collect();
        (page, total)
    }
}

/// Declares a paginated list response: `{ items, total, page, page_size }`.
///
/// ```ignore
/// temps_core::paginated_response!(
///     /// One page of a project's funnels, newest first.
///     FunnelList, FunnelResponse
/// );
/// ```
#[macro_export]
macro_rules! paginated_response {
    ($(#[$meta:meta])* $name:ident, $item:ty) => {
        $(#[$meta])*
        #[derive(::serde::Serialize, ::utoipa::ToSchema)]
        pub struct $name {
            /// The items on this page.
            pub items: Vec<$item>,
            /// Items across all pages.
            pub total: u64,
            /// The page served, starting at 1.
            pub page: u64,
            /// The page size served (at most 100).
            pub page_size: u64,
        }

        impl $name {
            /// Wraps one page of `items` with the page that was served.
            pub fn new(items: Vec<$item>, total: u64, query: $crate::pagination::PageQuery) -> Self {
                let (page, page_size) = query.normalize();
                Self { items, total, page, page_size }
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_clamps() {
        assert_eq!(PageQuery::default().normalize(), (1, 20));
        let q = PageQuery {
            page: Some(0),
            page_size: Some(0),
        };
        assert_eq!(q.normalize(), (1, 1));
        let q = PageQuery {
            page: Some(3),
            page_size: Some(500),
        };
        assert_eq!(q.normalize(), (3, 100));
        assert_eq!(q.offset(), 200);
        assert_eq!(q.page_index(), 2);
        let q = PageQuery {
            page: Some(u64::MAX),
            page_size: Some(u64::MAX),
        };
        assert_eq!(q.normalize(), (MAX_PAGE, 100));
    }

    #[test]
    fn slice_returns_the_page_and_the_total() {
        let items: Vec<u32> = (1..=45).collect();
        let q = PageQuery {
            page: Some(3),
            page_size: Some(20),
        };
        assert_eq!(q.slice(items.clone()), ((41..=45).collect(), 45));
        let past_the_end = PageQuery {
            page: Some(9),
            page_size: None,
        };
        assert_eq!(past_the_end.slice(items), (Vec::new(), 45));
    }

    paginated_response!(
        /// Test envelope.
        NumberList,
        u32
    );

    #[test]
    fn envelope_echoes_the_served_page() {
        let list = NumberList::new(
            vec![1, 2],
            7,
            PageQuery {
                page: None,
                page_size: Some(999),
            },
        );
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"items": [1, 2], "total": 7, "page": 1, "page_size": 100})
        );
    }
}
