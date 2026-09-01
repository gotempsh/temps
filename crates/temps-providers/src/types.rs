// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableInfo {
    pub name: String,
    pub value: String,
    /// Whether this variable contains sensitive data (passwords, keys, tokens)
    #[schema(example = false)]
    pub sensitive: bool,
}
