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
    branch
        .to_lowercase()
        .replace('/', "-")
        .replace('_', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(63) // DNS label max length
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
