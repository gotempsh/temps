// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Access-token-only snapshots of subscription sign-ins.
//!
//! A `claude auth login` / `codex login` credential holds a short-lived access
//! token and a **rotating refresh token**. If that file were copied into
//! several sandboxes, the first copy to refresh would invalidate every other
//! copy (and the host's own), so Temps keeps exactly one holder of the refresh
//! token: the CLI-owned directory on the host (see [`super::host_cli`]).
//! Everything that fans out from there — the encrypted settings row, autofixer
//! sandboxes, the workspace model relay — receives a snapshot built here that
//! carries the access token and its expiry but never the refresh token.
//!
//! When a snapshot is about to expire, [`resolve_claude_oauth`] lets the
//! CLI refresh its own store (single flight) and re-snapshots from it.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use thiserror::Error;

use super::host_cli;

/// Largest credential file read from a CLI-owned store.
const MAX_CREDENTIAL_FILE_BYTES: u64 = 1024 * 1024;

/// A refreshed token with less than this left is treated as unusable.
const MIN_USABLE_REMAINING: Duration = Duration::from_secs(120);

/// How long the CLI may take to refresh its own credential.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Error)]
pub enum CredentialSnapshotError {
    #[error("The {provider_id} credential is not valid JSON: {reason}")]
    InvalidJson { provider_id: String, reason: String },

    #[error("The {provider_id} credential does not contain an access token")]
    MissingAccessToken { provider_id: String },

    #[error("The {provider_id} credential file is larger than {max_bytes} bytes")]
    TooLarge { provider_id: String, max_bytes: u64 },

    #[error(
        "The {provider_id} subscription sign-in expires in {remaining_secs}s and could not be \
         refreshed: {reason}. Sign in again from Settings > AI Agents"
    )]
    RefreshFailed {
        provider_id: String,
        remaining_secs: i64,
        reason: String,
    },

    #[error("I/O error reading the {provider_id} credential at {path}: {reason}")]
    Io {
        provider_id: String,
        path: String,
        reason: String,
    },
}

/// A Claude subscription sign-in with its refresh token removed.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaudeLoginSnapshot {
    access_token: String,
    expires_at_ms: Option<i64>,
    scopes: Vec<String>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
}

impl std::fmt::Debug for ClaudeLoginSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeLoginSnapshot")
            .field("access_token", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("scopes", &self.scopes)
            .field("subscription_type", &self.subscription_type)
            .finish()
    }
}

#[derive(Deserialize)]
struct ClaudeFileShape {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOauthShape>,
}

// `refreshToken` is deliberately not a field: it is never deserialized, so no
// code path can retain it.
#[derive(Deserialize)]
struct ClaudeOauthShape {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
}

impl ClaudeLoginSnapshot {
    /// Parse the JSON a Claude CLI keeps in `.credentials.json` (or an earlier
    /// snapshot of it), dropping the refresh token.
    pub fn parse(raw: &str) -> Result<Self, CredentialSnapshotError> {
        let shape: ClaudeFileShape =
            serde_json::from_str(raw).map_err(|error| CredentialSnapshotError::InvalidJson {
                provider_id: "claude_cli".to_string(),
                reason: error.to_string(),
            })?;
        let oauth =
            shape
                .claude_ai_oauth
                .ok_or_else(|| CredentialSnapshotError::MissingAccessToken {
                    provider_id: "claude_cli".to_string(),
                })?;
        let access_token = oauth
            .access_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
            .ok_or_else(|| CredentialSnapshotError::MissingAccessToken {
                provider_id: "claude_cli".to_string(),
            })?;
        Ok(Self {
            access_token,
            expires_at_ms: oauth.expires_at,
            scopes: oauth.scopes,
            subscription_type: oauth.subscription_type,
            rate_limit_tier: oauth.rate_limit_tier,
        })
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at_ms
    }

    pub fn subscription_type(&self) -> Option<&str> {
        self.subscription_type.as_deref()
    }

