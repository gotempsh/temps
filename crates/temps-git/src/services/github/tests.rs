// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use serde_json::json;

fn complete_reply() -> serde_json::Value {
    json!({
        "id": 123456,
        "name": "temps-example",
        "client_id": "Iv1.example",
        "client_secret": "client-secret",
        "webhook_secret": "webhook-secret",
        "pem": "test-private-key-value"
    })
}

fn rejected_fields(reply: &serde_json::Value) -> String {
    match validate_manifest_reply(reply) {
        Err(GithubAppServiceError::Validation(message)) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn complete_reply_is_accepted() {
    assert!(validate_manifest_reply(&complete_reply()).is_ok());
}

#[test]
fn github_error_body_is_rejected() {
    let message = rejected_fields(&json!({
        "message": "Not Found",
        "documentation_url": "https://docs.github.com/rest"
    }));
    for field in [
        "id",
        "name",
        "client_id",
        "client_secret",
        "webhook_secret",
        "pem",
    ] {
        assert!(message.contains(field), "{field} missing from: {message}");
    }
}

#[test]
fn each_required_field_is_checked() {
    for field in [
        "id",
        "name",
        "client_id",
        "client_secret",
        "webhook_secret",
        "pem",
    ] {
        let mut reply = complete_reply();
        reply.as_object_mut().unwrap().remove(field);
        assert!(
            rejected_fields(&reply).contains(field),
            "removing {field} must be rejected"
        );
    }
}

#[test]
fn empty_and_blank_values_are_rejected() {
    for (field, value) in [
        ("name", json!("")),
        ("name", json!("  ")),
        ("client_id", json!("")),
        ("client_secret", json!("   ")),
        ("webhook_secret", json!("")),
        ("pem", json!("\n")),
        ("id", json!(0)),
        ("id", json!(-4)),
        ("id", json!(i64::from(i32::MAX) + 1)),
        ("id", json!("123")),
        ("webhook_secret", json!(null)),
    ] {
        let mut reply = complete_reply();
        reply[field] = value.clone();
        assert!(
            rejected_fields(&reply).contains(field),
            "{field}={value} must be rejected"
        );
    }
}

#[test]
fn the_error_names_fields_but_never_their_values() {
    let mut reply = complete_reply();
    reply["webhook_secret"] = json!("");
    let message = rejected_fields(&reply);
    assert!(message.contains("webhook_secret"));
    assert!(!message.contains("client-secret"));
    assert!(!message.contains("test-private-key-value"));
}

#[test]
fn shared_secret_warning_is_rate_limited_and_recovers_at_the_boundary() {
    let warning = SharedSecretWarning::new();
    assert!(warning.should_warn(2, 0));
    assert!(!warning.should_warn(2, 0));
    assert!(!warning.should_warn(3, 299));
    assert!(warning.should_warn(3, 300));
    assert!(!warning.should_warn(2, 599));
    assert!(warning.should_warn(2, 600));
}

#[test]
fn distinct_secrets_do_not_consume_the_warning_interval() {
    let warning = SharedSecretWarning::new();
    assert!(!warning.should_warn(0, 0));
    assert!(!warning.should_warn(1, 0));
    assert!(warning.should_warn(2, 0));
    assert!(!warning.should_warn(1, 300));
    assert!(warning.should_warn(2, 300));
}

#[test]
fn concurrent_shared_secret_deliveries_emit_only_one_warning() {
    let warning = Arc::new(SharedSecretWarning::new());
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let warning = warning.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                warning.should_warn(2, 0)
            })
        })
        .collect();
    let warnings = threads
        .into_iter()
        .filter_map(|thread| thread.join().expect("warning task completes").then_some(()))
        .count();
    assert_eq!(warnings, 1);
}
