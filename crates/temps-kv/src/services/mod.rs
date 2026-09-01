// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! KV Service implementation
//!
//! This module provides a key-value service backed by Redis.
//! It uses the RedisService from temps-providers for container management,
//! version control, and backup/restore functionality.

mod config;
mod kv_service;

pub use config::{KvConfig, KvInputConfig};
pub use kv_service::{KvService, SetOptions};
