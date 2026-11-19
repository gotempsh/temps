// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`ApiToolsHandle`] and [`WriteApiToolsHandle`] — shared, lazily-populated
//! holders for [`InternalApiCaller`].
//!
//! The caller can only be constructed after the Axum router is fully assembled (in
//! `console.rs`), but plugins that expose API tools need a handle they can receive
//! at service-registration time and share with adapters. The solution is a two-phase
//! pattern:
//!
//! 1. A plugin (or the AI chat plugin) registers an empty `ApiToolsHandle` as a
//!    service during `register_services`.
//! 2. After `build_split_application()` completes in `console.rs`, the code there
//!    constructs the `InternalApiCaller` and calls `handle.set(caller)`.
//! 3. Adapters call `handle.get()` at tool-execution time; if `None` is returned
//!    (startup is incomplete or something went wrong), they surface a graceful error.
//!
//! `ApiToolsHandle` is `Clone` — each plugin/adapter clone is shallow (same `Arc`
//! pointing to the same inner `ArcSwapOption`), so every `set()` is visible to all
//! clones immediately.
//!
//! [`WriteApiToolsHandle`] is a distinct newtype for the write-only caller.
//! Registering it as a separate type in the DI container prevents collisions with
//! the read-only `ApiToolsHandle` (two `Arc<ApiToolsHandle>` of the same type would
//! silently shadow each other in the registry).

use arc_swap::ArcSwapOption;
use std::sync::Arc;

use crate::InternalApiCaller;

/// A cloneable, lazily-populated handle to the shared [`InternalApiCaller`].
///
/// - All clones share the same inner `ArcSwapOption` (via `Arc`).
/// - `set` atomically replaces the caller; existing snapshots remain valid.
/// - `get` returns `None` until `set` has been called.
#[derive(Clone, Default)]
pub struct ApiToolsHandle(Arc<ArcSwapOption<InternalApiCaller>>);

impl ApiToolsHandle {
    /// Create a new, empty handle.
    pub fn new() -> Self {
        Self(Arc::new(ArcSwapOption::empty()))
    }

    /// Populate the handle with the constructed caller.
    ///
    /// Atomically publishes a complete replacement to every clone.
    pub fn set(&self, caller: InternalApiCaller) {
        self.0.store(Some(Arc::new(caller)));
    }

    /// Return the caller if it has been set, or `None` during startup.
    pub fn get(&self) -> Option<Arc<InternalApiCaller>> {
        self.0.load_full()
    }
}

/// A distinct newtype for the **write** API caller handle.
///
/// Using a separate type avoids DI collisions when both the read and write handles
/// are registered as services at the same time — the container disambiguates by
/// type, and `WriteApiToolsHandle` ≠ `ApiToolsHandle`.
///
/// Console wiring calls `WriteApiToolsHandle::set(caller)` where `caller` was built
/// with [`InternalApiCaller::new_write_allowlisted`]. Until that call completes,
/// `get()` returns `None` and write features degrade gracefully to "unavailable".
#[derive(Clone, Default)]
pub struct WriteApiToolsHandle(ApiToolsHandle);

impl WriteApiToolsHandle {
    /// Create a new, empty write handle.
    pub fn new() -> Self {
        Self(ApiToolsHandle::new())
    }

    /// Populate the handle with the write-only caller.
    ///
    /// Atomically replaces the caller (see [`ApiToolsHandle::set`]).
    pub fn set(&self, caller: InternalApiCaller) {
        self.0.set(caller);
    }

    /// Return the write caller if it has been set, or `None` during startup.
    pub fn get(&self) -> Option<Arc<InternalApiCaller>> {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_is_visible_to_clones_and_preserves_inflight_snapshot() {
        let handle = ApiToolsHandle::new();
        let clone = handle.clone();
        assert!(clone.get().is_none());
        let api = utoipa::openapi::OpenApi::default();
        handle.set(InternalApiCaller::new(axum::Router::new(), &api, vec![]));
        let first = clone.get().unwrap();
        handle.set(InternalApiCaller::new(axum::Router::new(), &api, vec![]));
        let second = clone.get().unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&second, &handle.get().unwrap()));
    }
}
