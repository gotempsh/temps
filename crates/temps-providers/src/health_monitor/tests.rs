// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::services::CpuLimit;

#[test]
fn health_status_strings() {
    assert_eq!(HealthProbeStatus::Operational.as_str(), "operational");
    assert_eq!(HealthProbeStatus::Degraded.as_str(), "degraded");
    assert_eq!(HealthProbeStatus::Down.as_str(), "down");
}

fn running_service() -> external_services::Model {
    let now = Utc::now();
    external_services::Model {
        id: 7,
        name: "metrics-svc".to_string(),
        service_type: "postgres".to_string(),
        topology: "standalone".to_string(),
        status: "running".to_string(),
        created_at: now,
        updated_at: now,
        node_id: None,
        version: None,
        slug: None,
        config: None,
        error_message: None,
        health_status: None,
        last_health_check_at: None,
        last_health_error: None,
        consecutive_health_failures: 0,
        health_metadata: None,
        metrics_enabled: true,
        default_backup_provisioned: false,
        container_name: None,
        created_by_user_id: None,
        continuous_archive_s3_source_id: None,
        continuous_archive_pinned_at: None,
        workload_mode: Default::default(),
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
    }
}

fn polled(cpu_percent: Option<f64>, cpu_limit: Option<CpuLimit>) -> PolledContainerStats {
    PolledContainerStats {
        sample: crate::services::ContainerStatsSample {
            role: "primary".to_string(),
            container_name: "service-postgres".to_string(),
            cpu_percent,
            memory_usage_bytes: Some(8_870_000_000),
            memory_limit_bytes: Some(34_359_738_368),
            memory_percent: Some(25.8),
            online_cpus: Some(16),
        },
        cpu_limit,
    }
}

fn point_value(points: &[MetricPoint], name: &str) -> Option<f64> {
    points.iter().find(|p| p.name == name).map(|p| p.value)
}

/// A capped container records cores used (Docker's 100 == one core), the
/// share of its limit and the limit itself, from one tick, so the tile
/// can read "0.68 / 6 cores" and alerts threshold the share of the limit.
#[test]
fn capped_container_records_cores_utilization_and_limit() {
    let points = container_metric_points(
        &running_service(),
        &[polled(Some(68.1), Some(CpuLimit::Cores(6.0)))],
        Utc::now(),
    );

    assert_eq!(point_value(&points, "container.cpu_percent"), Some(68.1));
    let utilization =
        point_value(&points, "container.cpu_utilization_percent").expect("utilization is recorded");
    assert!((utilization - 11.35).abs() < 1e-9, "{utilization}");
    assert_eq!(point_value(&points, "container.cpu_limit_cores"), Some(6.0));
    assert_eq!(
        point_value(&points, "container.memory_used_bytes"),
        Some(8_870_000_000.0)
    );
    for point in &points {
        assert!(matches!(point.source_kind, SourceKind::Database));
        assert_eq!(point.source_id, 7);
        // No new label keys: one series per container, as before.
        let mut keys: Vec<&str> = point.labels.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["container_name", "role"]);
    }
}

/// Uncapped, utilization is against the host's cores and the limit is
/// recorded as 0, so a removed cap replaces the old value in the latest
/// values instead of lingering there.
#[test]
fn uncapped_container_records_a_zero_limit() {
    let points = container_metric_points(
        &running_service(),
        &[polled(Some(80.0), Some(CpuLimit::Unlimited))],
        Utc::now(),
    );

    assert_eq!(
        point_value(&points, "container.cpu_utilization_percent"),
        Some(5.0)
    );
    assert_eq!(point_value(&points, "container.cpu_limit_cores"), Some(0.0));
}

