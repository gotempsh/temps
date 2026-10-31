// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hot-path proxy metrics.
//!
//! The counters themselves live in [`temps_metrics::proxy`], shared with the
//! worker edge proxy in `temps-node-agent` so both count traffic the same way
//! (ADR 061). This module re-exports them under the path the proxy has always
//! used.

pub use temps_metrics::proxy::*;
