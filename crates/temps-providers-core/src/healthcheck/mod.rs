// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Docker healthcheck every managed service container is created with.
//!
//! Every engine that gives its container a healthcheck (PostgreSQL, Redis,
//! MongoDB, MariaDB, RustFS and MinIO) builds it here, through
//! [`managed_health_config`] for its own create request or
//! [`managed_probes`] for the workload spec, so the cadence is decided in one
//! place.
//!
//! The check runs every [`HealthcheckTimings::start_interval_secs`] while the
//! container is starting, so the readiness waits after create, start, upgrade
//! and restore still see it healthy within about two seconds, and every
//! [`HealthcheckTimings::interval_secs`] once it is healthy. Each check is a
//! process Docker execs inside the container (`pg_isready`, `redis-cli`,
//! `mongosh`, `curl`), so the steady-state interval is what a healthy
//! service pays for, forever.
//!
//! Docker applies `StartInterval` only from API 1.44 (Engine 25). A daemon
//! that reports an older API ignores it, and would run even the startup
//! checks at the steady-state interval; there the steady-state interval falls
//! back to the startup one (see [`effective_timings`]). The daemon's API
//! version is asked once per process and cached.

use std::sync::OnceLock;
use std::time::Duration;

use bollard::models::HealthConfig;
use bollard::Docker;
use serde::Serialize;
use temps_workload::{Probe, ProbeKind, Probes};
use tracing::{debug, warn};
use utoipa::ToSchema;

use crate::externalsvc::recreate_guard::ManagedEngine;

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// The first Docker API version that honours `HealthConfig.StartInterval`.
pub const START_INTERVAL_MIN_API: (u32, u32) = (1, 44);

/// How long asking the daemon for its version may take before the fallback
/// is used for this container.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The five numbers of a Docker healthcheck, in whole seconds (retries is a
/// count).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct HealthcheckTimings {
    /// Seconds between checks once the container is healthy.
    #[schema(example = 30)]
    pub interval_secs: u32,
    /// Seconds between checks while the container is starting, within
    /// `start_period_secs`.
    #[schema(example = 2)]
    pub start_interval_secs: u32,
    /// Seconds after start during which failed checks do not count and
    /// checks run every `start_interval_secs`.
    #[schema(example = 90)]
    pub start_period_secs: u32,
    /// Seconds one check may run before it counts as failed.
    #[schema(example = 5)]
    pub timeout_secs: u32,
    /// Consecutive failed checks that make the container unhealthy.
    #[schema(example = 3)]
    pub retries: u32,
}

impl HealthcheckTimings {
    /// Temps' own timings for `engine`, used when nothing overrides them.
    ///
    /// The start period is each engine's readiness-wait budget after create
    /// and start (90 s for most engines, 120 s for PostgreSQL's upgrade path
    /// and MariaDB, 60 s for RustFS). Docker probes at the start interval
    /// only inside the start period, so an engine slower than it would be
    /// noticed at the steady-state interval. A longer start period costs
    /// nothing once the container is healthy: Docker switches to `interval`
    /// at the first passing check.
    ///
    /// MongoDB keeps a 10 s timeout: its check starts `mongosh`, a Node.js
    /// CLI, and authenticates, which takes seconds on a loaded host.
    pub const fn builtin(engine: ManagedEngine) -> Self {
        let (start_period_secs, timeout_secs) = match engine {
            ManagedEngine::Postgres => (120, 5),
            ManagedEngine::Redis => (90, 5),
            ManagedEngine::MongoDb => (90, 10),
            ManagedEngine::MariaDb => (120, 5),
            ManagedEngine::Rustfs => (60, 5),
            ManagedEngine::Minio => (90, 5),
        };
        Self {
            interval_secs: 30,
            start_interval_secs: 2,
            start_period_secs,
            timeout_secs,
            retries: 3,
        }
    }
}

