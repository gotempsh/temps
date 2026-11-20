// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Telegram notification provider, delivered through the Telegram Bot API
//! (`sendMessage`).
//!
//! The bot token is the credential and it travels in the request *path*
//! (`/bot<token>/sendMessage`), so every error built here strips the request
//! URL and never echoes the token. Telegram's own error description ("Bad
//! Request: chat not found", "Forbidden: bot was blocked by the user") is
//! surfaced verbatim, with a short hint for the common setup mistakes, so a
//! provider test tells the operator exactly what to fix.

use crate::error::Result;
use crate::services::{
    html_escape, is_slack_visible_metadata_key, metadata_label, NotificationProvider,
};
use crate::types::{Notification, NotificationSeverity};
use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;
use temps_core::retry::RetryConfig;
use tracing::{error, warn};

/// Provider type stored in `notification_providers.provider_type`.
pub const TELEGRAM_PROVIDER_TYPE: &str = "telegram";

/// Telegram Bot API base. Fixed: the bot token is only ever sent here.
const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

const TELEGRAM_TIMEOUT_SECS: u64 = 30;

/// Stands in for the bot token wherever response text would have shown it.
const REDACTED_TOKEN: &str = "<redacted>";

/// Visible-text budget. Telegram rejects messages longer than 4096 characters
/// after entity parsing; these per-part bounds keep the worst case
/// (title + message + every metadata row + link + severity line) under it.
const TITLE_MAX_CHARS: usize = 200;
const MESSAGE_MAX_CHARS: usize = 2_400;
const METADATA_MAX_ENTRIES: usize = 10;
const METADATA_LABEL_MAX_CHARS: usize = 40;
const METADATA_VALUE_MAX_CHARS: usize = 80;
const ACTION_LABEL_MAX_CHARS: usize = 60;

/// Telegram notification provider configuration as stored (encrypted).
#[derive(Clone, Serialize, Deserialize)]
pub struct TelegramProvider {
    /// Bot token issued by @BotFather (`<bot id>:<secret>`). Stored encrypted
    /// and masked in API responses.
    pub bot_token: String,
    /// Target chat: a numeric user/group/channel id (`-100…` for supergroups
    /// and channels) or a public `@channelusername`.
    #[serde(deserialize_with = "deserialize_chat_id")]
    pub chat_id: String,
    /// Optional forum topic (`message_thread_id`) inside a forum-style group.
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    /// Override for the Bot API base URL. Never serialized into stored config:
    /// it exists only so tests can point the provider at a local mock server.
    #[serde(skip)]
    pub api_base: Option<String>,
}

impl std::fmt::Debug for TelegramProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramProvider")
            .field("bot_token", &"***")
            .field("chat_id", &self.chat_id)
            .field("message_thread_id", &self.message_thread_id)
            .finish()
    }
}

/// Accept the chat id as a JSON string or a JSON integer. Telegram documents
/// it as "Integer or String", and API clients send either.
pub fn deserialize_chat_id<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ChatId {
        Integer(i64),
        Text(String),
    }
    Ok(match ChatId::deserialize(deserializer)? {
        ChatId::Integer(id) => id.to_string(),
        ChatId::Text(text) => text,
    })
}

/// Why a Telegram configuration was refused or a delivery failed. Messages
/// carry the chat id and Telegram's reason, never the bot token.
#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("Invalid Telegram configuration: {reason}")]
    InvalidConfig { reason: String },

    #[error("Failed to build the HTTP client for Telegram chat {chat_id}: {reason}")]
    ClientBuild { chat_id: String, reason: String },

    #[error("Telegram Bot API request for chat {chat_id} failed: {reason}")]
    Request { chat_id: String, reason: String },

    #[error(
        "Telegram rejected the message for chat {chat_id} (HTTP {status}): {description}{guidance}"
    )]
    Rejected {
        chat_id: String,
        status: u16,
        /// Telegram's own `description`, e.g. "Bad Request: chat not found".
        description: String,
        /// Empty, or ". <what to check>" for a recognised setup mistake.
        guidance: String,
    },

    #[error(
        "Telegram Bot API returned an unreadable response for chat {chat_id} (HTTP {status}): {reason}"
    )]
    InvalidResponse {
        chat_id: String,
        status: u16,
        reason: String,
    },
}

