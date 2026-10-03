// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OpenAI-compatible endpoints for the OpenCode harness.
//!
//! The saved credential is one encrypted JSON document holding both the base
//! URL and the API key. Keeping them together means the key can only ever be
//! sent to the endpoint it was saved with: pointing it somewhere else requires
//! entering the key again. The key never enters a sandbox. Workspace turns
//! reach the endpoint through the host-side model relay, and every connection
//! the host makes to the endpoint goes through [`external_only_http_client`],
//! which refuses to connect to private, loopback, link-local or metadata
//! addresses, including after a DNS rebind.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Auth flavor id stored in `ProviderConfig.auth_type`.
pub const OPENAI_COMPATIBLE_AUTH_TYPE: &str = "openai_compatible";

/// OpenCode provider id the endpoint is registered under inside the sandbox.
/// Models are selected as `openai-compatible/<upstream model id>`.
pub const OPENCODE_COMPATIBLE_PROVIDER_ID: &str = "openai-compatible";

const MAX_BASE_URL_LENGTH: usize = 2_048;
const MAX_API_KEY_LENGTH: usize = 16_384;
const MAX_MODEL_ID_LENGTH: usize = 256;
/// Upper bound on models accepted from one `/models` response.
pub const MAX_DISCOVERED_MODELS: usize = 500;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OpenAiCompatibleError {
    #[error(
        "The OpenAI-compatible credential is not valid JSON with 'base_url' and 'api_key' fields"
    )]
    InvalidDocument,
    #[error("Base URL '{base_url}' is invalid: {reason}")]
    InvalidBaseUrl { base_url: String, reason: String },
    #[error("The API key for {base_url} is empty, too long, or contains whitespace or control characters")]
    InvalidApiKey { base_url: String },
    #[error("Model '{model}' is invalid: {reason}")]
    InvalidModel { model: String, reason: String },
    #[error(
        "The model list returned by {base_url} is not an OpenAI-compatible '/models' response"
    )]
    InvalidModelList { base_url: String },
}

/// A validated endpoint credential. Deliberately has no `Debug` or `Clone`
/// implementation that could print or spread the key.
pub struct OpenAiCompatibleCredential {
    base_url: String,
    api_key: String,
    verified_model: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CredentialDocument {
    base_url: String,
    api_key: String,
    /// Upstream model a successful verification reached with this exact
    /// base URL and key. Written only by the server after verifying; never
    /// accepted from a client (see [`OpenAiCompatibleCredential::without_verified_model`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verified_model: Option<String>,
}

impl OpenAiCompatibleCredential {
    /// Parse and validate the stored (or submitted) JSON credential. The base
    /// URL is canonicalized, so equal endpoints always compare equal.
    pub fn parse(raw: &str) -> Result<Self, OpenAiCompatibleError> {
        let document = serde_json::from_str::<CredentialDocument>(raw)
            .map_err(|_| OpenAiCompatibleError::InvalidDocument)?;
        let base_url = validate_base_url(&document.base_url)?;
        let api_key = document.api_key.trim();
        if api_key.is_empty()
            || api_key.len() > MAX_API_KEY_LENGTH
            || api_key
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(OpenAiCompatibleError::InvalidApiKey { base_url });
        }
        let verified_model = match document.verified_model {
            Some(model) => {
                validate_upstream_model(&model)?;
                Some(model)
            }
            None => None,
        };
        Ok(Self {
            base_url,
            api_key: api_key.to_string(),
            verified_model,
        })
    }

    /// Drop any recorded verification. Applied to every submitted credential,
    /// so a client cannot claim a model was verified.
    pub fn without_verified_model(mut self) -> Self {
        self.verified_model = None;
        self
    }

    /// Record the model a successful verification reached, given as an
    /// OpenCode selection (`openai-compatible/<model>`).
    pub fn with_verified_selection(
        mut self,
        selection: &str,
    ) -> Result<Self, OpenAiCompatibleError> {
        self.verified_model = Some(upstream_model_from_selection(selection)?.to_string());
        Ok(self)
    }

    /// Upstream model id this endpoint was last verified with, if any.
    pub fn verified_model(&self) -> Option<&str> {
        self.verified_model.as_deref()
    }

    /// Canonical JSON document to encrypt and store.
    pub fn to_document(&self) -> Result<String, OpenAiCompatibleError> {
        serde_json::to_string(&CredentialDocument {
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            verified_model: self.verified_model.clone(),
        })
        .map_err(|_| OpenAiCompatibleError::InvalidDocument)
    }

    /// Canonical base URL without a trailing slash, e.g.
    /// `https://openrouter.ai/api/v1`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// `(base_url, api_key, verified_model)`.
    pub fn into_parts(self) -> (String, String, Option<String>) {
        (self.base_url, self.api_key, self.verified_model)
    }
}

