// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A project's visitor identity settings (ADR 109 D3): whether its visitors
//! are recognised by stored ids (`persistent`) or by a daily hash that stores
//! nothing on the device (`cookieless`).

pub mod handlers;
pub mod service;
pub mod types;

pub use handlers::{routes, AnalyticsIdentityApiDoc, AnalyticsIdentityState};
pub use service::AnalyticsIdentityService;
pub use types::*;
