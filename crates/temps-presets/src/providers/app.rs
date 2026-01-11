//! App helper for framework detection
//!
//! Provides utilities for analyzing project structure and files
//! Works with Git provider tree API results

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Represents a project's file structure from Git tree API
#[derive(Debug, Clone)]
pub struct App {
    /// Root path of the project
    pub source: PathBuf,
    /// Map of file paths to their contents
    /// Key: relative path from source (e.g., "package.json", "src/index.ts")
    /// Value: file contents as string
    pub files: HashMap<String, String>,
    /// List of all paths (files and directories) in the project
    pub paths: Vec<String>,
}

impl App {
    /// Create an App from Git tree API results
    ///
    /// # Arguments
    /// * `source` - Root path of the project
    /// * `tree_files` - Map of file paths to contents from Git provider API
    pub fn from_tree(source: PathBuf, tree_files: HashMap<String, String>) -> Self {
        let paths: Vec<String> = tree_files.keys().cloned().collect();

        Self {
            source,
            files: tree_files,
            paths,
        }
    }

    /// Create an App from local filesystem
    pub fn new(source: PathBuf) -> Self {
        let mut files = HashMap::new();
        let mut paths = Vec::new();

        // Try to walk directory, but don't fail if we can't
        if source.exists() && source.is_dir() {
            let _ = Self::walk_dir(&source, &source, &mut files, &mut paths);
        }

        Self {
            source,
            files,
            paths,
        }
    }

    /// Create an App from local filesystem with error handling (for testing)
    #[cfg(test)]
    pub fn new_with_errors(path: &str) -> Result<Self> {
        let current_dir = std::env::current_dir()?;
        let source = current_dir
            .join(path)
            .canonicalize()
            .context("Failed to read app source directory")?;

        let mut files = HashMap::new();
        let mut paths = Vec::new();

        // Walk the directory tree
        Self::walk_dir(&source, &source, &mut files, &mut paths)?;

        Ok(Self {
            source,
            files,
            paths,
        })
    }

