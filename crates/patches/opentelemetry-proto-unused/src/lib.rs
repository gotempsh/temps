// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Intentionally empty. See this crate's `Cargo.toml` for why it exists:
//! it replaces an `opentelemetry-proto` dependency that `relay-event-schema`
//! declares but never uses, so the vulnerable `opentelemetry_sdk` 0.30 it
//! would pull in (GHSA-w9wp-h8wv-79jx) is not compiled into Temps.

#![no_std]
