// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network policy enforced on real microVMs (ADR-055 §9, §17).
//!
//! A microVM's traffic never reaches the host's nftables: it leaves through
//! the SDK's user-space network stack, which enforces its own policy. These
//! tests boot real VMs with policies produced by
//! `temps_workload_netpol::to_sdk_policy` and prove the translation keeps
//! the semantics the nftables backend has (`temps-workload-netpol`'s
//! `docker_vm_e2e`): restricted egress reaches only what a rule allows, a
//! deny beats an allow, `none` reaches nothing, and the host always gets in.
//!
//! The host side is two TCP listeners on loopback, which a guest reaches
//! through its gateway (`host.microsandbox.internal`); each greets with
//! `ok-<port>`, so a probe that prints the greeting really connected.
//!
//! The translation is tested on a `SingleTenant` backend, which enforces
//! only the policy it is given. Temps runs sandboxes `MultiTenant`, where
//! the SDK adds a public-only egress floor of its own: a guest never
//! reaches the host or a private range, whatever the policy allows. One
//! test pins that floor, since it is what keeps a sandbox off the host.
//!
//! Skips at runtime, printing why, without a hypervisor or a runtime. The
//! runtime lives in a short test-only home under `/tmp`, never the server's
//! data dir; set `TEMPS_E2E_MICROSANDBOX_INSTALL=1` to download
//! the pinned runtime there on first run.

#![cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]
#![allow(
    clippy::disallowed_methods,
    reason = "integration test: reads fixture configuration from the environment"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use microsandbox::sandbox::DeploymentProfile;
use microsandbox::setup::InstallOptions;
use microsandbox::{Backend, LocalBackend, NetworkPolicy, Sandbox};
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::{
    to_sdk_policy, Action, Direction, IdentityMap, L4Protocol, Origin, Peer, PortRange, Profile,
    Rule, WorkloadPolicy,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

const IMAGE: &str = "alpine:3.20";
const HOST: &str = "host.microsandbox.internal";
/// Guest port the ingress test listens on.
const GUEST_PORT: u16 = 8080;

/// One VM at a time: each boots a kernel, and a loaded host times out
/// probes when several do at once.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A short, test-only home. The runtime derives Unix socket paths from it,
/// and macOS caps those at 104 bytes, which a path under the target
/// directory of a worktree already exceeds.
fn home() -> PathBuf {
    PathBuf::from("/tmp").join(format!("temps-msb-e2e-{}", whoami()))
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".to_string())
}

fn hypervisor() -> Result<(), String> {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "kern.hv_support"])
            .output()
            .map_err(|e| format!("sysctl kern.hv_support: {e}"))?;
        if String::from_utf8_lossy(&out.stdout).trim() == "1" {
            return Ok(());
        }
        return Err("Hypervisor.framework is not supported on this Mac".to_string());
    }
    if std::path::Path::new("/dev/kvm").exists() {
        Ok(())
    } else {
        Err("/dev/kvm is missing".to_string())
    }
}

/// A backend with a runtime, or `None` after printing why the test skips.
async fn backend(profile: DeploymentProfile) -> Option<Arc<dyn Backend>> {
    if let Err(reason) = hypervisor() {
        println!("no hypervisor, skipping: {reason}");
        return None;
    }
    let home = home();
    std::fs::create_dir_all(&home).expect("create the test runtime home");
    let backend = LocalBackend::builder()
        .home(&home)
        .config_path(home.join("config.json"))
        .deployment_profile(profile)
        .build_lazy()
        .expect("build the local backend");
    if microsandbox::setup::resolve_runtime(backend.config()).is_err() {
        if std::env::var_os("TEMPS_E2E_MICROSANDBOX_INSTALL").is_none() {
            println!(
                "no microsandbox runtime under {}, skipping; set \
                 TEMPS_E2E_MICROSANDBOX_INSTALL=1 to install the pinned one there",
                home.display()
            );
            return None;
        }
        microsandbox::setup::ensure_runtime(backend.config(), InstallOptions::default())
            .await
            .expect("install the pinned microsandbox runtime");
    }
    Some(Arc::new(backend) as Arc<dyn Backend>)
}

