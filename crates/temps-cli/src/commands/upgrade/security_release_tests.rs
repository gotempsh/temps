// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn release(tag: &str, body: Option<&str>) -> GitHubRelease {
    serde_json::from_value(serde_json::json!({
        "tag_name": tag, "prerelease": false, "draft": false,
        "assets": [{"name": "temps-linux-amd64.tar.gz", "browser_download_url": "https://example.invalid/asset", "size": 1}],
        "html_url": "https://example.invalid/notes", "body": body,
    })).unwrap()
}

#[test]
fn release_marker_is_explicit_and_backwards_compatible() {
    assert!(!release("v1.2.0", None).is_security_release());
    assert!(!release("v1.2.0", Some("Security improvements")).is_security_release());
    let release = release(
        "v1.2.0",
        Some("Release notes\n<!-- temps-security-release -->\n"),
    );
    assert!(release.is_security_release());
    assert!(release.is_security_update_for("v1.1.0"));
    assert!(!release.is_security_update_for("v1.2.0"));
    assert!(!release.is_security_update_for("v1.3.0"));
}

#[test]
fn newer_release_does_not_inherit_a_fix_from_a_backport() {
    let selected = pick_installable_release_for_channel(
        vec![
            release("v1.1.5", Some("<!-- temps-security-release -->")),
            release("v1.2.0", None),
        ],
        UpgradeChannel::Stable,
        "linux-amd64",
    )
    .unwrap();
    assert_eq!(selected.tag_name, "v1.2.0");
    assert!(!selected.is_security_update_for("v1.1.4"));
}

#[test]
fn selected_release_can_explicitly_declare_affected_ranges() {
    let release = release(
        "v1.3.0",
        Some("<!-- temps-security-affects: v1.1.0..v1.1.5, v1.2.0..v1.2.2 -->"),
    );
    assert!(release.is_security_update_for("v1.1.0"));
    assert!(release.is_security_update_for("v1.1.4"));
    assert!(!release.is_security_update_for("v1.1.5"));
    assert!(release.is_security_update_for("v1.2.1"));
    assert!(!release.is_security_update_for("v1.2.2"));
    assert!(!release.is_security_update_for("v1.0.0"));
}

#[test]
fn malformed_or_future_fixed_bounds_cannot_claim_an_available_fix() {
    let release = release("v1.2.0", Some("<!-- temps-security-release -->\n<!-- temps-security-affects: invalid..v1.2.0, v1.1.0..v1.3.0 -->"));
    assert!(!release.is_security_update_for("v1.1.1"));
}
