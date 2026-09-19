// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded, unsigned discovery metadata for GitHub-source plugins.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use utoipa::ToSchema;

pub const SOURCE: &str =
    "https://raw.githubusercontent.com/gotempsh/plugins/main/registry/catalog.json";
const MAX_BYTES: usize = 512 * 1024;
const TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryCatalogPlugin {
    pub name: String,
    pub title: String,
    pub summary: String,
    pub description: String,
    pub author: String,
    pub category: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_name: Option<String>,
    pub docs_url: Option<String>,
    pub logo_url: Option<String>,
    pub screenshots: Vec<RepositoryScreenshot>,
    pub latest_version: String,
    pub platforms: Vec<String>,
    pub commit: String,
    pub readme_url: Option<String>,
    pub validation: RepositoryValidation,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RepositoryScreenshot {
    pub url: String,
    pub alt: String,
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RepositoryValidation {
    pub metadata: String,
    pub build: String,
}

#[derive(Debug, Deserialize)]
struct Document {
    schema_version: u32,
    plugins: Vec<RepositoryCatalogPlugin>,
}

#[derive(Debug, Clone, Error)]
pub enum SourceCatalogError {
    #[error("GitHub plugin source catalog request failed: {reason}")]
    Fetch { reason: String },
    #[error("GitHub plugin source catalog is invalid: {reason}")]
    Invalid { reason: String },
}

struct Cached {
    expires: Instant,
    result: Result<Vec<RepositoryCatalogPlugin>, SourceCatalogError>,
}

/// Mutex coalesces concurrent misses; no polling or network traffic until a read.
#[derive(Default)]
pub struct SourceCatalog {
    cache: Mutex<Option<Cached>>,
    #[cfg(test)]
    test_source: Option<String>,
}

impl SourceCatalog {
    pub async fn list(
        &self,
        platform: &str,
    ) -> Result<Vec<RepositoryCatalogPlugin>, SourceCatalogError> {
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache
            .as_ref()
            .filter(|entry| entry.expires > Instant::now())
        {
            return cached
                .result
                .clone()
                .map(|plugins| filter_platform(&plugins, platform));
        }
        #[cfg(test)]
        let source = self.test_source.as_deref().unwrap_or(SOURCE);
        #[cfg(not(test))]
        let source = SOURCE;
        let result = Self::fetch(source).await;
        let expires = Instant::now()
            + if result.is_ok() {
                TTL
            } else {
                Duration::from_secs(15)
            };
        *cache = Some(Cached {
            expires,
            result: result.clone(),
        });
        result.map(|plugins| filter_platform(&plugins, platform))
    }

    async fn fetch(source: &str) -> Result<Vec<RepositoryCatalogPlugin>, SourceCatalogError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SourceCatalogError::Fetch {
                reason: error.to_string(),
            })?;
        let response =
            client
                .get(source)
                .send()
                .await
                .map_err(|error| SourceCatalogError::Fetch {
                    reason: error.to_string(),
                })?;
        if !response.status().is_success() {
            return Err(SourceCatalogError::Fetch {
                reason: format!("HTTP {}", response.status()),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BYTES as u64)
        {
            return Err(SourceCatalogError::Invalid {
                reason: "response exceeds 512 KiB".into(),
            });
        }
        let mut stream = response.bytes_stream();
        use futures::StreamExt as _;
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| SourceCatalogError::Fetch {
                reason: error.to_string(),
            })?;
            if body.len().saturating_add(chunk.len()) > MAX_BYTES {
                return Err(SourceCatalogError::Invalid {
                    reason: "response exceeds 512 KiB".into(),
                });
            }
            body.extend_from_slice(&chunk);
        }
        parse_document(&body)
    }
}

fn filter_platform(
    plugins: &[RepositoryCatalogPlugin],
    platform: &str,
) -> Vec<RepositoryCatalogPlugin> {
    plugins
        .iter()
        .filter(|plugin| plugin.platforms.iter().any(|entry| entry == platform))
        .cloned()
        .collect()
}

fn valid_https_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && matches!(
                url.host_str(),
                Some("github.com" | "raw.githubusercontent.com")
            )
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query().is_none()
    })
}

