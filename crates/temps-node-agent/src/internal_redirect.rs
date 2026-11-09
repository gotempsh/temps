// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The nftables redirect of containers' port-80 calls to the internal proxy's
//! own port (ADR-098 D2).
//!
//! Containers call `http://<environment>.<project>.temps.local`, which
//! resolves to the overlay bridge's address, port 80. Port 80 of a worker
//! belongs to whatever the operator runs there, so the proxy listens on
//! [`crate::internal_routing::INTERNAL_PROXY_PORT`] of the bridge address
//! instead, and one rule in a table of its own rewrites the destination of
//! exactly those calls:
//!
//! - addressed to the bridge's own address, TCP port 80, from any interface
//!   (a container's call arrives on whichever bridge its network uses);
//! - while the bridge address is in the `agent_alive` set, whose element
//!   expires [`LIVENESS_TIMEOUT`] after the agent last replaced the table, so
//!   an agent that stops refreshing it stops redirecting by itself.
//!
//! Host-originated traffic takes the `output` hook and is never matched. The
//! chain runs at nat `prerouting` priority [`PRIORITY`], ahead of Docker's
//! `dstnat` (-100), and the kernel applies the first NAT mapping a
//! connection gets, so a Docker DNAT for a published port 80 cannot take
//! these calls. The table is replaced in one nft transaction: a failed
//! refresh leaves the last installed rules in place.

use std::net::{IpAddr, Ipv4Addr};
use std::process::Stdio;
use std::time::Duration;

use futures::future::BoxFuture;
use thiserror::Error;
use tokio::io::AsyncWriteExt;

/// The table holding the redirect. Nothing else lives in it.
pub const TABLE: &str = "temps_internal_proxy";

/// nat `prerouting` priority: before Docker's `dstnat` chains (-100).
pub const PRIORITY: i32 = -110;

/// Marks the rule as Temps', for anyone reading `nft list ruleset`.
pub const RULE_COMMENT: &str = "temps-internal-proxy-redirect-v1";

/// The set whose element keeps the rule matching.
pub const LIVENESS_SET: &str = "agent_alive";

/// How long the rule keeps matching after the last refresh: three refresh
/// intervals, so two missed passes do not interrupt routing.
pub const LIVENESS_TIMEOUT: Duration = Duration::from_secs(90);

/// The port containers call.
pub const CONTAINER_HTTP_PORT: u16 = 80;

/// Longest an nft invocation may take before it is killed.
const NFT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RedirectError {
    #[error(
        "the overlay bridge address {address} is not IPv4; the internal proxy's redirect \
         covers IPv4 only"
    )]
    NotIpv4 { address: IpAddr },
    #[error(
        "nft (nftables) is not installed on this worker. Install it (`apt install nftables` or \
         `dnf install nftables`); the agent retries every 30 s"
    )]
    NftMissing,
    #[error(
        "the agent may not change nftables rules while trying to {operation} ({reason}): it \
         needs root or CAP_NET_ADMIN. Run the agent service as root; it retries every 30 s"
    )]
    PermissionDenied {
        operation: &'static str,
        reason: String,
    },
    #[error("could not run nft to {operation}: {reason}")]
    Io {
        operation: &'static str,
        reason: String,
    },
    #[error(
        "nft refused to {operation} (exit {exit_code}): {reason}. The previous rules, if any, \
         are unchanged; the agent retries every 30 s"
    )]
    Rejected {
        operation: &'static str,
        exit_code: i32,
        reason: String,
    },
    #[error("nft did not {operation} within {timeout_secs} s and was stopped")]
    Timeout {
        operation: &'static str,
        timeout_secs: u64,
    },
}

/// One node's redirect: port 80 of `address` to `address:proxy_port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    address: Ipv4Addr,
    proxy_port: u16,
    liveness: Duration,
}

impl Redirect {
    /// The bridge address must be IPv4: the internal names are.
    pub fn new(address: IpAddr, proxy_port: u16) -> Result<Self, RedirectError> {
        let IpAddr::V4(address) = address else {
            return Err(RedirectError::NotIpv4 { address });
        };
        Ok(Self {
            address,
            proxy_port,
            liveness: LIVENESS_TIMEOUT,
        })
    }

    /// Replace the liveness timeout (tests: watch the rule expire).
    pub fn with_liveness(mut self, liveness: Duration) -> Self {
        self.liveness = liveness;
        self
    }

    pub fn address(&self) -> Ipv4Addr {
        self.address
    }

