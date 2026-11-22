// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: panicking is how a test fails"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use temps_entities::{dns_query_log, dns_resolver_reports, environments, projects};
use temps_metrics::{
    LabelledMetric, LatestByLabelQuery, LatestQuery, MetricPoint, MetricsError, MetricsStore,
    RangeQuery, SourceKind,
};
use temps_node_dns::observe::{
    DnsCounters, DnsQueryEntry, DnsReport, DnsResolverState, QueryOutcome, ZoneClass, ZoneCounters,
    DNS_REPORT_VERSION,
};

use super::ingest::{counters_delta, stamp};
use super::*;

// ── Pure functions ───────────────────────────────────────────────────────────

fn entry(seq: u64, name: Option<&str>, outcome: QueryOutcome, source: &str) -> DnsQueryEntry {
    DnsQueryEntry {
        seq,
        age_ms: 1_000,
        name: name.map(str::to_string),
        qtype: "A".into(),
        zone: if name.is_some() {
            ZoneClass::Internal
        } else {
            ZoneClass::External
        },
        outcome,
        latency_us: 250,
        answers: u16::from(outcome == QueryOutcome::Noerror),
        source: source.parse().unwrap(),
    }
}

fn report(instance: &str, counters: DnsCounters, queries: Vec<DnsQueryEntry>) -> DnsReport {
    DnsReport {
        version: DNS_REPORT_VERSION,
        instance_id: instance.into(),
        build_version: "0.9.0".into(),
        resolver: DnsResolverState {
            running: true,
            tasks_alive: true,
            listen_addrs: vec!["10.237.0.1:53".into()],
            zone_generation: 42,
            record_count: 7,
            last_sync_age_secs: Some(3),
            consecutive_sync_failures: 0,
            last_error: None,
        },
        counters,
        queries,
        queries_truncated: false,
    }
}

fn counters(noerror: u64, nxdomain: u64, latency: u64) -> DnsCounters {
    DnsCounters {
        internal: ZoneCounters {
            noerror,
            nxdomain,
            latency_us_sum: latency,
            ..Default::default()
        },
        external: ZoneCounters {
            noerror: noerror * 2,
            ..Default::default()
        },
        dropped_log_entries: 0,
    }
}

#[test]
fn a_well_formed_report_is_accepted() {
    let r = report(
        "abc",
        counters(1, 0, 10),
        vec![
            entry(
                1,
                Some("db.temps.local"),
                QueryOutcome::Noerror,
                "10.237.1.2",
            ),
            entry(2, None, QueryOutcome::Servfail, "10.237.1.2"),
        ],
    );
    assert_eq!(validate_report(&r), Ok(()));
}

#[test]
fn reports_breaking_a_bound_are_refused_with_the_reason() {
    let mut r = report("abc", DnsCounters::default(), vec![]);
    r.version = 99;
    assert!(validate_report(&r).unwrap_err().contains("version 99"));

    let r = report("", DnsCounters::default(), vec![]);
    assert!(validate_report(&r).unwrap_err().contains("instance_id"));

    let long = format!("{}.temps.local", "a".repeat(300));
    let r = report(
        "abc",
        DnsCounters::default(),
        vec![entry(1, Some(&long), QueryOutcome::Noerror, "10.0.0.1")],
    );
    assert!(validate_report(&r).unwrap_err().contains("invalid name"));

    let r = report(
        "abc",
        DnsCounters::default(),
        vec![entry(
            1,
            Some("bad name;drop"),
            QueryOutcome::Noerror,
            "10.0.0.1",
        )],
    );
    assert!(validate_report(&r).is_err());

    // A forwarded name must never arrive, even from a misbehaving agent.
    let mut forwarded = entry(
        1,
        Some("tenant.example.com"),
        QueryOutcome::Noerror,
        "10.0.0.1",
    );
    forwarded.zone = ZoneClass::External;
    let r = report("abc", DnsCounters::default(), vec![forwarded]);
    assert!(validate_report(&r).unwrap_err().contains("forwarded name"));

    let many = (1..=600)
        .map(|i| entry(i, Some("a.temps.local"), QueryOutcome::Noerror, "10.0.0.1"))
        .collect();
    let r = report("abc", DnsCounters::default(), many);
    assert!(validate_report(&r)
        .unwrap_err()
        .contains("600 query entries"));

    let mut bad_type = entry(1, Some("a.temps.local"), QueryOutcome::Noerror, "10.0.0.1");
    bad_type.qtype = "A A".into();
    let r = report("abc", DnsCounters::default(), vec![bad_type]);
    assert!(validate_report(&r).unwrap_err().contains("invalid type"));
}

