// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keeps Docker containers started by tests from outliving the tests.
//!
//! `testcontainers` removes a container when its handle is dropped. That
//! never happens for a handle parked in a `static` (a shared per-binary
//! database), because Rust does not drop statics at process exit, and it never
//! happens for a process killed with SIGKILL. Under `cargo nextest` every
//! test is its own process, so a shared-container helper leaked one running
//! database per test, hundreds on a developer machine.
//!
//! Call [`TestContainerExt::with_test_labels`] on every container request a
//! test makes. It
//!
//! 1. labels the container (`temps.test=true`, owner pid, a per-process
//!    nonce, host name and creation time) so stragglers can be found with
//!    `docker ps -a --filter label=temps.test=true`;
//! 2. registers an `atexit` hook that force-removes, volumes included, every
//!    container this process labelled, covering statics and panics;
//! 3. sweeps containers left by dead runs (owner pid gone and older than
//!    [`DEAD_OWNER_GRACE_SECS`], or older than [`MAX_AGE_SECS`] whatever the
//!    owner) the first time it is called in a process.
//!
//! Everything here is best effort and never fails a test: an unreachable
//! Docker daemon is reported on stderr and skipped.

use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bollard::query_parameters::{ListContainersOptionsBuilder, RemoveContainerOptionsBuilder};
use bollard::Docker;
use testcontainers::{ContainerRequest, Image, ImageExt};

/// Label present on every container started through this crate.
pub const LABEL_TEST: &str = "temps.test";
/// Pid of the test process that started the container.
pub const LABEL_PID: &str = "temps.test.pid";
/// Unique per test process, so a recycled pid is never mistaken for the owner.
pub const LABEL_NONCE: &str = "temps.test.nonce";
/// Host that ran the test process; pid liveness only means anything there.
pub const LABEL_HOST: &str = "temps.test.host";
/// Unix seconds at which the process labelled the container.
pub const LABEL_CREATED_AT: &str = "temps.test.created_at";

/// A container whose owner process is gone is reaped once it is this old,
/// leaving room for a container created moments before its owner exited
/// normally and is still being removed.
pub const DEAD_OWNER_GRACE_SECS: u64 = 10 * 60;
/// Any labelled container older than this is reaped, covering a recycled pid
/// and a container started on another host.
pub const MAX_AGE_SECS: u64 = 6 * 60 * 60;

/// How long exit cleanup and the startup sweep may talk to Docker.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(30);

static INIT: Once = Once::new();

/// Adds the test labels to a container request and makes sure the exit hook
/// and the startup sweep are in place.
pub trait TestContainerExt<I: Image> {
    /// Labels the container and registers cleanup for it. See the crate docs.
    fn with_test_labels(self) -> ContainerRequest<I>;
}

impl<I: Image, T: ImageExt<I>> TestContainerExt<I> for T {
    fn with_test_labels(self) -> ContainerRequest<I> {
        init();
        self.with_labels(process_labels(unix_now()))
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn host_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for `buffer.len()` bytes and gethostname
    // never writes past that length.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return String::new();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// Unique to this process for its whole life.
fn process_nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        format!("{}-{nanos}", std::process::id())
    })
}

fn process_labels(created_at: u64) -> Vec<(String, String)> {
    vec![
        (LABEL_TEST.to_string(), "true".to_string()),
        (LABEL_PID.to_string(), std::process::id().to_string()),
        (LABEL_NONCE.to_string(), process_nonce().to_string()),
        (LABEL_HOST.to_string(), host_name()),
        (LABEL_CREATED_AT.to_string(), created_at.to_string()),
    ]
}

fn init() {
    INIT.call_once(|| {
        // SAFETY: `remove_own_containers_at_exit` is an `extern "C" fn` that
        // takes no arguments and does not unwind.
        let registered = unsafe { libc::atexit(remove_own_containers_at_exit) };
        if registered != 0 {
            eprintln!(
                "temps-test-containers: could not register the exit hook; \
                 containers labelled {LABEL_TEST}=true may outlive this process"
            );
        }
        // A fresh thread and runtime: the caller may be inside a
        // current-thread runtime that cannot be blocked.
        run_blocking(|docker| async move {
            reap_stale(&docker, unix_now()).await;
        });
    });
}

