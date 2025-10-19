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
        format!("{}***{}", &data[..4], &data[data.len()-4..])
    }
}