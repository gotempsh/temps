// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Opt-in v2 request enrichment. The HTTP handler supplies the trusted-proxy
//! resolved IP, never an SDK attribute or an unchecked forwarded header.

use std::collections::BTreeMap;

use super::{
    envelope::EnvelopeError,
    logs::{Batch, IngestSettings},
};
use temps_analytics_session_replay::services::BrowserInfo;

pub(super) fn validate(settings: Option<&IngestSettings>) -> Result<(), EnvelopeError> {
    if settings.is_some_and(|settings| {
        [&settings.infer_ip, &settings.infer_user_agent]
            .into_iter()
            .any(|value| {
                value
                    .as_deref()
                    .is_some_and(|v| v != "auto" && v != "never")
            })
    }) {
        return Err(EnvelopeError::InvalidPayload(
            "invalid inference setting".into(),
        ));
    }
    Ok(())
}

pub(crate) struct RequestInference {
    ip: Option<String>,
    browser: std::cell::OnceCell<BrowserInfo>,
    user_agent: Option<String>,
}

impl RequestInference {
    pub(crate) fn new(ip: &str, headers: &http::HeaderMap) -> Self {
        let ua = headers
            .get(http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() <= 4096);
        Self {
            ip: ip.parse::<std::net::IpAddr>().ok().map(|ip| ip.to_string()),
            browser: std::cell::OnceCell::new(),
            user_agent: ua.map(str::to_owned),
        }
    }

    pub(crate) fn enrich<'a>(
        &self,
        payload: &[u8],
        records: impl Iterator<Item = &'a mut BTreeMap<String, String>>,
    ) -> Result<(), EnvelopeError> {
        // Borrowed bounded parse: no second copy of bodies or attribute values.
        let batch: Batch<'_> = serde_json::from_slice(payload)
            .map_err(|_| EnvelopeError::InvalidPayload("invalid inference batch".into()))?;
        if batch.version != Some(2) {
            return Ok(());
        }
        let Some(settings) = batch.ingest_settings else {
            return Ok(());
        };
        for attributes in records {
            if settings.infer_ip.as_deref() == Some("auto") {
                if let Some(ip) = &self.ip {
                    attributes
                        .entry("user.ip_address".into())
                        .or_insert_with(|| ip.clone());
                }
            }
            if settings.infer_user_agent.as_deref() == Some("auto") {
                let browser = self
                    .browser
                    .get_or_init(|| BrowserInfo::from_user_agent(self.user_agent.as_deref()));
                for (key, value) in [
                    ("browser.name", &browser.browser),
                    ("browser.version", &browser.browser_version),
                    ("os.name", &browser.operating_system),
                    ("os.version", &browser.operating_system_version),
                ] {
                    if let Some(value) = value {
                        attributes
                            .entry(key.into())
                            .or_insert_with(|| value.clone());
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_is_versioned_opt_in_and_preserves_explicit_values() {
        let mut headers = http::HeaderMap::new();
        headers.insert("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36".parse().unwrap());
        let request = RequestInference::new("192.0.2.1", &headers);
        for (payload, inferred) in [
            (
                r#"{"items":[],"version":2,"ingest_settings":{"infer_ip":"auto","infer_user_agent":"auto"}}"#,
                true,
            ),
            (
                r#"{"items":[],"ingest_settings":{"infer_ip":"auto","infer_user_agent":"auto"}}"#,
                false,
            ),
            (
                r#"{"items":[],"version":2,"ingest_settings":{"infer_ip":"never","infer_user_agent":"never"}}"#,
                false,
            ),
            (r#"{"items":[],"version":2}"#, false),
        ] {
            let mut attrs = BTreeMap::from([("browser.name".into(), "Explicit browser".into())]);
            request
                .enrich(payload.as_bytes(), std::iter::once(&mut attrs))
                .unwrap();
            assert_eq!(attrs.contains_key("user.ip_address"), inferred);
            assert_eq!(attrs.contains_key("browser.version"), inferred);
            assert_eq!(attrs["browser.name"], "Explicit browser");
            if inferred {
                assert_eq!(attrs["user.ip_address"], "192.0.2.1");
            }
        }
    }
}