#[test]
fn counter_deltas_refuse_a_counter_going_backwards() {
    let delta = counters_delta(&counters(10, 3, 500), &counters(4, 1, 200)).unwrap();
    assert_eq!(delta.internal.noerror, 6);
    assert_eq!(delta.internal.nxdomain, 2);
    assert_eq!(delta.internal.latency_us_sum, 300);
    assert_eq!(delta.external.noerror, 12);
    assert_eq!(counters_delta(&counters(1, 0, 0), &counters(4, 0, 0)), None);
}

#[test]
fn metric_points_cover_every_name_as_delta_counters() {
    let at = Utc::now();
    let points = metric_points(3, &counters(6, 2, 300), at);
    let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, DNS_METRIC_NAMES.to_vec());
    for point in &points {
        assert!(
            temps_metrics::store::is_delta_counter(&point.name),
            "{} must be summed per bucket",
            point.name
        );
        assert!(temps_metrics::validate_metric_name(&point.name).is_ok());
        assert_eq!(point.source_kind, SourceKind::Node);
        assert_eq!(point.source_id, 3);
        assert_eq!(point.time, at);
    }
    let value = |name: &str| points.iter().find(|p| p.name == name).unwrap().value;
    assert_eq!(value("dns.internal.noerror"), 6.0);
    assert_eq!(value("dns.internal.nxdomain"), 2.0);
    assert_eq!(value("dns.internal.latency_us"), 300.0);
    assert_eq!(value("dns.external.noerror"), 12.0);
}

#[test]
fn entries_are_stamped_inside_the_pull_window() {
    let received = Utc::now();
    let window_start = received - Duration::seconds(30);
    let mut e = entry(1, Some("a.temps.local"), QueryOutcome::Noerror, "10.0.0.1");
    e.age_ms = 5_000;
    assert_eq!(
        stamp(&e, received, window_start),
        received - Duration::seconds(5)
    );
    // A worker claiming an entry a day old cannot date it before the window.
    e.age_ms = 86_400_000;
    assert_eq!(stamp(&e, received, window_start), window_start);
    e.age_ms = u64::MAX;
    assert_eq!(stamp(&e, received, window_start), window_start);
    e.age_ms = 0;
    assert_eq!(stamp(&e, received, window_start), received);
}

#[test]
fn report_states_round_trip() {
    for state in [
        ReportState::Ok,
        ReportState::AgentTooOld,
        ReportState::Unreachable,
        ReportState::Rejected,
        ReportState::Invalid,
    ] {
        assert_eq!(ReportState::parse(state.as_str()), Some(state));
    }
    assert_eq!(ReportState::parse("nope"), None);
}

// ── Against a real database ──────────────────────────────────────────────────

#[derive(Default)]
struct RecordingStore {
    points: Mutex<Vec<MetricPoint>>,
}

#[async_trait]
impl MetricsStore for RecordingStore {
    async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
        self.points.lock().unwrap().extend(points);
        Ok(())
    }
    async fn query_range(
        &self,
        filter: RangeQuery,
    ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError> {
        // Every recorded point of that name and node, in its own bucket.
        Ok(self
            .points
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.name == filter.name && p.source_id == filter.source_id)
            .map(|p| (p.time, p.value))
            .collect())
    }
    async fn query_range_filled(
        &self,
        filter: RangeQuery,
    ) -> Result<Vec<(DateTime<Utc>, Option<f64>)>, MetricsError> {
        Ok(self
            .query_range(filter)
            .await?
            .into_iter()
            .map(|(t, v)| (t, Some(v)))
            .collect())
    }
    async fn query_latest(
        &self,
        _filter: LatestQuery,
    ) -> Result<HashMap<String, f64>, MetricsError> {
        Ok(HashMap::new())
    }
    async fn query_latest_by_label(
        &self,
        _filter: LatestByLabelQuery,
    ) -> Result<Vec<LabelledMetric>, MetricsError> {
        Ok(Vec::new())
    }
    async fn latest_timestamp(
        &self,
        _source_kind: SourceKind,
        _source_id: i32,
    ) -> Result<Option<DateTime<Utc>>, MetricsError> {
        Ok(None)
    }
    async fn prune(&self, _older_than: DateTime<Utc>) -> Result<u64, MetricsError> {
        Ok(0)
    }
}