/// Whether the Docker daemon honours `HealthConfig.StartInterval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartIntervalSupport {
    /// API 1.44 or newer.
    Supported,
    /// An older API, or a version that could not be read: Docker would run
    /// startup checks at the steady-state interval.
    Unsupported,
}

impl StartIntervalSupport {
    /// From the daemon's reported API version (`"1.44"`). `None` when it is
    /// not a `major.minor` version.
    pub fn from_api_version(api_version: &str) -> Option<Self> {
        let (major, minor) = api_version.trim().split_once('.')?;
        let version = (major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?);
        Some(if version >= START_INTERVAL_MIN_API {
            Self::Supported
        } else {
            Self::Unsupported
        })
    }
}

/// The timings Docker is asked for on a daemon with `support`.
///
/// Without `StartInterval` support the startup checks would run at
/// `interval_secs`, so readiness waits would see a starting container only
/// every 30 s. There the steady-state interval falls back to the start
/// interval (the cadence every managed container had before the steady-state
/// interval existed), keeping startup detection as fast as it was.
pub fn effective_timings(
    timings: HealthcheckTimings,
    support: StartIntervalSupport,
) -> HealthcheckTimings {
    match support {
        StartIntervalSupport::Supported => timings,
        StartIntervalSupport::Unsupported => HealthcheckTimings {
            interval_secs: timings.interval_secs.min(timings.start_interval_secs),
            ..timings
        },
    }
}

fn nanos(secs: u32) -> i64 {
    i64::from(secs) * NANOS_PER_SEC
}

/// Docker's `HealthConfig` for a `CMD-SHELL` `script` with `timings`.
pub fn health_config(script: impl Into<String>, timings: &HealthcheckTimings) -> HealthConfig {
    HealthConfig {
        test: Some(vec!["CMD-SHELL".to_string(), script.into()]),
        interval: Some(nanos(timings.interval_secs)),
        timeout: Some(nanos(timings.timeout_secs)),
        retries: Some(i64::from(timings.retries)),
        start_period: Some(nanos(timings.start_period_secs)),
        start_interval: Some(nanos(timings.start_interval_secs)),
    }
}

/// The workload-spec probes for a `/bin/sh -c` `script` with `timings`.
///
/// The Docker driver maps them back onto one healthcheck: readiness is the
/// check (interval, timeout, retries) and startup the grace period (start
/// period, start interval). See `temps_workload_docker::data::healthcheck`.
pub fn probes(script: impl Into<String>, timings: &HealthcheckTimings) -> Probes {
    let exec = ProbeKind::Exec {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.into()],
    };
    Probes {
        startup: Some(Probe {
            initial_delay_secs: timings.start_period_secs,
            period_secs: timings.start_interval_secs,
            ..Probe::new(exec.clone())
        }),
        readiness: Some(Probe {
            period_secs: timings.interval_secs,
            timeout_secs: timings.timeout_secs,
            failure_threshold: timings.retries,
            ..Probe::new(exec)
        }),
        liveness: None,
    }
}

/// `probes` as the daemon with `support` should receive them: the
/// readiness period falls back to the startup period where `StartInterval`
/// is ignored (see [`effective_timings`]).
pub fn probes_for_daemon(probes: &Probes, support: StartIntervalSupport) -> Probes {
    let mut probes = probes.clone();
    if support == StartIntervalSupport::Unsupported {
        if let (Some(startup), Some(readiness)) = (&probes.startup, probes.readiness.as_mut()) {
            readiness.period_secs = readiness.period_secs.min(startup.period_secs);
        }
    }
    probes
}

/// What the local daemon supports, once known. One process talks to one
/// local daemon (the control plane's, or a worker agent's own).
static LOCAL_DAEMON_SUPPORT: OnceLock<StartIntervalSupport> = OnceLock::new();

