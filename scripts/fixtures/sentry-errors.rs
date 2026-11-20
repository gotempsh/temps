// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// sentry 0.41.0, source 673e8dd6ec744880283055e0450744e997767c7a.
use sentry::{
    protocol::{Breadcrumb, Context, User},
    ClientOptions, Level,
};
use std::{borrow::Cow, sync::Arc, time::Duration};
fn main() {
    let prefix = std::env::args().nth(1).expect("synthetic run key required");
    let _guard = sentry::init(ClientOptions {
        dsn: Some(
            std::env::var("SENTRY_DSN")
                .expect("DSN required")
                .parse()
                .expect("valid DSN"),
        ),
        release: Some(Cow::Borrowed("synthetic-release-2")),
        environment: Some(Cow::Borrowed("synthetic-test")),
        server_name: Some(Cow::Borrowed("synthetic.example")),
        default_integrations: false,
        attach_stacktrace: true,
        integrations: vec![Arc::new(
            sentry::integrations::backtrace::AttachStacktraceIntegration,
        )],
        before_send: Some(Arc::new(|mut event| {
            for exception in &mut event.exception.values {
                if let Some(stack) = &mut exception.stacktrace {
                    for frame in &mut stack.frames {
                        frame.filename = Some("synthetic/application.rs".into());
                        frame.abs_path = Some("/synthetic/application.rs".into());
                        frame.vars.clear();
                    }
                }
            }
            for thread in &mut event.threads.values {
                if let Some(stack) = &mut thread.stacktrace {
                    for frame in &mut stack.frames {
                        frame.filename = Some("synthetic/application.rs".into());
                        frame.abs_path = Some("/synthetic/application.rs".into());
                        frame.vars.clear();
                    }
                }
            }
            Some(event)
        })),
        ..Default::default()
    });
    sentry::configure_scope(|scope| {
        scope.set_user(Some(User {
            id: Some("synthetic-user".into()),
            email: Some("synthetic@example.invalid".into()),
            ..Default::default()
        }));
        scope.set_tag("synthetic.run", &prefix);
        scope.set_tag("synthetic.key", "test'value");
        scope.set_context(
            "synthetic",
            Context::Other(
                serde_json::from_value(serde_json::json!({"nullable":null,"array":[1,true,null]}))
                    .unwrap(),
            ),
        );
    });
    sentry::add_breadcrumb(Breadcrumb {
        category: Some("synthetic".into()),
        message: Some("Synthetic interaction".into()),
        ..Default::default()
    });
    let mut ids = Vec::new();
    for (index, fingerprint) in ["shared", "shared", "split"].iter().enumerate() {
        sentry::configure_scope(|scope| scope.set_fingerprint(Some(&[&prefix, fingerprint])));
        let error = std::io::Error::other(if index == 1 {
            "Synthetic different failure"
        } else {
            "Synthetic outer failure"
        });
        ids.push(sentry::capture_error(&error).simple().to_string());
    }
    sentry::configure_scope(|scope| {
        scope.set_fingerprint(Some(&["{{ default }}", &prefix, "message"]))
    });
    ids.push(
        sentry::capture_message("Synthetic SDK message", Level::Info)
            .simple()
            .to_string(),
    );
    sentry::capture_event(sentry::protocol::Event {
        event_id: ids[0].parse().unwrap(),
        message: Some("Synthetic retry identity".into()),
        ..Default::default()
    });
    assert!(sentry::Hub::current()
        .client()
        .unwrap()
        .flush(Some(Duration::from_secs(10))));
    println!(
        "{}",
        serde_json::json!({"sdk":"0.41.0","ids":ids,"retry":true})
    );
}