    pub fn proxy_port(&self) -> u16 {
        self.proxy_port
    }

    /// The complete table, as one nft transaction. `add` before `delete`
    /// makes the delete valid on the first install too.
    pub fn ruleset(&self) -> String {
        let Self {
            address,
            proxy_port,
            liveness,
        } = self;
        let liveness = liveness.as_secs().max(1);
        format!(
            "add table ip {TABLE}\n\
             delete table ip {TABLE}\n\
             add table ip {TABLE}\n\
             add set ip {TABLE} {LIVENESS_SET} {{ type ipv4_addr; flags timeout; }}\n\
             add element ip {TABLE} {LIVENESS_SET} {{ {address} timeout {liveness}s }}\n\
             add chain ip {TABLE} prerouting {{ type nat hook prerouting priority {PRIORITY}; policy accept; }}\n\
             add rule ip {TABLE} prerouting ip daddr {address} tcp dport {CONTAINER_HTTP_PORT} ip daddr @{LIVENESS_SET} counter dnat to {address}:{proxy_port} comment \"{RULE_COMMENT}\"\n"
        )
    }
}

/// Removes the table. Idempotent: declaring it first makes the delete
/// succeed when it is absent.
pub fn removal_ruleset() -> String {
    format!("add table ip {TABLE}\ndelete table ip {TABLE}\n")
}

/// Applies an nft script. A trait so the supervisor can be tested without
/// touching the host's rules.
pub trait NftApply: Send + Sync + 'static {
    fn apply<'a>(
        &'a self,
        operation: &'static str,
        ruleset: &'a str,
    ) -> BoxFuture<'a, Result<(), RedirectError>>;
}

/// The host's `nft`, found on `PATH`.
#[derive(Debug, Clone)]
pub struct NftCommand {
    program: String,
}

impl Default for NftCommand {
    fn default() -> Self {
        Self {
            program: "nft".to_string(),
        }
    }
}

impl NftCommand {
    /// A specific binary (tests: a program that does not exist).
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl NftApply for NftCommand {
    fn apply<'a>(
        &'a self,
        operation: &'static str,
        ruleset: &'a str,
    ) -> BoxFuture<'a, Result<(), RedirectError>> {
        Box::pin(async move {
            match tokio::time::timeout(NFT_TIMEOUT, run_nft(&self.program, operation, ruleset))
                .await
            {
                Ok(result) => result,
                Err(_) => Err(RedirectError::Timeout {
                    operation,
                    timeout_secs: NFT_TIMEOUT.as_secs(),
                }),
            }
        })
    }
}

async fn run_nft(
    program: &str,
    operation: &'static str,
    ruleset: &str,
) -> Result<(), RedirectError> {
    let mut child = tokio::process::Command::new(program)
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // The timeout drops this future; the process must not outlive it.
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => RedirectError::NftMissing,
            std::io::ErrorKind::PermissionDenied => RedirectError::PermissionDenied {
                operation,
                reason: error.to_string(),
            },
            _ => RedirectError::Io {
                operation,
                reason: format!("starting {program}: {error}"),
            },
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| RedirectError::Io {
        operation,
        reason: "nft did not expose its standard input".to_string(),
    })?;
    stdin
        .write_all(ruleset.as_bytes())
        .await
        .map_err(|error| RedirectError::Io {
            operation,
            reason: format!("writing the ruleset to nft: {error}"),
        })?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| RedirectError::Io {
            operation,
            reason: format!("waiting for nft: {error}"),
        })?;
    if output.status.success() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(classify_failure(
        operation,
        output.status.code().unwrap_or(-1),
        reason,
    ))
}

