// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request and response shapes of status page passwords.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Whether a page needs a password. The password is never returned.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PagePasswordResponse {
    pub status_page_id: i32,
    pub has_password: bool,
}

/// `password` set: protect the page with it (8 to 256 characters), ending
/// every visitor's session. `null`: remove the password.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetPagePasswordRequest {
    pub password: Option<String>,
}
