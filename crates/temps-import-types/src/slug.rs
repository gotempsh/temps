// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Slugs derived from source-platform names (projects, services, domains).

/// Turn a source-platform name into a slug for step IDs, project slugs and
/// subdomains.
///
/// Lowercases the name, replaces every character that is neither alphanumeric
/// nor `-` with `-`, and trims leading and trailing `-`. Runs of separators
/// are kept as they are (`"a  b"` becomes `"a--b"`), and non-ASCII letters
/// and digits survive (`char::is_alphanumeric` is Unicode-aware).
pub fn sanitize_slug(name: &str) -> String {
    name.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_slug;

    #[test]
    fn lowercases_and_keeps_alphanumerics_and_hyphens() {
        assert_eq!(sanitize_slug("My-App"), "my-app");
        assert_eq!(sanitize_slug("api2"), "api2");
    }

    #[test]
    fn replaces_every_other_character_with_a_hyphen() {
        assert_eq!(sanitize_slug("my app"), "my-app");
        assert_eq!(sanitize_slug("web_frontend"), "web-frontend");
        assert_eq!(sanitize_slug("app.example.com"), "app-example-com");
        assert_eq!(
            sanitize_slug("stack/service:latest"),
            "stack-service-latest"
        );
    }

    #[test]
    fn keeps_runs_of_separators() {
        assert_eq!(sanitize_slug("a  b"), "a--b");
        assert_eq!(sanitize_slug("a_-_b"), "a---b");
    }

    #[test]
    fn trims_leading_and_trailing_separators() {
        assert_eq!(sanitize_slug("  my app  "), "my-app");
        assert_eq!(sanitize_slug("--svc--"), "svc");
        assert_eq!(sanitize_slug("*.example.com"), "example-com");
    }

    #[test]
    fn keeps_non_ascii_alphanumerics() {
        assert_eq!(sanitize_slug("Café Ünïcode"), "café-ünïcode");
    }

    #[test]
    fn empty_and_all_separator_names_become_empty() {
        assert_eq!(sanitize_slug(""), "");
        assert_eq!(sanitize_slug("___"), "");
        assert_eq!(sanitize_slug("..."), "");
    }
}
