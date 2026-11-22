// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a user's external AI harness has connected to this instance.
//!
//! The console's "Let your AI operate Temps" onboarding mints a dedicated
//! admin key named [`AI_HARNESS_KEY_NAME`] and then waits for the harness to
//! authenticate. The CLI's `temps login` falls back to the browser device
//! flow when it is not handed a usable key, and that flow mints a separate
//! `cli-device:` key: the harness then works, but the dedicated key is never
//! used. Deriving the state here, from the key rows themselves, lets the
//! console recognise that path instead of waiting forever on a key nothing
//! will ever send.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use temps_entities::api_keys;
use utoipa::ToSchema;

/// Name of the dedicated key the AI onboarding page creates.
pub const AI_HARNESS_KEY_NAME: &str = "Temps AI harness";

/// Name prefix of keys minted by the CLI's browser device-login flow
/// (`temps login` without `--api-key`).
pub const CLI_DEVICE_KEY_PREFIX: &str = "cli-device:";

/// Console route where the harness connection is set up.
pub const AI_HARNESS_SETUP_PATH: &str = "/setup/ai";

/// Where the harness onboarding stands for the calling user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiHarnessState {
    /// No usable dedicated key exists: nothing has been started, or the key
    /// was deactivated or expired before it was ever used.
    Missing,
    /// The dedicated key exists and no credential has proven the harness
    /// connected yet.
    Waiting,
    /// A credential set up for the harness has authenticated a request.
    Connected,
}

/// Which credential proved the harness connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiHarnessCredentialKind {
    /// The dedicated key the onboarding page created.
    DedicatedKey,
    /// A key minted by a browser-approved `temps login` after the dedicated
    /// key was created.
    CliLogin,
}

/// The credential that authenticated the harness. Never carries the secret,
/// only the identifiers the API-key list already shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AiHarnessCredential {
    pub id: i32,
    pub name: String,
    pub key_prefix: String,
    pub kind: AiHarnessCredentialKind,
    #[schema(value_type = String, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub last_used_at: UtcDateTime,
}

/// `GET /api-keys/ai-harness/status`: the caller's AI harness connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AiHarnessStatusResponse {
    pub status: AiHarnessState,
    /// The dedicated harness key, when one exists (active or not).
    pub key_id: Option<i32>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2026-01-01T00:00:00Z")]
    pub key_created_at: Option<UtcDateTime>,
    /// Set when `status` is `connected`.
    pub connected_via: Option<AiHarnessCredential>,
    /// Why the status is not `connected`, in words the console can show.
    pub reason: Option<String>,
    /// Console route that resolves `reason`.
    pub setup_path: Option<String>,
}

