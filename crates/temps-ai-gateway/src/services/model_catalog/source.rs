// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fetching and validating a models.dev-compatible model catalog.
//!
//! The document is a JSON object keyed by provider id; each provider holds a
//! `models` object keyed by model id. Only the providers Temps serves are
//! read (the rest are skipped while parsing, never materialised), each model
//! is checked against a typed schema, and an entry that fails is skipped and
//! counted rather than failing the whole catalog. Prices are USD per 1M
//! tokens, as on models.dev.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use url::Url;

use super::error::ModelCatalogError;
use crate::providers::ProviderHttpClient;
use crate::services::pricing::{is_valid_catalog_price, CatalogPrice};

/// Largest catalog body accepted. models.dev's full catalog of every provider
/// is under 8 MiB; this leaves room to grow while bounding memory.
pub const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
/// Most models accepted across the supported providers.
pub const MAX_CATALOG_MODELS: usize = 20_000;
/// Whole-request deadline for a catalog fetch.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

const MAX_MODEL_ID_LEN: usize = 200;
const MAX_DISPLAY_NAME_LEN: usize = 200;
const MAX_RELEASE_DATE_LEN: usize = 32;
const MAX_TOKEN_LIMIT: u64 = 100_000_000;

/// Catalog provider ids Temps reads, and the Temps provider each maps to.
/// models.dev calls Gemini `google`; the Temps id is accepted too.
const PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "anthropic"),
    ("openai", "openai"),
    ("google", "gemini"),
    ("gemini", "gemini"),
    ("xai", "xai"),
    ("openrouter", "openrouter"),
];

fn temps_provider(catalog_provider: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find(|(id, _)| *id == catalog_provider)
        .map(|(_, temps)| *temps)
}

/// One validated catalog model.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModel {
    pub provider: String,
    pub model_id: String,
    pub display_name: String,
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    /// `null` when the catalog does not say.
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub deprecated: bool,
    pub release_date: Option<String>,
}

impl CatalogModel {
    /// The model's price, when the catalog publishes both input and output.
    pub fn price(&self) -> Option<CatalogPrice> {
        Some(CatalogPrice {
            provider: self.provider.clone(),
            model: self.model_id.clone(),
            display_name: self.display_name.clone(),
            input_per_million: self.input_per_million?,
            output_per_million: self.output_per_million?,
            cache_read_per_million: self.cache_read_per_million,
            cache_write_per_million: self.cache_write_per_million,
            deprecated: self.deprecated,
        })
    }
}

/// The validated models of a catalog and how many entries were rejected.
#[derive(Debug, Default)]
pub struct ParsedCatalog {
    pub models: Vec<CatalogModel>,
    pub skipped: usize,
}

// ----------------------------------------------------------------------------
// Schema
// ----------------------------------------------------------------------------

#[derive(Deserialize)]
struct ProviderDoc {
    /// Kept as raw values so one malformed model is skipped, not fatal.
    #[serde(default)]
    models: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct ModelDoc {
    #[serde(default)]
    name: Option<String>,
    // Absent means the catalog does not say, not "no".
    #[serde(default)]
    attachment: Option<bool>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    tool_call: Option<bool>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    modalities: Option<ModalitiesDoc>,
    #[serde(default)]
    limit: Option<LimitDoc>,
    #[serde(default)]
    cost: Option<CostDoc>,
    /// models.dev marks retired models `"deprecated"`.
    #[serde(default)]
    status: Option<String>,
    /// Set by the catalog Temps publishes.
    #[serde(default)]
    deprecated: bool,
}

#[derive(Deserialize, Default)]
struct ModalitiesDoc {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Deserialize, Default)]
struct LimitDoc {
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
}

#[derive(Deserialize, Default)]
struct CostDoc {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
}

