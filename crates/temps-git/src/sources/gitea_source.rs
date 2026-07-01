//! Gitea API implementation of ProjectSource
//!
//! Fetches files directly from the Gitea REST API on-demand,
//! enabling framework detection without cloning the repository.

use anyhow::Result;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::sync::Arc;
use temps_presets::source::ProjectSource;
use tracing::{debug, warn};

#[derive(Deserialize)]
struct GiteaTreeEntry {
    #[allow(dead_code)]
    sha: Option<String>,
    #[allow(dead_code)]
    name: String,
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
}

#[derive(Deserialize)]
struct GiteaFileContent {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    path: String,
    content: Option<String>,
    encoding: Option<String>,
}

/// Gitea-backed `ProjectSource`.
///
/// Fetches files directly from `GET /api/v1/repos/{owner}/{repo}/contents/{path}?ref={ref}`
/// without cloning the repository. Auth: `Authorization: token {pat}`.
pub struct GiteaSource {
    client: Arc<reqwest::Client>,
    base_url: String,
    owner: String,
    repo: String,
    reference: String,
    access_token: String,
}

impl GiteaSource {
    /// Create a new Gitea source.
    ///
    /// # Arguments
    /// * `client` — shared reqwest client
    /// * `base_url` — Gitea instance web root, e.g. `https://git.example.com`
    /// * `owner` — repository owner login
    /// * `repo` — repository name
    /// * `reference` — branch, tag, or commit SHA
    /// * `access_token` — Gitea PAT
    pub fn new(
        client: Arc<reqwest::Client>,
        base_url: String,
        owner: String,
        repo: String,
        reference: String,
        access_token: String,
    ) -> Self {
        Self {
            client,
            base_url,
            owner,
            repo,
            reference,
            access_token,
        }
    }

    fn api_base(&self) -> String {
        format!("{}/api/v1", self.base_url.trim_end_matches('/'))
    }

    fn get_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("token {}", self.access_token))
                .expect("PAT token must be a valid header value"),
        );
        headers
    }

    fn encode_path(&self, path: &str) -> String {
        urlencoding::encode(path).to_string()
    }
}

#[async_trait]
impl ProjectSource for GiteaSource {
    async fn has_file(&self, path: &str) -> bool {
        debug!(
            "Checking if file exists in Gitea {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.api_base(),
            self.owner,
            self.repo,
            encoded_path,
            urlencoding::encode(&self.reference)
        );

        match self
            .client
            .head(&url)
            .headers(self.get_headers())
            .send()
            .await
        {
            Ok(response) => response.status().is_success(),
            Err(e) => {
                warn!("Failed to check Gitea file existence for {}: {}", path, e);
                false
            }
        }
    }

    async fn has_directory(&self, path: &str) -> bool {
        debug!(
            "Checking if directory exists in Gitea {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        // Gitea returns a list when the contents endpoint targets a directory.
        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.api_base(),
            self.owner,
            self.repo,
            encoded_path,
            urlencoding::encode(&self.reference)
        );

        match self
            .client
            .get(&url)
            .headers(self.get_headers())
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    // If the body is a JSON array, it's a directory listing.
                    match response.json::<Vec<GiteaTreeEntry>>().await {
                        Ok(entries) => !entries.is_empty(),
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
            Err(e) => {
                warn!(
                    "Failed to check Gitea directory existence for {}: {}",
                    path, e
                );
                false
            }
        }
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        debug!(
            "Reading file from Gitea {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.api_base(),
            self.owner,
            self.repo,
            encoded_path,
            urlencoding::encode(&self.reference)
        );

        let response = self
            .client
            .get(&url)
            .headers(self.get_headers())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch file from Gitea: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Gitea API error: {} - File: {}",
                response.status(),
                path
            ));
        }

        let file: GiteaFileContent = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Gitea response: {}", e))?;

        let content = file.content.unwrap_or_default();
        let encoding = file.encoding.unwrap_or_default();

        if encoding == "base64" {
            // Gitea (like GitHub) may wrap base64 content in newlines.
            let decoded = STANDARD
                .decode(content.replace('\n', ""))
                .map_err(|e| anyhow::anyhow!("Failed to decode base64 content: {}", e))?;

            let content_str = String::from_utf8(decoded)
                .map_err(|e| anyhow::anyhow!("File content is not valid UTF-8: {}", e))?;

            Ok(content_str.replace("\r\n", "\n"))
        } else {
            Ok(content.replace("\r\n", "\n"))
        }
    }

    async fn list_files(&self) -> Result<Vec<String>> {
        debug!(
            "Listing files in Gitea {}/{} @ {}",
            self.owner, self.repo, self.reference
        );

        // Gitea does not have a recursive tree endpoint by default; we use the
        // Git trees API to walk the tree.
        let url = format!(
            "{}/repos/{}/{}/git/trees/{}?recursive=true",
            self.api_base(),
            self.owner,
            self.repo,
            urlencoding::encode(&self.reference)
        );

        let response = self
            .client
            .get(&url)
            .headers(self.get_headers())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch tree from Gitea: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Gitea API error listing tree: {}",
                response.status()
            ));
        }

        #[derive(Deserialize)]
        struct GiteaTreeResponse {
            tree: Vec<GiteaTreeEntry>,
        }

        let tree_resp: GiteaTreeResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Gitea tree response: {}", e))?;

        let files: Vec<String> = tree_resp
            .tree
            .into_iter()
            .filter(|e| e.entry_type == "blob")
            .map(|e| e.path)
            .collect();

        debug!("Found {} files in Gitea repository", files.len());
        Ok(files)
    }

    async fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        debug!(
            "Finding files matching pattern '{}' in Gitea {}/{} @ {}",
            pattern, self.owner, self.repo, self.reference
        );

        let all_files = self.list_files().await?;

        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

        let matched: Vec<String> = all_files.into_iter().filter(|f| re.is_match(f)).collect();

        debug!(
            "Found {} files matching pattern '{}'",
            matched.len(),
            pattern
        );
        Ok(matched)
    }
}

/// Convert a glob pattern to a regex string (shared with the GitLab source).
fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    regex.push_str("(?:.*/)?");
                } else {
                    regex.push_str(".*");
                }
            }
            '*' => {
                regex.push_str("[^/]*");
            }
            '?' => {
                regex.push_str("[^/]");
            }
            '.' | '(' | ')' | '+' | '|' | '^' | '$' | '@' | '%' => {
                regex.push('\\');
                regex.push(ch);
            }
            _ => {
                regex.push(ch);
            }
        }
    }

    regex.push('$');
    regex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_to_regex_star() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
    }

    #[test]
    fn test_glob_to_regex_double_star() {
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
    }

    #[test]
    fn test_glob_to_regex_nested() {
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
    }

    #[test]
    fn test_api_base_strips_trailing_slash() {
        let source = GiteaSource::new(
            Arc::new(reqwest::Client::new()),
            "https://git.example.com/".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            "main".to_string(),
            "token".to_string(),
        );
        assert_eq!(source.api_base(), "https://git.example.com/api/v1");
    }
}
