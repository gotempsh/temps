//! GitLab API implementation of ProjectSource
//!
//! This implementation fetches files directly from GitLab API on-demand

use anyhow::Result;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::sync::Arc;
use temps_presets::source::ProjectSource;
use tracing::{debug, warn};

#[derive(Debug, Deserialize)]
struct GitLabTreeEntry {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    name: String,
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
    #[allow(dead_code)]
    mode: String,
}

#[derive(Debug, Deserialize)]
struct GitLabFileContent {
    #[allow(dead_code)]
    file_name: String,
    #[allow(dead_code)]
    file_path: String,
    #[allow(dead_code)]
    size: i64,
    encoding: String,
    content: String,
    #[allow(dead_code)]
    #[serde(rename = "ref")]
    reference: String,
}

/// GitLab-backed ProjectSource
///
/// Fetches files directly from GitLab API without cloning the repository
pub struct GitLabSource {
    client: Arc<reqwest::Client>,
    base_url: String,
    project_id: String, // Can be numeric ID or "namespace/project" format
    reference: String,  // branch, tag, or commit SHA
    access_token: String,
}

impl GitLabSource {
    /// Create a new GitLab source
    ///
    /// # Arguments
    /// * `client` - Reqwest client
    /// * `base_url` - GitLab instance URL (e.g., "https://gitlab.com")
    /// * `project_id` - Project ID or "namespace/project" (URL-encoded)
    /// * `reference` - Branch name, tag, or commit SHA
    /// * `access_token` - GitLab access token (PAT or OAuth)
    pub fn new(
        client: Arc<reqwest::Client>,
        base_url: String,
        project_id: String,
        reference: String,
        access_token: String,
    ) -> Self {
        Self {
            client,
            base_url,
            project_id,
            reference,
            access_token,
        }
    }

    fn get_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "PRIVATE-TOKEN",
            reqwest::header::HeaderValue::from_str(&self.access_token).unwrap(),
        );
        headers
    }

    fn encode_path(&self, path: &str) -> String {
        urlencoding::encode(path).to_string()
    }
}

#[async_trait]
impl ProjectSource for GitLabSource {
    async fn has_file(&self, path: &str) -> bool {
        debug!(
            "Checking if file exists: {}/{} @ {}",
            self.project_id, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/files/{}?ref={}",
            self.base_url,
            urlencoding::encode(&self.project_id),
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
                warn!("Failed to check file existence: {}", e);
                false
            }
        }
    }

    async fn has_directory(&self, path: &str) -> bool {
        debug!(
            "Checking if directory exists: {}/{} @ {}",
            self.project_id, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/tree?path={}&ref={}",
            self.base_url,
            urlencoding::encode(&self.project_id),
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
                    match response.json::<Vec<GitLabTreeEntry>>().await {
                        Ok(entries) => !entries.is_empty(),
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
            Err(e) => {
                warn!("Failed to check directory existence: {}", e);
                false
            }
        }
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        debug!(
            "Reading file: {}/{} @ {}",
            self.project_id, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/files/{}?ref={}",
            self.base_url,
            urlencoding::encode(&self.project_id),
            encoded_path,
            urlencoding::encode(&self.reference)
        );

        let response = self
            .client
            .get(&url)
            .headers(self.get_headers())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch file from GitLab: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "GitLab API error: {} - File: {}",
                response.status(),
                path
            ));
        }

        let file: GitLabFileContent = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitLab response: {}", e))?;

        // GitLab returns base64-encoded content
        if file.encoding == "base64" {
            let decoded = STANDARD
                .decode(file.content.replace('\n', ""))
                .map_err(|e| anyhow::anyhow!("Failed to decode base64 content: {}", e))?;

            let content_str = String::from_utf8(decoded)
                .map_err(|e| anyhow::anyhow!("File content is not valid UTF-8: {}", e))?;

            Ok(content_str.replace("\r\n", "\n"))
        } else {
            // Text encoding (rare)
            Ok(file.content.replace("\r\n", "\n"))
        }
    }

    async fn list_files(&self) -> Result<Vec<String>> {
        debug!(
            "Listing all files: {} @ {}",
            self.project_id, self.reference
        );

        // Get the tree recursively
        let url = format!(
            "{}/api/v4/projects/{}/repository/tree?recursive=true&ref={}&per_page=100",
            self.base_url,
            urlencoding::encode(&self.project_id),
            urlencoding::encode(&self.reference)
        );

        let mut all_files = Vec::new();
        let mut page = 1;

        loop {
            let paginated_url = format!("{}&page={}", url, page);

            let response = self
                .client
                .get(&paginated_url)
                .headers(self.get_headers())
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to fetch tree from GitLab: {}", e))?;

            if !response.status().is_success() {
                return Err(anyhow::anyhow!("GitLab API error: {}", response.status()));
            }

            let entries: Vec<GitLabTreeEntry> = response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse GitLab tree response: {}", e))?;

            if entries.is_empty() {
                break;
            }

            // Filter only files (blobs)
            for entry in entries {
                if entry.entry_type == "blob" {
                    all_files.push(entry.path);
                }
            }

            page += 1;

            // Safety: limit to 100 pages (10,000 files max)
            if page > 100 {
                warn!("Reached pagination limit (100 pages)");
                break;
            }
        }

        debug!("Found {} files", all_files.len());
        Ok(all_files)
    }

    async fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        debug!(
            "Finding files matching pattern '{}': {} @ {}",
            pattern, self.project_id, self.reference
        );

        let all_files = self.list_files().await?;

        // Convert glob pattern to regex
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

        let matched_files: Vec<String> = all_files.into_iter().filter(|f| re.is_match(f)).collect();

        debug!(
            "Found {} files matching pattern '{}'",
            matched_files.len(),
            pattern
        );
        Ok(matched_files)
    }
}

/// Convert glob pattern to regex
fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next(); // consume second *
                if chars.peek() == Some(&'/') {
                    chars.next(); // consume /
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
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
        assert_eq!(glob_to_regex("test?.js"), r"^test[^/]\.js$");
    }
}
