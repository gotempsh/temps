// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// sentry 0.41.0, revision 673e8dd6ec744880283055e0450744e997767c7a.
use sentry::{protocol::SpanStatus, ClientOptions, TransactionContext};
use std::{borrow::Cow, time::Duration};
fn main() {
    let prefix = std::env::args().nth(1).expect("synthetic run key required");
    let _guard = sentry::init(ClientOptions {
        dsn: Some(
            std::env::var("SENTRY_DSN")
                .expect("DSN required")
                .parse()
                .expect("valid DSN"),
        ),
        release: Some(Cow::Borrowed("synthetic-tracing-release")),
        environment: Some(Cow::Borrowed("synthetic-test")),
        server_name: Some(Cow::Borrowed("synthetic.example")),
        default_integrations: false,
        traces_sample_rate: 1.0,
        ..Default::default()
    });
    let root = sentry::start_transaction(TransactionContext::new(
        "Synthetic cross SDK root",
        "http.server",
    ));
    let trace = root.get_trace_context();
    root.set_data(
        "service.name",
        serde_json::json!("synthetic-tracing-service"),
    );
    root.set_data("sentry.kind", serde_json::json!(2));
    root.set_tag("synthetic.run", &prefix);
    root.set_status(SpanStatus::Ok);
    let child = root.start_child("db.query", "Synthetic cross SDK child");
    let child_id = child.get_span_id();
    child.set_data("synthetic.flag", serde_json::json!(true));
    child.set_data("sentry.kind", serde_json::json!(3));
    child.set_tag("synthetic.run", &prefix);
    child.set_status(SpanStatus::InternalError);
    std::thread::sleep(Duration::from_millis(2));
    child.finish();
    root.finish();
    assert!(sentry::Hub::current()
        .client()
        .unwrap()
        .flush(Some(Duration::from_secs(10))));
    println!(
        "{}",
        serde_json::json!({"sdk":"0.41.0","trace_id":trace.trace_id.to_string(),"root_id":trace.span_id.to_string(),"child_id":child_id.to_string()})
    );
}
