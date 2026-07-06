//! GitLab Webhook Management Service
//!
//! Handles auto-installation and removal of per-project GitLab webhooks.
//! Used by the project lifecycle (connect/disconnect) and the reinstall endpoint.

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

#[derive(Error, Debug)]
pub enum GitLabWebhookError {
    #[error("GitLab API error for project {project_path}: HTTP {status} — {body}")]
    ApiError {
        project_path: String,
        status: u16,
        body: String,
    },

    #[error(
        "Insufficient permissions for project {project_path}: access_level={access_level} (need >= 40 Maintainer)"
    )]
    InsufficientPermissions {
        project_path: String,
        access_level: i32,
    },

    #[error("GitLab connection {connection_id} not found or has no access token")]
    ConnectionNotFound { connection_id: i32 },

    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── API response shapes ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HookCreatedResponse {
    id: i64,
}

/// Minimal project info from `GET /projects/:id`
#[derive(Deserialize)]
struct GitLabProjectInfo {
    permissions: Option<GitLabPermissions>,
}

#[derive(Deserialize)]
struct GitLabPermissions {
    project_access: Option<GitLabAccessLevel>,
    group_access: Option<GitLabAccessLevel>,
}

#[derive(Deserialize)]
struct GitLabAccessLevel {
    access_level: i32,
}

// ── Request body for creating a hook ────────────────────────────────────────

#[derive(Serialize)]
struct CreateHookBody<'a> {
    url: &'a str,
    /// HMAC-SHA256 signing token. GitLab sends `HMAC(body, signing_token)` in
    /// the `webhook-signature: v1,{base64}` header on every event. Requires
    /// GitLab 17.0+. The legacy plaintext `token` field is intentionally NOT
    /// used — see the receiver in `handlers/gitlab.rs`.
    signing_token: &'a str,
    push_events: bool,
    tag_push_events: bool,
    enable_ssl_verification: bool,
    merge_requests_events: bool,
}

// ── Auth method ──────────────────────────────────────────────────────────────

/// How the GitLab access token was acquired, which determines which HTTP
/// authentication header GitLab expects.
///
/// GitLab officially supports two auth header styles:
/// - PAT / App tokens: `PRIVATE-TOKEN: <token>`
/// - OAuth 2.0 tokens: `Authorization: Bearer <token>`
///
/// Using `PRIVATE-TOKEN` for OAuth tokens works on gitlab.com today but is
/// undocumented behaviour and breaks on stricter self-hosted instances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookAuthMethod {
    /// Personal Access Token or GitLab App token — uses `PRIVATE-TOKEN` header.
    Pat,
    /// OAuth 2.0 token — uses `Authorization: Bearer` header.
    OAuth,
    /// Any other / unknown auth method. Falls back to `PRIVATE-TOKEN` for
    /// backwards-compatibility.
    Other,
}

impl WebhookAuthMethod {
    /// Convert the string stored in `git_providers.auth_method` into the enum.
    ///
    /// Known values: `"app"`, `"oauth"`, `"pat"`, `"basic"`, `"ssh"`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "oauth" => Self::OAuth,
            "pat" | "app" => Self::Pat,
            _ => Self::Other,
        }
    }
}

// ── Service ──────────────────────────────────────────────────────────────────

/// Thin client wrapper for the three GitLab webhook management calls.
/// All methods take a resolved `access_token` and the GitLab `base_url`
/// (e.g. `https://gitlab.com`); token resolution is the caller's job.
pub struct GitLabWebhookClient {
    http: reqwest::Client,
    base_url: String,
    access_token: String,
    auth_method: WebhookAuthMethod,
}

impl GitLabWebhookClient {
    /// Build a client from an already-resolved access token, base URL, and
    /// the auth method that determines which HTTP header is used.
    pub fn new(base_url: String, access_token: String, auth_method: WebhookAuthMethod) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("Temps-Engine/1.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build reqwest client with static config");

