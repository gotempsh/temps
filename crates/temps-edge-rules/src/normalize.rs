// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Path normalization for the `path` field (ADR-052 §2, path matching).
//!
//! Decodes percent-escapes of unreserved characters, upper-cases the hex of
//! every other escape, collapses repeated slashes and resolves `.`/`..`
//! segments. A backslash stays a literal character. The upstream always
//! receives the original path; this form exists only for matching.

use std::borrow::Cow;

/// The normalized path and whether the raw path was already in that form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPath<'a> {
    pub path: Cow<'a, str>,
    pub canonical: bool,
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Fast check: a path with no `%`, no `//` and no dot segment is already
/// canonical, which is the common case and needs no allocation.
fn obviously_canonical(raw: &str) -> bool {
    if !raw.starts_with('/') || raw.contains('%') || raw.contains("//") {
        return false;
    }
    !raw.split('/')
        .skip(1)
        .any(|segment| segment == "." || segment == "..")
}

pub fn normalize_path(raw: &str) -> NormalizedPath<'_> {
    if obviously_canonical(raw) {
        return NormalizedPath {
            path: Cow::Borrowed(raw),
            canonical: true,
        };
    }

    // 1. Leading slash.
    let mut prefixed = String::with_capacity(raw.len() + 1);
    if !raw.starts_with('/') {
        prefixed.push('/');
    }
    prefixed.push_str(raw);

    // 2. Percent-escapes: decode unreserved, upper-case the rest.
    let bytes = prefixed.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                let value = hi * 16 + lo;
                if is_unreserved(value) {
                    decoded.push(value);
                } else {
                    decoded.push(b'%');
                    decoded.push(bytes[i + 1].to_ascii_uppercase());
                    decoded.push(bytes[i + 2].to_ascii_uppercase());
                }
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    // Only ASCII bytes were substituted, so UTF-8 validity is preserved.
    let decoded = String::from_utf8_lossy(&decoded).into_owned();

    // 3. Collapse repeated slashes.
    let mut collapsed = String::with_capacity(decoded.len());
    let mut previous_slash = false;
    for ch in decoded.chars() {
        if ch == '/' {
            if previous_slash {
                continue;
            }
            previous_slash = true;
        } else {
            previous_slash = false;
        }
        collapsed.push(ch);
    }

    // 4. Dot segments.
    let segments: Vec<&str> = collapsed.split('/').skip(1).collect();
    let mut out: Vec<&str> = Vec::with_capacity(segments.len());
    let count = segments.len();
    for (index, segment) in segments.into_iter().enumerate() {
        let last = index + 1 == count;
        match segment {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            other => out.push(other),
        }
    }
    let mut path = String::with_capacity(collapsed.len());
    path.push('/');
    path.push_str(&out.join("/"));
    let canonical = path == raw;
    NormalizedPath {
        path: Cow::Owned(path),
        canonical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(raw: &str) -> (String, bool) {
        let n = normalize_path(raw);
        (n.path.into_owned(), n.canonical)
    }

    #[test]
    fn plain_paths_are_canonical_and_borrowed() {
        let n = normalize_path("/admin/users");
        assert!(matches!(n.path, Cow::Borrowed(_)));
        assert!(n.canonical);
        assert_eq!(norm("/"), ("/".into(), true));
        assert_eq!(
            norm("/a.b/c..d/.well-known"),
            ("/a.b/c..d/.well-known".into(), true)
        );
    }

    #[test]
    fn decodes_unreserved_escapes() {
        assert_eq!(norm("/%61dmin"), ("/admin".into(), false));
        assert_eq!(norm("/%7Euser"), ("/~user".into(), false));
    }

    #[test]
    fn upper_cases_other_escapes() {
        assert_eq!(norm("/a%2fb"), ("/a%2Fb".into(), false));
        assert_eq!(norm("/a%2Fb"), ("/a%2Fb".into(), true));
        assert_eq!(norm("/%e2%82%ac"), ("/%E2%82%AC".into(), false));
    }

    #[test]
    fn collapses_slashes_and_resolves_dots() {
        assert_eq!(norm("//admin"), ("/admin".into(), false));
        assert_eq!(norm("/x/../admin"), ("/admin".into(), false));
        assert_eq!(norm("/x/./admin"), ("/x/admin".into(), false));
        assert_eq!(norm("/a/b/.."), ("/a/".into(), false));
        assert_eq!(norm("/a/b/."), ("/a/b/".into(), false));
        assert_eq!(norm("/../../etc/passwd"), ("/etc/passwd".into(), false));
        assert_eq!(norm("/%2e%2e/admin"), ("/admin".into(), false));
    }

    #[test]
    fn backslash_is_literal() {
        assert_eq!(norm("/a\\..\\b"), ("/a\\..\\b".into(), true));
    }

    #[test]
    fn adds_a_leading_slash() {
        assert_eq!(norm("admin"), ("/admin".into(), false));
        assert_eq!(norm(""), ("/".into(), false));
    }

    #[test]
    fn malformed_escapes_are_left_alone() {
        assert_eq!(norm("/a%zzb"), ("/a%zzb".into(), true));
        assert_eq!(norm("/a%2"), ("/a%2".into(), true));
        assert_eq!(norm("/a%"), ("/a%".into(), true));
    }

    #[test]
    fn non_ascii_is_preserved() {
        assert_eq!(norm("/caf\u{e9}//x"), ("/caf\u{e9}/x".into(), false));
    }
}
