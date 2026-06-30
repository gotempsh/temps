//! Ergonomic, schema-derived helpers over the object-safe [`AiService`](crate::AiService).
//!
//! Object-safety forbids generic methods on the trait object, so the typed API
//! lives here as free functions: [`complete_typed`] derives a JSON Schema from a
//! Rust type, asks the provider to return matching JSON, and deserializes.

use serde::de::DeserializeOwned;

use crate::service::{AiRequest, AiService};

/// Plain-text completion. `None` if AI is unavailable or the call fails.
pub async fn complete_text(ai: &dyn AiService, request: AiRequest) -> Option<String> {
    ai.complete(request).await.ok().map(|r| r.text)
}

/// Typed, structured completion — the foundation's headline capability.
///
/// Derives `T`'s JSON Schema (via `schemars`), asks the provider to return
/// matching JSON, and deserializes into `T`. Returns `None` on any failure
/// (AI unavailable, provider error, or a reply that doesn't deserialize into
/// `T`) — never a wrong value.
///
/// ```ignore
/// #[derive(serde::Deserialize, schemars::JsonSchema)]
/// struct Risk { level: String, reasons: Vec<String> }
///
/// let risk: Option<Risk> = complete_typed(ai.as_ref(),
///     AiRequest { purpose: "deploy.risk".into(), prompt, ..Default::default() }).await;
/// ```
pub async fn complete_typed<T>(ai: &dyn AiService, mut request: AiRequest) -> Option<T>
where
    T: schemars::JsonSchema + DeserializeOwned,
{
    if request.response_schema.is_none() {
        request.response_schema = serde_json::to_value(schemars::schema_for!(T)).ok();
    }
    let resp = ai.complete(request).await.ok()?;
    let value = resp.json.or_else(|| extract_json_block(&resp.text))?;
    serde_json::from_value(value).ok()
}

/// Best-effort extraction of a JSON value from a model's text reply.
///
/// Handles the common ways providers wrap JSON when not in a strict JSON mode:
/// a ```` ```json ```` fenced block, a bare fenced block, or the first balanced
/// `{...}` / `[...]` span in the text. Returns `None` if nothing parses.
pub fn extract_json_block(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();

    // 1. The whole reply is JSON.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(v);
    }

    // 2. A fenced code block (```json ... ``` or ``` ... ```).
    if let Some(after) = trimmed.split("```").nth(1) {
        let body = after.strip_prefix("json").unwrap_or(after);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) {
            return Some(v);
        }
    }

    // 3. The first balanced object/array span anywhere in the text.
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let Some(span) = balanced_span(trimmed, open, close) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(span) {
                return Some(v);
            }
        }
    }

    None
}

/// The first balanced `open..close` span in `s`, respecting string literals so a
/// brace inside a JSON string doesn't throw off the depth count.
fn balanced_span(s: &str, open: char, close: char) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = s.find(open)?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            ch if ch == open => depth += 1,
            ch if ch == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{AiError, AiResponse};
    use async_trait::async_trait;

    /// A mock that returns a canned text reply.
    struct CannedAi(String);

    #[async_trait]
    impl AiService for CannedAi {
        async fn is_available(&self) -> bool {
            true
        }
        async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
            Ok(AiResponse {
                text: self.0.clone(),
                json: None,
                model: "mock".into(),
            })
        }
        async fn chat_stream(
            &self,
            _request: crate::streaming::ChatTurnRequest,
        ) -> Result<crate::streaming::TokenStream, AiError> {
            let text = self.0.clone();
            Ok(Box::pin(futures::stream::once(async move { Ok(text) })))
        }
    }

    #[derive(serde::Deserialize, schemars::JsonSchema, PartialEq, Debug)]
    struct Note {
        headline: String,
        score: i32,
    }

    #[test]
    fn test_extract_plain_json() {
        let v = extract_json_block(r#"{"a": 1}"#).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn test_extract_fenced_json() {
        let v = extract_json_block("Sure:\n```json\n{\"a\": 2}\n```\nDone").unwrap();
        assert_eq!(v["a"], 2);
    }

    #[test]
    fn test_extract_embedded_object_with_braces_in_string() {
        let v = extract_json_block(r#"prefix {"msg": "a } b", "n": 3} suffix"#).unwrap();
        assert_eq!(v["n"], 3);
        assert_eq!(v["msg"], "a } b");
    }

    #[test]
    fn test_extract_none_when_no_json() {
        assert!(extract_json_block("no json here").is_none());
    }

    #[tokio::test]
    async fn test_complete_typed_parses_fenced_reply() {
        let ai = CannedAi("```json\n{\"headline\": \"hot\", \"score\": 5}\n```".into());
        let note: Option<Note> = complete_typed(&ai, AiRequest::default()).await;
        assert_eq!(
            note,
            Some(Note {
                headline: "hot".into(),
                score: 5
            })
        );
    }

    #[tokio::test]
    async fn test_complete_typed_none_on_garbage() {
        let ai = CannedAi("I cannot help with that.".into());
        let note: Option<Note> = complete_typed(&ai, AiRequest::default()).await;
        assert_eq!(note, None);
    }

    #[tokio::test]
    async fn test_complete_text() {
        let ai = CannedAi("hello".into());
        assert_eq!(
            complete_text(&ai, AiRequest::default()).await,
            Some("hello".to_string())
        );
    }
}
