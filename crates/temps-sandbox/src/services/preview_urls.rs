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
/// broken settings read falls back to the proxy listener so sandbox
/// endpoints keep working.
///
/// If a second consumer of this logic appears, prefer extracting a shared
/// `PreviewUrlParts::from_platform_config` helper in `temps-core` rather
/// than copy-pasting.
pub async fn load(platform_config: &Arc<ConfigService>) -> PreviewUrlParts {
    let proxy_port = platform_config.proxy_port();

    match platform_config.get_settings().await {
        Ok(s) => {
            let (protocol, port) = scheme_and_port(s.external_url.as_deref(), proxy_port);

            let domain = if s.preview_domain.is_empty() {
                "localho.st".to_string()
            } else {
                temps_core::public_base_domain(&s.preview_domain)
            };

            PreviewUrlParts {
                protocol,
                domain,
                port,
            }
        }
        Err(e) => {
            let (protocol, port) = scheme_and_port(None, proxy_port);
            tracing::warn!(
                "failed to load platform settings for sandbox preview URLs: {} — falling back to {}://localho.st:{}",
                e,
                protocol,
                proxy_port
            );
            PreviewUrlParts {
                protocol,
                domain: "localho.st".to_string(),
                port,
            }
        }
    }
}

/// Decide the scheme and explicit port a preview hostname should carry.
///
/// Split out from [`load`] so the rule is testable without a live
/// `ConfigService` (which needs a database).
///
/// With no `external_url` the public port IS the proxy listener port, exactly
/// as `compute_environment_url` resolves deployment/environment URLs. Assuming
/// `https` on the implicit :443 instead yields a hostname that only resolves on
/// an instance already terminating TLS on 443 with a wildcard certificate.
/// Anywhere else — a local instance on :8080, any self-host on a non-standard
/// port, an instance whose wildcard DNS isn't set up yet — every preview URL
/// then points at a port nothing is listening on, and the preview pane never
/// loads. `proxy_port()` is the single source of truth for that port.
fn scheme_and_port(external_url: Option<&str>, proxy_port: u16) -> (String, Option<u16>) {
    let (protocol, port) = match external_url {
        Some(external_url) => {
            if let Ok(parsed) = url::Url::parse(external_url) {
                (parsed.scheme().to_string(), parsed.port())
            } else if external_url.starts_with("http://") {
                ("http".to_string(), None)
            } else {
                ("https".to_string(), None)
            }
        }
        None => ("http".to_string(), Some(proxy_port)),
    };

    // An explicit :443/:80 matching the scheme is noise in the hostname.
    let port =
        port.filter(|p| !((protocol == "https" && *p == 443) || (protocol == "http" && *p == 80)));

    (protocol, port)
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

    /// The regression this module existed to cause: with no `external_url`
    /// configured, previews used to be handed `https://…` with no port, i.e.
    /// :443 — unreachable on every instance not already terminating TLS there,
    /// which is the default for a fresh self-host.
    #[test]
    fn without_external_url_falls_back_to_the_proxy_listener() {
        assert_eq!(
            scheme_and_port(None, 8080),
            ("http".to_string(), Some(8080))
        );

        let parts = PreviewUrlParts {
            protocol: "http".to_string(),
            domain: "localho.st".to_string(),
            port: Some(8080),
        };
        assert_eq!(
            parts.url_for("sbx_abcd1234ef567890", 5173),
            "http://ws-abcd1234ef567890-5173.localho.st:8080"
        );
    }

    #[test]
    fn external_url_still_wins_over_the_listener_port() {
        assert_eq!(
            scheme_and_port(Some("https://temps.example"), 8080),
            ("https".to_string(), None)
        );
        assert_eq!(
            scheme_and_port(Some("https://temps.example:8443"), 8080),
            ("https".to_string(), Some(8443))
        );
        assert_eq!(
            scheme_and_port(Some("http://temps.example:9000"), 8080),
            ("http".to_string(), Some(9000))
        );
    }

    #[test]
    fn default_ports_are_left_implicit() {
        assert_eq!(
            scheme_and_port(Some("https://temps.example:443"), 8080),
            ("https".to_string(), None)
        );
        assert_eq!(
            scheme_and_port(Some("http://temps.example:80"), 8080),
            ("http".to_string(), None)
        );
        // A proxy genuinely on :80 needs no explicit port either.
        assert_eq!(scheme_and_port(None, 80), ("http".to_string(), None));
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
