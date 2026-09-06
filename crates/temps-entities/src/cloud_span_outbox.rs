// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compatibility shim: `cloud_span_outbox` was renamed to
//! `cloud_telemetry_outbox` in ADR-043 (see
//! `m20260903_000001_generalize_cloud_telemetry_outbox`).
//!
//! The table name in Postgres is now `cloud_telemetry_outbox`.
//! `temps-cloud-client/src/outbox.rs` has been updated to reference the new
//! table name; this shim exists only to keep existing import paths compiling
//! without a second simultaneous rename of every callsite.
//!
//! **New code must use [`super::cloud_telemetry_outbox`] directly.**
//! This module exists only so existing import paths that would fail at compile
//! time do not need a second simultaneous change.
//!
//! The `CloudSpanOutboxState` type alias and the re-exported `Entity` /
//! `Model` / `ActiveModel` point at the canonical implementations in
//! `cloud_telemetry_outbox`, which now has `table_name =
//! "cloud_telemetry_outbox"`. Any code that still uses this shim and executes
//! SQL against `cloud_span_outbox` (the old table name) will fail at runtime
//! once the migration has run — that is intentional and expected; the caller
//! must be updated to use the new table name.

use super::cloud_telemetry_outbox;

pub use cloud_telemetry_outbox::ActiveModel;
pub use cloud_telemetry_outbox::CloudTelemetryOutboxEntityType as CloudSpanOutboxEntityType;
pub use cloud_telemetry_outbox::CloudTelemetryOutboxState as CloudSpanOutboxState;
pub use cloud_telemetry_outbox::Column;
pub use cloud_telemetry_outbox::Entity;
pub use cloud_telemetry_outbox::Model;
pub use cloud_telemetry_outbox::Relation;