/// The first tick (no rate yet) and an unknown limit record no CPU
/// utilization, never a zero or a host-relative guess; memory still is.
#[test]
fn first_tick_and_unknown_limit_record_no_cpu_utilization() {
    let first = container_metric_points(
        &running_service(),
        &[polled(None, Some(CpuLimit::Cores(6.0)))],
        Utc::now(),
    );
    assert_eq!(point_value(&first, "container.cpu_percent"), None);
    assert_eq!(
        point_value(&first, "container.cpu_utilization_percent"),
        None
    );
    assert!(point_value(&first, "container.memory_used_bytes").is_some());

    let unknown =
        container_metric_points(&running_service(), &[polled(Some(68.1), None)], Utc::now());
    assert_eq!(point_value(&unknown, "container.cpu_percent"), Some(68.1));
    assert_eq!(
        point_value(&unknown, "container.cpu_utilization_percent"),
        None
    );
    assert_eq!(point_value(&unknown, "container.cpu_limit_cores"), None);
}

fn member(
    name: &str,
    cpu_percent: Option<f64>,
    cpu_limit: Option<CpuLimit>,
) -> PolledContainerStats {
    let mut stats = polled(cpu_percent, cpu_limit);
    stats.sample.container_name = name.to_string();
    stats
}

fn unlabelled_value(points: &[MetricPoint], name: &str) -> Option<f64> {
    points
        .iter()
        .find(|p| p.name == name && p.labels.is_empty())
        .map(|p| p.value)
}

/// A cluster whose members have different caps gets one unlabelled limit
/// and share: the busiest member's own, from one tick. The tile never
/// pairs one member's usage with another member's cap.
#[test]
fn cluster_records_its_busiest_member_as_the_service_reading() {
    let points = container_metric_points(
        &running_service(),
        &[
            member("member-0", Some(150.0), Some(CpuLimit::Cores(2.0))),
            member("member-1", Some(30.0), Some(CpuLimit::Cores(4.0))),
        ],
        Utc::now(),
    );

    // member-0: 1.5 of its 2 cores (75%), busier than 0.3 of 4 (7.5%).
    assert_eq!(
        unlabelled_value(&points, "container.cluster_cpu_percent"),
        Some(150.0)
    );
    assert_eq!(
        unlabelled_value(&points, "container.cpu_limit_cores"),
        Some(2.0)
    );
    assert_eq!(
        unlabelled_value(&points, "container.cpu_utilization_percent"),
        Some(75.0)
    );
    // Per-member series are still recorded under their labels.
    let labelled_cpu = points
        .iter()
        .filter(|p| p.name == "container.cpu_percent" && !p.labels.is_empty())
        .count();
    assert_eq!(labelled_cpu, 2);
}

/// Every `container.cpu_percent` row recorded before the service-wide
/// reading existed is labelled per member. A chart follows the series
/// with the fewest label keys, so an unlabelled `cpu_percent` would hide
/// that history from cluster charts until it aged out of retention.
#[test]
fn cluster_cpu_percent_stays_labelled_so_its_history_still_charts() {
    let capped = Some(CpuLimit::Cores(2.0));
    let points = container_metric_points(
        &running_service(),
        &[
            member("member-0", Some(150.0), capped),
            member("member-1", Some(30.0), capped),
        ],
        Utc::now(),
    );
    assert!(points
        .iter()
        .filter(|p| p.name == "container.cpu_percent")
        .all(|p| p.labels.len() == 2));
    assert_eq!(unlabelled_value(&points, "container.cpu_percent"), None);
}

/// The service reading never sums members, so a threshold keeps one
/// container's meaning: three members at 0.5 of 2 cores read 25%, not
/// 1.5 cores' worth.
#[test]
fn cluster_reading_does_not_grow_with_member_count() {
    let capped = Some(CpuLimit::Cores(2.0));
    let busiest = busiest_member(&[
        member("member-0", Some(50.0), capped),
        member("member-1", Some(50.0), capped),
        member("member-2", Some(50.0), capped),
    ])
    .expect("every member reported");
    assert_eq!(busiest.utilization_percent, 25.0);
    assert_eq!(busiest.limit_cores, 2.0);
}