/// The subset of a Bot API response Temps reads.
#[derive(Debug, Deserialize)]
struct TelegramApiResponse {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<TelegramResponseParameters>,
}

#[derive(Debug, Deserialize)]
struct TelegramResponseParameters {
    #[serde(default)]
    retry_after: Option<u64>,
    #[serde(default)]
    migrate_to_chat_id: Option<i64>,
}

/// What to check for the setup mistakes operators actually hit, keyed off
/// Telegram's description (and the parameters that come with some of them).
fn rejection_guidance(
    status: u16,
    description: &str,
    parameters: Option<&TelegramResponseParameters>,
) -> String {
    if let Some(new_id) = parameters.and_then(|p| p.migrate_to_chat_id) {
        return format!(". The group was upgraded to a supergroup: change the chat ID to {new_id}");
    }
    if let Some(seconds) = parameters.and_then(|p| p.retry_after) {
        return format!(". Telegram is rate limiting this bot: retry after {seconds} seconds");
    }
    let lower = description.to_ascii_lowercase();
    let hint = if status == 401 || lower.contains("unauthorized") {
        "The bot token is invalid or was revoked: copy a current token from @BotFather"
    } else if lower.contains("chat not found") {
        "Check the chat ID, and that the bot was added to the group or channel \
         (for a private chat, the user must send the bot a message first)"
    } else if lower.contains("bot was blocked by the user") {
        "The recipient blocked the bot: unblock it in Telegram or use another chat"
    } else if lower.contains("thread not found") || lower.contains("topic") {
        "Check the topic (thread) ID, or clear it to post to the group's General topic"
    } else if lower.contains("not a member") || lower.contains("kicked") {
        "Add the bot to the chat again"
    } else if lower.contains("not enough rights") || lower.contains("have no rights") {
        "Give the bot permission to post messages in this chat"
    } else {
        return String::new();
    };
    format!(". {hint}")
}

/// Truncate to `max_chars` characters, marking the cut with an ellipsis.
fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn severity_label(severity: NotificationSeverity) -> &'static str {
    match severity {
        NotificationSeverity::Debug => "Debug",
        NotificationSeverity::Info => "Info",
        NotificationSeverity::Warning => "Warning",
        NotificationSeverity::Error => "Error",
        NotificationSeverity::Critical => "Critical",
        NotificationSeverity::Emergency => "Emergency",
    }
}

/// Only plain http(s) links become a tappable button; anything else (a
/// `javascript:` URL, a relative path) is dropped rather than sent.
fn safe_action_url(url: &str) -> Option<&str> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(url.trim())
}

impl TelegramProvider {
    fn api_base(&self) -> &str {
        self.api_base.as_deref().unwrap_or(TELEGRAM_API_BASE)
    }

    fn token(&self) -> &str {
        self.bot_token.trim()
    }

    fn chat(&self) -> &str {
        self.chat_id.trim()
    }