/// Loopback listeners that greet with `ok-<port>`.
async fn greeters(count: usize) -> Vec<u16> {
    let mut ports = Vec::new();
    for _ in 0..count {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a host listener");
        let port = listener.local_addr().expect("listener address").port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.write_all(format!("ok-{port}\n").as_bytes()).await;
            }
        });
        ports.push(port);
    }
    ports
}

fn subject() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Sandbox, 1)
}

fn rule(id: i64, direction: Direction, cidr: &str, port: Option<u16>, action: Action) -> Rule {
    Rule {
        id,
        direction,
        peer: Peer::Cidr(cidr.parse().expect("valid CIDR")),
        ports: port.map(PortRange::single),
        protocol: if port.is_some() {
            L4Protocol::Tcp
        } else {
            L4Protocol::Any
        },
        action,
        origin: Origin::Operator,
    }
}

fn sdk(policy: &WorkloadPolicy) -> NetworkPolicy {
    let translated = to_sdk_policy(policy, &IdentityMap::new());
    let json = serde_json::to_value(&translated.policy).expect("serialize the policy");
    println!("policy: {json}");
    serde_json::from_value(json).expect("the SDK reads the translated policy")
}

/// Boots one VM with `policy`, runs `body`, and destroys the VM even when
/// `body` panics.
async fn with_vm<F, Fut>(
    profile: DeploymentProfile,
    name: &str,
    policy: NetworkPolicy,
    publish: Option<(u16, u16)>,
    body: F,
) where
    F: FnOnce(Sandbox) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures::FutureExt;
    let Some(backend) = backend(profile).await else {
        return;
    };
    let _serial = SERIAL.lock().await;
    let mut builder = Sandbox::builder(name)
        .image(IMAGE)
        .cpus(1)
        .memory(256)
        .network(|n| n.policy(policy))
        .replace();
    if let Some((host, guest)) = publish {
        builder = builder.port(host, guest);
    }
    let sandbox = microsandbox::with_backend(backend.clone(), builder.create())
        .await
        .unwrap_or_else(|e| panic!("boot {name}: {e}"));
    let outcome = std::panic::AssertUnwindSafe(body(sandbox.clone()))
        .catch_unwind()
        .await;
    if let Err(e) = microsandbox::with_backend(backend, sandbox.destroy()).await {
        println!("destroy {name}: {e}");
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// Whether the guest read the greeting of `host:port`.
async fn reaches(sandbox: &Sandbox, host: &str, port: u16) -> bool {
    // stdin stays open long enough to read the greeting; busybox nc exits
    // at stdin EOF.
    let script = format!("sleep 2 | nc -w 3 {host} {port} 2>&1; true");
    let out = sandbox
        .shell(script.clone())
        .await
        .unwrap_or_else(|e| panic!("exec `{script}`: {e}"));
    let stdout = out.stdout().unwrap_or_default();
    println!("{host}:{port} -> {}", stdout.trim());
    stdout.contains(&format!("ok-{port}"))
}

fn unique(prefix: &str) -> String {
    format!("temps-netpol-{prefix}-{}", std::process::id())
}

#[tokio::test]
async fn restricted_egress_reaches_only_the_allowed_port() {
    let ports = greeters(2).await;
    let (allowed, other) = (ports[0], ports[1]);
    let policy = WorkloadPolicy {
        subject: subject(),
        egress: Profile::Restricted,
        ingress: Profile::Open,
        rules: vec![rule(
            1,
            Direction::Egress,
            "0.0.0.0/0",
            Some(allowed),
            Action::Allow,
        )],
    };
    with_vm(
        DeploymentProfile::SingleTenant,
        &unique("restricted"),
        sdk(&policy),
        None,
        |vm| async move {
            assert!(
                reaches(&vm, HOST, allowed).await,
                "the allowed port must be reachable"
            );
            assert!(
                !reaches(&vm, HOST, other).await,
                "a port no rule allows must not be"
            );
            // Gateway DNS survives restricted egress, so peers resolve by name.
            let dns = vm
                .shell(format!("getent hosts {HOST} || nslookup {HOST}"))
                .await
                .expect("exec name lookup");
            assert_eq!(
                dns.status().code,
                0,
                "{HOST} must resolve: {:?}",
                dns.stdout()
            );
        },
    )
    .await;
}

#[tokio::test]
async fn a_deny_beats_an_allow_under_open_egress() {
    let ports = greeters(2).await;
    let (open, denied) = (ports[0], ports[1]);
    let policy = WorkloadPolicy {
        subject: subject(),
        egress: Profile::Open,
        ingress: Profile::Open,
        rules: vec![rule(
            1,
            Direction::Egress,
            "0.0.0.0/0",
            Some(denied),
            Action::Deny,
        )],
    };
    with_vm(
        DeploymentProfile::SingleTenant,
        &unique("deny"),
        sdk(&policy),
        None,
        |vm| async move {
            assert!(
                reaches(&vm, HOST, open).await,
                "open egress reaches the host"
            );
            assert!(!reaches(&vm, HOST, denied).await, "the deny must win");
        },
    )
    .await;
}

#[tokio::test]
async fn none_egress_reaches_nothing() {
    let ports = greeters(1).await;
    let policy = WorkloadPolicy {
        subject: subject(),
        egress: Profile::None,
        ingress: Profile::Open,
        // An allow under `none` has no effect.
        rules: vec![rule(
            1,
            Direction::Egress,
            "0.0.0.0/0",
            Some(ports[0]),
            Action::Allow,
        )],
    };
    with_vm(
        DeploymentProfile::SingleTenant,
        &unique("none"),
        sdk(&policy),
        None,
        |vm| async move {
            assert!(
                !reaches(&vm, HOST, ports[0]).await,
                "none must reach nothing"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn the_host_gets_in_through_isolation_and_a_deny() {
    let host_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("pick a free port");
        probe.local_addr().expect("probe address").port()
    };
    let policy = WorkloadPolicy {
        subject: subject(),
        egress: Profile::Open,
        ingress: Profile::Restricted,
        rules: vec![rule(1, Direction::Ingress, "0.0.0.0/0", None, Action::Deny)],
    };
    with_vm(
        DeploymentProfile::SingleTenant,
        &unique("ingress"),
        sdk(&policy),
        Some((host_port, GUEST_PORT)),
        |vm| async move {
            vm.shell(format!(
                "nohup sh -c 'while true; do echo ok-{GUEST_PORT} | nc -l -p {GUEST_PORT}; done' \
                 >/dev/null 2>&1 &"
            ))
            .await
            .expect("start the guest listener");
            let mut greeting = String::new();
            for _ in 0..20 {
                if let Ok(Ok(mut stream)) = tokio::time::timeout(
                    Duration::from_secs(2),
                    tokio::net::TcpStream::connect(("127.0.0.1", host_port)),
                )
                .await
                {
                    use tokio::io::AsyncReadExt;
                    let mut buf = vec![0u8; 64];
                    if let Ok(Ok(n)) =
                        tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await
                    {
                        greeting = String::from_utf8_lossy(&buf[..n]).into_owned();
                        if greeting.contains("ok-") {
                            break;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            assert!(
                greeting.contains(&format!("ok-{GUEST_PORT}")),
                "the host must reach an ingress-isolated VM whatever its denies, got {greeting:?}"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn multi_tenant_floor_keeps_open_egress_off_the_host() {
    let ports = greeters(1).await;
    let policy = WorkloadPolicy {
        subject: subject(),
        egress: Profile::Open,
        ingress: Profile::Open,
        rules: Vec::new(),
    };
    with_vm(
        DeploymentProfile::MultiTenant,
        &unique("floor"),
        sdk(&policy),
        None,
        |vm| async move {
            assert!(
                !reaches(&vm, HOST, ports[0]).await,
                "the multi-tenant floor must keep a sandbox off the host"
            );
        },
    )
    .await;
}