    /// Time left on the access token. `None` expiry is treated as unbounded
    /// (a pasted `setup-token` has none); an expired token has zero left.
    pub fn remaining(&self, now_ms: i64) -> Duration {
        match self.expires_at_ms {
            None => Duration::MAX,
            Some(expires) if expires <= now_ms => Duration::ZERO,
            Some(expires) => Duration::from_millis((expires - now_ms) as u64),
        }
    }

    /// The JSON persisted in encrypted settings: the CLI's own field names,
    /// access token only, nothing synthesized.
    pub fn to_stored_json(&self) -> String {
        let mut oauth = Map::new();
        oauth.insert("accessToken".into(), json!(self.access_token));
        if let Some(expires) = self.expires_at_ms {
            oauth.insert("expiresAt".into(), json!(expires));
        }
        oauth.insert("scopes".into(), json!(self.scopes));
        if let Some(subscription) = &self.subscription_type {
            oauth.insert("subscriptionType".into(), json!(subscription));
        }
        if let Some(tier) = &self.rate_limit_tier {
            oauth.insert("rateLimitTier".into(), json!(tier));
        }
        json!({ "claudeAiOauth": Value::Object(oauth) }).to_string()
    }
}

/// What a saved `claude_cli` subscription credential contains.
#[derive(Debug)]
pub enum StoredClaudeCredential {
    /// A bare long-lived token pasted from `claude setup-token`. Static: it
    /// has no refresh token, so copying it is harmless.
    LegacyToken(String),
    /// A snapshot produced by a console sign-in.
    LoginSnapshot(ClaudeLoginSnapshot),
}

impl StoredClaudeCredential {
    pub fn classify(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.starts_with('{') {
            if let Ok(snapshot) = ClaudeLoginSnapshot::parse(trimmed) {
                return Self::LoginSnapshot(snapshot);
            }
        }
        Self::LegacyToken(trimmed.to_string())
    }
}

/// A usable bearer for one consumer, with what is known about it.
pub struct ResolvedClaudeCredential {
    pub access_token: String,
    pub expires_at_ms: Option<i64>,
    /// True when the token came from the CLI's own store this call rather
    /// than from the saved snapshot.
    pub refreshed: bool,
    /// A snapshot a caller that can persist should save in place of the one it
    /// passed in. `None` for legacy tokens and for unchanged snapshots.
    pub updated_snapshot_json: Option<String>,
}

impl std::fmt::Debug for ResolvedClaudeCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedClaudeCredential")
            .field("access_token", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("refreshed", &self.refreshed)
            .finish()
    }
}

static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Return a Claude bearer that stays valid for at least `min_remaining`.
///
/// Legacy tokens pass through. A login snapshot is used as-is while it has
/// enough time left; otherwise the CLI that owns the refresh token is asked to
/// refresh (single flight per process) and the result is re-snapshotted. The
/// refresh token never leaves the CLI-owned directory.
pub async fn resolve_claude_oauth(
    raw: &str,
    min_remaining: Duration,
) -> Result<ResolvedClaudeCredential, CredentialSnapshotError> {
    let snapshot = match StoredClaudeCredential::classify(raw) {
        StoredClaudeCredential::LegacyToken(token) => {
            return Ok(ResolvedClaudeCredential {
                access_token: token,
                expires_at_ms: None,
                refreshed: false,
                updated_snapshot_json: None,
            });
        }
        StoredClaudeCredential::LoginSnapshot(snapshot) => snapshot,
    };
    if snapshot.remaining(now_ms()) >= min_remaining {
        return Ok(ResolvedClaudeCredential {
            access_token: snapshot.access_token.clone(),
            expires_at_ms: snapshot.expires_at_ms,
            refreshed: false,
            updated_snapshot_json: None,
        });
    }

    let remaining_secs = snapshot.remaining(now_ms()).as_secs() as i64;
    let failed = |reason: String| CredentialSnapshotError::RefreshFailed {
        provider_id: "claude_cli".to_string(),
        remaining_secs,
        reason,
    };

    let _single_flight = REFRESH_LOCK.lock().await;
    let location = host_cli::active_managed_location("claude_cli").ok_or_else(|| {
        failed("this server has no Temps-managed Claude sign-in to refresh from".to_string())
    })?;

    // Another caller may have refreshed while this one waited for the lock, or
    // the CLI may have refreshed during a host turn.
    let current = read_claude_snapshot(&location).await?;
    if current.remaining(now_ms()) >= min_remaining {
        return Ok(from_store(current));
    }

    super::claude::refresh_managed_login(&location, REFRESH_TIMEOUT)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let refreshed = read_claude_snapshot(&location).await?;
    if refreshed.remaining(now_ms()) < MIN_USABLE_REMAINING {
        return Err(failed(
            "the CLI did not produce a fresh access token".to_string(),
        ));
    }
    if refreshed.remaining(now_ms()) < min_remaining {
        tracing::warn!(
            provider_id = "claude_cli",
            remaining_secs = refreshed.remaining(now_ms()).as_secs(),
            requested_secs = min_remaining.as_secs(),
            "refreshed subscription token is shorter than the requested validity"
        );
    }
    Ok(from_store(refreshed))
}