        Self {
            http,
            base_url,
            access_token,
            auth_method,
        }
    }

    /// Returns the `(header-name, header-value)` pair appropriate for the
    /// auth method stored on this client.
    ///
    /// - OAuth tokens must use `Authorization: Bearer <token>` (RFC 6750).
    /// - PAT / App / unknown tokens use the GitLab-specific `PRIVATE-TOKEN` header.
    fn auth_header(&self) -> (&'static str, String) {
        match self.auth_method {
            WebhookAuthMethod::OAuth => ("Authorization", format!("Bearer {}", self.access_token)),
            WebhookAuthMethod::Pat | WebhookAuthMethod::Other => {
                ("PRIVATE-TOKEN", self.access_token.clone())
            }
        }
    }

    fn encoded_path(owner: &str, repo: &str) -> String {
        let path = format!("{}/{}", owner, repo);
        urlencoding::encode(&path).to_string()
    }

    /// `GET /projects/:id?with_custom_attributes=false`
    /// Returns the caller's effective access level (project or group).
    /// Returns 0 when the permission block is absent (public project, no token).
    pub async fn get_project_access_level(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<i32, GitLabWebhookError> {
        let encoded = Self::encoded_path(owner, repo);
        let url = format!(
            "{}/api/v4/projects/{}?with_custom_attributes=false",
            self.base_url, encoded
        );

        let (header_name, header_value) = self.auth_header();
        let response = self
            .http
            .get(&url)
            .header(header_name, &header_value)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GitLabWebhookError::ApiError {
                project_path: format!("{}/{}", owner, repo),
                status: status.as_u16(),
                body,
            });
        }

        let info: GitLabProjectInfo = response.json().await?;

        // GitLab returns the *effective* access level under `permissions`.
        // It can be split between project_access (direct member) and group_access
        // (inherited from the group). We take the maximum of whichever are present.
        let level = {
            let project_level = info
                .permissions
                .as_ref()
                .and_then(|p| p.project_access.as_ref())
                .map(|a| a.access_level)
                .unwrap_or(0);

            let group_level = info
                .permissions
                .as_ref()
                .and_then(|p| p.group_access.as_ref())
                .map(|a| a.access_level)
                .unwrap_or(0);

            project_level.max(group_level)
        };

        debug!("GitLab access level for {}/{}: {}", owner, repo, level);

        Ok(level)
    }

    /// `POST /projects/:id/hooks` — installs the webhook and returns the GitLab hook ID.
    pub async fn install_webhook(
        &self,
        owner: &str,
        repo: &str,
        webhook_url: &str,
        signing_token: &str,
    ) -> Result<i64, GitLabWebhookError> {
        let encoded = Self::encoded_path(owner, repo);
        let url = format!("{}/api/v4/projects/{}/hooks", self.base_url, encoded);

        let body = CreateHookBody {
            url: webhook_url,
            signing_token,
            push_events: true,
            tag_push_events: true,
            enable_ssl_verification: true,
            merge_requests_events: false,
        };

        let (header_name, header_value) = self.auth_header();
        let response = self
            .http
            .post(&url)
            .header(header_name, &header_value)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(GitLabWebhookError::ApiError {
                project_path: format!("{}/{}", owner, repo),
                status: status.as_u16(),
                body: body_text,
            });
        }

        let created: HookCreatedResponse = response.json().await?;
        info!(
            "Installed GitLab webhook {} for {}/{}",
            created.id, owner, repo
        );
        Ok(created.id)
    }

    /// `DELETE /projects/:id/hooks/:hook_id` — idempotent on 404.
    pub async fn delete_webhook(
        &self,
        owner: &str,
        repo: &str,
        hook_id: i64,
    ) -> Result<(), GitLabWebhookError> {
        let encoded = Self::encoded_path(owner, repo);
        let url = format!(
            "{}/api/v4/projects/{}/hooks/{}",
            self.base_url, encoded, hook_id
        );

        let (header_name, header_value) = self.auth_header();
        let response = self
            .http
            .delete(&url)
            .header(header_name, &header_value)
            .send()
            .await?;

        let status = response.status();

        // 404 → already gone, treat as success (idempotent)
        if status == StatusCode::NOT_FOUND {
            warn!(
                "GitLab webhook {} for {}/{} was already absent (404), treating as success",
                hook_id, owner, repo
            );
            return Ok(());
        }

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GitLabWebhookError::ApiError {
                project_path: format!("{}/{}", owner, repo),
                status: status.as_u16(),
                body,
            });
        }

        info!("Deleted GitLab webhook {} for {}/{}", hook_id, owner, repo);
        Ok(())
    }
}

