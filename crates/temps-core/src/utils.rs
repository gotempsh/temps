// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Common utility functions

use uuid::Uuid;

/// Generate a new UUID v4
pub fn generate_id() -> Uuid {
    Uuid::new_v4()
}

/// Generate a slug from a string
pub fn generate_slug(input: &str) -> String {
    input
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .replace("--", "-")
        .trim_matches('-')
        .to_string()
}

/// Mask sensitive data for logging
pub fn mask_sensitive(data: &str) -> String {
    if data.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}***{}", &data[..4], &data[data.len() - 4..])
    }
}

/// Slugify a git branch name to create a valid environment name.
///
/// This function:
/// - Converts to lowercase
/// - Replaces '/' and '_' with '-'
/// - Removes non-alphanumeric characters (except '-')
/// - Trims leading/trailing '-'
/// - Limits to 63 characters (DNS label max length)
///
/// # Examples
///
/// ```
/// use temps_core::slugify_branch_name;
///
/// assert_eq!(slugify_branch_name("feature/new-auth"), "feature-new-auth");
/// assert_eq!(slugify_branch_name("bugfix/fix-123"), "bugfix-fix-123");
/// assert_eq!(slugify_branch_name("FEAT/Add_User"), "feat-add-user");
/// assert_eq!(slugify_branch_name("fix/issue#123"), "fix-issue-123");
/// ```
pub fn slugify_branch_name(branch: &str) -> String {
    let slugified: String = branch
        .to_lowercase()
        .replace(['/', '_'], "-")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Collapse consecutive dashes into a single dash
    let mut result = String::with_capacity(slugified.len());
    let mut prev_dash = false;
    for c in slugified.chars() {
        if c == '-' {
            if !prev_dash {
                result.push('-');
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }

    result
        .trim_matches('-')
        .chars()
        .take(63) // DNS label max length
        .collect()
}

/// Validate that a string is a safe PostgreSQL interval literal (e.g. "1 hour", "5 minutes").
/// Prevents SQL injection when interval values must be interpolated into raw SQL.
/// Returns true only for strings matching the pattern `<positive integer> <valid unit>`.
pub fn is_valid_sql_interval(interval: &str) -> bool {
    const VALID_UNITS: &[&str] = &[
        "microsecond",
        "microseconds",
        "millisecond",
        "milliseconds",
        "second",
        "seconds",
        "minute",
        "minutes",
        "hour",
        "hours",
        "day",
        "days",
        "week",
        "weeks",
        "month",
        "months",
        "year",
        "years",
    ];

    let parts: Vec<&str> = interval.split_whitespace().collect();
    if parts.len() != 2 {
        return false;
    }

    parts[0].parse::<u32>().is_ok() && VALID_UNITS.contains(&parts[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_sql_interval() {
        assert!(is_valid_sql_interval("1 hour"));
        assert!(is_valid_sql_interval("5 minutes"));
        assert!(is_valid_sql_interval("30 seconds"));
        assert!(is_valid_sql_interval("7 days"));
        assert!(is_valid_sql_interval("1 month"));
        assert!(!is_valid_sql_interval("1 hour; DROP TABLE events;--"));
        assert!(!is_valid_sql_interval("abc hours"));
        assert!(!is_valid_sql_interval("1"));
        assert!(!is_valid_sql_interval("hour"));
        assert!(!is_valid_sql_interval(""));
        assert!(!is_valid_sql_interval("1 hour 2"));
        assert!(!is_valid_sql_interval("-1 hour"));
    }

    #[test]
    fn test_slugify_branch_name() {
        assert_eq!(slugify_branch_name("feature/new-auth"), "feature-new-auth");
        assert_eq!(slugify_branch_name("bugfix/fix-123"), "bugfix-fix-123");
        assert_eq!(slugify_branch_name("FEAT/Add_User"), "feat-add-user");
        assert_eq!(slugify_branch_name("fix/issue#123"), "fix-issue-123");
        assert_eq!(slugify_branch_name("main"), "main");
        assert_eq!(slugify_branch_name("develop"), "develop");
    }

    #[test]
    fn test_slugify_removes_special_chars() {
        assert_eq!(slugify_branch_name("fix/bug@#$%123"), "fix-bug-123");
        assert_eq!(
            slugify_branch_name("feature/add_new!feature"),
            "feature-add-new-feature"
        );
    }

    #[test]
    fn test_slugify_trims_dashes() {
        assert_eq!(slugify_branch_name("-feature-"), "feature");
        assert_eq!(slugify_branch_name("--fix--"), "fix");
    }

    #[test]
    fn test_slugify_respects_length_limit() {
        let long_branch = "a".repeat(100);
        let result = slugify_branch_name(&long_branch);
        assert_eq!(result.len(), 63);
    }

    #[test]
    fn test_slugify_handles_empty() {
        assert_eq!(slugify_branch_name(""), "");
        assert_eq!(slugify_branch_name("---"), "");
    }
}