    /// Walk directory tree and collect files
    fn walk_dir(dir: &Path, base: &Path, files: &mut HashMap<String, String>, paths: &mut Vec<String>) -> Result<()> {
        use std::fs;

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Ok(relative) = path.strip_prefix(base) {
                let relative_str = relative.to_string_lossy().to_string();
                paths.push(relative_str.clone());

                if path.is_file() {
                    if let Ok(contents) = fs::read_to_string(&path) {
                        files.insert(relative_str, contents.replace("\r\n", "\n"));
                    }
                } else if path.is_dir() {
                    Self::walk_dir(&path, base, files, paths)?;
                }
            }
        }
        Ok(())
    }

    /// Check if a file exists
    pub fn includes_file(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    /// Check if a directory exists
    pub fn includes_directory(&self, name: &str) -> bool {
        let dir_prefix = format!("{}/", name);
        self.paths.iter().any(|p| p.starts_with(&dir_prefix) || p == name)
    }

    /// Find files matching a glob-like pattern
    /// Supports simple patterns like "*.json", "src/**/*.ts", "**/*.config.js"
    pub fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

        let matches: Vec<String> = self.files
            .keys()
            .filter(|path| re.is_match(path))
            .cloned()
            .collect();

        Ok(matches)
    }

    /// Find directories matching a glob-like pattern
    pub fn find_directories(&self, pattern: &str) -> Result<Vec<String>> {
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

        // Get all unique directory paths
        let mut dirs: Vec<String> = self.paths
            .iter()
            .filter_map(|p| {
                let path = Path::new(p);
                path.parent()
                    .and_then(|parent| parent.to_str())
                    .map(|s| s.to_string())
            })
            .collect();

        dirs.sort();
        dirs.dedup();

        let matches: Vec<String> = dirs
            .into_iter()
            .filter(|dir| re.is_match(dir))
            .collect();

        Ok(matches)
    }

    /// Check if any paths match a pattern
    pub fn has_match(&self, pattern: &str) -> bool {
        self.find_files(pattern).is_ok_and(|v| !v.is_empty())
    }

    /// Read the contents of a file
    pub fn read_file(&self, name: &str) -> Result<String> {
        self.files
            .get(name)
            .cloned()
            .with_context(|| format!("File not found: {}", name))
    }

    /// Try to json-parse a file
    pub fn read_json<T>(&self, name: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let contents = self.read_file(name)?;
        let value: T = serde_json::from_str(&contents)
            .with_context(|| format!("Error reading {} as JSON", name))?;
        Ok(value)
    }

    /// Parse jsonc files as json by ignoring all kinds of comments
    pub fn read_jsonc<T>(&self, name: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let mut cleaned_jsonc = String::new();
        let contents = self.read_file(name)?;
        let mut chars = contents.chars().peekable();

        while let Some(current_char) = chars.next() {
            match current_char {
                '/' if chars.peek() == Some(&'/') => {
                    // Skip line comments
                    while let Some(&next_char) = chars.peek() {
                        chars.next();
                        if next_char == '\n' {
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'*') => {
                    // Skip block comments
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('*') if chars.peek() == Some(&'/') => {
                                chars.next();
                                break;
                            }
                            None => break,
                            _ => {}
                        }
                    }
                }
                _ => cleaned_jsonc.push(current_char),
            }
        }

        let value: T = serde_json::from_str(&cleaned_jsonc)
            .with_context(|| format!("Error reading {} as JSONC", name))?;
        Ok(value)
    }

    /// Try to toml-parse a file
    pub fn read_toml<T>(&self, name: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let contents = self.read_file(name)?;
        let toml_file = toml::from_str(&contents)
            .with_context(|| format!("Error reading {} as TOML", name))?;
        Ok(toml_file)
    }

    /// Try to yaml-parse a file
    pub fn read_yaml<T>(&self, name: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let contents = self.read_file(name)?;
        let yaml_file = serde_yaml::from_str(&contents)
            .with_context(|| format!("Error reading {} as YAML", name))?;
        Ok(yaml_file)
    }

    /// Check whether filenames matching a pattern contain a regex match
    pub fn find_match(&self, re: &regex::Regex, pattern: &str) -> Result<bool> {
        let paths = self.find_files(pattern)?;

        for path in paths {
            if let Some(contents) = self.files.get(&path) {
                if re.is_match(contents) {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Get all files in the project
    pub fn all_files(&self) -> Vec<&str> {
        self.files.keys().map(|s| s.as_str()).collect()
    }

    /// Get all paths (files and directories)
    pub fn all_paths(&self) -> &[String] {
        &self.paths
    }

    /// Helper: Check if package.json has a dependency
    pub fn has_dependency(&self, dep: &str) -> bool {
        if let Ok(pkg) = self.read_json::<serde_json::Value>("package.json") {
            pkg.get("dependencies")
                .and_then(|d| d.get(dep))
                .is_some()
        } else {
            false
        }
    }

    /// Helper: Check if package.json has a dev dependency
    pub fn has_dev_dependency(&self, dep: &str) -> bool {
        if let Ok(pkg) = self.read_json::<serde_json::Value>("package.json") {
            pkg.get("devDependencies")
                .and_then(|d| d.get(dep))
                .is_some()
        } else {
            false
        }
    }

    /// Helper: Check if package.json has any dependency (regular or dev)
    pub fn has_any_dependency(&self, dep: &str) -> bool {
        self.has_dependency(dep) || self.has_dev_dependency(dep)
    }

    /// Helper: Check if package.json has a script
    pub fn has_script(&self, script: &str) -> bool {
        if let Ok(pkg) = self.read_json::<serde_json::Value>("package.json") {
            pkg.get("scripts")
                .and_then(|s| s.get(script))
                .is_some()
        } else {
            false
        }
    }

    /// Helper: Get dependency version from package.json
    pub fn get_dependency_version(&self, dep: &str) -> Option<String> {
        if let Ok(pkg) = self.read_json::<serde_json::Value>("package.json") {
            pkg.get("dependencies")
                .and_then(|d| d.get(dep))
                .or_else(|| pkg.get("devDependencies").and_then(|d| d.get(dep)))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
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

    #[test]
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
        assert_eq!(glob_to_regex("test?.js"), r"^test[^/]\.js$");
    }

    #[test]
    fn test_from_tree() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), r#"{"name":"test"}"#.to_string());
        files.insert("src/index.ts".to_string(), "console.log('hello')".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        assert_eq!(app.paths.len(), 2);
        assert!(app.includes_file("package.json"));
        assert!(app.includes_file("src/index.ts"));
    }

    #[test]
    fn test_includes_file() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("tsconfig.json".to_string(), "{}".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        assert!(app.includes_file("package.json"));
        assert!(app.includes_file("tsconfig.json"));
        assert!(!app.includes_file("missing.json"));
    }

    #[test]
    fn test_includes_directory() {
        let mut files = HashMap::new();
        files.insert("src/index.ts".to_string(), "".to_string());
        files.insert("src/utils/helper.ts".to_string(), "".to_string());
        files.insert("tests/test.ts".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        assert!(app.includes_directory("src"));
        assert!(app.includes_directory("src/utils"));
        assert!(app.includes_directory("tests"));
        assert!(!app.includes_directory("dist"));
    }

    #[test]
    fn test_find_files() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("tsconfig.json".to_string(), "{}".to_string());
        files.insert("src/index.ts".to_string(), "".to_string());
        files.insert("src/utils/helper.ts".to_string(), "".to_string());
        files.insert("tests/test.js".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        let json_files = app.find_files("*.json").unwrap();
        assert_eq!(json_files.len(), 2);
        assert!(json_files.contains(&"package.json".to_string()));
        assert!(json_files.contains(&"tsconfig.json".to_string()));

        let ts_files = app.find_files("**/*.ts").unwrap();
        assert_eq!(ts_files.len(), 2);

        let src_ts_files = app.find_files("src/**/*.ts").unwrap();
        assert_eq!(src_ts_files.len(), 2);
    }

    #[test]
    fn test_read_file() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"name":"test","version":"1.0.0"}"#.to_string()
        );

        let app = App::from_tree(PathBuf::from("/test"), files);

        let content = app.read_file("package.json").unwrap();
        assert!(content.contains("test"));

        let result = app.read_file("missing.json");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_json() {
        use serde_json::Value;

        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"name":"test","version":"1.0.0"}"#.to_string()
        );

        let app = App::from_tree(PathBuf::from("/test"), files);

        let json: Value = app.read_json("package.json").unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["version"], "1.0.0");
    }

    #[test]
    fn test_read_jsonc() {
        use serde_json::Value;

        let mut files = HashMap::new();
        files.insert(
            "tsconfig.json".to_string(),
            r#"{
                // This is a comment
                "compilerOptions": {
                    /* Block comment */
                    "target": "es2015"
                }
            }"#.to_string()
        );

        let app = App::from_tree(PathBuf::from("/test"), files);

        let json: Value = app.read_jsonc("tsconfig.json").unwrap();
        assert_eq!(json["compilerOptions"]["target"], "es2015");
    }

    #[test]
    fn test_has_match() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("yarn.lock".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        assert!(app.has_match("*.json"));
        assert!(app.has_match("*.lock"));
        assert!(!app.has_match("*.ts"));
    }

    #[test]
    fn test_find_match() {
        let mut files = HashMap::new();
        files.insert(
            "src/App.tsx".to_string(),
            r#"import React from 'react';
            function App() { return <div className="app">Hello</div>; }"#.to_string()
        );
        files.insert(
            "src/index.ts".to_string(),
            "console.log('hello')".to_string()
        );

        let app = App::from_tree(PathBuf::from("/test"), files);

        let re = regex::Regex::new(r"className").unwrap();
        assert!(app.find_match(&re, "**/*.tsx").unwrap());

        let re2 = regex::Regex::new(r"nonexistent").unwrap();
        assert!(!app.find_match(&re2, "**/*.tsx").unwrap());
    }
}
