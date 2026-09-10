// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared helpers for reading repository content via the Git provider API.
//!
//! Used by both [`super::repo_tools::RepoToolsProvider`] (the sentinel that
//! exposes repo exploration tools in every project chat) and, if needed, any
//! other provider that reads repo files. Extracted from `deployment.rs` so the
//! logic lives in one place and the byte cap / path-validation rules stay in
//! sync across providers.

use base64::Engine;

/// Max bytes of a repo file fed back to the model in one `read_repo_file` call.
/// Sized to keep a large source file from blowing the model's context window.
pub(crate) const MAX_REPO_FILE_BYTES: usize = 16_000;

/// Reject a model-supplied repo path that is absolute, escapes the repo root,
/// or uses Windows-style separators. The Git provider only ever reads files
/// inside the repo; without this a path like `../../etc/passwd` could traverse
/// outside it. Returns a human-readable reason on rejection so the model gets
/// text it can recover from, never a panic.
pub(crate) fn validate_repo_path(path: &str) -> Result<(), String> {
    if path.contains('\\') {
        return Err(
            "Invalid path: use forward slashes ('/') for a repo-relative path, not backslashes."
                .to_string(),
        );
    }
    // An absolute path (`/...`) or a Windows drive prefix (`C:`) must be rejected.
    if path.starts_with('/') || path.contains(':') {
        return Err("Invalid path: provide a repo-relative path, not an absolute one.".to_string());
    }
    for segment in path.split('/') {
        if segment == ".." || segment == "." {
            return Err(
                "Invalid path: '.' and '..' segments are not allowed; provide a path inside the \
                 repository."
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Decode a provider `FileContent`. GitHub returns base64 (with embedded
/// newlines); GitLab / raw providers return utf-8. Falls back to the raw
/// string if base64 decoding fails so the model still sees *something*.
pub(crate) fn decode_file_content(content: &str, encoding: &str) -> String {
    if encoding.eq_ignore_ascii_case("base64") {
        let stripped: String = content.split_whitespace().collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(stripped) {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    content.to_string()
}

/// Bound a file body so a large file can't blow the model's context. Never
/// slices on a multibyte char boundary: the cut index is retreated to the
/// previous valid `char` boundary so a UTF-8 char straddling it isn't split.
pub(crate) fn bound(content: &str, path: &str) -> String {
    if content.len() <= MAX_REPO_FILE_BYTES {
        return content.to_string();
    }
    let mut end = MAX_REPO_FILE_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let head = &content[..end];
    format!(
        "{head}\n\n[truncated — '{path}' is {} bytes; showing the first {}]",
        content.len(),
        MAX_REPO_FILE_BYTES
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_repo_path_rejects_traversal() {
        assert!(validate_repo_path("../../etc/passwd").is_err());
        assert!(validate_repo_path("src/../../../secret").is_err());
        assert!(validate_repo_path("./hidden").is_err());
        assert!(validate_repo_path("src/./x").is_err());
        assert!(validate_repo_path("..").is_err());
        assert!(validate_repo_path("src\\windows").is_err());
        assert!(validate_repo_path("/etc/passwd").is_err());
        assert!(validate_repo_path("C:/Windows").is_err());
    }

    #[test]
    fn validate_repo_path_accepts_normal() {
        assert!(validate_repo_path("tsconfig.json").is_ok());
        assert!(validate_repo_path("src/app/page.tsx").is_ok());
        assert!(validate_repo_path("a/b/c/d.txt").is_ok());
        // A dot inside a filename (not a whole segment) is fine.
        assert!(validate_repo_path("src/next.config.js").is_ok());
        assert!(validate_repo_path(".gitignore").is_ok());
    }

    #[test]
    fn decode_file_content_base64() {
        let raw = base64::engine::general_purpose::STANDARD.encode("hello\nworld");
        let with_newlines = format!("{}\n{}", &raw[..4], &raw[4..]);
        assert_eq!(
            decode_file_content(&with_newlines, "base64"),
            "hello\nworld"
        );
    }

    #[test]
    fn decode_file_content_utf8_passthrough() {
        assert_eq!(decode_file_content("{ \"a\": 1 }", "utf-8"), "{ \"a\": 1 }");
    }

    #[test]
    fn decode_file_content_bad_base64_falls_back() {
        assert_eq!(
            decode_file_content("!!!not base64!!!", "base64"),
            "!!!not base64!!!"
        );
    }

    #[test]
    fn bound_truncates() {
        let big = "x".repeat(MAX_REPO_FILE_BYTES + 100);
        let out = bound(&big, "big.txt");
        assert!(out.len() < big.len() + 100);
        assert!(out.contains("truncated"));
        let small = "small";
        assert_eq!(bound(small, "s.txt"), "small");
    }

    #[test]
    fn bound_multibyte_boundary_does_not_panic() {
        // Place a 4-byte emoji so the fixed cut lands on its 3rd byte.
        let big = "a".repeat(MAX_REPO_FILE_BYTES - 2) + &"😀".repeat(10);
        assert!(!big.is_char_boundary(MAX_REPO_FILE_BYTES));
        let out = bound(&big, "emoji.txt");
        assert!(out.contains("truncated"));
        let accented = "café—".repeat(MAX_REPO_FILE_BYTES);
        let out2 = bound(&accented, "accent.txt");
        assert!(out2.contains("truncated"));
    }
}
