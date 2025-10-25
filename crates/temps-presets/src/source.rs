//! Source abstraction for accessing project files
//!
//! This module provides traits for accessing project files from different sources:
//! - Local filesystem
//! - GitHub API (via git provider tree API)
//! - Any other remote source
//!
//! Presets use these traits to detect frameworks and read configuration files
//! without needing to know where the files come from.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;

/// Trait for accessing project file tree and contents
///
/// Implementations can fetch from:
/// - Local filesystem
/// - GitHub/GitLab API
/// - S3/Object storage
/// - Any other source
#[async_trait]
pub trait ProjectSource: Send + Sync {
    /// Check if a file exists in the project
    ///
    /// # Arguments
    /// * `path` - Relative path from project root (e.g., "package.json", "src/index.ts")
    ///
    /// # Returns
    /// `true` if the file exists, `false` otherwise
    async fn has_file(&self, path: &str) -> bool;

    /// Check if a directory exists in the project
    ///
    /// # Arguments
    /// * `path` - Relative path from project root (e.g., "src", "node_modules")
    ///
    /// # Returns
    /// `true` if the directory exists, `false` otherwise
    async fn has_directory(&self, path: &str) -> bool;

    /// Read file contents
    ///
    /// # Arguments
    /// * `path` - Relative path from project root
    ///
    /// # Returns
    /// File contents as a string, or error if file doesn't exist or can't be read
    async fn read_file(&self, path: &str) -> Result<String>;

    /// Get list of all files in the project
    ///
    /// # Returns
    /// Vector of relative file paths from project root
    async fn list_files(&self) -> Result<Vec<String>>;

    /// Find files matching a pattern (supports glob-like patterns)
    ///
    /// # Arguments
    /// * `pattern` - Pattern like "*.json", "**/*.ts", "src/**/*.tsx"
    ///
    /// # Returns
    /// Vector of matching file paths
    async fn find_files(&self, pattern: &str) -> Result<Vec<String>>;
}

/// In-memory implementation of ProjectSource
///
/// Used when you already have the file tree (e.g., from GitHub API)
pub struct InMemorySource {
    files: HashMap<String, String>,
    paths: Vec<String>,
}

impl InMemorySource {
    /// Create from a map of file paths to contents
    ///
    /// # Example
    /// ```
    /// use std::collections::HashMap;
    /// use temps_presets::source::InMemorySource;
    ///
    /// let mut files = HashMap::new();
    /// files.insert("package.json".to_string(), r#"{"name":"test"}"#.to_string());
    /// files.insert("src/index.ts".to_string(), "console.log('hello')".to_string());
    ///
    /// let source = InMemorySource::from_files(files);
    /// ```
    pub fn from_files(files: HashMap<String, String>) -> Self {
        let paths = files.keys().cloned().collect();
        Self { files, paths }
    }

    /// Create from GitHub tree API response
    ///
    /// # Arguments
    /// * `tree` - File tree from GitHub API (path -> content mapping)
    pub fn from_git_tree(tree: HashMap<String, String>) -> Self {
        Self::from_files(tree)
    }
}

#[async_trait]
impl ProjectSource for InMemorySource {
    async fn has_file(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    async fn has_directory(&self, path: &str) -> bool {
        let dir_prefix = format!("{}/", path);
        self.paths
            .iter()
            .any(|p| p.starts_with(&dir_prefix) || p == path)
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path))
    }

    async fn list_files(&self) -> Result<Vec<String>> {
        Ok(self.paths.clone())
    }

    async fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

        Ok(self
            .paths
            .iter()
            .filter(|p| re.is_match(p))
            .cloned()
            .collect())
    }
}

/// Filesystem implementation of ProjectSource
///
/// Reads files directly from the local filesystem
pub struct FilesystemSource {
    root_path: std::path::PathBuf,
}

impl FilesystemSource {
    /// Create a new filesystem source
    ///
    /// # Arguments
    /// * `root_path` - Absolute path to project root directory
    pub fn new(root_path: std::path::PathBuf) -> Self {
        Self { root_path }
    }
}

