//! Preview URL construction for standalone sandboxes. Reuses the same
//! `ws-<id>-<port>.<domain>` hostname scheme used by workspace sessions
//! so the existing preview gateway routes both kinds of sandbox without
//! modification.
//!
//! The gateway (`crates/temps-preview-gateway`) parses the hostname as
//! `ws-<sid>-<port>` and forwards to `temps-sandbox-<sid>:<port>`. Since
//! standalone sandbox IDs are offset to ≥ 1,000,000 they can never
//! collide with agent-run IDs in the Docker container namespace.

use std::sync::Arc;

use temps_config::ConfigService;

#[derive(Clone, Debug)]
pub struct PreviewUrlParts {
    pub protocol: String,
    pub domain: String,
    pub port: Option<u16>,
}

impl PreviewUrlParts {
    /// Compute the public URL for a given sandbox public_id + port.
    ///
    /// `public_id` is the opaque `sbx_<16hex>` identifier — never the
    /// numeric primary key. The numeric id leaks ordering/enumeration
    /// across tenants; `public_id` is unguessable, which matters because
    /// the preview hostname is all the auth a sandbox port has.
    ///
    /// The `sbx_` prefix is stripped before embedding in the hostname —
    /// underscores are not valid in DNS labels (RFC 1123) so we encode
    /// just the 16-hex-char suffix. The gateway re-adds the `sbx_`
    /// prefix when it resolves the container name.
    pub fn url_for(&self, public_id: &str, port: u16) -> String {
        let label = public_id.strip_prefix("sbx_").unwrap_or(public_id);
        let host = format!("ws-{}-{}.{}", label, port, self.domain);
        let host_with_port = match self.port {
            Some(p) => format!("{}:{}", host, p),
            None => host,
        };
        format!("{}://{}", self.protocol, host_with_port)
    }

    /// Template string with `{port}` placeholder — used by the UI to
    /// render a "any port → URL" hint without round-tripping every
    /// integer port through the backend.
    pub fn host_template(&self, public_id: &str) -> String {
        let label = public_id.strip_prefix("sbx_").unwrap_or(public_id);
        let host = format!("ws-{}-{{port}}.{}", label, self.domain);
        let host_with_port = match self.port {
            Some(p) => format!("{}:{}", host, p),
            None => host,
        };
        format!("{}://{}", self.protocol, host_with_port)
    }
}

/// Load preview URL parts from platform settings. Never errors — a
/// broken settings read falls back to `https://localho.st` so sandbox
/// endpoints keep working.
///
/// If a second consumer of this logic appears, prefer extracting a shared
/// `PreviewUrlParts::from_platform_config` helper in `temps-core` rather
/// than copy-pasting.
pub async fn load(platform_config: &Arc<ConfigService>) -> PreviewUrlParts {
    match platform_config.get_settings().await {
        Ok(s) => {
            let (protocol, port) = if let Some(ref external_url) = s.external_url {
                if let Ok(parsed) = url::Url::parse(external_url) {
                    (parsed.scheme().to_string(), parsed.port())
                } else if external_url.starts_with("https://") {
                    ("https".to_string(), None)
                } else if external_url.starts_with("http://") {
                    ("http".to_string(), None)
                } else {
                    ("https".to_string(), None)
                }
            } else {
                ("https".to_string(), None)
            };

            let domain = if s.preview_domain.is_empty() {
                "localho.st".to_string()
            } else {
                temps_core::public_base_domain(&s.preview_domain)
            };

            let port = port.filter(|p| {
                !((protocol == "https" && *p == 443) || (protocol == "http" && *p == 80))
            });

            PreviewUrlParts {
                protocol,
                domain,
                port,
            }
        }
        Err(e) => {
            tracing::warn!(
                "failed to load platform settings for sandbox preview URLs: {} — falling back to https://localho.st",
                e
            );
            PreviewUrlParts {
                protocol: "https".to_string(),
                domain: "localho.st".to_string(),
                port: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_for_default_parts_produces_expected_host() {
        let parts = PreviewUrlParts {
            protocol: "https".to_string(),
            domain: "localho.st".to_string(),
            port: None,
        };
        assert_eq!(
            parts.url_for("sbx_abcd1234ef567890", 3000),
            "https://ws-abcd1234ef567890-3000.localho.st"
        );
    }

    #[test]
    fn url_for_with_external_port_appends_port() {
        let parts = PreviewUrlParts {
            protocol: "http".to_string(),
            domain: "example.test".to_string(),
            port: Some(8080),
        };
        assert_eq!(
            parts.url_for("sbx_deadbeef00001122", 5173),
            "http://ws-deadbeef00001122-5173.example.test:8080"
        );
    }

    #[test]
    fn host_template_has_port_placeholder() {
        let parts = PreviewUrlParts {
            protocol: "https".to_string(),
            domain: "localho.st".to_string(),
            port: None,
        };
        assert_eq!(
            parts.host_template("sbx_abcd1234ef567890"),
            "https://ws-abcd1234ef567890-{port}.localho.st"
        );
    }
}