extern "C" fn remove_own_containers_at_exit() {
    run_blocking(|docker| async move {
        let filters = vec![format!("{LABEL_NONCE}={}", process_nonce())];
        remove_matching(&docker, filters).await;
    });
}

/// Runs `work` against a Docker client on its own thread and runtime, waiting
/// at most [`DOCKER_TIMEOUT`]. Never panics: this runs from `atexit`.
fn run_blocking<F, Fut>(work: F)
where
    F: FnOnce(Docker) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name("temps-test-containers".to_string())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let Ok(docker) = Docker::connect_with_defaults() else {
                return;
            };
            runtime.block_on(async {
                if tokio::time::timeout(DOCKER_TIMEOUT, work(docker))
                    .await
                    .is_err()
                {
                    eprintln!("temps-test-containers: Docker did not answer within {DOCKER_TIMEOUT:?}; skipped");
                }
            });
        });
    if let Ok(handle) = handle {
        let _ = handle.join();
    }
}

async fn list_labelled(
    docker: &Docker,
    label_filters: Vec<String>,
) -> Vec<bollard::models::ContainerSummary> {
    let mut filters = std::collections::HashMap::new();
    filters.insert("label".to_string(), label_filters);
    let options = ListContainersOptionsBuilder::new()
        .all(true)
        .filters(&filters)
        .build();
    match docker.list_containers(Some(options)).await {
        Ok(containers) => containers,
        Err(error) => {
            eprintln!("temps-test-containers: could not list test containers: {error}");
            Vec::new()
        }
    }
}

async fn remove_container(docker: &Docker, id: &str) {
    let options = RemoveContainerOptionsBuilder::new()
        .force(true)
        .v(true)
        .build();
    if let Err(error) = docker.remove_container(id, Some(options)).await {
        // Already gone is the expected outcome for a container its handle dropped.
        let message = error.to_string();
        if !message.contains("No such container") && !message.contains("404") {
            eprintln!("temps-test-containers: could not remove container {id}: {error}");
        }
    }
}

async fn remove_matching(docker: &Docker, label_filters: Vec<String>) {
    for container in list_labelled(docker, label_filters).await {
        if let Some(id) = container.id {
            remove_container(docker, &id).await;
        }
    }
}

/// What the sweep needs to know about one labelled container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub pid: Option<u32>,
    pub host: Option<String>,
    pub created_at: Option<u64>,
}

impl Candidate {
    fn from_labels(labels: &std::collections::HashMap<String, String>) -> Self {
        Self {
            pid: labels.get(LABEL_PID).and_then(|v| v.parse().ok()),
            host: labels.get(LABEL_HOST).cloned(),
            created_at: labels.get(LABEL_CREATED_AT).and_then(|v| v.parse().ok()),
        }
    }
}

/// Whether the sweep should remove a container. `pid_alive` answers for a pid
/// on this host. A container with no readable creation time is treated as old:
/// it is labelled `temps.test=true`, so it is ours to clean.
pub fn should_reap(
    candidate: &Candidate,
    now: u64,
    local_host: &str,
    pid_alive: impl Fn(u32) -> bool,
) -> bool {
    let age = candidate
        .created_at
        .map(|created| now.saturating_sub(created))
        .unwrap_or(u64::MAX);
    if age > MAX_AGE_SECS {
        return true;
    }
    let same_host = candidate.host.as_deref() == Some(local_host);
    match candidate.pid {
        Some(pid) if same_host => !pid_alive(pid) && age > DEAD_OWNER_GRACE_SECS,
        _ => false,
    }
}

fn local_pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks for existence and permission.
    let status = unsafe { libc::kill(pid, 0) };
    status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Removes every labelled container [`should_reap`] approves. Returns the