/// A stopped or unreadable member is left out: the surviving members keep
/// the service reading current every tick, so an older reading never
/// stays "latest" while the cluster runs degraded, and a survivor
/// saturating after failover reaches the alert.
#[test]
fn a_down_member_does_not_freeze_the_service_reading() {
    let capped = Some(CpuLimit::Cores(2.0));
    let points = container_metric_points(
        &running_service(),
        &[
            member("member-0", Some(190.0), capped),
            member("member-1", None, None),
        ],
        Utc::now(),
    );
    assert_eq!(
        unlabelled_value(&points, "container.cpu_utilization_percent"),
        Some(95.0)
    );

    // A member with a rate but no known limit is left out too.
    let busiest = busiest_member(&[
        member("member-0", Some(20.0), capped),
        member("member-1", Some(150.0), None),
    ])
    .expect("member-0 is complete");
    assert_eq!(busiest.utilization_percent, 10.0);
}

/// An uncapped busiest member still has its own cores recorded for the
/// service, so the tile shows them rather than any member's usage beside
/// a zero limit.
#[test]
fn an_uncapped_busiest_member_records_its_own_cores() {
    let points = container_metric_points(
        &running_service(),
        &[
            member("member-0", Some(50.0), Some(CpuLimit::Cores(2.0))),
            member("member-1", Some(800.0), Some(CpuLimit::Unlimited)),
        ],
        Utc::now(),
    );
    assert_eq!(
        unlabelled_value(&points, "container.cluster_cpu_percent"),
        Some(800.0)
    );
    assert_eq!(
        unlabelled_value(&points, "container.cpu_limit_cores"),
        Some(0.0)
    );
}

#[test]
fn an_uncapped_busiest_member_reads_a_zero_limit() {
    let busiest = busiest_member(&[
        member("member-0", Some(50.0), Some(CpuLimit::Cores(2.0))),
        member("member-1", Some(800.0), Some(CpuLimit::Unlimited)),
    ])
    .expect("both members reported");
    // member-1: 8 cores of the host's 16 (50%), busier than 0.5 of 2 (25%).
    assert_eq!(busiest.cpu_percent, 800.0);
    assert_eq!(busiest.limit_cores, 0.0);
    assert_eq!(busiest.utilization_percent, 50.0);
}

/// Nothing when no member reported a rate and a limit, and nothing for a
/// single container, whose own series already pair up.
#[test]
fn no_service_reading_without_a_complete_member_or_for_one_container() {
    let capped = Some(CpuLimit::Cores(2.0));
    assert_eq!(
        busiest_member(&[
            member("member-0", None, capped),
            member("member-1", Some(20.0), None)
        ]),
        None
    );
    assert_eq!(
        busiest_member(&[member("member-0", Some(50.0), capped)]),
        None
    );

    let single = container_metric_points(
        &running_service(),
        &[member("member-0", Some(50.0), capped)],
        Utc::now(),
    );
    assert!(single.iter().all(|p| !p.labels.is_empty()));
}

/// A sampler whose future is dropped mid-pass (a manual check whose
/// client disconnected) leaves the service's baselines in place.
#[tokio::test]
async fn a_dropped_sampler_keeps_the_service_baselines() {
    let store = StatsBaselines::default();
    store.for_service(7).await.lock().await.insert(
        "service-postgres".into(),
        ContainerStatsBaseline::seeded_for_test(),
    );

    let handle = store.for_service(7).await;
    let sampler = async {
        let _baselines = handle.lock().await;
        std::future::pending::<()>().await;
    };
    assert!(tokio::time::timeout(Duration::from_millis(10), sampler)
        .await
        .is_err());

    let baselines = store.for_service(7).await;
    assert!(baselines.lock().await.contains_key("service-postgres"));
}