/// Whether the local daemon honours `StartInterval`, asked once per process.
///
/// A version that cannot be read is not cached: that container gets the
/// fallback, and the next create asks again.
pub async fn start_interval_support(docker: &Docker) -> StartIntervalSupport {
    detect_start_interval_support(&LOCAL_DAEMON_SUPPORT, async {
        match tokio::time::timeout(VERSION_TIMEOUT, docker.version()).await {
            Ok(Ok(version)) => Ok(version.api_version),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!("no answer within {}s", VERSION_TIMEOUT.as_secs())),
        }
    })
    .await
}

/// [`start_interval_support`] against any cache and version source, so the
/// caching and the fallback can be tested without a daemon.
async fn detect_start_interval_support<F>(
    cache: &OnceLock<StartIntervalSupport>,
    api_version: F,
) -> StartIntervalSupport
where
    F: std::future::Future<Output = Result<Option<String>, String>>,
{
    if let Some(support) = cache.get() {
        return *support;
    }
    match api_version.await {
        Ok(Some(version)) => match StartIntervalSupport::from_api_version(&version) {
            Some(support) => {
                if support == StartIntervalSupport::Unsupported {
                    warn!(
                        api_version = %version,
                        min_api_version = %format!("{}.{}", START_INTERVAL_MIN_API.0, START_INTERVAL_MIN_API.1),
                        "Docker daemon ignores healthcheck StartInterval; managed service \
                         containers keep checking at their startup interval once healthy"
                    );
                } else {
                    debug!(api_version = %version, "Docker daemon honours healthcheck StartInterval");
                }
                *cache.get_or_init(|| support)
            }
            None => {
                warn!(
                    api_version = %version,
                    "Docker daemon reported an API version that is not major.minor; using the \
                     startup healthcheck interval for this container"
                );
                StartIntervalSupport::Unsupported
            }
        },
        Ok(None) => {
            warn!(
                "Docker daemon reported no API version; using the startup healthcheck interval \
                 for this container"
            );
            StartIntervalSupport::Unsupported
        }
        Err(reason) => {
            warn!(
                %reason,
                "Could not read the Docker daemon's API version; using the startup healthcheck \
                 interval for this container"
            );
            StartIntervalSupport::Unsupported
        }
    }
}

/// The timings a new `engine` container named `container_name` gets.
pub fn resolved_timings(engine: ManagedEngine, _container_name: &str) -> HealthcheckTimings {
    HealthcheckTimings::builtin(engine)
}

/// The healthcheck an engine puts in its own create request for
/// `container_name`: the resolved timings, adjusted for what the local
/// daemon supports.
pub async fn managed_health_config(
    docker: &Docker,
    engine: ManagedEngine,
    container_name: &str,
    script: impl Into<String>,
) -> HealthConfig {
    let support = start_interval_support(docker).await;
    let timings = effective_timings(resolved_timings(engine, container_name), support);
    debug!(
        container = container_name,
        %engine,
        interval_secs = timings.interval_secs,
        start_interval_secs = timings.start_interval_secs,
        start_period_secs = timings.start_period_secs,
        "Managed service healthcheck"
    );
    health_config(script, &timings)
}