// ── HMAC-SHA256 signature helpers ────────────────────────────────────────────

/// Verify a GitLab `webhook-signature` header against a raw payload, the
/// `webhook-id` and `webhook-timestamp` request headers, and the stored
/// signing token.
///
/// GitLab 19.0+ uses Standard Webhooks-style signing:
///   - The signing token is `whsec_{base64(key)}` where `key` is 32 bytes.
///   - The signed message is the byte string `{webhook_id}.{timestamp}.{body}`.
///   - The signature header is `webhook-signature: v1,{base64(HMAC-SHA256(message, key))}`.
///   - Multiple signatures may appear space-separated in the header
///     (e.g. during key rotation): `v1,sigA v1,sigB`. We accept the message
///     if ANY of them validates.
///
/// Returns `true` only when at least one signature matches.
pub fn verify_gitlab_signature(
    payload: &[u8],
    header_value: &str,
    webhook_id: &str,
    timestamp: &str,
    signing_token: &str,
) -> bool {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    // Derive the HMAC key from the `whsec_<base64>` token.
    let b64_key = match signing_token.strip_prefix("whsec_") {
        Some(rest) => rest,
        // Reject tokens without the required prefix — they were never valid
        // for GitLab to sign with, so any signature must be forged.
        None => return false,
    };
    let key = match STANDARD.decode(b64_key) {
        Ok(k) => k,
        Err(_) => return false,
    };

    // Build the message: "{webhook_id}.{timestamp}.{body}".
    // The id+timestamp parts are ASCII strings; we concatenate as bytes.
    let mut message: Vec<u8> =
        Vec::with_capacity(webhook_id.len() + 1 + timestamp.len() + 1 + payload.len());
    message.extend_from_slice(webhook_id.as_bytes());
    message.push(b'.');
    message.extend_from_slice(timestamp.as_bytes());
    message.push(b'.');
    message.extend_from_slice(payload);

    // The header may carry multiple signatures separated by spaces. Accept
    // the request if any of them validates — this covers GitLab's key
    // rotation window.
    for sig_token in header_value.split_whitespace() {
        let b64_part = match sig_token.split_once(',') {
            Some((_version, sig)) => sig,
            None => sig_token,
        };
        let decoded = match STANDARD.decode(b64_part) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let mut mac = match Hmac::<Sha256>::new_from_slice(&key) {
            Ok(m) => m,
            Err(_) => continue,
        };
        mac.update(&message);
        if mac.verify_slice(&decoded).is_ok() {
            return true;
        }
    }
    false
}