/// Validate and canonicalize an endpoint base URL. Only public HTTPS
/// endpoints are accepted: the API key travels to this URL, so it must be
/// encrypted in transit and must never target the Temps host's own network.
pub fn validate_base_url(raw: &str) -> Result<String, OpenAiCompatibleError> {
    let trimmed = raw.trim();
    let invalid = |reason: &str| OpenAiCompatibleError::InvalidBaseUrl {
        base_url: trimmed.chars().take(200).collect(),
        reason: reason.to_string(),
    };
    if trimmed.is_empty() {
        return Err(invalid("it is empty"));
    }
    if trimmed.len() > MAX_BASE_URL_LENGTH {
        return Err(invalid("it is longer than 2048 characters"));
    }
    let parsed = temps_core::url_validation::validate_external_url(trimmed).map_err(|error| {
        invalid(&format!(
            "{error}. Only public endpoints are supported; private, loopback and metadata addresses are blocked"
        ))
    })?;
    if parsed.scheme() != "https" {
        return Err(invalid("it must use https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid("it must not contain credentials"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(invalid("it must not contain a query string or fragment"));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// Validate an upstream model id as sent in the `model` field, e.g.
/// `gpt-4o-mini` or `meta-llama/llama-3.3-70b-instruct:free`.
pub fn validate_upstream_model(model: &str) -> Result<&str, OpenAiCompatibleError> {
    let invalid = |reason: &str| OpenAiCompatibleError::InvalidModel {
        model: model.chars().take(MAX_MODEL_ID_LENGTH).collect(),
        reason: reason.to_string(),
    };
    if model.is_empty() {
        return Err(invalid("it is empty"));
    }
    if model.len() > MAX_MODEL_ID_LENGTH {
        return Err(invalid("it is longer than 256 characters"));
    }
    if model.starts_with(['-', '/']) || model.ends_with('/') || model.contains("..") {
        return Err(invalid("it is not a model identifier"));
    }
    if !model.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(character, '-' | '_' | '.' | '/' | ':' | '@' | '+')
    }) {
        return Err(invalid(
            "only letters, digits and - _ . / : @ + are allowed",
        ));
    }
    Ok(model)
}

/// Split an OpenCode model selection (`openai-compatible/<model>`) into the
/// upstream model id, rejecting selections for any other provider.
pub fn upstream_model_from_selection(selection: &str) -> Result<&str, OpenAiCompatibleError> {
    let model = selection
        .strip_prefix(OPENCODE_COMPATIBLE_PROVIDER_ID)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(|| OpenAiCompatibleError::InvalidModel {
            model: selection.chars().take(MAX_MODEL_ID_LENGTH).collect(),
            reason: format!("it must start with '{OPENCODE_COMPATIBLE_PROVIDER_ID}/'"),
        })?;
    validate_upstream_model(model)
}

/// OpenCode model selection for an upstream model id.
pub fn opencode_selection(upstream_model: &str) -> String {
    format!("{OPENCODE_COMPATIBLE_PROVIDER_ID}/{upstream_model}")
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Parse an OpenAI `GET /models` response into valid upstream model ids,
/// sorted and de-duplicated. Entries with unusable ids are skipped rather than
/// failing the whole list; at most [`MAX_DISCOVERED_MODELS`] are kept.
///
/// `verified_model` is the model the connection was verified with. It is
/// always kept, even past the cap or when the endpoint omits it from its
/// list: chat rejects models missing from an authoritative list, so dropping
/// it would break a connection that is known to work.
pub fn parse_model_list(
    base_url: &str,
    body: &[u8],
    verified_model: Option<&str>,
) -> Result<Vec<String>, OpenAiCompatibleError> {
    let list = serde_json::from_slice::<ModelList>(body).map_err(|_| {
        OpenAiCompatibleError::InvalidModelList {
            base_url: base_url.to_string(),
        }
    })?;
    let verified = verified_model.filter(|model| validate_upstream_model(model).is_ok());
    let mut models = list
        .data
        .into_iter()
        .map(|entry| entry.id)
        .filter(|id| validate_upstream_model(id).is_ok() && Some(id.as_str()) != verified)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    match verified {
        Some(verified) => {
            models.truncate(MAX_DISCOVERED_MODELS - 1);
            let position = models
                .binary_search_by(|model| model.as_str().cmp(verified))
                .unwrap_or_else(|position| position);
            models.insert(position, verified.to_string());
        }
        None => models.truncate(MAX_DISCOVERED_MODELS),
    }
    Ok(models)
}

/// DNS resolver that rejects an entire answer when any address is private or
/// otherwise non-public, so an endpoint hostname cannot rebind to metadata or
/// an internal service between validation and connect time.
#[derive(Debug)]
pub struct ExternalOnlyResolver;

impl reqwest::dns::Resolve for ExternalOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?
                .collect();
            if addresses.is_empty() {
                return Err(format!("hostname '{host}' resolved to no addresses").into());
            }
            for address in &addresses {
                let result = match address.ip() {
                    std::net::IpAddr::V4(ip) => temps_core::url_validation::validate_ipv4(&ip),
                    std::net::IpAddr::V6(ip) => temps_core::url_validation::validate_ipv6(&ip),
                };
                if result.is_err() {
                    return Err(format!(
                        "hostname '{host}' resolved to a blocked internal address"
                    )
                    .into());
                }
            }
            let addresses: reqwest::dns::Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

/// HTTP client for user-supplied endpoints: public addresses only, and no
/// redirects, because an otherwise-public endpoint must not be able to forward
/// the API key to a redirect target of its choosing.
///
/// Inherited proxy settings (`HTTPS_PROXY`, `ALL_PROXY`, system proxies) are
/// ignored: through a proxy the endpoint's hostname would be resolved by the
/// proxy instead of [`ExternalOnlyResolver`], which could reach an internal
/// address the resolver exists to block.
pub fn external_only_http_client(
    request_timeout: Option<Duration>,
) -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(ExternalOnlyResolver));
    match request_timeout {
        Some(timeout) => builder.timeout(timeout),
        None => builder,
    }
    .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_canonicalizes_a_public_https_endpoint() {
        let credential = OpenAiCompatibleCredential::parse(
            r#"{"base_url":"  https://openrouter.ai/api/v1/ ","api_key":" sk-or-123 "}"#,
        )
        .unwrap();
        assert_eq!(credential.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(credential.api_key(), "sk-or-123");
        let document = credential.to_document().unwrap();
        assert_eq!(
            document,
            r#"{"base_url":"https://openrouter.ai/api/v1","api_key":"sk-or-123"}"#
        );
        // Round-trips to the same canonical endpoint.
        let again = OpenAiCompatibleCredential::parse(&document).unwrap();
        assert_eq!(again.base_url(), credential.base_url());
    }

    #[test]
    fn rejects_documents_with_unknown_or_missing_fields() {
        for raw in [
            "",
            "sk-plain-key",
            r#"{"base_url":"https://api.example.com/v1"}"#,
            r#"{"api_key":"sk"}"#,
            r#"{"base_url":"https://api.example.com/v1","api_key":"sk","headers":{}}"#,
        ] {
            assert_eq!(
                OpenAiCompatibleCredential::parse(raw).err(),
                Some(OpenAiCompatibleError::InvalidDocument),
                "accepted {raw}"
            );
        }
    }

    #[test]
    fn only_public_https_base_urls_without_credentials_are_accepted() {
        for blocked in [
            "http://api.example.com/v1",
            "https://localhost:8443/v1",
            "https://127.0.0.1/v1",
            "https://10.0.0.5/v1",
            "https://192.168.1.20/v1",
            "https://169.254.169.254/latest",
            "https://[::1]/v1",
            "https://user:pass@api.example.com/v1",
            "https://api.example.com/v1?key=abc",
            "https://api.example.com/v1#frag",
            "ftp://api.example.com/v1",
            "not a url",
            "",
        ] {
            assert!(
                matches!(
                    validate_base_url(blocked),
                    Err(OpenAiCompatibleError::InvalidBaseUrl { .. })
                ),
                "accepted {blocked}"
            );
        }
        assert_eq!(
            validate_base_url("https://api.groq.com/openai/v1").unwrap(),
            "https://api.groq.com/openai/v1"
        );
    }

    #[test]
    fn rejects_unusable_api_keys_and_names_the_endpoint() {
        for key in ["", "   ", "sk with space", "sk\u{0}nul"] {
            let raw =
                serde_json::json!({ "base_url": "https://api.example.com/v1", "api_key": key })
                    .to_string();
            assert_eq!(
                OpenAiCompatibleCredential::parse(&raw).err(),
                Some(OpenAiCompatibleError::InvalidApiKey {
                    base_url: "https://api.example.com/v1".to_string()
                })
            );
        }
    }

    #[test]
    fn model_selection_must_target_the_compatible_provider() {
        assert_eq!(
            upstream_model_from_selection("openai-compatible/meta-llama/llama-3.3-70b:free")
                .unwrap(),
            "meta-llama/llama-3.3-70b:free"
        );
        for invalid in [
            "openai/gpt-5",
            "openai-compatible/",
            "openai-compatible",
            "openai-compatible/-rm",
            "openai-compatible/../etc",
            "openai-compatible/model with space",
            "openai-compatible/model?x=1",
        ] {
            assert!(
                upstream_model_from_selection(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        assert_eq!(
            opencode_selection("gpt-4o-mini"),
            "openai-compatible/gpt-4o-mini"
        );
    }

    #[test]
    fn the_verified_model_round_trips_and_is_validated() {
        let stored = OpenAiCompatibleCredential::parse(
            r#"{"base_url":"https://api.example.com/v1","api_key":"sk-test"}"#,
        )
        .unwrap()
        .with_verified_selection("openai-compatible/vendor/model")
        .unwrap()
        .to_document()
        .unwrap();
        let parsed = OpenAiCompatibleCredential::parse(&stored).unwrap();
        assert_eq!(parsed.verified_model(), Some("vendor/model"));
        assert_eq!(parsed.without_verified_model().verified_model(), None);
        // Documents written before this field existed still parse.
        assert_eq!(
            OpenAiCompatibleCredential::parse(
                r#"{"base_url":"https://api.example.com/v1","api_key":"sk-test"}"#
            )
            .unwrap()
            .verified_model(),
            None
        );
        assert!(matches!(
            OpenAiCompatibleCredential::parse(
                r#"{"base_url":"https://api.example.com/v1","api_key":"sk-test","verified_model":"bad id"}"#
            )
            .err(),
            Some(OpenAiCompatibleError::InvalidModel { .. })
        ));
        assert!(OpenAiCompatibleCredential::parse(
            r#"{"base_url":"https://api.example.com/v1","api_key":"sk-test"}"#
        )
        .unwrap()
        .with_verified_selection("anthropic/not-an-endpoint-model")
        .is_err());
    }

    #[test]
    fn the_verified_model_survives_the_cap_and_an_incomplete_list() {
        let many = serde_json::json!({
            "data": (0..600).map(|index| serde_json::json!({ "id": format!("m{index:04}") })).collect::<Vec<_>>()
        })
        .to_string();
        // Sorted last, so plain truncation would drop it.
        let models =
            parse_model_list("https://api.example.com/v1", many.as_bytes(), Some("m0599")).unwrap();
        assert_eq!(models.len(), MAX_DISCOVERED_MODELS);
        assert_eq!(models.last().map(String::as_str), Some("m0599"));
        assert!(models.windows(2).all(|pair| pair[0] < pair[1]));

        // An endpoint that does not list the model it served still keeps it.
        let body = serde_json::json!({ "data": [{ "id": "zeta" }, { "id": "alpha" }] }).to_string();
        assert_eq!(
            parse_model_list(
                "https://api.example.com/v1",
                body.as_bytes(),
                Some("vendor/served-model")
            )
            .unwrap(),
            vec!["alpha", "vendor/served-model", "zeta"]
        );
        // Listed once even when the endpoint also lists it.
        assert_eq!(
            parse_model_list("https://api.example.com/v1", body.as_bytes(), Some("zeta")).unwrap(),
            vec!["alpha", "zeta"]
        );
        // An invalid saved value is never injected.
        assert_eq!(
            parse_model_list(
                "https://api.example.com/v1",
                body.as_bytes(),
                Some("bad id")
            )
            .unwrap(),
            vec!["alpha", "zeta"]
        );
    }

    #[test]
    fn model_lists_are_filtered_sorted_and_bounded() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "zeta", "object": "model" },
                { "id": "alpha", "owned_by": "x" },
                { "id": "alpha" },
                { "id": "bad id" },
                { "id": "-flag" }
            ]
        })
        .to_string();
        assert_eq!(
            parse_model_list("https://api.example.com/v1", body.as_bytes(), None).unwrap(),
            vec!["alpha".to_string(), "zeta".to_string()]
        );
        let many = serde_json::json!({
            "data": (0..600).map(|index| serde_json::json!({ "id": format!("m{index:04}") })).collect::<Vec<_>>()
        })
        .to_string();
        assert_eq!(
            parse_model_list("https://api.example.com/v1", many.as_bytes(), None)
                .unwrap()
                .len(),
            MAX_DISCOVERED_MODELS
        );
        assert_eq!(
            parse_model_list("https://api.example.com/v1", b"{\"models\":[]}", None).err(),
            Some(OpenAiCompatibleError::InvalidModelList {
                base_url: "https://api.example.com/v1".to_string()
            })
        );
    }

    #[test]
    fn external_only_client_builds() {
        assert!(external_only_http_client(Some(Duration::from_secs(5))).is_ok());
        assert!(external_only_http_client(None).is_ok());
    }
}