fn from_store(snapshot: ClaudeLoginSnapshot) -> ResolvedClaudeCredential {
    ResolvedClaudeCredential {
        updated_snapshot_json: Some(snapshot.to_stored_json()),
        access_token: snapshot.access_token,
        expires_at_ms: snapshot.expires_at_ms,
        refreshed: true,
    }
}

/// Read a CLI-owned Claude credential file and drop its refresh token.
pub async fn read_claude_snapshot(
    location: &host_cli::ProviderLocation,
) -> Result<ClaudeLoginSnapshot, CredentialSnapshotError> {
    let raw = read_credential(location, "claude_cli").await?;
    ClaudeLoginSnapshot::parse(&raw)
}

/// Read the credential file of a CLI-owned directory without following a
/// symlink at any component below its `HOME`: the run user owns that tree and
/// could otherwise point the server's read at another secret.
async fn read_credential(
    location: &host_cli::ProviderLocation,
    provider_id: &str,
) -> Result<String, CredentialSnapshotError> {
    let home = location.home.clone();
    let relative = location.credential_relative();
    let display = location.credential_file.display().to_string();
    let io_error = |error: std::io::Error| CredentialSnapshotError::Io {
        provider_id: provider_id.to_string(),
        path: display.clone(),
        reason: error.to_string(),
    };
    let bytes = tokio::task::spawn_blocking(move || {
        host_cli::read_regular_file_below(&home, &relative, MAX_CREDENTIAL_FILE_BYTES)
    })
    .await
    .map_err(|error| io_error(std::io::Error::other(error)))?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData
            && error.to_string().contains("larger than")
        {
            return CredentialSnapshotError::TooLarge {
                provider_id: provider_id.to_string(),
                max_bytes: MAX_CREDENTIAL_FILE_BYTES,
            };
        }
        io_error(error)
    })?;
    String::from_utf8(bytes).map_err(|error| CredentialSnapshotError::InvalidJson {
        provider_id: provider_id.to_string(),
        reason: format!("the credential file is not valid UTF-8: {error}"),
    })
}

/// Remove the refresh token from a Codex `auth.json` before it is stored or
/// copied anywhere other than the CLI-owned directory.
///
/// `tokens.refresh_token` is blanked rather than deleted: Codex's own parser
/// requires the field, and an empty value makes a copy fail cleanly instead of
/// rotating the host's refresh token.
pub fn strip_codex_refresh_token(raw: &str) -> Result<String, CredentialSnapshotError> {
    let mut value: Value =
        serde_json::from_str(raw).map_err(|error| CredentialSnapshotError::InvalidJson {
            provider_id: "codex_cli".to_string(),
            reason: error.to_string(),
        })?;
    if let Some(tokens) = value.get_mut("tokens").and_then(Value::as_object_mut) {
        if tokens.contains_key("refresh_token") {
            tokens.insert("refresh_token".to_string(), Value::String(String::new()));
        }
    }
    serde_json::to_string(&value).map_err(|error| CredentialSnapshotError::InvalidJson {
        provider_id: "codex_cli".to_string(),
        reason: error.to_string(),
    })
}