/// number removed.
pub async fn reap_stale(docker: &Docker, now: u64) -> usize {
    let local_host = host_name();
    let own_nonce = process_nonce();
    let mut removed = 0;
    for container in list_labelled(docker, vec![format!("{LABEL_TEST}=true")]).await {
        let (Some(id), Some(labels)) = (container.id, container.labels) else {
            continue;
        };
        if labels.get(LABEL_NONCE).map(String::as_str) == Some(own_nonce) {
            continue;
        }
        let candidate = Candidate::from_labels(&labels);
        if should_reap(&candidate, now, &local_host, local_pid_alive) {
            remove_container(docker, &id).await;
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::{runners::AsyncRunner, GenericImage};

    fn candidate(pid: Option<u32>, host: &str, age: u64, now: u64) -> Candidate {
        Candidate {
            pid,
            host: Some(host.to_string()),
            created_at: Some(now - age),
        }
    }

    #[test]
    fn live_owner_keeps_its_container() {
        let c = candidate(Some(10), "h", DEAD_OWNER_GRACE_SECS + 5, 100_000);
        assert!(!should_reap(&c, 100_000, "h", |_| true));
    }

    #[test]
    fn dead_owner_is_reaped_only_after_the_grace_period() {
        let young = candidate(Some(10), "h", DEAD_OWNER_GRACE_SECS - 1, 100_000);
        let old = candidate(Some(10), "h", DEAD_OWNER_GRACE_SECS + 1, 100_000);
        assert!(!should_reap(&young, 100_000, "h", |_| false));
        assert!(should_reap(&old, 100_000, "h", |_| false));
    }

    #[test]
    fn other_hosts_pid_is_not_judged_until_the_hard_age_cap() {
        let mid = candidate(Some(10), "other", MAX_AGE_SECS - 1, 100_000);
        let old = candidate(Some(10), "other", MAX_AGE_SECS + 1, 100_000);
        assert!(!should_reap(&mid, 100_000, "h", |_| false));
        assert!(should_reap(&old, 100_000, "h", |_| true));
    }

    #[test]
    fn unreadable_creation_time_counts_as_old() {
        let c = Candidate {
            pid: None,
            host: None,
            created_at: None,
        };
        assert!(should_reap(&c, 100_000, "h", |_| true));
    }

    #[test]
    fn labels_identify_this_process() {
        let labels = process_labels(42);
        let get = |key: &str| {
            labels
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get(LABEL_TEST).as_deref(), Some("true"));
        assert_eq!(get(LABEL_PID), Some(std::process::id().to_string()));
        assert_eq!(get(LABEL_CREATED_AT).as_deref(), Some("42"));
        assert!(local_pid_alive(std::process::id()));
    }

    /// A labelled container of a dead, old owner is removed by the sweep and
    /// one of a live owner is left alone.
    #[tokio::test]
    async fn sweep_removes_dead_owner_container_and_keeps_live_one() {
        let Ok(docker) = Docker::connect_with_defaults() else {
            return;
        };
        if docker.ping().await.is_err() {
            eprintln!("skipping: Docker is not available");
            return;
        }
        let now = unix_now();
        let stale = (now - DEAD_OWNER_GRACE_SECS - 60).to_string();
        let start = |pid: String, created: String| async move {
            GenericImage::new("alpine", "3")
                .with_cmd(["sleep", "300"])
                .with_labels([
                    (LABEL_TEST.to_string(), "true".to_string()),
                    (LABEL_PID.to_string(), pid),
                    (
                        LABEL_NONCE.to_string(),
                        "unit-test-other-process".to_string(),
                    ),
                    (LABEL_HOST.to_string(), host_name()),
                    (LABEL_CREATED_AT.to_string(), created),
                ])
                .start()
                .await
        };
        // Pid beyond any real pid_max is never alive.
        let dead = match start("2147483000".to_string(), stale.clone()).await {
            Ok(c) => c,
            Err(error) => {
                eprintln!("skipping: could not start alpine: {error}");
                return;
            }
        };
        let live = start(std::process::id().to_string(), stale)
            .await
            .expect("start live");
        let dead_id = dead.id().to_string();

        reap_stale(&docker, now).await;

        assert!(
            docker.inspect_container(&dead_id, None).await.is_err(),
            "dead owner's container must be gone"
        );
        assert!(
            docker.inspect_container(live.id(), None).await.is_ok(),
            "live owner's container must stay"
        );
        drop(live);
        std::mem::forget(dead);
    }
}
