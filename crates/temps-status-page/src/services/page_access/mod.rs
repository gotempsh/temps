// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An optional password on a status page (ADR 108 D2).
//!
//! The admin sets a password; visitors type it once into a plain HTML form
//! and get a signed cookie that opens the page, its JSON and its subscribe
//! form for a week. The check runs on the snapshot alone (an HMAC over the
//! page id and the password's version), so a protected page still costs no
//! query per request. Changing or removing the password bumps the version,
//! which ends every session at once. The feed is not served while a page is
//! protected: a feed reader cannot type a password.
//!
//! Unlock attempts are bounded twice: failures per client and page are
//! limited, and only a few argon2 verifications run at once, so the form
//! cannot be used to guess the password or to load the server.

pub mod cookie;
pub mod error;
pub mod limiter;
pub mod service;
pub mod types;

pub use error::PageAccessError;
pub use service::{PageAccessService, UnlockOutcome};
pub use types::*;

#[cfg(test)]
mod tests;
