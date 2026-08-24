//! Remote plugin catalogue, and the local verification applied to it.
//!
//! The catalogue answers "what plugins exist?" — a question this binary
//! cannot answer on its own, because plugins are published on a release
//! cadence that has nothing to do with when an operator last upgraded. So the
//! list is fetched from the registry, where a human has approved each entry.
//!
//! **The catalogue carries no authority whatsoever.** Everything it says is
//! re-checked here against compile-time facts before it is allowed to
//! influence anything:
//!
//! * A name this release does not know is reported as *not installable*.
//!   It is still shown — an operator whose version predates a plugin should
//!   learn it exists and that upgrading is what unlocks it, rather than
//!   seeing nothing and concluding temps cannot do it.
//! * A `manifest_url` that disagrees with this release's compile-time URL for
//!   that plugin is refused outright. That is the field an attacker would
//!   want: whoever controls the manifest controls the asset URLs and the
//!   SHA-256 digests, and therefore the bytes that get executed. Nothing here
//!   is ever passed to the installer — `install_plugin` uses its own
//!   compile-time constant — so this check exists to *report* the discrepancy
//!   loudly rather than to prevent it silently.
//!
//! The result is that a fully compromised catalogue can hide a plugin from
//! the browse list. It cannot add one, redirect an install, or change which
//! bytes run on this host.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

use crate::install::read_body_capped;

/// The catalogue is small JSON — a few hundred bytes per plugin. Anything
/// remotely near this cap means the endpoint is not returning a catalogue.
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;

/// How long to wait for the registry before giving up.
///
/// Short, because this is a page load: an operator opening the plugins screen
/// on an air-gapped box must get the "registry unreachable" state promptly,
/// not stare at a spinner for half a minute.
const CATALOG_TIMEOUT_SECS: u64 = 10;

/// The registry catalogue endpoint.
///
/// Compile-time, for the same reason `KNOWN_PLUGINS[].manifest_url` is: the
/// set of hosts this binary will talk to about plugins is fixed at build
/// time, and no request body or database row can extend it.
pub const CATALOG_URL: &str = "https://temps.sh/api/plugins";

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("Failed to build HTTP client for plugin catalog {url}: {reason}")]
    Client { url: String, reason: String },

    #[error("Failed to fetch plugin catalog from {url}: {reason}")]
    Fetch { url: String, reason: String },

    #[error("Plugin catalog {url} returned HTTP {status}")]
    Status { url: String, status: u16 },

    #[error("Plugin catalog {url} exceeded the {limit} byte limit")]
    TooLarge { url: String, limit: u64 },

    #[error("Failed to parse plugin catalog from {url}: {reason}")]
    Parse { url: String, reason: String },
}

/// One entry exactly as the registry published it.
///
/// Every field is untrusted. Deserialized into its own type, separate from
/// the response DTO, so nothing can accidentally reach a client without
/// passing through [`verify`].
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteCatalogEntry {
    pub name: String,
    pub title: String,
    pub summary: String,
    pub description: String,
    pub author: String,
    pub category: String,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub docs_url: Option<String>,
    pub manifest_url: String,
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    plugins: Vec<RemoteCatalogEntry>,
}

/// Why a listed plugin cannot be installed by *this* build.
///
/// A distinct enum rather than a free-text reason so the console can render
/// the two cases differently — "upgrade temps" and "do not trust this
/// registry" call for very different UI — and so a reviewer can see at a
/// glance that both are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogRejection {
    /// The registry lists a plugin this release has never heard of.
    UnknownToThisRelease,
    /// The registry named a different manifest URL than this release trusts.
    ManifestUrlMismatch,
}

/// The verdict of local verification for one catalogue entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    /// Whether this build would accept an install request for this plugin.
    pub installable: bool,
    /// Why not, when `installable` is false.
    pub rejection: Option<CatalogRejection>,
    /// Operator-facing explanation. Always populated when rejected.
    pub reason: Option<String>,
}

impl Verification {
    fn ok() -> Self {
        Self {
            installable: true,
            rejection: None,
            reason: None,
        }
    }

    fn reject(rejection: CatalogRejection, reason: String) -> Self {
        Self {
            installable: false,
            rejection: Some(rejection),
            reason: Some(reason),
        }
    }
}

