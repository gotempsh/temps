// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `Data` profile on Docker (ADR-055 §4, Phase 4): the host config
//! every managed database container gets, on the control plane and on a
//! worker alike.
//!
//! Before Phase 4 the control-plane engines and the worker agent each built
//! their own request, and the two had drifted (ADR-055 defect 1). A
//! database's request is now [`data_container_create_body`] of its admitted
//! spec, whichever node runs it.
//!
//! `Data` is today's control-plane database host config: `no-new-privileges`,
//! the engine's PID cap (512 to start with; it counts threads, and MongoDB
//! and MariaDB use one per connection), an init process, a fixed restart
//! policy and the engine's healthcheck. The init process reaps children the
//! engine did not fork (a timed-out healthcheck, an orphaned `docker exec`),
//! which PostgreSQL would otherwise treat as a crashed backend. It
//! deliberately does **not** drop every capability the way `Trusted` does:
//! the official database entrypoints start as root and need `CHOWN`,
//! `SETUID` and `SETGID` to fix the data directory's ownership and step down
//! to their service user, and no database container has ever run with them
//! dropped.

use bollard::models::{ContainerCreateBody, HealthConfig, HostConfig};
use temps_workload::{Attachments, InstanceSpec, Probe, ProbeKind, Probes};

use crate::container_create_body;

/// The PID cap a database engine starts from (§4: set per engine).
pub const DATA_DEFAULT_PIDS_LIMIT: i64 = 512;

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Host settings a database needs that are not part of `Resources` (§5):
/// `/dev/shm` for PostgreSQL's parallel workers, and a relative CPU weight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DataExtras {
    pub shm_size_bytes: Option<i64>,
    pub cpu_shares: Option<i64>,
}

/// The `Data` host config, before ports, networks and limits.
pub fn data_host_config(binds: Vec<String>) -> HostConfig {
    HostConfig {
        security_opt: Some(vec!["no-new-privileges:true".to_string()]),
        pids_limit: Some(DATA_DEFAULT_PIDS_LIMIT),
        init: Some(true),
        binds: (!binds.is_empty()).then_some(binds),
        ..Default::default()
    }
}

/// Docker's `Test` for a probe. An exec of `/bin/sh -c <script>` is Docker's
/// own `CMD-SHELL` form; any other exec is `CMD`. Only exec probes have a
/// Docker healthcheck form.
fn probe_test(kind: &ProbeKind) -> Option<Vec<String>> {
    let ProbeKind::Exec { command } = kind else {
        return None;
    };
    match command.as_slice() {
        [shell, flag, script] if shell == "/bin/sh" && flag == "-c" => {
            Some(vec!["CMD-SHELL".to_string(), script.clone()])
        }
        [] => None,
        args => Some(
            std::iter::once("CMD".to_string())
                .chain(args.iter().cloned())
                .collect(),
        ),
    }
}

fn secs(value: u32) -> i64 {
    i64::from(value) * NANOS_PER_SEC
}

/// Docker's single healthcheck from §13's probes.
///
/// The readiness probe is the check (interval, timeout, retries); the
/// startup probe is the grace period: its initial delay is Docker's start
/// period and its period the start interval. Liveness is not mapped: Docker
/// never restarts an unhealthy container by itself, and liveness has no
/// default (§13).
pub fn healthcheck(probes: &Probes) -> Option<HealthConfig> {
    let readiness: &Probe = probes.readiness.as_ref()?;
    let test = probe_test(&readiness.kind)?;
    let startup = probes.startup.as_ref();
    Some(HealthConfig {
        test: Some(test),
        interval: Some(secs(readiness.period_secs)),
        timeout: Some(secs(readiness.timeout_secs)),
        retries: Some(i64::from(readiness.failure_threshold)),
        start_period: startup.map(|p| secs(p.initial_delay_secs)),
        start_interval: startup.map(|p| secs(p.period_secs)),
    })
}