/// The supported providers of a catalog document. Other providers' values
/// are skipped by the deserializer without being built.
struct CatalogDocument(Vec<(&'static str, ProviderDoc)>);

impl<'de> Deserialize<'de> for CatalogDocument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DocumentVisitor;

        impl<'de> Visitor<'de> for DocumentVisitor {
            type Value = CatalogDocument;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object keyed by provider id")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut providers = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    match temps_provider(&key) {
                        Some(provider) => providers.push((provider, map.next_value()?)),
                        None => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(CatalogDocument(providers))
            }
        }

        deserializer.deserialize_map(DocumentVisitor)
    }
}

// ----------------------------------------------------------------------------
// Validation
// ----------------------------------------------------------------------------

fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MODEL_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`'))
}

fn clean_text(text: &str, max_len: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max_len)
        .collect::<String>()
        .trim()
        .to_string()
}

fn token_limit(value: Option<u64>) -> Option<i64> {
    value
        .filter(|tokens| (1..=MAX_TOKEN_LIMIT).contains(tokens))
        .and_then(|tokens| i64::try_from(tokens).ok())
}

/// Validate one model entry; `None` rejects it.
fn validate(provider: &'static str, id: &str, raw: serde_json::Value) -> Option<CatalogModel> {
    if !valid_model_id(id) {
        return None;
    }
    let doc: ModelDoc = serde_json::from_value(raw).ok()?;
    let cost = doc.cost.unwrap_or_default();
    // A price that is present must be valid: a malformed price is a broken
    // entry, not a missing one, and must not fall back to "unpriced".
    let price = |value: Option<f64>| -> Result<Option<f64>, ()> {
        match value {
            None => Ok(None),
            Some(price) if is_valid_catalog_price(price) => Ok(Some(price)),
            Some(_) => Err(()),
        }
    };
    let input = price(cost.input).ok()?;
    let output = price(cost.output).ok()?;
    let cache_read = price(cost.cache_read).ok()?;
    let cache_write = price(cost.cache_write).ok()?;
    let display_name = doc
        .name
        .as_deref()
        .map(|name| clean_text(name, MAX_DISPLAY_NAME_LEN))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| id.to_string());
    let limit = doc.limit.unwrap_or_default();
    let image_input = doc
        .modalities
        .as_ref()
        .map(|modalities| modalities.input.iter().any(|kind| kind == "image"));
    let supports_vision = match (doc.attachment, image_input) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (None, None) => None,
        _ => Some(false),
    };
    Some(CatalogModel {
        provider: provider.to_string(),
        model_id: id.to_string(),
        display_name,
        input_per_million: input,
        output_per_million: output,
        cache_read_per_million: cache_read,
        cache_write_per_million: cache_write,
        context_window: token_limit(limit.context),
        max_output_tokens: token_limit(limit.output),
        supports_tools: doc.tool_call,
        supports_vision,
        supports_reasoning: doc.reasoning,
        deprecated: doc.deprecated || doc.status.as_deref() == Some("deprecated"),
        release_date: doc
            .release_date
            .map(|date| clean_text(&date, MAX_RELEASE_DATE_LEN))
            .filter(|date| !date.is_empty()),
    })
}

/// Parse and validate a catalog document. `url` is only used in errors.
pub fn parse_catalog(url: &str, body: &[u8]) -> Result<ParsedCatalog, ModelCatalogError> {
    let document: CatalogDocument =
        serde_json::from_slice(body).map_err(|error| ModelCatalogError::Parse {
            url: url.to_string(),
            reason: error.to_string(),
        })?;
    let mut parsed = ParsedCatalog::default();
    let mut seen = std::collections::HashSet::new();
    for (provider, doc) in document.0 {
        let mut entries: Vec<(String, serde_json::Value)> = doc.models.into_iter().collect();
        // Deterministic order, so the stored catalog and its errors do not
        // depend on hash order.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (id, raw) in entries {
            if parsed.models.len() >= MAX_CATALOG_MODELS {
                return Err(ModelCatalogError::TooManyEntries {
                    url: url.to_string(),
                    limit: MAX_CATALOG_MODELS,
                });
            }
            // `google` and `gemini` both map to Gemini: the first listing wins.
            if !seen.insert((provider, id.clone())) {
                continue;
            }
            match validate(provider, &id, raw) {
                Some(model) => parsed.models.push(model),
                None => parsed.skipped += 1,
            }
        }
    }
    if parsed.models.is_empty() {
        return Err(ModelCatalogError::Empty {
            url: url.to_string(),
            skipped: parsed.skipped,
        });
    }
    Ok(parsed)
}

// ----------------------------------------------------------------------------
// Source URL and fetch
// ----------------------------------------------------------------------------

/// Check a catalog source URL: `https://` only, no credentials, no query
/// string or fragment, and not a private, loopback or metadata address.
/// Hostnames are checked again when the connection is made (the hardened
/// client's resolver), and redirects are never followed, so a public host
/// cannot hand the request to a private one.
///
/// A catalog is a public document. The saved URL is returned to every gateway
/// reader, stored on each catalog entry and written to refresh logs, so
/// nothing that could carry an access token (userinfo, a signed query, a
/// fragment) is accepted.
pub fn validate_source_url(url: &str) -> Result<Url, ModelCatalogError> {
    let invalid = |reason: &str| ModelCatalogError::InvalidSourceUrl {
        url: redacted_source_url(url),
        reason: reason.to_string(),
    };
    if url.len() > 2048 {
        return Err(invalid("the URL is longer than 2048 characters"));
    }
    let parsed = Url::parse(url).map_err(|error| invalid(&error.to_string()))?;
    if parsed.scheme() != "https" {
        return Err(invalid("only https:// URLs are allowed"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(
            "the URL must not carry credentials; a catalog is a public document",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(invalid(
            "the URL must not have a query string or fragment; a catalog is a public \
             document and signed or token-bearing URLs are not supported",
        ));
    }
    temps_core::url_validation::validate_external_url(url)
        .map_err(|error| invalid(&error.to_string()))?;
    Ok(parsed)
}

/// A rejected source URL as it may appear in an error: userinfo, query and
/// fragment removed, so a mistyped signed URL is never echoed back or logged.
/// Input the URL parser cannot read is withheld when it could hold a
/// credential (`@`, `?` or `#`), since there is no reliable way to cut it out.
fn redacted_source_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) if url.contains(['@', '?', '#']) => {
            "(withheld: the URL may contain credentials)".to_string()
        }
        Err(_) => url.to_string(),
    }
}