fn parse_document(body: &[u8]) -> Result<Vec<RepositoryCatalogPlugin>, SourceCatalogError> {
    let document: Document =
        serde_json::from_slice(body).map_err(|error| SourceCatalogError::Invalid {
            reason: error.to_string(),
        })?;
    if !matches!(document.schema_version, 1 | 2) || document.plugins.len() > 100 {
        return Err(SourceCatalogError::Invalid {
            reason: "unsupported schema version or too many entries".into(),
        });
    }
    let mut names = HashSet::new();
    let mut repositories = HashSet::new();
    for plugin in &document.plugins {
        let valid = crate::install::validate_plugin_name(&plugin.name).is_ok()
            && crate::repository::parse_repository(&plugin.repository).is_ok()
            && (document.schema_version == 2 || plugin.path.is_none())
            && matches!(crate::repository::normalize_path(plugin.path.as_deref(), &plugin.repository), Ok(path) if path == plugin.path)
            && plugin
                .ref_name
                .as_deref()
                .is_none_or(crate::repository::valid_git_ref)
            && plugin.commit.len() == 40
            && plugin.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            && bounded_text(&plugin.title, 160)
            && bounded_text(&plugin.summary, 500)
            && bounded_text(&plugin.description, 4000)
            && bounded_text(&plugin.author, 160)
            && bounded_text(&plugin.category, 80)
            && crate::install::validate_version(&plugin.name, &plugin.latest_version).is_ok()
            && plugin.platforms.len() <= 6
            && plugin.platforms.iter().all(|platform| {
                matches!(
                    platform.as_str(),
                    "linux-amd64-gnu"
                        | "linux-arm64-gnu"
                        | "linux-amd64-musl"
                        | "linux-arm64-musl"
                        | "darwin-amd64"
                        | "darwin-arm64"
                )
            })
            && plugin.readme_url.as_deref().is_none_or(valid_https_url)
            && plugin.docs_url.as_deref().is_none_or(valid_https_url)
            && plugin.logo_url.as_deref().is_none_or(valid_https_url)
            && plugin.screenshots.len() <= 8
            && plugin.screenshots.iter().all(|shot| {
                valid_https_url(&shot.url)
                    && bounded_text(&shot.alt, 200)
                    && shot
                        .caption
                        .as_deref()
                        .is_none_or(|caption| caption.len() <= 500)
            });
        if !valid
            || !names.insert(plugin.name.clone())
            || !repositories.insert((plugin.repository.to_ascii_lowercase(), plugin.path.clone()))
        {
            return Err(SourceCatalogError::Invalid {
                reason: format!("unsafe or duplicate plugin entry '{}'", plugin.name),
            });
        }
    }
    Ok(document.plugins)
}