/// Samplers of one service share its lock and take turns; another
/// service is not blocked by it.
#[tokio::test]
async fn samplers_of_one_service_share_its_lock() {
    let store = StatsBaselines::default();
    let a = store.for_service(7).await;
    let b = store.for_service(7).await;
    assert!(Arc::ptr_eq(&a, &b));

    let _held = a.lock().await;
    assert!(b.try_lock().is_err());
    let other = store.for_service(8).await;
    assert!(other.try_lock().is_ok());
}

#[tokio::test]
async fn baselines_of_unsampled_services_are_dropped() {
    let store = StatsBaselines::default();
    store.for_service(7).await;
    store.for_service(8).await;

    store.retain(&[7].into_iter().collect()).await;

    assert_eq!(store.services.lock().await.len(), 1);
    assert!(store.services.lock().await.contains_key(&7));
}

/// Only running local services with metrics on are sampled; every other
/// service's baselines are pruned each cycle.
#[test]
fn only_running_local_services_with_metrics_are_sampled() {
    assert!(samples_container_metrics(&running_service()));
    let stopped = external_services::Model {
        status: "stopped".to_string(),
        ..running_service()
    };
    let remote = external_services::Model {
        node_id: Some(3),
        ..running_service()
    };
    let metrics_off = external_services::Model {
        metrics_enabled: false,
        ..running_service()
    };
    for service in [stopped, remote, metrics_off] {
        assert!(!samples_container_metrics(&service));
    }
}

#[test]
fn running_services_are_probed() {
    assert_eq!(lifecycle_probe("running"), LifecycleProbe::Probe);
}

/// A service that is still being created must not produce a failed check:
/// that is what made a brand-new Redis read "Down" with "failed 1 check
/// ... status is 'pending'" and then sit at 50% uptime.
#[test]
fn provisioning_and_stopped_services_are_not_checks() {
    for status in [
        "pending",
        "creating",
        "starting",
        "restarting",
        "stopping",
        "stopped",
    ] {
        assert_eq!(
            lifecycle_probe(status),
            LifecycleProbe::NotApplicable,
            "status '{status}' must not count as a failed check"
        );
    }
}

#[test]
fn failed_and_unknown_services_are_down() {
    assert_eq!(lifecycle_probe("failed"), LifecycleProbe::Down);
    assert_eq!(lifecycle_probe("something-new"), LifecycleProbe::Down);
}

#[test]
fn merge_writes_new_key_into_empty_metadata() {
    let merged = merge_health_metadata(None, "postgres_wal", Some(&serde_json::json!({"a": 1})));
    let merged = merged.expect("expected merged value");
    assert_eq!(merged["postgres_wal"]["a"], 1);
}

#[test]
fn merge_preserves_sibling_keys() {
    let existing = serde_json::json!({"redis_memory": {"used_bytes": 42}});
    let merged = merge_health_metadata(
        Some(&existing),
        "postgres_wal",
        Some(&serde_json::json!({"pg_wal_bytes": 100})),
    )
    .expect("expected merged value");
    assert_eq!(merged["redis_memory"]["used_bytes"], 42);
    assert_eq!(merged["postgres_wal"]["pg_wal_bytes"], 100);
}

#[test]
fn merge_overwrites_same_key() {
    let existing = serde_json::json!({"postgres_wal": {"old": true}});
    let merged = merge_health_metadata(
        Some(&existing),
        "postgres_wal",
        Some(&serde_json::json!({"new": true})),
    )
    .expect("expected merged value");
    assert!(merged["postgres_wal"].get("old").is_none());
    assert_eq!(merged["postgres_wal"]["new"], true);
}

#[test]
fn merge_returns_none_when_snapshot_missing() {
    let existing = serde_json::json!({"postgres_wal": {"old": true}});
    let merged = merge_health_metadata::<serde_json::Value>(Some(&existing), "postgres_wal", None);
    assert!(merged.is_none());
}