/// The workload-spec probes of a database `engine` container named
/// `container_name`. The daemon adjustment happens where the container is
/// created ([`probes_for_daemon`]), on the host that creates it.
pub fn managed_probes(
    engine: ManagedEngine,
    container_name: &str,
    script: impl Into<String>,
) -> Probes {
    probes(script, &resolved_timings(engine, container_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ENGINES: [ManagedEngine; 6] = [
        ManagedEngine::Postgres,
        ManagedEngine::Redis,
        ManagedEngine::MongoDb,
        ManagedEngine::MariaDb,
        ManagedEngine::Rustfs,
        ManagedEngine::Minio,
    ];

    #[test]
    fn every_engine_checks_every_30s_once_healthy_and_every_2s_while_starting() {
        for engine in ENGINES {
            let timings = HealthcheckTimings::builtin(engine);
            assert_eq!(timings.interval_secs, 30, "{engine}");
            assert_eq!(timings.start_interval_secs, 2, "{engine}");
            assert_eq!(timings.retries, 3, "{engine}");
            assert!(timings.timeout_secs < timings.interval_secs, "{engine}");
            // The fast window covers at least one readiness-wait budget.
            assert!(timings.start_period_secs >= 60, "{engine}");
        }
        assert_eq!(
            HealthcheckTimings::builtin(ManagedEngine::Redis).timeout_secs,
            5
        );
        assert_eq!(
            HealthcheckTimings::builtin(ManagedEngine::MongoDb).timeout_secs,
            10
        );
    }

    #[test]
    fn health_config_is_in_nanoseconds_with_a_shell_test() {
        let config = health_config(
            "redis-cli ping",
            &HealthcheckTimings::builtin(ManagedEngine::Redis),
        );
        assert_eq!(
            config.test,
            Some(vec!["CMD-SHELL".to_string(), "redis-cli ping".to_string()])
        );
        assert_eq!(config.interval, Some(30_000_000_000));
        assert_eq!(config.start_interval, Some(2_000_000_000));
        assert_eq!(config.start_period, Some(90_000_000_000));
        assert_eq!(config.timeout, Some(5_000_000_000));
        assert_eq!(config.retries, Some(3));
    }

    #[test]
    fn api_versions_before_1_44_do_not_support_start_interval() {
        use StartIntervalSupport::*;
        for (version, expected) in [
            ("1.43", Some(Unsupported)),
            ("1.41", Some(Unsupported)),
            ("1.44", Some(Supported)),
            ("1.54", Some(Supported)),
            ("2.0", Some(Supported)),
            (" 1.45 ", Some(Supported)),
            ("", None),
            ("1", None),
            ("one.two", None),
        ] {
            assert_eq!(
                StartIntervalSupport::from_api_version(version),
                expected,
                "{version:?}"
            );
        }
    }

    #[test]
    fn an_old_daemon_keeps_checking_at_the_startup_interval() {
        let timings = HealthcheckTimings::builtin(ManagedEngine::Rustfs);
        assert_eq!(
            effective_timings(timings, StartIntervalSupport::Supported),
            timings
        );
        let old = effective_timings(timings, StartIntervalSupport::Unsupported);
        assert_eq!(old.interval_secs, 2);
        assert_eq!(
            HealthcheckTimings {
                interval_secs: timings.interval_secs,
                ..old
            },
            timings,
            "only the interval changes"
        );
        // A start interval longer than the interval never slows it down.
        let slow_start = HealthcheckTimings {
            interval_secs: 10,
            start_interval_secs: 20,
            ..timings
        };
        assert_eq!(
            effective_timings(slow_start, StartIntervalSupport::Unsupported).interval_secs,
            10
        );
    }

    #[test]
    fn old_daemon_probes_get_the_startup_period() {
        let probes = probes(
            "true",
            &HealthcheckTimings::builtin(ManagedEngine::Postgres),
        );
        let old = probes_for_daemon(&probes, StartIntervalSupport::Unsupported);
        assert_eq!(old.readiness.as_ref().map(|p| p.period_secs), Some(2));
        assert_eq!(old.startup, probes.startup);
        assert_eq!(
            probes_for_daemon(&probes, StartIntervalSupport::Supported),
            probes
        );
    }

    #[test]
    fn probes_carry_every_timing() {
        let timings = HealthcheckTimings::builtin(ManagedEngine::MongoDb);
        let probes = probes("mongosh", &timings);
        let startup = probes.startup.expect("startup probe");
        let readiness = probes.readiness.expect("readiness probe");
        assert_eq!(startup.initial_delay_secs, timings.start_period_secs);
        assert_eq!(startup.period_secs, timings.start_interval_secs);
        assert_eq!(readiness.period_secs, timings.interval_secs);
        assert_eq!(readiness.timeout_secs, timings.timeout_secs);
        assert_eq!(readiness.failure_threshold, timings.retries);
        assert!(probes.liveness.is_none());
    }

    #[tokio::test]
    async fn the_daemon_version_is_asked_once_and_cached() {
        let cache = OnceLock::new();
        let asked = AtomicUsize::new(0);
        for _ in 0..3 {
            let support = detect_start_interval_support(&cache, async {
                asked.fetch_add(1, Ordering::SeqCst);
                Ok(Some("1.43".to_string()))
            })
            .await;
            assert_eq!(support, StartIntervalSupport::Unsupported);
        }
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_unreadable_version_falls_back_and_is_asked_again() {
        let cache = OnceLock::new();
        let failed =
            detect_start_interval_support(&cache, async { Err("connection refused".to_string()) })
                .await;
        assert_eq!(failed, StartIntervalSupport::Unsupported);
        assert!(cache.get().is_none(), "a failure is not cached");
        let missing = detect_start_interval_support(&cache, async { Ok(None) }).await;
        assert_eq!(missing, StartIntervalSupport::Unsupported);
        let garbled =
            detect_start_interval_support(&cache, async { Ok(Some("v-next".to_string())) }).await;
        assert_eq!(garbled, StartIntervalSupport::Unsupported);
        assert!(cache.get().is_none());
        let modern =
            detect_start_interval_support(&cache, async { Ok(Some("1.54".to_string())) }).await;
        assert_eq!(modern, StartIntervalSupport::Supported);
        assert_eq!(cache.get(), Some(&StartIntervalSupport::Supported));
    }
}

#[cfg(test)]
mod docker_tests {
    use super::*;
    use bollard::models::ContainerCreateBody;
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, InspectContainerOptions, RemoveContainerOptionsBuilder,
    };

    const IMAGE: &str = "busybox:1.36";

    /// A container created with the shared builder carries the steady-state
    /// interval and the startup interval as Docker stores them. Skips when no
    /// daemon answers.
    #[tokio::test]
    async fn a_container_gets_the_builder_s_interval_and_start_interval() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(error) => {
                println!("Docker client unavailable, skipping healthcheck test: {error}");
                return;
            }
        };
        if let Err(error) = docker.ping().await {
            println!("Docker daemon not responding, skipping healthcheck test: {error}");
            return;
        }
        crate::utils::pull_image_with_retry(&docker, IMAGE, None)
            .await
            .expect("pull the test image");
        let name = format!("temps-healthcheck-test-{}", uuid::Uuid::new_v4().simple());
        let health = managed_health_config(&docker, ManagedEngine::Rustfs, &name, "true").await;
        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(IMAGE.to_string()),
                    cmd: Some(vec!["sleep".to_string(), "60".to_string()]),
                    healthcheck: Some(health),
                    ..Default::default()
                },
            )
            .await
            .expect("create the test container");
        let inspected = docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await;
        // Only this test's container, removed before asserting.
        let _ = docker
            .remove_container(
                &name,
                Some(RemoveContainerOptionsBuilder::new().force(true).build()),
            )
            .await;
        let stored = inspected
            .expect("inspect the test container")
            .config
            .and_then(|config| config.healthcheck)
            .expect("the container has a healthcheck");

        let expected = effective_timings(
            HealthcheckTimings::builtin(ManagedEngine::Rustfs),
            start_interval_support(&docker).await,
        );
        assert_eq!(stored.interval, Some(nanos(expected.interval_secs)));
        assert_eq!(stored.start_interval, Some(2_000_000_000));
        assert_eq!(stored.start_period, Some(60_000_000_000));
        assert_eq!(stored.timeout, Some(5_000_000_000));
        assert_eq!(stored.retries, Some(3));
        if start_interval_support(&docker).await == StartIntervalSupport::Supported {
            assert_eq!(stored.interval, Some(30_000_000_000));
        }
    }
}
