// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a plugin name and a plugin version may look like.
//!
//! A name becomes a URL segment (`/x/{name}/...`), a database key and a
//! directory name, so it is restricted to what is safe in all three.

/// Whether `name` is a plugin name: lowercase letters, digits and inner
/// hyphens, at most 64 characters.
pub fn valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Whether `version` is a version a plugin may declare: at most 64 letters,
/// digits, dots, hyphens and plus signs, and never a path.
pub fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version != "."
        && version != ".."
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_name_is_lowercase_letters_digits_and_inner_hyphens() {
        for name in ["reports", "audit-trail", "a", "s3-sync-2"] {
            assert!(valid_plugin_name(name), "{name}");
        }
        let too_long = "a".repeat(65);
        for name in [
            "",
            "-reports",
            "reports-",
            "Reports",
            "re ports",
            "re/ports",
            "../reports",
            "re.ports",
            "réports",
            too_long.as_str(),
        ] {
            assert!(!valid_plugin_name(name), "{name}");
        }
        assert!(valid_plugin_name(&"a".repeat(64)));
    }

    #[test]
    fn a_version_is_never_a_path() {
        for version in ["1.0.0", "0.1.0-beta.64", "2024.11+build7", "v3"] {
            assert!(valid_version(version), "{version}");
        }
        let too_long = "1".repeat(65);
        for version in [
            "",
            ".",
            "..",
            "1/2",
            "1\\2",
            "1 0",
            "../etc",
            too_long.as_str(),
        ] {
            assert!(!valid_version(version), "{version}");
        }
    }
}
