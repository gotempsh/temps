// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Escaping for text placed into HTML and into SQL `LIKE` patterns.

/// Escape text for an HTML text node or a double- or single-quoted attribute.
///
/// Replaces `&`, `<`, `>`, `"` and `'` with `&amp;`, `&lt;`, `&gt;`, `&quot;`
/// and `&#39;`; every other character passes through unchanged.
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape the `LIKE`/`ILIKE` metacharacters `\`, `%` and `_` so `raw` is
/// matched literally; callers add their own wildcards around the result
/// (`%{escaped}%` for a substring match).
///
/// Each metacharacter is prefixed with `\`, the default escape character in
/// both PostgreSQL and ClickHouse, so no `ESCAPE` clause is needed. The
/// backslash is escaped first, so the backslashes added for `%` and `_` are
/// not escaped again.
pub fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::{escape_html, escape_like};

    #[test]
    fn escape_html_replaces_the_five_special_characters() {
        assert_eq!(escape_html("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&#39;");
        assert_eq!(
            escape_html("<script>alert('x')</script>"),
            "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"
        );
    }

    #[test]
    fn escape_html_does_not_double_escape_and_keeps_other_text() {
        assert_eq!(escape_html("&amp;"), "&amp;amp;");
        assert_eq!(escape_html("Café / ünïcode 100%"), "Café / ünïcode 100%");
        assert_eq!(escape_html(""), "");
    }

    #[test]
    fn escape_like_escapes_each_metacharacter() {
        assert_eq!(escape_like("a%b"), "a\\%b");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
        assert_eq!(escape_like(r"50%_off\"), r"50\%\_off\\");
    }

    #[test]
    fn escape_like_escapes_the_backslash_first() {
        // A naive order would turn the `\` added for `%` into `\\`.
        assert_eq!(escape_like("%_\\"), "\\%\\_\\\\");
        assert_eq!(escape_like("\\%"), "\\\\\\%");
    }

    #[test]
    fn escape_like_leaves_plain_text_unchanged() {
        assert_eq!(escape_like("plain"), "plain");
        assert_eq!(escape_like("/api/users"), "/api/users");
        assert_eq!(escape_like(""), "");
    }
}