#[async_trait]
impl ProjectSource for FilesystemSource {
    async fn has_file(&self, path: &str) -> bool {
        let full_path = self.root_path.join(path);
        full_path.exists() && full_path.is_file()
    }

    async fn has_directory(&self, path: &str) -> bool {
        let full_path = self.root_path.join(path);
        full_path.exists() && full_path.is_dir()
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        let full_path = self.root_path.join(path);
        let contents = tokio::fs::read_to_string(&full_path).await?;
        Ok(contents.replace("\r\n", "\n"))
    }

    async fn list_files(&self) -> Result<Vec<String>> {
        let mut files = Vec::new();
        Self::walk_dir(&self.root_path, &self.root_path, &mut files).await?;
        Ok(files)
    }

    async fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        let files = self.list_files().await?;
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

        Ok(files.into_iter().filter(|p| re.is_match(p)).collect())
    }
}

impl FilesystemSource {
    fn walk_dir<'a>(
        dir: &'a std::path::Path,
        base: &'a std::path::Path,
        files: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = tokio::fs::read_dir(dir).await?;

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();

                if let Ok(relative) = path.strip_prefix(base) {
                    let relative_str = relative.to_string_lossy().to_string();

                    if path.is_file() {
                        files.push(relative_str);
                    } else if path.is_dir() {
                        Self::walk_dir(&path, base, files).await?;
                    }
                }
            }

            Ok(())
        })
    }
}

/// Convert glob pattern to regex
fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                // ** matches any number of directories
                chars.next(); // consume second *
                if chars.peek() == Some(&'/') {
                    chars.next(); // consume /
                    regex.push_str("(?:.*/)?");
                } else {
                    regex.push_str(".*");
                }
            }
            '*' => {
                // * matches anything except /
                regex.push_str("[^/]*");
            }
            '?' => {
                regex.push_str("[^/]");
            }
            '.' | '(' | ')' | '+' | '|' | '^' | '$' | '@' | '%' => {
                // Escape special regex characters
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

    #[tokio::test]
    async fn test_in_memory_source() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), r#"{"name":"test"}"#.to_string());
        files.insert("src/index.ts".to_string(), "console.log('hello')".to_string());

        let source = InMemorySource::from_files(files);

        assert!(source.has_file("package.json").await);
        assert!(source.has_file("src/index.ts").await);
        assert!(!source.has_file("nonexistent.txt").await);

        assert!(source.has_directory("src").await);
        assert!(!source.has_directory("dist").await);

        let content = source.read_file("package.json").await.unwrap();
        assert!(content.contains("test"));

        let files = source.list_files().await.unwrap();
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn test_in_memory_find_files() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("src/index.ts".to_string(), "".to_string());
        files.insert("src/utils/helper.ts".to_string(), "".to_string());
        files.insert("README.md".to_string(), "".to_string());

        let source = InMemorySource::from_files(files);

        let ts_files = source.find_files("**/*.ts").await.unwrap();
        assert_eq!(ts_files.len(), 2);

        let json_files = source.find_files("*.json").await.unwrap();
        assert_eq!(json_files.len(), 1);

        let src_files = source.find_files("src/**/*").await.unwrap();
        assert_eq!(src_files.len(), 2);
    }

    #[tokio::test]
    async fn test_from_git_tree() {
        let mut tree = HashMap::new();
        tree.insert("package.json".to_string(), r#"{"name":"app"}"#.to_string());
        tree.insert("next.config.js".to_string(), "module.exports = {}".to_string());

        let source = InMemorySource::from_git_tree(tree);

        assert!(source.has_file("package.json").await);
        assert!(source.has_file("next.config.js").await);

        let content = source.read_file("package.json").await.unwrap();
        assert!(content.contains("app"));
    }

    #[test]
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
        assert_eq!(glob_to_regex("test?.js"), r"^test[^/]\.js$");
    }
}