/// Whether a Codex `auth.json` carries ChatGPT subscription tokens (as
/// opposed to an API key).
pub fn codex_has_subscription_tokens(raw: &str) -> bool {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .pointer("/tokens/access_token")
                .and_then(Value::as_str)
                .map(|token| !token.trim().is_empty())
        })
        .unwrap_or(false)
}

/// Read a CLI-owned Codex `auth.json` and return it without its refresh token.
pub async fn read_codex_snapshot(
    location: &host_cli::ProviderLocation,
) -> Result<String, CredentialSnapshotError> {
    let raw = read_credential(location, "codex_cli").await?;
    if !codex_has_subscription_tokens(&raw) {
        return Err(CredentialSnapshotError::MissingAccessToken {
            provider_id: "codex_cli".to_string(),
        });
    }
    strip_codex_refresh_token(&raw)
}

/// Expiry (unix milliseconds) of a Codex access token, from the `exp` claim of
/// its JWT payload. `None` when the token is not a JWT or carries no `exp`.
pub fn codex_access_token_expiry_ms(auth_json: &str) -> Option<i64> {
    use base64::Engine;

    let value: Value = serde_json::from_str(auth_json).ok()?;
    let token = value.pointer("/tokens/access_token")?.as_str()?;
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims.get("exp")?.as_i64().map(|seconds| seconds * 1000)
}

/// A Codex `auth.json` whose refresh token has been blanked is a snapshot taken
/// from a console sign-in; a pasted file still carries its own refresh token.
fn is_codex_login_snapshot(auth_json: &str) -> bool {
    serde_json::from_str::<Value>(auth_json)
        .ok()
        .and_then(|value| {
            let tokens = value.get("tokens")?;
            let has_access = tokens
                .get("access_token")
                .and_then(Value::as_str)
                .is_some_and(|token| !token.trim().is_empty());
            let refresh_blank = tokens
                .get("refresh_token")
                .and_then(Value::as_str)
                .is_none_or(|token| token.trim().is_empty());
            Some(has_access && refresh_blank)
        })
        .unwrap_or(false)
}