/// Check one catalogue entry against this build's compile-time facts.
///
/// `trusted_manifest_url` is `None` when the name is absent from
/// `KNOWN_PLUGINS`, and `Some(url)` with that entry's compile-time manifest
/// URL otherwise. Pure and total, so the rules are testable without a
/// registry, a network, or a running server.
pub fn verify(entry: &RemoteCatalogEntry, trusted_manifest_url: Option<&str>) -> Verification {
    let Some(trusted) = trusted_manifest_url else {
        return Verification::reject(
            CatalogRejection::UnknownToThisRelease,
            format!(
                "'{}' is published in the registry but is not in this temps release's \
                 installable-plugin allowlist. Upgrade temps to install it.",
                entry.name
            ),
        );
    };

    if entry.manifest_url != trusted {
        return Verification::reject(
            CatalogRejection::ManifestUrlMismatch,
            format!(
                "The registry advertises manifest URL '{}' for '{}', but this release \
                 trusts '{}'. Refusing to list it as installable — the manifest names the \
                 asset URLs and checksums that would be executed.",
                entry.manifest_url, entry.name, trusted
            ),
        );
    }

    Verification::ok()
}

/// Fetch the published catalogue.
///
/// `url` must be a compile-time constant supplied by the caller, never a
/// value taken from a request — same invariant as
/// [`crate::install::PluginInstaller::fetch_manifest`].
pub async fn fetch_catalog(url: &str) -> Result<Vec<RemoteCatalogEntry>, CatalogError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(CATALOG_TIMEOUT_SECS))
        .build()
        .map_err(|e| CatalogError::Client {
            url: url.to_string(),
            reason: e.to_string(),
        })?;

    let response = client
        .get(url)
        .header("User-Agent", "temps-plugin-installer")
        .send()
        .await
        .map_err(|e| CatalogError::Fetch {
            url: url.to_string(),
            reason: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(CatalogError::Status {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }

    let body = read_body_capped(response, url, "Catalog", MAX_CATALOG_BYTES)
        .await
        .map_err(|_| CatalogError::TooLarge {
            url: url.to_string(),
            limit: MAX_CATALOG_BYTES,
        })?;

    let parsed =
        serde_json::from_slice::<CatalogResponse>(&body).map_err(|e| CatalogError::Parse {
            url: url.to_string(),
            reason: e.to_string(),
        })?;

    Ok(parsed.plugins)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, manifest_url: &str) -> RemoteCatalogEntry {
        RemoteCatalogEntry {
            name: name.to_string(),
            title: "Test".to_string(),
            summary: "Test".to_string(),
            description: "Test".to_string(),
            author: "Test".to_string(),
            category: "Test".to_string(),
            repository: None,
            docs_url: None,
            manifest_url: manifest_url.to_string(),
            latest_version: Some("1.0.0".to_string()),
            platforms: vec!["linux-amd64".to_string()],
        }
    }

    #[test]
    fn matching_manifest_url_is_installable() {
        let e = entry(
            "vibetemps",
            "https://temps.sh/api/plugins/vibetemps/manifest.json",
        );
        let v = verify(
            &e,
            Some("https://temps.sh/api/plugins/vibetemps/manifest.json"),
        );
        assert!(v.installable);
        assert_eq!(v.rejection, None);
    }

    #[test]
    fn unknown_plugin_is_listed_but_not_installable() {
        let e = entry(
            "something-new",
            "https://temps.sh/api/plugins/something-new/manifest.json",
        );
        let v = verify(&e, None);
        assert!(!v.installable);
        assert_eq!(v.rejection, Some(CatalogRejection::UnknownToThisRelease));
        // The operator must be told upgrading is the fix, not left guessing.
        assert!(v.reason.unwrap().contains("Upgrade temps"));
    }

    #[test]
    fn redirected_manifest_url_is_refused() {
        let e = entry("vibetemps", "https://evil.example.com/manifest.json");
        let v = verify(
            &e,
            Some("https://temps.sh/api/plugins/vibetemps/manifest.json"),
        );
        assert!(!v.installable);
        assert_eq!(v.rejection, Some(CatalogRejection::ManifestUrlMismatch));
        // Both URLs belong in the message: "they differ" is not actionable.
        let reason = v.reason.unwrap();
        assert!(reason.contains("evil.example.com"));
        assert!(reason.contains("temps.sh"));
    }

    #[test]
    fn catalog_response_parses_with_optional_fields_absent() {
        let json = br#"{"plugins":[{"name":"vibetemps","title":"VibeTemps",
            "summary":"s","description":"d","author":"Temps","category":"Development",
            "manifest_url":"https://temps.sh/api/plugins/vibetemps/manifest.json"}]}"#;
        let parsed: CatalogResponse = serde_json::from_slice(json).expect("parses");
        assert_eq!(parsed.plugins.len(), 1);
        // A listing with no published release must not fail the whole fetch.
        assert_eq!(parsed.plugins[0].latest_version, None);
        assert!(parsed.plugins[0].platforms.is_empty());
    }
}
