// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Serialize;
use temps_core::UtcDateTime;

// Environment variable types
#[derive(Debug, Clone, Serialize)]
pub struct EnvVarEnvironment {
    pub id: i32,
    pub name: String,
    pub main_url: String,
    pub current_deployment_id: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct EnvVarWithEnvironments {
    pub id: i32,
    pub project_id: i32,
    pub key: String,
    /// Plaintext value. `None` when `is_secret = true` so list responses stay
    /// masked. Callers that need plaintext for deploy must go through
    /// `EnvVarService::get_for_deploy` which decrypts independently of the
    /// secret flag.
    pub value: Option<String>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub environments: Vec<EnvVarEnvironment>,
    pub include_in_preview: bool,
    pub is_secret: bool,
}

/// Result of an env-var update. Carries the updated row plus whether this
/// particular write flipped the variable from regular to secret, so the handler
/// can audit that one-way transition without re-reading the row.
#[derive(Debug, Serialize)]
pub struct UpdateEnvVarOutcome {
    pub var: EnvVarWithEnvironments,
    /// True only when the row was non-secret before this update and is secret
    /// after it. A no-op update of an already-secret row reports `false`.
    pub promoted_to_secret: bool,
}

// Secret types. Deliberately NO `value` field — secret plaintext never leaves
// the service boundary except via SecretService::get_for_deploy.
#[derive(Debug, Clone, Serialize)]
pub struct SecretEnvironmentRef {
    pub id: i32,
    pub name: String,
    pub main_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretWithEnvironments {
    pub id: i32,
    pub project_id: i32,
    pub key: String,
    pub include_in_preview: bool,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub environments: Vec<SecretEnvironmentRef>,
    /// Docker Compose services this secret is restricted to. Empty means
    /// every service in the stack receives it.
    pub compose_services: Vec<String>,
}