/// Return a Codex `auth.json` safe to hand to a sandbox or relay for at least
/// `min_remaining`.
///
/// A pasted file is returned untouched. A console-sign-in snapshot is used as
/// is while its access token has enough time left; otherwise the host CLI that
/// owns the refresh token renews it (single flight) and the file is re-read
/// with the refresh token blanked again.
pub async fn resolve_codex_auth(
    raw: &str,
    min_remaining: Duration,
) -> Result<String, CredentialSnapshotError> {
    if !is_codex_login_snapshot(raw) {
        return Ok(raw.to_string());
    }
    let remaining = |auth_json: &str| -> Duration {
        match codex_access_token_expiry_ms(auth_json) {
            None => Duration::MAX,
            Some(expires) if expires <= now_ms() => Duration::ZERO,
            Some(expires) => Duration::from_millis((expires - now_ms()) as u64),
        }
    };
    if remaining(raw) >= min_remaining {
        return Ok(raw.to_string());
    }

    let remaining_secs = remaining(raw).as_secs() as i64;
    let failed = |reason: String| CredentialSnapshotError::RefreshFailed {
        provider_id: "codex_cli".to_string(),
        remaining_secs,
        reason,
    };
    let _single_flight = REFRESH_LOCK.lock().await;
    let location = host_cli::active_managed_location("codex_cli").ok_or_else(|| {
        failed("this server has no Temps-managed Codex sign-in to refresh from".to_string())
    })?;
    let current = read_codex_snapshot(&location).await?;
    if remaining(&current) >= min_remaining {
        return Ok(current);
    }
    super::codex::refresh_managed_login(&location, REFRESH_TIMEOUT)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let refreshed = read_codex_snapshot(&location).await?;
    if remaining(&refreshed) < MIN_USABLE_REMAINING {
        return Err(failed(
            "the CLI did not produce a fresh access token".to_string(),
        ));
    }
    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login_file(expires_at: i64) -> String {
        json!({
            "claudeAiOauth": {
                "accessToken": "access-token-value",
                "refreshToken": "refresh-token-value",
                "expiresAt": expires_at,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "pro",
                "rateLimitTier": "default_claude_pro"
            },
            "mcpOAuth": {"server": {"accessToken": "unrelated"}}
        })
        .to_string()
    }

    #[test]
    fn snapshot_drops_the_refresh_token_and_unrelated_sections() {
        let snapshot = ClaudeLoginSnapshot::parse(&login_file(4_102_444_800_000)).expect("parse");
        let stored = snapshot.to_stored_json();
        assert!(!stored.contains("refresh-token-value"));
        assert!(!stored.contains("refreshToken"));
        assert!(!stored.contains("mcpOAuth"));
        assert!(!stored.contains("unrelated"));
        assert!(stored.contains("access-token-value"));
        assert_eq!(snapshot.subscription_type(), Some("pro"));
    }

    #[test]
    fn snapshot_round_trips_through_its_stored_form() {
        let first = ClaudeLoginSnapshot::parse(&login_file(4_102_444_800_000)).expect("parse");
        let second = ClaudeLoginSnapshot::parse(&first.to_stored_json()).expect("reparse");
        assert_eq!(first, second);
    }

    #[test]
    fn snapshot_requires_an_access_token() {
        for raw in [
            "{}",
            r#"{"claudeAiOauth":{}}"#,
            r#"{"claudeAiOauth":{"accessToken":"  "}}"#,
            "not json",
        ] {
            assert!(ClaudeLoginSnapshot::parse(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn debug_output_redacts_the_access_token() {
        let snapshot = ClaudeLoginSnapshot::parse(&login_file(1)).expect("parse");
        assert!(!format!("{snapshot:?}").contains("access-token-value"));
    }

    #[test]
    fn remaining_handles_expired_future_and_unbounded_tokens() {
        let snapshot = ClaudeLoginSnapshot::parse(&login_file(10_000)).expect("parse");
        assert_eq!(snapshot.remaining(20_000), Duration::ZERO);
        assert_eq!(snapshot.remaining(4_000), Duration::from_secs(6));
        let unbounded =
            ClaudeLoginSnapshot::parse(r#"{"claudeAiOauth":{"accessToken":"t"}}"#).expect("parse");
        assert_eq!(unbounded.remaining(0), Duration::MAX);
    }

    #[test]
    fn classify_separates_pasted_tokens_from_login_snapshots() {
        assert!(matches!(
            StoredClaudeCredential::classify("sk-ant-oat01-abc"),
            StoredClaudeCredential::LegacyToken(token) if token == "sk-ant-oat01-abc"
        ));
        assert!(matches!(
            StoredClaudeCredential::classify(&login_file(1)),
            StoredClaudeCredential::LoginSnapshot(_)
        ));
        // JSON without an access token is not a snapshot; treat it as opaque.
        assert!(matches!(
            StoredClaudeCredential::classify(r#"{"other":1}"#),
            StoredClaudeCredential::LegacyToken(_)
        ));
    }

    #[tokio::test]
    async fn legacy_tokens_resolve_without_touching_the_cli() {
        let resolved = resolve_claude_oauth("sk-ant-oat01-abc", Duration::from_secs(3600))
            .await
            .expect("legacy token");
        assert_eq!(resolved.access_token, "sk-ant-oat01-abc");
        assert!(!resolved.refreshed);
        assert!(resolved.updated_snapshot_json.is_none());
    }

    #[tokio::test]
    async fn fresh_snapshot_resolves_without_a_refresh() {
        let far_future = chrono::Utc::now().timestamp_millis() + 6 * 3600 * 1000;
        let stored = ClaudeLoginSnapshot::parse(&login_file(far_future))
            .expect("parse")
            .to_stored_json();
        let resolved = resolve_claude_oauth(&stored, Duration::from_secs(3600))
            .await
            .expect("fresh snapshot");
        assert_eq!(resolved.access_token, "access-token-value");
        assert!(!resolved.refreshed);
    }

    #[tokio::test]
    async fn expired_snapshot_without_a_managed_sign_in_fails_with_guidance() {
        let stored = ClaudeLoginSnapshot::parse(&login_file(1))
            .expect("parse")
            .to_stored_json();
        let error = resolve_claude_oauth(&stored, Duration::from_secs(3600))
            .await
            .expect_err("no managed login to refresh from");
        let message = error.to_string();
        assert!(message.contains("Sign in again"), "{message}");
        assert!(!message.contains("access-token-value"));
    }

    #[test]
    fn codex_refresh_token_is_blanked_not_removed() {
        let raw = json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "id",
                "access_token": "access",
                "refresh_token": "rotating-secret",
                "account_id": "acct"
            },
            "last_refresh": "2026-01-01T00:00:00Z"
        })
        .to_string();
        let stripped = strip_codex_refresh_token(&raw).expect("strip");
        assert!(!stripped.contains("rotating-secret"));
        let value: Value = serde_json::from_str(&stripped).expect("json");
        assert_eq!(value["tokens"]["refresh_token"], "");
        assert_eq!(value["tokens"]["access_token"], "access");
        assert_eq!(value["tokens"]["account_id"], "acct");
    }

    #[test]
    fn codex_subscription_tokens_are_detected() {
        assert!(codex_has_subscription_tokens(
            r#"{"tokens":{"access_token":"a"}}"#
        ));
        assert!(!codex_has_subscription_tokens(
            r#"{"OPENAI_API_KEY":"sk-x"}"#
        ));
        assert!(!codex_has_subscription_tokens("garbage"));
    }

    fn jwt_with_exp(exp_seconds: i64) -> String {
        use base64::Engine;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            engine.encode(r#"{"alg":"none"}"#),
            engine.encode(json!({ "exp": exp_seconds }).to_string())
        )
    }

    fn codex_auth(exp_seconds: i64, refresh: &str) -> String {
        json!({
            "tokens": {
                "access_token": jwt_with_exp(exp_seconds),
                "refresh_token": refresh,
                "account_id": "acct"
            }
        })
        .to_string()
    }

    #[test]
    fn codex_expiry_is_read_from_the_access_token_claims() {
        assert_eq!(
            codex_access_token_expiry_ms(&codex_auth(1_900_000_000, "")),
            Some(1_900_000_000_000)
        );
        assert_eq!(
            codex_access_token_expiry_ms(r#"{"tokens":{"access_token":"opaque"}}"#),
            None
        );
    }

    #[tokio::test]
    async fn pasted_codex_auth_is_returned_untouched() {
        let raw = codex_auth(1, "rotating-secret");
        let resolved = resolve_codex_auth(&raw, Duration::from_secs(3600))
            .await
            .expect("pasted file");
        assert_eq!(resolved, raw);
    }

    #[tokio::test]
    async fn fresh_codex_snapshot_resolves_without_a_refresh() {
        let far_future = chrono::Utc::now().timestamp() + 3 * 3600;
        let raw = codex_auth(far_future, "");
        let resolved = resolve_codex_auth(&raw, Duration::from_secs(3600))
            .await
            .expect("fresh snapshot");
        assert_eq!(resolved, raw);
    }

    #[tokio::test]
    async fn expired_codex_snapshot_without_a_managed_sign_in_fails_with_guidance() {
        let raw = codex_auth(1, "");
        let error = resolve_codex_auth(&raw, Duration::from_secs(3600))
            .await
            .expect_err("nothing to refresh from");
        assert!(error.to_string().contains("Sign in again"), "{error}");
    }
}