/// Generate a signing token in the format GitLab requires.
///
/// GitLab validates `signing_token` against the regex `^whsec_[A-Za-z0-9+/=]+$`
/// where the base64-decoded portion must be 32 bytes. We produce
/// `whsec_{base64(32_random_bytes)}` to satisfy that constraint.
///
/// The full string (including the `whsec_` prefix) is what we store and what
/// the verifier uses to derive the HMAC key — see `verify_gitlab_signature`.
pub fn generate_signing_token() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("whsec_{}", STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    // ── helpers ───────────────────────────────────────────────────────────

    /// Build a valid `v1,{base64}` Standard-Webhooks signature for a given
    /// payload + headers + signing token. Used in tests to produce a header
    /// the verifier should accept.
    fn make_gitlab_signature(
        payload: &[u8],
        webhook_id: &str,
        timestamp: &str,
        signing_token: &str,
    ) -> String {
        // Reproduce the verifier's key-derivation: strip `whsec_`, base64-decode.
        let b64_key = signing_token
            .strip_prefix("whsec_")
            .expect("test signing tokens must use the whsec_ prefix");
        let key = STANDARD
            .decode(b64_key)
            .expect("test key must be valid base64");

        let mut message: Vec<u8> = Vec::new();
        message.extend_from_slice(webhook_id.as_bytes());
        message.push(b'.');
        message.extend_from_slice(timestamp.as_bytes());
        message.push(b'.');
        message.extend_from_slice(payload);

        let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
        mac.update(&message);
        let code_bytes = mac.finalize().into_bytes();
        format!("v1,{}", STANDARD.encode(code_bytes))
    }

    /// A fixed valid token for tests (32 zero bytes, base64-encoded).
    fn test_token() -> String {
        format!("whsec_{}", STANDARD.encode([0u8; 32]))
    }

    // ── verify_gitlab_signature (Standard-Webhooks path) ──────────────────

    #[test]
    fn test_verify_gitlab_signature_valid_v1_header() {
        let payload = b"push event body";
        let token = test_token();
        let header = make_gitlab_signature(payload, "wh_abc", "1700000000", &token);
        assert!(
            verify_gitlab_signature(payload, &header, "wh_abc", "1700000000", &token),
            "v1,<base64> header with correct key, id, and timestamp must verify"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_wrong_key_rejects() {
        let payload = b"push event body";
        let real_token = test_token();
        let other_token = format!("whsec_{}", STANDARD.encode([1u8; 32]));
        let header = make_gitlab_signature(payload, "wh_a", "1700000000", &real_token);
        assert!(
            !verify_gitlab_signature(payload, &header, "wh_a", "1700000000", &other_token),
            "signature computed with a different key must be rejected"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_tampered_payload_rejects() {
        let token = test_token();
        let header = make_gitlab_signature(b"original", "wh_a", "1700000000", &token);
        assert!(
            !verify_gitlab_signature(b"tampered", &header, "wh_a", "1700000000", &token),
            "signature over original payload must not validate a different payload"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_wrong_timestamp_rejects() {
        let token = test_token();
        let payload = b"body";
        let header = make_gitlab_signature(payload, "wh_a", "1700000000", &token);
        assert!(
            !verify_gitlab_signature(payload, &header, "wh_a", "1700000099", &token),
            "different timestamp must invalidate the signature"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_wrong_webhook_id_rejects() {
        let token = test_token();
        let payload = b"body";
        let header = make_gitlab_signature(payload, "wh_a", "1700000000", &token);
        assert!(
            !verify_gitlab_signature(payload, &header, "wh_b", "1700000000", &token),
            "different webhook-id must invalidate the signature"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_token_without_whsec_prefix_rejects() {
        // A signing token missing the `whsec_` prefix is malformed and
        // cannot have produced any GitLab signature.
        let bad_token = STANDARD.encode([0u8; 32]); // no prefix
        assert!(
            !verify_gitlab_signature(b"body", "v1,xxx", "wh_a", "1700000000", &bad_token),
            "signing token without whsec_ prefix must be rejected"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_invalid_base64_rejects() {
        let token = test_token();
        assert!(
            !verify_gitlab_signature(
                b"payload",
                "v1,!!!not-valid-base64!!!",
                "wh_a",
                "1700000000",
                &token,
            ),
            "invalid base64 must be rejected without panicking"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_empty_header_rejects() {
        let token = test_token();
        assert!(
            !verify_gitlab_signature(b"payload", "", "wh_a", "1700000000", &token),
            "empty header must be rejected"
        );
    }

    #[test]
    fn test_verify_gitlab_signature_multiple_signatures_one_valid() {
        // Header carries two signatures (e.g. during key rotation).
        // The first is junk, the second is correct.
        let token = test_token();
        let payload = b"body";
        let valid = make_gitlab_signature(payload, "wh_a", "1700000000", &token);
        let combined = format!("v1,{} {}", STANDARD.encode([0u8; 32]), valid);
        assert!(
            verify_gitlab_signature(payload, &combined, "wh_a", "1700000000", &token),
            "if any space-separated signature validates, the request must be accepted"
        );
    }

    #[test]
    fn test_generate_signing_token_has_whsec_prefix() {
        let token = generate_signing_token();
        assert!(
            token.starts_with("whsec_"),
            "GitLab requires the whsec_ prefix"
        );
        let b64 = token.strip_prefix("whsec_").unwrap();
        let decoded = STANDARD.decode(b64).expect("must be valid base64");
        assert_eq!(
            decoded.len(),
            32,
            "the base64-decoded portion must be exactly 32 bytes"
        );
    }

    #[test]
    fn test_generate_signing_token_is_unique() {
        let t1 = generate_signing_token();
        let t2 = generate_signing_token();
        assert_ne!(t1, t2, "tokens must be unique (random)");
    }

    // ── encoded_path helper ───────────────────────────────────────────────

    #[test]
    fn test_encoded_path_simple() {
        let enc = GitLabWebhookClient::encoded_path("myorg", "myrepo");
        assert_eq!(enc, "myorg%2Fmyrepo");
    }

    #[test]
    fn test_encoded_path_nested_group() {
        let enc = GitLabWebhookClient::encoded_path("group/subgroup", "repo");
        // owner is already a slash-path; combined it becomes group/subgroup/repo
        // which urlencodes as group%2Fsubgroup%2Frepo
        assert_eq!(enc, "group%2Fsubgroup%2Frepo");
    }

    // ── HTTP client integration tests (require network mock) ──────────────
    //
    // These tests use `mockito` to stand up a local HTTP server so we can
    // verify the correct endpoints are called without hitting real GitLab.

    // ── WebhookAuthMethod::from_str ───────────────────────────────────────

    #[test]
    fn test_auth_method_from_str_oauth() {
        assert_eq!(
            WebhookAuthMethod::from_str("oauth"),
            WebhookAuthMethod::OAuth
        );
    }

    #[test]
    fn test_auth_method_from_str_pat() {
        assert_eq!(WebhookAuthMethod::from_str("pat"), WebhookAuthMethod::Pat);
    }

    #[test]
    fn test_auth_method_from_str_app() {
        assert_eq!(WebhookAuthMethod::from_str("app"), WebhookAuthMethod::Pat);
    }

    #[test]
    fn test_auth_method_from_str_unknown_falls_back_to_other() {
        assert_eq!(
            WebhookAuthMethod::from_str("basic"),
            WebhookAuthMethod::Other
        );
        assert_eq!(WebhookAuthMethod::from_str("ssh"), WebhookAuthMethod::Other);
        assert_eq!(WebhookAuthMethod::from_str(""), WebhookAuthMethod::Other);
    }

    // ── auth_header selection ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_pat_uses_private_token_header() {
        let mut server = mockito::Server::new_async().await;
        // Expect the PRIVATE-TOKEN header to be present with the right value.
        let mock = server
            .mock("POST", "/api/v4/projects/myorg%2Fmyrepo/hooks")
            .match_header("PRIVATE-TOKEN", "my-pat-token")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id": 10}"#)
            .create_async()
            .await;

        let client = GitLabWebhookClient::new(
            server.url(),
            "my-pat-token".to_string(),
            WebhookAuthMethod::Pat,
        );
        client
            .install_webhook("myorg", "myrepo", "https://example.com/wh", "secret")
            .await
            .expect("install should succeed with PAT");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_oauth_uses_bearer_authorization_header() {
        let mut server = mockito::Server::new_async().await;
        // Expect `Authorization: Bearer <token>` — NOT `PRIVATE-TOKEN`.
        let mock = server
            .mock("POST", "/api/v4/projects/myorg%2Fmyrepo/hooks")
            .match_header("Authorization", "Bearer my-oauth-token")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id": 20}"#)
            .create_async()
            .await;

        let client = GitLabWebhookClient::new(
            server.url(),
            "my-oauth-token".to_string(),
            WebhookAuthMethod::OAuth,
        );
        client
            .install_webhook("myorg", "myrepo", "https://example.com/wh", "secret")
            .await
            .expect("install should succeed with OAuth Bearer");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_other_auth_method_falls_back_to_private_token() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v4/projects/myorg%2Fmyrepo/hooks")
            .match_header("PRIVATE-TOKEN", "unknown-token")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id": 30}"#)
            .create_async()
            .await;

        let client = GitLabWebhookClient::new(
            server.url(),
            "unknown-token".to_string(),
            WebhookAuthMethod::Other,
        );
        client
            .install_webhook("myorg", "myrepo", "https://example.com/wh", "secret")
            .await
            .expect("install should succeed with Other (fallback to PRIVATE-TOKEN)");

        mock.assert_async().await;
    }

    // ── HTTP client integration tests (require network mock) ──────────────
    //
    // These tests use `mockito` to stand up a local HTTP server so we can
    // verify the correct endpoints are called without hitting real GitLab.

    #[tokio::test]
    async fn test_install_webhook_happy_path() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v4/projects/myorg%2Fmyrepo/hooks")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id": 42}"#)
            .create_async()
            .await;

        let client = GitLabWebhookClient::new(
            server.url(),
            "test-token".to_string(),
            WebhookAuthMethod::Pat,
        );
        let hook_id = client
            .install_webhook("myorg", "myrepo", "https://example.com/wh", "secret")
            .await
            .expect("install should succeed");

        assert_eq!(hook_id, 42);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_delete_webhook_happy_path() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("DELETE", "/api/v4/projects/myorg%2Fmyrepo/hooks/99")
            .with_status(204)
            .create_async()
            .await;

        let client = GitLabWebhookClient::new(
            server.url(),
            "test-token".to_string(),
            WebhookAuthMethod::Pat,
        );
        client
            .delete_webhook("myorg", "myrepo", 99)
            .await
            .expect("delete should succeed");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_delete_webhook_idempotent_on_404() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("DELETE", "/api/v4/projects/myorg%2Fmyrepo/hooks/77")
            .with_status(404)
            .create_async()
            .await;

        let client = GitLabWebhookClient::new(
            server.url(),
            "test-token".to_string(),
            WebhookAuthMethod::Pat,
        );
        // Should NOT return an error on 404.
        client
            .delete_webhook("myorg", "myrepo", 77)
            .await
            .expect("delete on 404 should succeed (idempotent)");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_project_access_level_maintainer() {
        let body = r#"{
            "permissions": {
                "project_access": {"access_level": 40},
                "group_access": null
            }
        }"#;

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock(
                "GET",
                "/api/v4/projects/myorg%2Fmyrepo?with_custom_attributes=false",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client =
            GitLabWebhookClient::new(server.url(), "tok".to_string(), WebhookAuthMethod::Pat);
        let level = client
            .get_project_access_level("myorg", "myrepo")
            .await
            .expect("access level call should succeed");

        assert_eq!(level, 40);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_project_access_level_developer_only() {
        // Developer has access_level 30, which is below the Maintainer threshold.
        let body = r#"{
            "permissions": {
                "project_access": {"access_level": 30},
                "group_access": null
            }
        }"#;

        let mut server = mockito::Server::new_async().await;
        server
            .mock(
                "GET",
                "/api/v4/projects/myorg%2Fmyrepo?with_custom_attributes=false",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client =
            GitLabWebhookClient::new(server.url(), "tok".to_string(), WebhookAuthMethod::Pat);
        let level = client
            .get_project_access_level("myorg", "myrepo")
            .await
            .expect("call should succeed");

        assert!(level < 40, "developer should be below Maintainer threshold");
    }

    #[tokio::test]
    async fn test_install_webhook_server_error_returns_err() {
        let mut server = mockito::Server::new_async().await;
        // Simulate GitLab returning 500.
        server
            .mock("POST", "/api/v4/projects/myorg%2Fmyrepo/hooks")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client =
            GitLabWebhookClient::new(server.url(), "tok".to_string(), WebhookAuthMethod::Pat);
        let result = client
            .install_webhook("myorg", "myrepo", "https://example.com/wh", "secret")
            .await;

        assert!(result.is_err(), "500 should be an error");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("500") || err.contains("Internal Server Error"));
    }
}