/// nft reports a missing capability as a rejected script; say what it is.
fn classify_failure(operation: &'static str, exit_code: i32, reason: String) -> RedirectError {
    if reason.contains("Operation not permitted") {
        RedirectError::PermissionDenied { operation, reason }
    } else {
        RedirectError::Rejected {
            operation,
            exit_code,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redirect() -> Redirect {
        Redirect::new("10.205.0.1".parse().unwrap(), 591).unwrap()
    }

    /// The ruleset ADR-098 D2 documents, byte for byte.
    #[test]
    fn the_ruleset_is_the_one_the_adr_documents() {
        assert_eq!(
            redirect().ruleset(),
            "add table ip temps_internal_proxy\n\
             delete table ip temps_internal_proxy\n\
             add table ip temps_internal_proxy\n\
             add set ip temps_internal_proxy agent_alive { type ipv4_addr; flags timeout; }\n\
             add element ip temps_internal_proxy agent_alive { 10.205.0.1 timeout 90s }\n\
             add chain ip temps_internal_proxy prerouting { type nat hook prerouting priority -110; policy accept; }\n\
             add rule ip temps_internal_proxy prerouting ip daddr 10.205.0.1 tcp dport 80 ip daddr @agent_alive counter dnat to 10.205.0.1:591 comment \"temps-internal-proxy-redirect-v1\"\n"
        );
    }

    #[test]
    fn the_rule_is_scoped_to_the_bridge_address_and_port_80() {
        let ruleset = redirect().ruleset();
        let rules: Vec<&str> = ruleset
            .lines()
            .filter(|line| line.starts_with("add rule"))
            .collect();
        assert_eq!(rules.len(), 1, "exactly one rule: {ruleset}");
        let rule = rules[0];
        for needle in [
            "ip daddr 10.205.0.1",
            "tcp dport 80",
            "ip daddr @agent_alive",
            "dnat to 10.205.0.1:591",
        ] {
            assert!(rule.contains(needle), "{needle} missing from {rule}");
        }
        // Any interface: containers on a network other than the overlay
        // call the same address (ADR-098 D2).
        assert!(!rule.contains("iifname"), "{rule}");
        // Prerouting only: host-originated traffic (output) is never seen.
        assert!(!ruleset.contains("hook output"));
        assert!(ruleset.contains("hook prerouting priority -110"));
        // Must run before Docker's dstnat chains.
        const { assert!(PRIORITY < -100) };
        // Its own IPv4 table; nothing wider.
        assert!(!ruleset.contains("inet "));
        assert!(!ruleset.contains("ip6"));
        assert!(!ruleset.contains("0.0.0.0"));
        assert!(!ruleset.contains("masquerade"));
        assert!(!ruleset.contains("flush"));
        // Replaced in one transaction: declared, deleted, recreated.
        let lines: Vec<&str> = ruleset.lines().collect();
        assert_eq!(
            &lines[..3],
            [
                "add table ip temps_internal_proxy",
                "delete table ip temps_internal_proxy",
                "add table ip temps_internal_proxy",
            ]
        );
    }

    #[test]
    fn liveness_is_whole_seconds_and_never_zero() {
        let short = redirect()
            .with_liveness(Duration::from_millis(10))
            .ruleset();
        assert!(short.contains("{ 10.205.0.1 timeout 1s }"), "{short}");
        let custom = redirect().with_liveness(Duration::from_secs(3)).ruleset();
        assert!(custom.contains("timeout 3s"), "{custom}");
    }

    #[test]
    fn removal_only_touches_its_own_table() {
        assert_eq!(
            removal_ruleset(),
            "add table ip temps_internal_proxy\ndelete table ip temps_internal_proxy\n"
        );
    }

    #[test]
    fn an_ipv6_bridge_address_is_refused() {
        let v6 = Redirect::new("fd00::1".parse().unwrap(), 591).unwrap_err();
        assert_eq!(
            v6,
            RedirectError::NotIpv4 {
                address: "fd00::1".parse().unwrap()
            }
        );
    }

    #[test]
    fn a_missing_permission_is_told_apart_from_a_rejected_ruleset() {
        let denied = classify_failure(
            "install the internal proxy's redirect",
            1,
            "Error: Could not process rule: Operation not permitted".into(),
        );
        assert!(matches!(denied, RedirectError::PermissionDenied { .. }));
        assert!(denied.to_string().contains("CAP_NET_ADMIN"));
        let rejected = classify_failure("install", 1, "Error: syntax error".into());
        assert!(matches!(
            rejected,
            RedirectError::Rejected { exit_code: 1, .. }
        ));
        assert!(rejected
            .to_string()
            .contains("previous rules, if any, are unchanged"));
    }

    #[tokio::test]
    async fn a_missing_nft_binary_says_to_install_nftables() {
        let nft = NftCommand::with_program("/nonexistent/temps-test-nft");
        let error = nft
            .apply("install", &redirect().ruleset())
            .await
            .unwrap_err();
        assert_eq!(error, RedirectError::NftMissing);
        assert!(error.to_string().contains("apt install nftables"));
    }

    /// The redirect against a real kernel, entirely inside a private network
    /// namespace: the host's rules and interfaces are never touched. Runs
    /// only as root on Linux with `nft`, `ip`, `unshare` and `nsenter`
    /// (CI's Linux jobs run unprivileged and skip it).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_redirect_works_against_a_real_kernel_in_a_private_netns() {
        let outcome = std::thread::spawn(netns::run)
            .join()
            .expect("netns test thread");
        match outcome {
            Ok(()) => {}
            Err(netns::Skip(reason)) => eprintln!("skipping the nftables netns test: {reason}"),
        }
    }

    #[cfg(target_os = "linux")]
    mod netns {
        use super::*;
        use std::io::{Read, Write};
        use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
        use std::os::fd::AsRawFd;
        use std::process::{Child, Command};

        pub struct Skip(pub String);

        const BRIDGE: &str = "br-t98";
        const OTHER: &str = "br-t98o";
        const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 205, 0, 1);

        fn sh(program: &str, args: &[&str]) {
            let output = Command::new(program).args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{program} {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        /// Not on `PATH`: spawning it fails, whatever its exit status.
        fn tool_missing(program: &str) -> bool {
            Command::new(program).arg("-h").output().is_err()
        }

        /// A process holding a fresh network namespace: a "container".
        struct Container(Child);

        impl Container {
            fn new() -> Self {
                Self(
                    Command::new("unshare")
                        .args(["--net", "sleep", "120"])
                        .spawn()
                        .unwrap(),
                )
            }
            fn pid(&self) -> String {
                self.0.id().to_string()
            }
            fn run(&self, args: &[&str]) {
                let pid = self.pid();
                let mut all = vec!["-t", pid.as_str(), "-n"];
                all.extend_from_slice(args);
                // `unshare` may not have switched namespaces yet.
                for _ in 0..50 {
                    if Command::new("nsenter")
                        .args(&all)
                        .output()
                        .unwrap()
                        .status
                        .success()
                    {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                sh("nsenter", &all);
            }
            /// Connect from inside the container and read the greeting.
            fn call(&self, to: SocketAddr) -> String {
                let path = format!("/proc/{}/ns/net", self.0.id());
                std::thread::spawn(move || {
                    let ns = std::fs::File::open(path).unwrap();
                    // SAFETY: `setns` on this short-lived thread only; the fd
                    // is valid for the call.
                    let rc = unsafe { libc::setns(ns.as_raw_fd(), libc::CLONE_NEWNET) };
                    assert_eq!(rc, 0, "setns: {}", std::io::Error::last_os_error());
                    greeting(to)
                })
                .join()
                .unwrap()
            }
        }

        impl Drop for Container {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn greeting(to: SocketAddr) -> String {
            match TcpStream::connect_timeout(&to, Duration::from_secs(2)) {
                Ok(mut stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut text = String::new();
                    let _ = stream.read_to_string(&mut text);
                    text
                }
                Err(error) => format!("error: {}", error.kind()),
            }
        }

        /// Answers every connection with `name` and the caller's address.
        fn serve(addr: &str, name: &'static str) {
            let listener = TcpListener::bind(addr).unwrap();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let peer = stream
                        .peer_addr()
                        .map(|p| p.ip().to_string())
                        .unwrap_or_default();
                    let _ = write!(stream, "{name} {peer}");
                }
            });
        }

        fn apply(script: &str) -> Result<(), RedirectError> {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(NftCommand::default().apply("test", script))
        }

        pub fn run() -> Result<(), Skip> {
            // SAFETY: plain syscall; checked below.
            if unsafe { libc::geteuid() } != 0 {
                return Err(Skip("not running as root".into()));
            }
            for tool in ["ip", "unshare", "nsenter"] {
                if tool_missing(tool) {
                    return Err(Skip(format!("{tool} is not installed")));
                }
            }
            // From here on this thread, and every command it starts, is in a
            // namespace of its own. Nothing below can reach the host's.
            // SAFETY: affects this thread only.
            if unsafe { libc::unshare(libc::CLONE_NEWNET) } != 0 {
                return Err(Skip(format!(
                    "cannot create a network namespace: {}",
                    std::io::Error::last_os_error()
                )));
            }
            if Command::new("nft")
                .args(["list", "ruleset"])
                .output()
                .map(|o| !o.status.success())
                .unwrap_or(true)
            {
                return Err(Skip("nft is not installed or cannot run here".into()));
            }

            sh("ip", &["link", "set", "lo", "up"]);
            for (bridge, cidr) in [(BRIDGE, "10.205.0.1/24"), (OTHER, "10.99.0.1/24")] {
                sh("ip", &["link", "add", bridge, "type", "bridge"]);
                sh("ip", &["addr", "add", cidr, "dev", bridge]);
                sh("ip", &["link", "set", bridge, "up"]);
            }
            // `app` sits on the overlay bridge; `other` on another bridge (a
            // container whose overlay attach failed), routed to the gateway
            // address through the host.
            let app = Container::new();
            let other = Container::new();
            for (container, bridge, host_end, inner, addr) in [
                (&app, BRIDGE, "t98h0", "t98c0", "10.205.0.2/24"),
                (&other, OTHER, "t98h1", "t98c1", "10.99.0.2/24"),
            ] {
                sh(
                    "ip",
                    &[
                        "link", "add", host_end, "type", "veth", "peer", "name", inner,
                    ],
                );
                sh("ip", &["link", "set", host_end, "master", bridge, "up"]);
                container.run(&["ip", "link", "set", "lo", "up"]);
                sh("ip", &["link", "set", inner, "netns", &container.pid()]);
                container.run(&["ip", "addr", "add", addr, "dev", inner]);
                container.run(&["ip", "link", "set", inner, "up"]);
            }
            other.run(&["ip", "route", "add", "default", "via", "10.99.0.1"]);

            // The operator's web server owns port 80 of every address.
            serve("0.0.0.0:80", "operator");
            serve("10.205.0.1:591", "temps");
            serve("10.205.0.1:8080", "plain");
            serve("10.205.0.1:8099", "docker");
            // Docker's DNAT for a container publishing port 80, at dstnat.
            apply(
                "add table ip t98_docker\n\
                 add chain ip t98_docker prerouting { type nat hook prerouting priority -100; policy accept; }\n\
                 add rule ip t98_docker prerouting fib daddr type local tcp dport 80 dnat to 10.205.0.1:8099\n",
            )
            .unwrap();

            let port80 = SocketAddr::from((GATEWAY, 80));
            assert_eq!(app.call(port80), "docker 10.205.0.2", "before the redirect");
            assert_eq!(
                other.call(port80),
                "docker 10.99.0.2",
                "before the redirect"
            );

            let rule = Redirect::new(GATEWAY.into(), 591)
                .unwrap()
                .with_liveness(Duration::from_secs(3));
            apply(&rule.ruleset()).expect("install");
            apply(&rule.ruleset()).expect("replace: idempotent");

            // Redirected ahead of Docker's DNAT, source address preserved.
            assert_eq!(app.call(port80), "temps 10.205.0.2");
            // Other ports of the bridge address: untouched.
            assert_eq!(
                app.call(SocketAddr::from((GATEWAY, 8080))),
                "plain 10.205.0.2"
            );
            // From another bridge too, with its source address.
            assert_eq!(other.call(port80), "temps 10.99.0.2");
            // Port 80 of another host address: untouched.
            assert_eq!(
                other.call(SocketAddr::from((Ipv4Addr::new(10, 99, 0, 1), 80))),
                "docker 10.99.0.2"
            );
            // The host's own calls never pass prerouting.
            assert_eq!(greeting(port80), "operator 10.205.0.1");

            // A refused refresh leaves the installed rules in place.
            let broken = format!(
                "{}add rule ip {TABLE} prerouting this is not nft\n",
                rule.ruleset()
            );
            assert!(matches!(
                apply(&broken),
                Err(RedirectError::Rejected { .. })
            ));
            assert_eq!(
                app.call(port80),
                "temps 10.205.0.2",
                "kept after a failed refresh"
            );

            // No refresh: the liveness element expires and the rule stops.
            std::thread::sleep(Duration::from_secs(4));
            assert_eq!(app.call(port80), "docker 10.205.0.2", "expired");
            apply(&rule.ruleset()).expect("refresh");
            assert_eq!(app.call(port80), "temps 10.205.0.2", "renewed");

            // Removal deletes only this table, and is idempotent.
            apply(&removal_ruleset()).expect("remove");
            apply(&removal_ruleset()).expect("remove again");
            assert_eq!(app.call(port80), "docker 10.205.0.2", "removed");
            let listed = Command::new("nft")
                .args(["list", "tables"])
                .output()
                .unwrap();
            let tables = String::from_utf8_lossy(&listed.stdout);
            assert!(!tables.contains(TABLE), "{tables}");
            assert!(tables.contains("t98_docker"), "{tables}");
            Ok(())
        }
    }
}