    fn method_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base(), self.token(), method)
    }

    /// Remove the bot token from text Telegram (or a proxy in front of it)
    /// sent back, before any of it reaches an error, a log or the console:
    /// an error page can echo the request path, which carries the token.
    fn redact_token(&self, text: &str) -> String {
        let token = self.token();
        let mut out = text.to_string();
        if !token.is_empty() {
            out = out.replace(token, REDACTED_TOKEN);
        }
        // The secret half alone is enough to use the bot.
        if let Some((_, secret)) = token.split_once(':') {
            if !secret.is_empty() {
                out = out.replace(secret, REDACTED_TOKEN);
            }
        }
        out
    }

    /// Check the configuration shape before anything is stored or sent.
    /// Errors never include the token itself.
    pub fn validate(&self) -> std::result::Result<(), TelegramError> {
        let token = self.token();
        if token.is_empty() {
            return Err(TelegramError::InvalidConfig {
                reason: "bot token is required (create a bot with @BotFather)".to_string(),
            });
        }
        let token_shape_ok = token.split_once(':').is_some_and(|(bot_id, secret)| {
            !bot_id.is_empty()
                && bot_id.chars().all(|c| c.is_ascii_digit())
                && !secret.is_empty()
                && secret
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        });
        if !token_shape_ok {
            return Err(TelegramError::InvalidConfig {
                reason: "bot token must look like '123456789:AA…' as issued by @BotFather"
                    .to_string(),
            });
        }

        let chat = self.chat();
        if chat.is_empty() {
            return Err(TelegramError::InvalidConfig {
                reason: "chat ID is required".to_string(),
            });
        }
        let chat_ok = match chat.strip_prefix('@') {
            Some(username) => {
                !username.is_empty()
                    && username.len() <= 64
                    && username
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
            }
            None => chat.parse::<i64>().is_ok(),
        };
        if !chat_ok {
            return Err(TelegramError::InvalidConfig {
                reason: format!(
                    "chat ID '{chat}' must be a numeric ID (e.g. 123456789 or -1001234567890) \
                     or a public channel username such as @my_channel"
                ),
            });
        }

        if let Some(thread_id) = self.message_thread_id {
            if thread_id <= 0 {
                return Err(TelegramError::InvalidConfig {
                    reason: format!("topic (thread) ID {thread_id} must be a positive number"),
                });
            }
        }
        Ok(())
    }

    /// `chat_id` as Telegram expects it: an integer when numeric, otherwise
    /// the `@username` string.
    fn chat_id_value(&self) -> serde_json::Value {
        match self.chat().parse::<i64>() {
            Ok(id) => serde_json::Value::from(id),
            Err(_) => serde_json::Value::from(self.chat()),
        }
    }

    /// Render the notification as Telegram HTML (`parse_mode: "HTML"`): every
    /// piece of notification text is escaped, so a title or message can never
    /// inject markup or links of its own.
    pub(crate) fn render_html(notification: &Notification) -> String {
        let severity = notification.effective_severity();
        let mut text = format!(
            "{} <b>{}</b>\nSeverity: <b>{}</b>",
            severity.emoji(),
            html_escape(&truncate_chars(notification.title.trim(), TITLE_MAX_CHARS)),
            severity_label(severity),
        );

        // Rich notifications keep HTML in `message` for email and put the
        // readable alternative in `text_body`, as Slack and webhooks use it.
        let message = notification.text_body().trim();
        if !message.is_empty() {
            text.push_str("\n\n");
            text.push_str(&html_escape(&truncate_chars(message, MESSAGE_MAX_CHARS)));
        }

        // Sorted so the same notification always renders the same way.
        let mut fields: Vec<(&String, &String)> = notification
            .metadata
            .iter()
            .filter(|(k, _)| is_slack_visible_metadata_key(k, &notification.metadata))
            .collect();
        fields.sort_by(|a, b| a.0.cmp(b.0));
        if !fields.is_empty() {
            text.push('\n');
            for (key, value) in fields.into_iter().take(METADATA_MAX_ENTRIES) {
                text.push_str(&format!(
                    "\n<b>{}:</b> {}",
                    html_escape(&truncate_chars(
                        &metadata_label(key),
                        METADATA_LABEL_MAX_CHARS
                    )),
                    html_escape(&truncate_chars(value.trim(), METADATA_VALUE_MAX_CHARS)),
                ));
            }
        }

        if let Some(url) = notification
            .metadata
            .get("_action_url")
            .and_then(|url| safe_action_url(url))
        {
            let label = notification
                .metadata
                .get("_action_label")
                .map(|label| label.trim())
                .filter(|label| !label.is_empty())
                .unwrap_or("View in Temps");
            text.push_str(&format!(
                "\n\n<a href=\"{}\">{}</a>",
                html_escape(url),
                html_escape(&truncate_chars(label, ACTION_LABEL_MAX_CHARS)),
            ));
        }

        text
    }

    fn http_client(&self) -> std::result::Result<reqwest::Client, TelegramError> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(TELEGRAM_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| TelegramError::ClientBuild {
                chat_id: self.chat().to_string(),
                reason: e.without_url().to_string(),
            })
    }

    /// Deliver the notification, retrying what a later attempt can fix.
    ///
    /// Retried: connection failures, 5xx answers and rate limits whose
    /// `retry_after` fits under `retry.max_delay`. Not retried: rejected
    /// settings (bad token, unknown chat), which fail the same way again, and
    /// timeouts, because Telegram may already have posted the message and a
    /// retry would post it twice. Providers are delivered one after another,
    /// so the bound also caps how long this can hold up the next one.
    pub(crate) async fn send_message(
        &self,
        notification: &Notification,
        retry: &RetryConfig,
    ) -> std::result::Result<(), TelegramError> {
        self.validate()?;
        let client = self.http_client()?;

        let mut payload = serde_json::json!({
            "chat_id": self.chat_id_value(),
            "text": Self::render_html(notification),
            "parse_mode": "HTML",
            "link_preview_options": { "is_disabled": true },
        });
        if let Some(thread_id) = self.message_thread_id {
            payload["message_thread_id"] = serde_json::Value::from(thread_id);
        }

        let max_attempts = retry.max_attempts.max(1);
        let mut attempt: u32 = 0;
        loop {
            let (error, next) = match self.post_message(&client, &payload).await {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            let delay = match next {
                RetryAdvice::Never => None,
                RetryAdvice::Backoff => Some(retry.compute_delay(attempt)),
                RetryAdvice::After(wait) => (wait <= retry.max_delay).then_some(wait),
            };
            let Some(delay) = delay.filter(|_| attempt + 1 < max_attempts) else {
                return Err(error);
            };
            warn!(
                chat_id = %self.chat(),
                attempt = attempt + 1,
                max_attempts,
                retry_in_ms = delay.as_millis() as u64,
                "Telegram delivery failed, retrying: {}",
                error
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    /// Post one `sendMessage` call and turn Telegram's answer into a result,
    /// with whether trying again could succeed.
    async fn post_message(
        &self,
        client: &reqwest::Client,
        payload: &serde_json::Value,
    ) -> std::result::Result<(), (TelegramError, RetryAdvice)> {
        let chat_id = self.chat().to_string();
        let response = client
            .post(self.method_url("sendMessage"))
            .json(payload)
            .send()
            .await
            .map_err(|e| {
                // Only a request that never reached Telegram is safe to repeat.
                let advice = if e.is_connect() {
                    RetryAdvice::Backoff
                } else {
                    RetryAdvice::Never
                };
                // The URL carries the bot token: never let it into the error.
                let error = TelegramError::Request {
                    chat_id: chat_id.clone(),
                    reason: e.without_url().to_string(),
                };
                (error, advice)
            })?;

        let status = response.status().as_u16();
        let server_error = (500..600).contains(&status);
        let body = response.text().await.map_err(|e| {
            let error = TelegramError::Request {
                chat_id: chat_id.clone(),
                reason: format!("failed to read the response body: {}", e.without_url()),
            };
            (error, RetryAdvice::Never)
        })?;
        // Before parsing, so neither the excerpt nor `description` can carry it.
        let body = self.redact_token(&body);

        let parsed: TelegramApiResponse = serde_json::from_str(&body).map_err(|e| {
            let error = TelegramError::InvalidResponse {
                chat_id: chat_id.clone(),
                status,
                reason: format!("{e}; body starts with '{}'", truncate_chars(&body, 200)),
            };
            // A proxy in front of Telegram answers an outage with HTML.
            let advice = if server_error {
                RetryAdvice::Backoff
            } else {
                RetryAdvice::Never
            };
            (error, advice)
        })?;

        if parsed.ok && (200..300).contains(&status) {
            return Ok(());
        }

        let retry_after = parsed.parameters.as_ref().and_then(|p| p.retry_after);
        let advice = match retry_after {
            Some(seconds) if status == 429 => {
                RetryAdvice::After(std::time::Duration::from_secs(seconds))
            }
            _ if server_error => RetryAdvice::Backoff,
            _ => RetryAdvice::Never,
        };
        let description = parsed
            .description
            .unwrap_or_else(|| "no description was returned".to_string());
        let guidance = rejection_guidance(status, &description, parsed.parameters.as_ref());
        let error = TelegramError::Rejected {
            chat_id,
            status,
            description,
            guidance,
        };
        Err((error, advice))
    }
}

/// Whether a failed `sendMessage` is worth another attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryAdvice {
    /// It would fail the same way, or could post the message twice.
    Never,
    /// Transient: wait the configured backoff.
    Backoff,
    /// Telegram asked to wait this long first.
    After(std::time::Duration),
}

/// Retries for automatic alerts: three attempts at most, 1 s then 2 s apart.
fn delivery_retry() -> RetryConfig {
    RetryConfig::new(3)
        .with_base_delay(std::time::Duration::from_secs(1))
        .with_max_delay(std::time::Duration::from_secs(10))
}

#[async_trait]
impl NotificationProvider for TelegramProvider {
    async fn initialize(&mut self, _db: Arc<DatabaseConnection>) -> Result<()> {
        self.validate()?;
        Ok(())
    }

    async fn send(&self, notification: &Notification) -> Result<()> {
        Ok(self.send_message(notification, &delivery_retry()).await?)
    }

    async fn send_test(&self, notification: &Notification) -> Result<String> {
        // One attempt: the operator is waiting and should see the real error.
        self.send_message(notification, &RetryConfig::new(1))
            .await?;
        Ok(format!(
            "Test message delivered to Telegram chat {}",
            self.chat()
        ))
    }

    async fn health_check(&self) -> Result<bool> {
        if let Err(e) = self.validate() {
            error!("Telegram provider health check failed: {}", e);
            return Ok(false);
        }
        let client = match self.http_client() {
            Ok(client) => client,
            Err(e) => {
                error!("Telegram provider health check failed: {}", e);
                return Ok(false);
            }
        };
        // `getMe` is side-effect free and fails fast on a bad token.
        match client.get(self.method_url("getMe")).send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(e) => {
                error!(
                    "Telegram provider health check failed for chat {}: {}",
                    self.chat(),
                    e.without_url()
                );
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "123456789:AAExampleSecretToken_value-01";

    fn provider(chat_id: &str) -> TelegramProvider {
        TelegramProvider {
            bot_token: TOKEN.to_string(),
            chat_id: chat_id.to_string(),
            message_thread_id: None,
            api_base: None,
        }
    }

    fn mock_provider(server: &MockServer, chat_id: &str) -> TelegramProvider {
        TelegramProvider {
            api_base: Some(server.uri()),
            ..provider(chat_id)
        }
    }

    #[test]
    fn validate_accepts_numeric_and_username_chats() {
        for chat in ["123456789", "-1001234567890", "@my_channel", " -42 "] {
            assert!(
                provider(chat).validate().is_ok(),
                "chat {chat} should be valid"
            );
        }
    }

    #[test]
    fn validate_rejects_malformed_tokens_without_echoing_them() {
        for token in [
            "",
            "   ",
            "not-a-token",
            "abc:def",
            "987654321:",
            ":secret",
            "123:has space",
        ] {
            let result = TelegramProvider {
                bot_token: token.to_string(),
                ..provider("123")
            }
            .validate();
            let error = result.expect_err("malformed token must be rejected");
            assert!(matches!(error, TelegramError::InvalidConfig { .. }));
            if token.trim().len() > 3 {
                assert!(
                    !error.to_string().contains(token.trim()),
                    "error must not echo the token: {error}"
                );
            }
        }
    }

    #[test]
    fn validate_rejects_bad_chat_ids_and_thread_ids() {
        for chat in ["", "my_channel", "@", "@bad-name", "12a"] {
            assert!(
                matches!(
                    provider(chat).validate(),
                    Err(TelegramError::InvalidConfig { .. })
                ),
                "chat {chat:?} should be rejected"
            );
        }
        let with_bad_thread = TelegramProvider {
            message_thread_id: Some(0),
            ..provider("-1001")
        };
        assert!(matches!(
            with_bad_thread.validate(),
            Err(TelegramError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn debug_output_masks_the_bot_token() {
        let rendered = format!("{:?}", provider("-1001"));
        assert!(!rendered.contains("AAExampleSecretToken"), "{rendered}");
        assert!(rendered.contains("-1001"));
    }

    #[test]
    fn stored_config_round_trips_and_never_persists_api_base() {
        let mut p = provider("-1001234567890");
        p.message_thread_id = Some(7);
        p.api_base = Some("http://127.0.0.1:1".to_string());
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("api_base"), "{json}");
        let parsed: TelegramProvider = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.chat_id, "-1001234567890");
        assert_eq!(parsed.message_thread_id, Some(7));
        assert_eq!(parsed.api_base, None);
    }

    #[test]
    fn chat_id_deserializes_from_an_integer() {
        let parsed: TelegramProvider = serde_json::from_value(serde_json::json!({
            "bot_token": TOKEN,
            "chat_id": -1001234567890_i64,
        }))
        .unwrap();
        assert_eq!(parsed.chat_id, "-1001234567890");
        assert_eq!(parsed.message_thread_id, None);
    }

    #[test]
    fn render_html_escapes_user_text_and_shows_severity_and_metadata() {
        let notification = Notification::new(
            "Deploy <failed> & rolled back",
            "See <a href=\"https://evil.example\">this</a>",
        )
        .with_severity(NotificationSeverity::Critical)
        .with_metadata("project_slug", "my-app")
        .with_metadata("project_id", "42")
        .with_metadata("environment", "production")
        .with_metadata("_chart_svg", "<svg></svg>");

        let html = TelegramProvider::render_html(&notification);

        assert!(html.starts_with("🚨 <b>Deploy &lt;failed&gt; &amp; rolled back</b>"));
        assert!(html.contains("Severity: <b>Critical</b>"));
        assert!(html.contains("&lt;a href=&quot;https://evil.example&quot;&gt;"));
        assert!(!html.contains("<a href=\"https://evil.example\""), "{html}");
        assert!(html.contains("<b>Project Slug:</b> my-app"), "{html}");
        assert!(html.contains("<b>Environment:</b> production"), "{html}");
        // Raw ids and `_`-prefixed channel payloads are not for people.
        assert!(!html.contains("42"), "{html}");
        assert!(!html.contains("svg"), "{html}");
        // Sorted rows render deterministically.
        assert!(html.find("Environment").unwrap() < html.find("Project Slug").unwrap());
    }

    #[test]
    fn render_html_uses_the_readable_body_of_a_rich_notification() {
        let rich = Notification::new(
            "Weekly digest",
            "<html><body><table><tr><td>Deployments</td></tr></table></body></html>",
        )
        .with_metadata(
            "text_body",
            "Deployments: 3\nhttps://temps.example/projects",
        );
        let html = TelegramProvider::render_html(&rich);
        assert!(
            html.contains("Deployments: 3\nhttps://temps.example/projects"),
            "{html}"
        );
        assert!(!html.contains("&lt;table&gt;"), "{html}");
        assert!(!html.contains("Text Body"), "{html}");

        // A blank alternative falls back to the message itself.
        let blank = Notification::new("Deploy failed", "Build exited with code 1")
            .with_metadata("text_body", "   ");
        let html = TelegramProvider::render_html(&blank);
        assert!(html.contains("Build exited with code 1"), "{html}");
    }

    #[test]
    fn render_html_links_back_to_the_console_only_for_http_urls() {
        let with_link = Notification::new("Error alert", "boom")
            .with_metadata(
                "_action_url",
                "https://temps.example/projects/demo/errors/42",
            )
            .with_metadata("_action_label", "View error details");
        let html = TelegramProvider::render_html(&with_link);
        assert!(
            html.ends_with(
                "<a href=\"https://temps.example/projects/demo/errors/42\">View error details</a>"
            ),
            "{html}"
        );
        assert!(!html.contains("_action_url"), "{html}");

        let with_script = Notification::new("Error alert", "boom")
            .with_metadata("_action_url", "javascript:alert(1)");
        let html = TelegramProvider::render_html(&with_script);
        assert!(!html.contains("<a "), "{html}");
        assert!(!html.contains("javascript"), "{html}");
    }

    #[test]
    fn render_html_stays_within_telegram_message_limit() {
        let mut notification = Notification::new("T".repeat(1_000), "M".repeat(10_000));
        for i in 0..50 {
            notification
                .metadata
                .insert(format!("key_{i:02}"), "v".repeat(500));
        }
        notification.metadata.insert(
            "_action_url".to_string(),
            "https://temps.example/x".to_string(),
        );
        notification
            .metadata
            .insert("_action_label".to_string(), "L".repeat(500));

        let html = TelegramProvider::render_html(&notification);
        // Visible text = the rendered HTML minus tags; the escapes in this
        // input are none, so stripping tags is an upper bound on what
        // Telegram counts.
        let mut visible = 0usize;
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => visible += 1,
                _ => {}
            }
        }
        assert!(visible <= 4_096, "visible length {visible} exceeds 4096");
    }

    #[tokio::test]
    async fn send_posts_html_message_to_send_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .and(body_partial_json(serde_json::json!({
                "chat_id": -1001234567890_i64,
                "parse_mode": "HTML",
                "message_thread_id": 7,
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": {}})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut p = mock_provider(&server, "-1001234567890");
        p.message_thread_id = Some(7);
        let message = p
            .send_test(&Notification::new("Temps test notification", "hello"))
            .await
            .expect("delivery should succeed");
        assert_eq!(
            message,
            "Test message delivered to Telegram chat -1001234567890"
        );

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(body["text"]
            .as_str()
            .unwrap()
            .contains("<b>Temps test notification</b>"));
    }

    #[tokio::test]
    async fn username_chat_ids_are_sent_as_strings() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .and(body_partial_json(
                serde_json::json!({"chat_id": "@my_channel"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        mock_provider(&server, "@my_channel")
            .send(&Notification::new("t", "m"))
            .await
            .expect("delivery should succeed");
    }

    #[tokio::test]
    async fn send_reports_telegrams_reason_with_guidance_and_no_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: chat not found",
            })))
            .mount(&server)
            .await;

        let error = mock_provider(&server, "-1009")
            .send_test(&Notification::new("t", "m"))
            .await
            .expect_err("a rejected message must fail the test");
        let message = error.to_string();
        assert!(message.contains("Bad Request: chat not found"), "{message}");
        assert!(message.contains("chat -1009"), "{message}");
        assert!(message.contains("Check the chat ID"), "{message}");
        assert!(!message.contains("AAExampleSecretToken"), "{message}");
    }

    #[tokio::test]
    async fn blocked_bot_and_bad_token_are_explained() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 403,
                "description": "Forbidden: bot was blocked by the user",
            })))
            .mount(&server)
            .await;
        let message = mock_provider(&server, "123")
            .send(&Notification::new("t", "m"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("Forbidden: bot was blocked by the user"),
            "{message}"
        );
        assert!(message.contains("HTTP 403"), "{message}");
        assert!(message.contains("unblock it"), "{message}");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 401,
                "description": "Unauthorized",
            })))
            .mount(&server)
            .await;
        let message = mock_provider(&server, "123")
            .send(&Notification::new("t", "m"))
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("@BotFather"), "{message}");
    }

    #[tokio::test]
    async fn supergroup_migration_names_the_new_chat_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: group chat was upgraded to a supergroup chat",
                "parameters": {"migrate_to_chat_id": -1009876543210_i64},
            })))
            .mount(&server)
            .await;
        let message = mock_provider(&server, "-12345")
            .send(&Notification::new("t", "m"))
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("-1009876543210"), "{message}");
    }

    #[tokio::test]
    async fn non_json_response_is_reported_with_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>"))
            .mount(&server)
            .await;
        let error = mock_provider(&server, "123")
            .send_message(&Notification::new("t", "m"), &fast_retry())
            .await
            .unwrap_err();
        assert!(
            matches!(error, TelegramError::InvalidResponse { status: 502, .. }),
            "{error}"
        );
        // A gateway outage is transient: every attempt was used.
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn transport_failure_does_not_leak_the_token() {
        // Nothing listens on port 1: the request fails before any response.
        let p = TelegramProvider {
            api_base: Some("http://127.0.0.1:1".to_string()),
            ..provider("123")
        };
        let message = p
            .send_message(&Notification::new("t", "m"), &fast_retry())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("Telegram Bot API request for chat 123 failed"),
            "{message}"
        );
        assert!(!message.contains("AAExampleSecretToken"), "{message}");
    }

    /// Three attempts with no waiting, so retry tests run instantly.
    fn fast_retry() -> RetryConfig {
        RetryConfig::new(3)
            .with_base_delay(std::time::Duration::ZERO)
            .with_max_delay(std::time::Duration::from_secs(1))
    }

    fn rejection(status: u16, description: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": status,
            "description": description,
        }))
    }

    fn delivered() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true}))
    }

    #[tokio::test]
    async fn a_server_error_is_retried_until_delivered() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rejection(500, "Internal Server Error"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(delivered())
            .mount(&server)
            .await;

        mock_provider(&server, "123")
            .send_message(&Notification::new("t", "m"), &fast_retry())
            .await
            .expect("the third attempt is delivered");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_rate_limit_is_retried_after_the_wait_telegram_asks_for() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests: retry after 0",
                "parameters": {"retry_after": 0},
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(delivered())
            .mount(&server)
            .await;

        mock_provider(&server, "123")
            .send_message(&Notification::new("t", "m"), &fast_retry())
            .await
            .expect("delivered after the requested wait");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_rate_limit_longer_than_the_cap_fails_at_once_with_the_wait() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests: retry after 60",
                "parameters": {"retry_after": 60},
            })))
            .mount(&server)
            .await;

        let message = mock_provider(&server, "123")
            .send_message(&Notification::new("t", "m"), &fast_retry())
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("retry after 60 seconds"), "{message}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejected_settings_are_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rejection(400, "Bad Request: chat not found"))
            .mount(&server)
            .await;

        mock_provider(&server, "123")
            .send_message(&Notification::new("t", "m"), &fast_retry())
            .await
            .unwrap_err();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_test_message_is_sent_once_so_the_error_shows_at_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rejection(500, "Internal Server Error"))
            .mount(&server)
            .await;

        mock_provider(&server, "123")
            .send_test(&Notification::new("t", "m"))
            .await
            .unwrap_err();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[test]
    fn automatic_delivery_retries_are_bounded() {
        let retry = delivery_retry();
        assert_eq!(retry.max_attempts, 3);
        let waits: Vec<_> = (0..retry.max_attempts - 1)
            .map(|attempt| retry.compute_delay(attempt))
            .collect();
        assert_eq!(
            waits,
            [
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(2)
            ]
        );
        assert!(retry.max_delay <= std::time::Duration::from_secs(10));
    }

    #[tokio::test]
    async fn response_text_that_echoes_the_token_is_redacted() {
        // A gateway error page quoting the request path.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_string(format!(
                "<html>Not Found: POST /bot{TOKEN}/sendMessage</html>"
            )))
            .mount(&server)
            .await;
        let message = mock_provider(&server, "123")
            .send_test(&Notification::new("t", "m"))
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("HTTP 404"), "{message}");
        assert!(message.contains("/bot<redacted>/sendMessage"), "{message}");
        assert!(!message.contains("AAExampleSecretToken"), "{message}");

        // Telegram's own description, quoting only the secret half.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rejection(
                400,
                "Bad Request: chat not found (token AAExampleSecretToken_value-01)",
            ))
            .mount(&server)
            .await;
        let message = mock_provider(&server, "123")
            .send_test(&Notification::new("t", "m"))
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("Bad Request: chat not found"), "{message}");
        assert!(message.contains("Check the chat ID"), "{message}");
        assert!(!message.contains("AAExampleSecretToken"), "{message}");
    }

    #[tokio::test]
    async fn health_check_uses_get_me() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/bot{TOKEN}/getMe")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;
        assert!(mock_provider(&server, "123").health_check().await.unwrap());
    }
}