/// Fetch a catalog body, bounded in time and size.
pub(crate) async fn fetch_catalog(
    client: &ProviderHttpClient,
    url: &Url,
) -> Result<Vec<u8>, ModelCatalogError> {
    let display = url.as_str().to_string();
    let request = async {
        let mut response = client
            .get(url.as_str())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "temps-model-catalog")
            .send()
            .await
            .map_err(|error| ModelCatalogError::Fetch {
                url: display.clone(),
                reason: describe_request_error(error),
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(ModelCatalogError::HttpStatus {
                url: display.clone(),
                status: status.as_u16(),
            });
        }
        let too_large = || ModelCatalogError::TooLarge {
            url: display.clone(),
            limit_bytes: MAX_CATALOG_BYTES,
        };
        if response
            .content_length()
            .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) =
            response
                .chunk()
                .await
                .map_err(|error| ModelCatalogError::Fetch {
                    url: display.clone(),
                    reason: describe_request_error(error),
                })?
        {
            if body.len() + chunk.len() > MAX_CATALOG_BYTES {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    };
    tokio::time::timeout(FETCH_TIMEOUT, request)
        .await
        .map_err(|_| ModelCatalogError::Timeout {
            url: url.as_str().to_string(),
            seconds: FETCH_TIMEOUT.as_secs(),
        })?
}

/// A request error without the URL reqwest appends (already in the message).
fn describe_request_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