/// Derive the harness status from the dedicated key and, when the dedicated
/// key has not been used, the most recently used device-login key created
/// after it (see [`crate::apikey_service::ApiKeyService::ai_harness_status`]
/// for the query that selects it).
pub fn derive_ai_harness_status(
    harness_key: Option<&api_keys::Model>,
    cli_login_key: Option<&api_keys::Model>,
) -> AiHarnessStatusResponse {
    let Some(key) = harness_key else {
        return AiHarnessStatusResponse {
            status: AiHarnessState::Missing,
            key_id: None,
            key_created_at: None,
            connected_via: None,
            reason: Some(format!(
                "No '{AI_HARNESS_KEY_NAME}' API key exists yet; create one to connect a harness"
            )),
            setup_path: Some(AI_HARNESS_SETUP_PATH.to_string()),
        };
    };

    let missing = |reason: String| AiHarnessStatusResponse {
        status: AiHarnessState::Missing,
        key_id: Some(key.id),
        key_created_at: Some(key.created_at),
        connected_via: None,
        reason: Some(reason),
        setup_path: Some(format!("/settings/keys/{}", key.id)),
    };

    if !key.is_active {
        return missing(format!(
            "The '{AI_HARNESS_KEY_NAME}' API key (id {}) is deactivated; reactivate or delete it",
            key.id
        ));
    }

    let connected = |via: &api_keys::Model, kind, last_used_at| AiHarnessStatusResponse {
        status: AiHarnessState::Connected,
        key_id: Some(key.id),
        key_created_at: Some(key.created_at),
        connected_via: Some(AiHarnessCredential {
            id: via.id,
            name: via.name.clone(),
            key_prefix: via.key_prefix.clone(),
            kind,
            last_used_at,
        }),
        reason: None,
        setup_path: None,
    };

    if let Some(last_used_at) = key.last_used_at {
        return connected(key, AiHarnessCredentialKind::DedicatedKey, last_used_at);
    }

    if key
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
    {
        return missing(format!(
            "The '{AI_HARNESS_KEY_NAME}' API key (id {}) expired before it was used; delete it and create a new one",
            key.id
        ));
    }

    // The caller selects this row; re-check the invariants here so a broader
    // query can never turn an unrelated or earlier credential into proof.
    if let Some(cli) = cli_login_key.filter(|cli| {
        cli.user_id == key.user_id
            && cli.is_active
            && cli.project_id.is_none()
            && cli.name.starts_with(CLI_DEVICE_KEY_PREFIX)
            && cli.created_at >= key.created_at
    }) {
        if let Some(last_used_at) = cli.last_used_at {
            return connected(cli, AiHarnessCredentialKind::CliLogin, last_used_at);
        }
    }

    AiHarnessStatusResponse {
        status: AiHarnessState::Waiting,
        key_id: Some(key.id),
        key_created_at: Some(key.created_at),
        connected_via: None,
        reason: Some(format!(
            "The '{AI_HARNESS_KEY_NAME}' API key (id {}) has not authenticated a request yet",
            key.id
        )),
        setup_path: Some(AI_HARNESS_SETUP_PATH.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn key(id: i32, name: &str) -> api_keys::Model {
        let created_at = Utc::now() - Duration::hours(1);
        api_keys::Model {
            id,
            name: name.to_string(),
            key_hash: format!("hash-{id}"),
            key_prefix: format!("tk_{id:05}"),
            user_id: 7,
            role_type: "admin".to_string(),
            permissions: None,
            is_active: true,
            expires_at: None,
            last_used_at: None,
            created_at,
            updated_at: created_at,
            service_id: None,
            project_id: None,
            environment_id: None,
        }
    }

    fn harness() -> api_keys::Model {
        key(1, AI_HARNESS_KEY_NAME)
    }

    fn cli_login_after(harness: &api_keys::Model) -> api_keys::Model {
        let mut cli = key(2, "cli-device:laptop-1767225600");
        cli.created_at = harness.created_at + Duration::minutes(2);
        cli.last_used_at = Some(cli.created_at + Duration::seconds(5));
        cli
    }

    #[test]
    fn no_dedicated_key_is_missing_and_points_at_setup() {
        let status = derive_ai_harness_status(None, None);
        assert_eq!(status.status, AiHarnessState::Missing);
        assert_eq!(status.key_id, None);
        assert_eq!(status.setup_path.as_deref(), Some(AI_HARNESS_SETUP_PATH));
    }

    #[test]
    fn unused_dedicated_key_is_waiting_and_names_the_key() {
        let status = derive_ai_harness_status(Some(&harness()), None);
        assert_eq!(status.status, AiHarnessState::Waiting);
        assert_eq!(status.key_id, Some(1));
        assert!(status.reason.as_deref().is_some_and(|r| r.contains("id 1")));
    }

    #[test]
    fn used_dedicated_key_is_connected_through_itself() {
        let mut key = harness();
        let used = key.created_at + Duration::minutes(1);
        key.last_used_at = Some(used);
        let status = derive_ai_harness_status(Some(&key), None);
        assert_eq!(status.status, AiHarnessState::Connected);
        let via = status
            .connected_via
            .expect("connected names its credential");
        assert_eq!(via.kind, AiHarnessCredentialKind::DedicatedKey);
        assert_eq!(via.id, 1);
        assert_eq!(via.last_used_at, used);
        assert_eq!(status.reason, None);
    }

    #[test]
    fn device_login_used_after_the_dedicated_key_counts_as_connected() {
        let key = harness();
        let cli = cli_login_after(&key);
        let status = derive_ai_harness_status(Some(&key), Some(&cli));
        assert_eq!(status.status, AiHarnessState::Connected);
        let via = status
            .connected_via
            .expect("connected names its credential");
        assert_eq!(via.kind, AiHarnessCredentialKind::CliLogin);
        assert_eq!(via.id, 2);
        assert_eq!(via.key_prefix, "tk_00002");
        assert_eq!(status.key_id, Some(1));
    }

    #[test]
    fn device_login_that_predates_the_dedicated_key_is_not_proof() {
        let key = harness();
        let mut cli = cli_login_after(&key);
        cli.created_at = key.created_at - Duration::days(3);
        let status = derive_ai_harness_status(Some(&key), Some(&cli));
        assert_eq!(status.status, AiHarnessState::Waiting);
    }

    #[test]
    fn unused_revoked_scoped_or_foreign_logins_are_not_proof() {
        let key = harness();
        let unused = cli_login_after(&key);
        let mut unused = unused;
        unused.last_used_at = None;

        let mut revoked = cli_login_after(&key);
        revoked.is_active = false;

        let mut scoped = cli_login_after(&key);
        scoped.project_id = Some(4);

        let mut other_user = cli_login_after(&key);
        other_user.user_id = 8;

        let mut manual = cli_login_after(&key);
        manual.name = "ci deploy key".to_string();

        for candidate in [unused, revoked, scoped, other_user, manual] {
            let status = derive_ai_harness_status(Some(&key), Some(&candidate));
            assert_eq!(
                status.status,
                AiHarnessState::Waiting,
                "{} must not mark the harness connected",
                candidate.name
            );
        }
    }

    #[test]
    fn deactivated_dedicated_key_is_missing_and_links_to_the_key() {
        let mut key = harness();
        key.is_active = false;
        key.last_used_at = Some(key.created_at);
        let status = derive_ai_harness_status(Some(&key), None);
        assert_eq!(status.status, AiHarnessState::Missing);
        assert_eq!(status.key_id, Some(1));
        assert_eq!(status.setup_path.as_deref(), Some("/settings/keys/1"));
    }

    #[test]
    fn dedicated_key_that_expired_unused_is_missing_not_waiting_forever() {
        let mut key = harness();
        key.expires_at = Some(Utc::now() - Duration::minutes(1));
        let status = derive_ai_harness_status(Some(&key), None);
        assert_eq!(status.status, AiHarnessState::Missing);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("expired")));
    }

    #[test]
    fn status_serializes_as_snake_case_strings() {
        let key = harness();
        let cli = cli_login_after(&key);
        let json = serde_json::to_value(derive_ai_harness_status(Some(&key), Some(&cli)))
            .expect("status serializes");
        assert_eq!(json["status"], "connected");
        assert_eq!(json["connected_via"]["kind"], "cli_login");
        assert!(json["connected_via"].get("key_hash").is_none());
    }
}
