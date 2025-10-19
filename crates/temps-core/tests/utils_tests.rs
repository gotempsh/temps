use temps_core::utils::{generate_id, generate_slug, mask_sensitive};

#[test]
fn test_generate_id() {
    let id1 = generate_id();
    let id2 = generate_id();

    // UUIDs should be different
    assert_ne!(id1, id2);

    // Should be valid UUIDs (version 4)
    assert_eq!(id1.get_version_num(), 4);
    assert_eq!(id2.get_version_num(), 4);
}

#[test]
fn test_generate_slug() {
    // Basic slug generation
    assert_eq!(generate_slug("Hello World"), "hello-world");

    // Handle special characters - the actual behavior replaces each non-alphanumeric with '-'
    assert_eq!(generate_slug("Hello, World!"), "hello-world");

    // Handle multiple spaces - each space becomes a dash, then consecutive dashes are replaced
    assert_eq!(generate_slug("Hello   World"), "hello--world");

    // Handle leading/trailing special chars
    assert_eq!(generate_slug("!@#Hello World$%^"), "hello-world");

    // Handle numbers and hyphens (should preserve)
    assert_eq!(generate_slug("test-123"), "test-123");

    // Empty string
    assert_eq!(generate_slug(""), "");

    // Only special characters
    assert_eq!(generate_slug("!@#$%^&*()"), "");

    // Already lowercase
    assert_eq!(generate_slug("hello-world"), "hello-world");
}

#[test]
fn test_mask_sensitive() {
    // Short strings (8 chars or less) should be completely masked
    assert_eq!(mask_sensitive("short"), "***");
    assert_eq!(mask_sensitive("12345678"), "***");

    // Longer strings should show first 4 and last 4 chars
    assert_eq!(mask_sensitive("1234567890"), "1234***7890");
    assert_eq!(mask_sensitive("secretpassword123"), "secr***d123");

    // 9 characters (minimum for partial masking)
    assert_eq!(mask_sensitive("123456789"), "1234***6789");

    // Empty string
    assert_eq!(mask_sensitive(""), "***");

    // Very long string
    let long_string = "this_is_a_very_long_secret_key_that_should_be_masked_properly";
    assert_eq!(mask_sensitive(long_string), "this***erly");
}