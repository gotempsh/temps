// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! URL construction shared by Rust CLI commands that call the management API.

/// Build a management API URL from an instance URL.
///
/// Operators configure `TEMPS_API_URL` as the address of their Temps instance,
/// while the HTTP server mounts management routes below `/api`. Accept both the
/// documented instance form (`https://temps.example.com`) and the historically
/// used explicit API form (`https://temps.example.com/api`) without producing a
/// missing or duplicated `/api` segment.
pub(crate) fn management_api_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let base = base.strip_suffix("/api").unwrap_or(base);
    format!("{base}/api/{}", path.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::management_api_url;

    #[test]
    fn builds_management_url_from_instance_root() {
        assert_eq!(
            management_api_url("http://127.0.0.1", "/domains"),
            "http://127.0.0.1/api/domains"
        );
    }

    #[test]
    fn accepts_existing_api_suffix_and_trailing_slashes() {
        assert_eq!(
            management_api_url("https://temps.example.com/api/", "domains/7"),
            "https://temps.example.com/api/domains/7"
        );
    }

    #[test]
    fn preserves_reverse_proxy_path_prefix() {
        assert_eq!(
            management_api_url("https://example.com/temps", "/domains"),
            "https://example.com/temps/api/domains"
        );
    }

    #[test]
    fn api_text_inside_hostname_is_not_treated_as_path_suffix() {
        assert_eq!(
            management_api_url("https://my-api.example.com", "/domains"),
            "https://my-api.example.com/api/domains"
        );
    }
}