fn bounded_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mock_catalog(response: &'static [u8]) -> (SourceCatalog, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream.write_all(response).await.unwrap();
        });
        (
            SourceCatalog {
                cache: Mutex::new(None),
                test_source: Some(format!("http://{address}/catalog.json")),
            },
            server,
        )
    }

    #[test]
    fn rejects_unsafe_and_duplicate_entries() {
        let entry = serde_json::json!({"name":"demo","title":"Demo","summary":"Summary","description":"Description","author":"Team","category":"Development","repository":"https://github.com/example/demo","docsUrl":null,"logoUrl":null,"screenshots":[],"latestVersion":"1.0.0","platforms":["linux-amd64-gnu"],"commit":"a".repeat(40),"readmeUrl":"https://raw.githubusercontent.com/example/demo/README.md","validation":{"metadata":"passed","build":"not_run"}});
        let body =
            serde_json::to_vec(&serde_json::json!({"schema_version":1,"plugins":[entry.clone()]}))
                .unwrap();
        let plugins = parse_document(&body).unwrap();
        assert_eq!(filter_platform(&plugins, "darwin-arm64").len(), 0);
        assert_eq!(filter_platform(&plugins, "linux-amd64-gnu").len(), 1);
        let duplicates = serde_json::to_vec(
            &serde_json::json!({"schema_version":1,"plugins":[entry.clone(),entry]}),
        )
        .unwrap();
        assert!(matches!(
            parse_document(&duplicates),
            Err(SourceCatalogError::Invalid { .. })
        ));
        let mut unsafe_entry = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
        unsafe_entry["plugins"][0]["repository"] =
            serde_json::json!("https://evil.test/example/demo");
        let unsafe_body = serde_json::to_vec(&unsafe_entry).unwrap();
        assert!(matches!(
            parse_document(&unsafe_body),
            Err(SourceCatalogError::Invalid { .. })
        ));
    }

    #[test]
    fn schema_two_allows_sibling_paths_and_rejects_unsafe_or_duplicate_paths() {
        let base = serde_json::json!({"name":"one","title":"One","summary":"Summary","description":"Description","author":"Team","category":"Development","repository":"https://github.com/example/plugins","path":"plugins/one","ref":"release/v1","docsUrl":null,"logoUrl":null,"screenshots":[],"latestVersion":"1.0.0","platforms":["linux-amd64-gnu"],"commit":"a".repeat(40),"readmeUrl":null,"validation":{"metadata":"passed","build":"not_run"}});
        let mut sibling = base.clone();
        sibling["name"] = "two".into();
        sibling["path"] = "plugins/two".into();
        let encoded = |version, plugins| {
            serde_json::to_vec(&serde_json::json!({"schema_version":version,"plugins":plugins}))
                .expect("catalog JSON")
        };
        assert_eq!(
            parse_document(&encoded(2, vec![base.clone(), sibling.clone()]))
                .expect("siblings")
                .len(),
            2
        );
        assert!(parse_document(&encoded(1, vec![base.clone()])).is_err());
        sibling["path"] = base["path"].clone();
        assert!(parse_document(&encoded(2, vec![base.clone(), sibling])).is_err());
        let mut unsafe_entry = base;
        unsafe_entry["path"] = "plugins/../other".into();
        assert!(parse_document(&encoded(2, vec![unsafe_entry])).is_err());
    }

    #[tokio::test]
    async fn cached_catalog_filters_platform_without_a_network_request() {
        let catalog = SourceCatalog::default();
        let plugin: RepositoryCatalogPlugin = serde_json::from_value(serde_json::json!({
            "name":"demo", "title":"Demo", "summary":"Summary", "description":"Description",
            "author":"Team", "category":"Development", "repository":"https://github.com/example/demo",
            "docsUrl":null, "logoUrl":null, "screenshots":[], "latestVersion":"1.0.0",
            "platforms":["linux-amd64-gnu"], "commit":"a".repeat(40),
            "readmeUrl":"https://raw.githubusercontent.com/example/demo/README.md",
            "validation":{"metadata":"passed","build":"not_run"}
        })).unwrap();
        *catalog.cache.lock().await = Some(Cached {
            expires: Instant::now() + TTL,
            result: Ok(vec![plugin]),
        });
        assert_eq!(catalog.list("linux-amd64-gnu").await.unwrap().len(), 1);
        assert!(catalog.list("darwin-arm64").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_fetch_is_cached_briefly() {
        let (catalog, server) = mock_catalog(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(matches!(
            catalog.list("linux-amd64-gnu").await,
            Err(SourceCatalogError::Fetch { .. })
        ));
        server.await.unwrap();
        assert!(matches!(
            catalog.list("linux-amd64-gnu").await,
            Err(SourceCatalogError::Fetch { .. })
        ));
    }

    #[tokio::test]
    async fn redirects_oversized_and_invalid_responses_fail_closed() {
        let responses: [&'static [u8]; 3] = [
            b"HTTP/1.1 302 Found\r\nLocation: https://example.invalid/catalog.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 524289\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        ];
        for response in responses {
            let (catalog, server) = mock_catalog(response).await;
            assert!(catalog.list("linux-amd64-gnu").await.is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_misses_share_one_fetch() {
        let (catalog, server) = mock_catalog(b"HTTP/1.1 200 OK\r\nContent-Length: 33\r\nConnection: close\r\n\r\n{\"schema_version\":1,\"plugins\":[]}").await;
        let catalog = std::sync::Arc::new(catalog);
        let (first, second) = tokio::join!(
            catalog.list("linux-amd64-gnu"),
            catalog.list("darwin-arm64")
        );
        assert!(first.is_ok());
        assert!(second.is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stalled_response_times_out_and_is_negatively_cached() {
        use tokio::io::AsyncReadExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            tokio::time::sleep(Duration::from_secs(9)).await;
        });
        let catalog = SourceCatalog {
            cache: Mutex::new(None),
            test_source: Some(format!("http://{address}/catalog.json")),
        };
        assert!(matches!(
            catalog.list("linux-amd64-gnu").await,
            Err(SourceCatalogError::Fetch { .. })
        ));
        assert!(matches!(
            catalog.list("linux-amd64-gnu").await,
            Err(SourceCatalogError::Fetch { .. })
        ));
        server.await.unwrap();
    }
}