async fn test_db() -> Option<temps_test_utils::TestDatabase> {
    match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error) => {
            eprintln!("Postgres unavailable; skipping DNS observability test: {error}");
            None
        }
    }
}

async fn seed_environment(
    db: &DatabaseConnection,
    project: &str,
    env: &str,
) -> environments::Model {
    use temps_entities::{preset::Preset, upstream_config::UpstreamList};
    let project = projects::ActiveModel {
        name: Set(format!("Project {project}")),
        slug: Set(project.to_string()),
        repo_owner: Set("owner".into()),
        repo_name: Set("repo".into()),
        main_branch: Set("main".into()),
        preset: Set(Preset::NextJs),
        directory: Set("/".into()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        is_deleted: Set(false),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    environments::ActiveModel {
        project_id: Set(project.id),
        name: Set(env.to_string()),
        slug: Set(env.to_string()),
        host: Set(format!("{env}.{}.example.test", project.slug)),
        subdomain: Set(format!("{env}.{}.example.test", project.slug)),
        upstreams: Set(UpstreamList::default()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn seed_endpoint(db: &DatabaseConnection, fqdn: &str, ip: &str, env_id: i32) {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO service_endpoints \
         (fqdn, record_type, target_ip, ttl, owner_kind, owner_id, node_id, generation) \
         VALUES ($1, 'A', $2, 5, 'environment', $3, NULL, 1)",
        [fqdn.into(), ip.into(), i64::from(env_id).into()],
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn ingest_stores_state_log_and_counter_deltas() {
    let Some(test_db) = test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let env = seed_environment(&db, "shop", "production").await;
    seed_endpoint(&db, "production.shop.temps.local", "10.237.1.10", env.id).await;
    let store = Arc::new(RecordingStore::default());
    let service = DnsObservabilityService::new(db.clone(), store.clone());

    // First pull: state and log are stored; no baseline yet, so no points.
    // Postgres keeps microseconds.
    let t0 = chrono::SubsecRound::trunc_subsecs(Utc::now(), 6);
    let first = report(
        "inst-1",
        counters(5, 1, 1_000),
        vec![
            entry(
                1,
                Some("production.shop.temps.local"),
                QueryOutcome::Noerror,
                "10.237.1.10",
            ),
            entry(
                2,
                Some("missing.shop.temps.local"),
                QueryOutcome::Nxdomain,
                "10.237.9.9",
            ),
            entry(3, None, QueryOutcome::Servfail, "10.237.1.10"),
        ],
    );
    let summary = service
        .ingest(0, ReportOutcome::Report(Box::new(first.clone())), t0)
        .await
        .unwrap();
    assert_eq!(summary.state, ReportState::Ok);
    assert_eq!(summary.queries_logged, 3);
    assert_eq!(summary.points_written, 0, "the first pull is a baseline");

    let row = dns_resolver_reports::Entity::find_by_id(0)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.report_state, "ok");
    assert_eq!(row.instance_id.as_deref(), Some("inst-1"));
    assert_eq!(row.build_version.as_deref(), Some("0.9.0"));
    assert_eq!(row.zone_generation, Some(42));
    assert_eq!(row.record_count, Some(7));
    assert_eq!(row.query_cursor, Some(3));
    assert_eq!(row.listen_address.as_deref(), Some("10.237.0.1:53"));

    // Attribution: the asking address is the environment's container.
    let (rows, total) = service
        .query_log(&QueryLogFilter::default(), 1, 20)
        .await
        .unwrap();
    assert_eq!(total, 3);
    let attributed = rows
        .iter()
        .find(|r| r.qname.as_deref() == Some("production.shop.temps.local"))
        .unwrap();
    assert_eq!(attributed.environment_id, Some(env.id));
    assert_eq!(attributed.project_id, Some(env.project_id));
    assert_eq!(attributed.project_name.as_deref(), Some("Project shop"));
    assert_eq!(attributed.environment_name.as_deref(), Some("production"));
    let unknown = rows
        .iter()
        .find(|r| r.qname.as_deref() == Some("missing.shop.temps.local"))
        .unwrap();
    assert_eq!(unknown.project_id, None);
    assert_eq!(unknown.source_ip, "10.237.9.9");
    let forwarded = rows.iter().find(|r| r.zone == "external").unwrap();
    assert_eq!(forwarded.qname, None, "a forwarded name is never stored");

    // Second pull of the same instance: entries past the cursor only, and
    // the counter deltas.
    let t1 = t0 + Duration::seconds(30);
    let mut second = report("inst-1", counters(9, 2, 1_800), first.queries.clone());
    second.queries.push(entry(
        4,
        Some("production.shop.temps.local"),
        QueryOutcome::Noerror,
        "10.237.1.10",
    ));
    let summary = service
        .ingest(0, ReportOutcome::Report(Box::new(second)), t1)
        .await
        .unwrap();
    assert_eq!(summary.queries_logged, 1, "entries 1-3 were already stored");
    assert_eq!(summary.points_written, DNS_METRIC_NAMES.len());
    let points = store.points.lock().unwrap().clone();
    let value = |name: &str| points.iter().find(|p| p.name == name).unwrap().value;
    assert_eq!(value("dns.internal.noerror"), 4.0);
    assert_eq!(value("dns.internal.nxdomain"), 1.0);
    assert_eq!(value("dns.internal.latency_us"), 800.0);

    // Traffic reads those points back per bucket.
    let traffic = service
        .traffic(0, t0, t1 + Duration::seconds(1), Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(traffic.buckets.len(), 1);
    let bucket = &traffic.buckets[0];
    assert_eq!(bucket.internal.noerror, 4.0);
    assert_eq!(bucket.internal.nxdomain, 1.0);
    assert_eq!(bucket.internal_latency_us, Some(160.0));

    // A restarted resolver (new instance) re-baselines and its ring is read
    // from the start.
    let t2 = t1 + Duration::seconds(30);
    let restarted = report(
        "inst-2",
        counters(1, 0, 10),
        vec![entry(
            1,
            Some("production.shop.temps.local"),
            QueryOutcome::Noerror,
            "10.237.1.10",
        )],
    );
    let summary = service
        .ingest(0, ReportOutcome::Report(Box::new(restarted)), t2)
        .await
        .unwrap();
    assert_eq!(summary.queries_logged, 1);
    assert_eq!(summary.points_written, 0);

    // A failed pull keeps what is known and says why.
    let summary = service
        .ingest(
            0,
            ReportOutcome::Failed {
                state: ReportState::Unreachable,
                reason: "connection refused".into(),
            },
            t2 + Duration::seconds(30),
        )
        .await
        .unwrap();
    assert_eq!(summary.state, ReportState::Unreachable);
    let row = dns_resolver_reports::Entity::find_by_id(0)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.report_state, "unreachable");
    assert_eq!(row.last_error.as_deref(), Some("connection refused"));
    assert_eq!(row.last_ok_at, Some(t2));
    assert_eq!(row.instance_id.as_deref(), Some("inst-2"));

    // An invalid report is recorded as such and stores nothing else.
    let mut bad = report("inst-2", counters(2, 0, 20), vec![]);
    bad.version = 7;
    let summary = service
        .ingest(
            0,
            ReportOutcome::Report(Box::new(bad)),
            t2 + Duration::seconds(60),
        )
        .await
        .unwrap();
    assert_eq!(summary.state, ReportState::Invalid);
    assert!(summary.rejected.unwrap().contains("version 7"));

    // Filters.
    let (rows, total) = service
        .query_log(
            &QueryLogFilter {
                outcome: Some(QueryOutcome::Nxdomain),
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].qname.as_deref(), Some("missing.shop.temps.local"));
    let (_, total) = service
        .query_log(
            &QueryLogFilter {
                name: Some("PRODUCTION".into()),
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(total, 3, "name search is case-insensitive");
    let (_, total) = service
        .query_log(
            &QueryLogFilter {
                name: Some("%".into()),
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(total, 0, "a wildcard character matches literally");
    let (rows, total) = service
        .query_log(&QueryLogFilter::default(), 2, 2)
        .await
        .unwrap();
    assert_eq!(total, 5);
    assert_eq!(rows.len(), 2, "page 2 of 5 rows at 2 per page");

    // Failure counts for the alert monitor.
    let failures = service
        .internal_failures_since(t0 - Duration::minutes(20))
        .await
        .unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].outcome, "nxdomain");
    let totals = service
        .internal_totals_since(t0 - Duration::minutes(20))
        .await
        .unwrap();
    assert_eq!(totals, vec![(0, 4)]);

    // Records with their owners.
    let (records, total) = service
        .records(&RecordFilter::default(), 1, 20)
        .await
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(records[0].fqdn, "production.shop.temps.local");
    assert_eq!(records[0].project_name.as_deref(), Some("Project shop"));
    assert_eq!(records[0].environment_id, Some(env.id));
    let (_, total) = service
        .records(
            &RecordFilter {
                search: Some("10.237.1".into()),
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(total, 1, "search matches the target too");
    assert!(matches!(
        service
            .records(
                &RecordFilter {
                    owner_kind: Some("bogus".into()),
                    ..Default::default()
                },
                1,
                20,
            )
            .await,
        Err(DnsObservabilityError::InvalidFilter {
            field: "owner_kind",
            ..
        })
    ));

    // Expected names include environments not published yet.
    seed_environment(&db, "blog", "staging").await;
    let expected = service.expected_names().await.unwrap();
    assert!(expected.contains("production.shop.temps.local"));
    assert!(expected.contains("staging.blog.temps.local"));

    // Prune keeps rows inside the retention.
    assert_eq!(service.prune_query_log().await.unwrap(), 0);
    dns_query_log::ActiveModel {
        node_id: Set(0),
        observed_at: Set(Utc::now() - Duration::hours(30)),
        qname: Set(Some("old.temps.local".into())),
        qtype: Set("A".into()),
        zone: Set("internal".into()),
        outcome: Set("noerror".into()),
        latency_us: Set(1),
        answer_count: Set(1),
        source_ip: Set("10.0.0.1".into()),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    assert_eq!(service.prune_query_log().await.unwrap(), 1);

    service.forget_node(0).await.unwrap();
    assert!(dns_resolver_reports::Entity::find_by_id(0)
        .one(db.as_ref())
        .await
        .unwrap()
        .is_none());
}

/// ADR-104 D4: the control plane's internal routing is stored on node 0's
/// report row next to its resolver report, survives the next ingest until it
/// is rewritten, and clears when the control plane stops serving.
#[tokio::test]
async fn the_control_plane_internal_routing_is_stored_with_its_report() {
    let Some(test_db) = test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let store = Arc::new(RecordingStore::default());
    let service = DnsObservabilityService::new(db.clone(), store);
    let read = || async {
        dns_resolver_reports::Entity::find_by_id(0)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap()
            .internal_proxy
    };

    let t0 = chrono::SubsecRound::trunc_subsecs(Utc::now(), 6);
    service
        .ingest(
            0,
            ReportOutcome::Report(Box::new(report("inst-1", counters(1, 0, 10), Vec::new()))),
            t0,
        )
        .await
        .unwrap();
    assert_eq!(read().await, None, "a new row has nothing reported");

    let serving = serde_json::json!({
        "address": "172.20.0.1:80",
        "listening": true,
        "mode": "redirect",
        "proxy_address": "172.20.0.1:591",
        "redirect": "installed",
    });
    service
        .record_control_plane_internal_proxy(Some(serving.clone()))
        .await
        .unwrap();
    assert_eq!(read().await, Some(serving.clone()));

    service
        .ingest(
            0,
            ReportOutcome::Report(Box::new(report("inst-1", counters(2, 0, 20), Vec::new()))),
            t0 + chrono::Duration::seconds(30),
        )
        .await
        .unwrap();
    assert_eq!(
        read().await,
        Some(serving),
        "an ingest keeps the stored state"
    );

    service
        .record_control_plane_internal_proxy(None)
        .await
        .unwrap();
    assert_eq!(read().await, None);
}
