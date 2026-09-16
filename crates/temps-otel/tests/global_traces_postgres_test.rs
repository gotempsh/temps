// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{Duration, Utc};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
use std::sync::Arc;
use temps_otel::{
    storage::{
        global_traces::{GlobalTraceQuery, TraceReadScope},
        timescaledb::TimescaleDbStorage,
        OtelStorage,
    },
    types::{TraceQuery, TraceSortField},
};

#[tokio::test]
async fn global_postgres_pagination_and_raw_spans_use_the_same_authorized_scope() {
    use testcontainers::{
        core::{ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };
    let image = GenericImage::new("postgres", "17-alpine")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "test");
    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Postgres unavailable: {e}");
            return;
        }
    };
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db = Arc::new(
        Database::connect(format!(
            "postgres://postgres:test@127.0.0.1:{port}/postgres"
        ))
        .await
        .unwrap(),
    );
    db.execute_unprepared("CREATE TABLE otel_spans (project_id integer, trace_id text, span_id text, parent_span_id text, name text, service_name text, deployment_environment text, kind text, status_code text, start_time timestamptz, duration_ms double precision, attributes jsonb, events jsonb, status_message text, deployment_id integer)").await.unwrap();
    db.execute_unprepared("CREATE INDEX ON otel_spans (trace_id, start_time DESC)")
        .await
        .unwrap();
    db.execute_unprepared("CREATE TABLE otel_trace_summaries (project_id integer NOT NULL, trace_id text NOT NULL, root_span_name text NOT NULL, service_name text NOT NULL, kind text NOT NULL, deployment_environment text, deployment_id integer, start_time timestamptz NOT NULL, duration_ms double precision NOT NULL, span_count bigint NOT NULL, error_count bigint NOT NULL, PRIMARY KEY (project_id, trace_id))").await.unwrap();
    db.execute_unprepared("CREATE INDEX ON otel_trace_summaries (project_id, start_time DESC)")
        .await
        .unwrap();
    // A fractional millisecond must not round past the inclusive upper bound.
    let now = chrono::DateTime::<Utc>::from_timestamp(1_700_000_000, 999_600_000).unwrap();
    db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "INSERT INTO otel_spans SELECT CASE WHEN i%2=0 THEN 1 ELSE 2 END, 'trace-'||lpad(i::text,3,'0'), 'root', NULL, 'GET /items', 'api', 'production', 'SERVER', 'OK', $1::timestamptz-i*interval '1 second', i+1, '{}'::jsonb, '[]'::jsonb, '', NULL FROM generate_series(0,44) i",[now.into()])).await.unwrap();
    db.execute_unprepared(
        "INSERT INTO otel_trace_summaries SELECT project_id, trace_id, 'summary:' || name, service_name, kind, deployment_environment, deployment_id, start_time, duration_ms, 1, 0 FROM otel_spans",
    )
    .await
    .unwrap();
    // This row is only 100 microseconds beyond the inclusive upper bound. A
    // millisecond-truncated predicate would incorrectly include it.
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO otel_trace_summaries VALUES (1, 'just-after-window', 'summary:GET /late', 'api', 'SERVER', 'production', NULL, $1::timestamptz + interval '100 microseconds', 1, 1, 0)",
        [now.into()],
    ))
    .await
    .unwrap();
    let storage = TimescaleDbStorage::new(db, None);
    let q = GlobalTraceQuery {
        filter: TraceQuery {
            limit: Some(20),
            offset: Some(20),
            ..Default::default()
        },
        scopes: [1, 2]
            .into_iter()
            .map(|project_id| TraceReadScope {
                project_id,
                from: now - Duration::hours(1),
                to: now,
                cloud: false,
                window_clamped_at: None,
            })
            .collect(),
        summaries: true,
        use_preaggregated_summaries: true,
        source_offset: 0,
    };
    let page = storage.global_trace_page(q.clone()).await.unwrap();
    assert_eq!(page.total, 45);
    assert_eq!(page.data.len(), 20);
    assert_eq!(page.data[0].trace_id, "trace-020");
    assert_eq!(page.data[0].name, "summary:GET /items");
    let mut scoped = q.clone();
    scoped.scopes.retain(|s| s.project_id == 2);
    scoped.filter.offset = Some(0);
    let page = storage.global_trace_page(scoped).await.unwrap();
    assert_eq!(page.total, 22);
    assert!(page.data.iter().all(|r| r.project_id == 2));
    let mut sorted = q.clone();
    sorted.filter.sort_by = TraceSortField::Duration;
    let page = storage.global_trace_page(sorted).await.unwrap();
    assert_eq!(page.data[0].trace_id, "trace-024");
    let mut raw = q;
    raw.summaries = false;
    let page = storage.global_trace_page(raw).await.unwrap();
    assert_eq!(page.total, 45);
    assert_eq!(page.data[0].clone().span().unwrap().span_id, "root");
}
