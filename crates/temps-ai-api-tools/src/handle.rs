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
//! pointing to the same inner `OnceLock`), so the first `set()` is visible to all
//! clones immediately.
//!
//! [`WriteApiToolsHandle`] is a distinct newtype for the write-only caller.
//! Registering it as a separate type in the DI container prevents collisions with
//! the read-only `ApiToolsHandle` (two `Arc<ApiToolsHandle>` of the same type would
//! silently shadow each other in the registry).

use std::sync::{Arc, OnceLock};

use crate::InternalApiCaller;

/// A cloneable, lazily-populated handle to the shared [`InternalApiCaller`].
///
/// - All clones share the same inner `OnceLock` (via `Arc`).
/// - `set` succeeds at most once; subsequent calls are no-ops.
/// - `get` returns `None` until `set` has been called.
#[derive(Clone, Default)]
pub struct ApiToolsHandle(Arc<OnceLock<Arc<InternalApiCaller>>>);

impl ApiToolsHandle {
    /// Create a new, empty handle.
    pub fn new() -> Self {
        Self(Arc::new(OnceLock::new()))
    }

    /// Populate the handle with the constructed caller.
    ///
    /// Silently ignores duplicate calls (the `OnceLock` enforces single
    /// initialisation; in practice this is called exactly once from `console.rs`).
    pub fn set(&self, caller: InternalApiCaller) {
        // OnceLock::set returns Err(val) when already set; we discard it
        // intentionally — duplicate set is a no-op, not an error.
        let _ = self.0.set(Arc::new(caller));
    }

    /// Return the caller if it has been set, or `None` during startup.
    pub fn get(&self) -> Option<Arc<InternalApiCaller>> {
        self.0.get().cloned()
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
    /// Silently ignores duplicate calls (see [`ApiToolsHandle::set`]).
    pub fn set(&self, caller: InternalApiCaller) {
        self.0.set(caller);
    }

    /// Return the write caller if it has been set, or `None` during startup.
    pub fn get(&self) -> Option<Arc<InternalApiCaller>> {
        self.0.get()
    }
}