/// The container-create request for a database instance: the shared body
/// (whose host config is [`data_host_config`] for the `Data` profile), the
/// engine healthcheck, and the database's extra host settings.
pub fn data_container_create_body(
    spec: &InstanceSpec,
    probes: &Probes,
    extras: &DataExtras,
    attach: &Attachments,
    binds: Vec<String>,
) -> ContainerCreateBody {
    let mut body = container_create_body(spec, attach, binds);
    body.healthcheck = healthcheck(probes);
    if let Some(host) = body.host_config.as_mut() {
        if extras.shm_size_bytes.is_some() {
            host.shm_size = extras.shm_size_bytes;
        }
        if extras.cpu_shares.is_some() {
            host.cpu_shares = extras.cpu_shares;
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use temps_workload::{
        InstanceLimits, LogRotation, NetworkAttachment, PortBinding, PortProtocol, RestartPolicy,
        SecurityProfile, WorkloadKind, WorkloadRef,
    };

    fn spec() -> InstanceSpec {
        InstanceSpec {
            owner: WorkloadRef::new(WorkloadKind::Service, 4),
            name: "postgres-orders".into(),
            image: "gotempsh/postgres-walg:18-bookworm".into(),
            platform: None,
            env: vec![("POSTGRES_USER".into(), "postgres".into())],
            secret_files: vec![],
            command: None,
            entrypoint: None,
            workdir: None,
            user: None,
            ports: vec![PortBinding {
                container_port: 5432,
                protocol: PortProtocol::Tcp,
                host_ip: "127.0.0.1".into(),
                host_port: Some(15432),
            }],
            restart: RestartPolicy::Always,
            limits: InstanceLimits::default(),
            log_rotation: Some(LogRotation {
                max_size: "20m".into(),
                max_file: 3,
            }),
            labels: BTreeMap::from([("temps.service_type".into(), "postgres".into())]),
            security: SecurityProfile::Data,
        }
    }

    fn probes() -> Probes {
        Probes {
            startup: Some(Probe {
                initial_delay_secs: 30,
                period_secs: 1,
                ..Probe::new(ProbeKind::Exec {
                    command: vec!["/bin/sh".into(), "-c".into(), "pg_isready".into()],
                })
            }),
            readiness: Some(Probe {
                period_secs: 1,
                timeout_secs: 3,
                failure_threshold: 3,
                ..Probe::new(ProbeKind::Exec {
                    command: vec!["/bin/sh".into(), "-c".into(), "pg_isready".into()],
                })
            }),
            liveness: None,
        }
    }

    #[test]
    fn the_data_profile_is_the_control_plane_database_host_config() {
        let body = data_container_create_body(
            &spec(),
            &probes(),
            &DataExtras {
                shm_size_bytes: Some(256 * 1024 * 1024),
                cpu_shares: None,
            },
            &Attachments {
                network: NetworkAttachment {
                    primary: Some("temps-app-network".into()),
                    ..Default::default()
                },
                volumes: vec!["postgres-orders_data:/var/lib/postgresql".into()],
            },
            vec![],
        );
        let host = body.host_config.as_ref().unwrap();
        assert_eq!(
            host.security_opt,
            Some(vec!["no-new-privileges:true".into()])
        );
        assert_eq!(host.pids_limit, Some(DATA_DEFAULT_PIDS_LIMIT));
        assert_eq!(
            host.cap_drop, None,
            "database entrypoints need their capabilities"
        );
        assert_eq!(host.init, Some(true));
        assert_eq!(
            host.restart_policy.as_ref().and_then(|p| p.name),
            Some(bollard::models::RestartPolicyNameEnum::ALWAYS)
        );
        assert_eq!(host.shm_size, Some(256 * 1024 * 1024));
        assert_eq!(
            host.binds,
            Some(vec!["postgres-orders_data:/var/lib/postgresql".into()])
        );
        let health = body.healthcheck.unwrap();
        assert_eq!(
            health.test,
            Some(vec!["CMD-SHELL".into(), "pg_isready".into()])
        );
        assert_eq!(health.interval, Some(1_000_000_000));
        assert_eq!(health.timeout, Some(3_000_000_000));
        assert_eq!(health.retries, Some(3));
        assert_eq!(health.start_period, Some(30_000_000_000));
        assert_eq!(health.start_interval, Some(1_000_000_000));
    }

    #[test]
    fn other_profiles_keep_the_hardened_host_config() {
        let mut trusted = spec();
        trusted.security = SecurityProfile::Trusted;
        let body = container_create_body(&trusted, &Attachments::default(), vec![]);
        assert_eq!(
            body.host_config.unwrap().cap_drop,
            Some(vec!["ALL".to_string()])
        );
    }

    #[test]
    fn only_exec_probes_become_a_docker_healthcheck() {
        let argv = Probes {
            readiness: Some(Probe::new(ProbeKind::Exec {
                command: vec!["redis-cli".into(), "ping".into()],
            })),
            ..Default::default()
        };
        assert_eq!(
            healthcheck(&argv).unwrap().test,
            Some(vec!["CMD".into(), "redis-cli".into(), "ping".into()])
        );
        let tcp = Probes {
            readiness: Some(Probe::new(ProbeKind::Tcp { port: 6379 })),
            ..Default::default()
        };
        assert_eq!(healthcheck(&tcp), None);
        assert_eq!(healthcheck(&Probes::default()), None);
    }
}
