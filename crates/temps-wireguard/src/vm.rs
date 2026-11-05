// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The mesh data plane of a macOS worker (ADR 090).
//!
//! On macOS every container runs in a Linux VM owned by the Docker app
//! (Docker Desktop, OrbStack, colima, Rancher Desktop). Container addresses,
//! published ports and the bridges between them live in that VM, so the
//! WireGuard interface that carries the mesh has to live there too: the Mac
//! itself has no route to any of them.
//!
//! The agent runs natively on the Mac and drives one small container in the
//! VM, the *mesh sidecar* ([`SIDECAR_CONTAINER`]), in the VM's host network
//! namespace with `CAP_NET_ADMIN` only. Every operation that Linux performs
//! in-process on its own kernel ([`crate::mesh`]: create the interface, set
//! its key and address, reconcile peers, read handshakes) is performed here
//! by running `ip` and `wg` in that sidecar through `docker exec`.
//!
//! The private key is passed on standard input, never on a command line: a
//! command line is visible to every process on the Mac (`ps`) and in the VM.
//!
//! Everything that parses or plans is plain code, compiled and tested on
//! every platform; only [`run`] touches a Docker daemon.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use crate::mesh::{MeshPeerStatus, MESH_INTERFACE};
use crate::WireGuardError;

/// The container in the Docker app's VM that holds this worker's end of the
/// mesh. One per VM: a Mac joins one control plane.
pub const SIDECAR_CONTAINER: &str = "temps-mac-mesh";

/// Where the agent reaches the Docker app and with which CLI. Pinned by
/// `temps join` (the Docker context current at join time) so a later
/// `docker context use` elsewhere never moves a running worker to another VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerEndpoint {
    /// The `docker` binary to run (absolute path: launchd's PATH is minimal).
    pub cli: String,
    /// The daemon, as `docker --host` takes it (`unix:///…/docker.sock`).
    pub host: String,
}

static ENDPOINT: OnceLock<DockerEndpoint> = OnceLock::new();

/// Pin the Docker endpoint every later VM operation uses. The first call
/// wins; later calls with the same endpoint are no-ops and a different one
/// is refused, so one process never talks to two VMs.
pub fn set_docker_endpoint(endpoint: DockerEndpoint) -> Result<(), WireGuardError> {
    let held = ENDPOINT.get_or_init(|| endpoint.clone());
    if *held != endpoint {
        return Err(WireGuardError::InvalidConfig(format!(
            "this process already drives the Docker VM at {} and cannot switch to {}",
            held.host, endpoint.host
        )));
    }
    Ok(())
}

/// The pinned endpoint.
pub fn docker_endpoint() -> Result<&'static DockerEndpoint, WireGuardError> {
    ENDPOINT.get().ok_or_else(|| {
        WireGuardError::InvalidConfig(
            "no Docker endpoint is pinned for the mesh sidecar; run `temps join` on this Mac \
             again, which records the Docker app it uses"
                .to_string(),
        )
    })
}

/// Longest any one sidecar command may take. A VM that is paused (the Mac
/// is asleep) or restarting fails the tick instead of hanging it.
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(30);

/// The `docker` arguments that run `argv` in the sidecar.
pub fn exec_args(host: &str, argv: &[&str], stdin: bool) -> Vec<String> {
    let mut args = vec!["--host".to_string(), host.to_string(), "exec".to_string()];
    if stdin {
        args.push("-i".to_string());
    }
    args.push(SIDECAR_CONTAINER.to_string());
    args.extend(argv.iter().map(|arg| arg.to_string()));
    args
}

/// Run `command` to completion, feeding `stdin` when given, within
/// `timeout`. Both output pipes are drained while it runs, so a command that
/// prints more than a pipe holds (a `wg show dump` of a large mesh, a build)
/// cannot block on a full pipe. `Ok(None)` when it was killed at the deadline.
pub fn output_within(
    mut command: Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> std::io::Result<Option<Output>> {
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buffer);
            }
            buffer
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // Dropped at the end of this block: closes stdin so the command sees
        // the end of its input.
        if let Err(error) = pipe.write_all(input) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Some(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    }))
}

/// Run `argv` in the sidecar, feeding `stdin` when given, and return its
/// standard output. `operation` names what was attempted, for the error.
pub fn run(operation: &str, argv: &[&str], stdin: Option<&[u8]>) -> Result<String, WireGuardError> {
    let endpoint = docker_endpoint()?;
    let failed = |reason: String| WireGuardError::OperationFailed {
        operation: format!("{operation} (in the {SIDECAR_CONTAINER} container)"),
        reason,
    };
    let mut command = Command::new(&endpoint.cli);
    command.args(exec_args(&endpoint.host, argv, stdin.is_some()));
    let output = output_within(command, stdin, EXEC_TIMEOUT)
        .map_err(|error| failed(format!("cannot run {}: {error}", endpoint.cli)))?
        .ok_or_else(|| {
            failed(format!(
                "no answer within {}s: the Docker app's VM may be paused (the Mac is asleep) or \
                 restarting",
                EXEC_TIMEOUT.as_secs()
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = if stderr.contains("No such container") || stderr.contains("is not running") {
            format!(
                "the mesh sidecar is not running in the Docker app's VM ({}); `temps agent` \
                 starts it, check `temps agent service status`",
                stderr.trim()
            )
        } else {
            format!("exited with {}: {}", output.status, stderr.trim())
        };
        return Err(failed(reason));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One peer line of `wg show <iface> dump`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpPeer {
    pub public_key: String,
    pub endpoint: Option<SocketAddr>,
    pub allowed_ips: Vec<String>,
    pub latest_handshake: Option<SystemTime>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// `wg show <iface> dump`: the interface line, then one line per peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dump {
    /// `None` when the interface has no private key yet.
    pub private_key: Option<String>,
    pub listen_port: u16,
    pub peers: Vec<DumpPeer>,
}

impl Dump {
    pub fn peer_status(&self) -> Vec<MeshPeerStatus> {
        self.peers
            .iter()
            .map(|peer| MeshPeerStatus {
                public_key: peer.public_key.clone(),
                endpoint: peer.endpoint,
                last_handshake: peer.latest_handshake,
                rx_bytes: peer.rx_bytes,
                tx_bytes: peer.tx_bytes,
            })
            .collect()
    }
}

fn none_or<T>(value: &str, parse: impl FnOnce(&str) -> Option<T>) -> Option<T> {
    match value {
        "(none)" | "off" | "" => None,
        value => parse(value),
    }
}

/// Parse `wg show <iface> dump` (tab-separated; see wg(8)).
pub fn parse_dump(text: &str) -> Result<Dump, WireGuardError> {
    let bad = |line: &str, why: &str| {
        WireGuardError::InterfaceError(format!(
            "unexpected `wg show {MESH_INTERFACE} dump` output ({why}): {line:?}"
        ))
    };
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let head = lines.next().ok_or_else(|| bad("", "empty"))?;
    let fields: Vec<&str> = head.split('\t').collect();
    // private-key, public-key, listen-port, fwmark
    if fields.len() != 4 {
        return Err(bad(head, "the interface line needs 4 fields"));
    }
    let private_key = none_or(fields[0], |value| Some(value.to_string()));
    let listen_port = fields[2]
        .parse::<u16>()
        .map_err(|_| bad(head, "listen port"))?;
    let mut peers = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        // public-key, preshared-key, endpoint, allowed-ips, latest-handshake,
        // rx, tx, persistent-keepalive
        if fields.len() != 8 {
            return Err(bad(line, "a peer line needs 8 fields"));
        }
        let handshake: u64 = fields[4].parse().map_err(|_| bad(line, "handshake"))?;
        peers.push(DumpPeer {
            public_key: fields[0].to_string(),
            endpoint: none_or(fields[2], |value| value.parse().ok()),
            allowed_ips: none_or(fields[3], |value| {
                Some(value.split(',').map(|ip| ip.trim().to_string()).collect())
            })
            .unwrap_or_default(),
            latest_handshake: (handshake > 0)
                .then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(handshake)),
            rx_bytes: fields[5].parse().map_err(|_| bad(line, "rx bytes"))?,
            tx_bytes: fields[6].parse().map_err(|_| bad(line, "tx bytes"))?,
        });
    }
    Ok(Dump {
        private_key,
        listen_port,
        peers,
    })
}

/// The IPv4 addresses of `ip -4 -o addr show dev <iface>`, as `a.b.c.d/len`.
pub fn parse_addresses(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            words.find(|word| *word == "inet")?;
            words.next().map(str::to_string)
        })
        .collect()
}

/// Whether `ip -o link show dev <iface>` reports the link administratively up.
pub fn parse_link_up(text: &str) -> bool {
    text.split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .is_some_and(|(flags, _)| flags.split(',').any(|flag| flag == "UP"))
}

/// The script that (re)configures the interface from scratch, the way
/// `configure_interface` does on Linux: key, port, the one address, MTU,
/// no peers, up. The private key arrives on standard input.
///
/// Every interpolated value is a typed number or address, never caller text,
/// so the script cannot be turned into another command.
pub fn configure_script(address: Ipv4Addr, prefix_len: u8, listen_port: u16, mtu: u32) -> String {
    let iface = MESH_INTERFACE;
    format!(
        "set -e
ip link show dev {iface} >/dev/null 2>&1 || ip link add dev {iface} type wireguard
umask 077
key=$(mktemp)
trap 'rm -f \"$key\"' EXIT
cat > \"$key\"
wg set {iface} private-key \"$key\" listen-port {listen_port}
for peer in $(wg show {iface} peers); do wg set {iface} peer \"$peer\" remove; done
ip -4 addr flush dev {iface}
ip addr add {address}/{prefix_len} dev {iface}
ip link set dev {iface} mtu {mtu} up
"
    )
}

/// The `wg set` arguments that add or update one peer.
pub fn set_peer_args(
    public_key: &str,
    endpoint: Option<SocketAddr>,
    allowed_ips: &[String],
    keepalive: u16,
) -> Vec<String> {
    let mut args = vec![
        "wg".to_string(),
        "set".to_string(),
        MESH_INTERFACE.to_string(),
        "peer".to_string(),
        public_key.to_string(),
    ];
    if let Some(endpoint) = endpoint {
        args.push("endpoint".to_string());
        args.push(endpoint.to_string());
    }
    args.push("persistent-keepalive".to_string());
    args.push(keepalive.to_string());
    args.push("allowed-ips".to_string());
    args.push(allowed_ips.join(","));
    args
}

/// Peers by public key, as [`crate::mesh`] plans changes against.
pub fn peers_by_key(dump: &Dump) -> HashMap<String, &DumpPeer> {
    dump.peers
        .iter()
        .map(|peer| (peer.public_key.clone(), peer))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn output_within_drains_more_than_a_pipe_holds() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 300000 /dev/zero"]);
        let output = output_within(command, None, Duration::from_secs(20))
            .unwrap()
            .expect("finishes well before the deadline");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 300_000);
    }

    #[cfg(unix)]
    #[test]
    fn output_within_kills_a_command_at_the_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let started = std::time::Instant::now();
        assert!(output_within(command, None, Duration::from_millis(200))
            .unwrap()
            .is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn output_within_feeds_stdin() {
        let mut command = Command::new("cat");
        command.arg("-");
        let output = output_within(command, Some(b"key material"), Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert_eq!(output.stdout, b"key material");
    }

    const DUMP: &str = "cHJpdmF0ZQ==\tcHVibGlj\t51820\toff
QUFBQQ==\t(none)\t192.168.5.2:51820\t10.210.0.1/32\t1730800000\t1024\t2048\t25
QkJCQg==\t(none)\t(none)\t10.210.0.7/32,10.210.0.9/32\t0\t0\t0\toff
";

    #[test]
    fn parses_the_interface_and_every_peer() {
        let dump = parse_dump(DUMP).unwrap();
        assert_eq!(dump.private_key.as_deref(), Some("cHJpdmF0ZQ=="));
        assert_eq!(dump.listen_port, 51820);
        assert_eq!(dump.peers.len(), 2);
        let first = &dump.peers[0];
        assert_eq!(first.public_key, "QUFBQQ==");
        assert_eq!(first.endpoint, Some("192.168.5.2:51820".parse().unwrap()));
        assert_eq!(first.allowed_ips, vec!["10.210.0.1/32".to_string()]);
        assert_eq!(
            first.latest_handshake,
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_730_800_000))
        );
        assert_eq!((first.rx_bytes, first.tx_bytes), (1024, 2048));
    }

    #[test]
    fn a_peer_never_handshaken_or_without_endpoint_has_neither() {
        let dump = parse_dump(DUMP).unwrap();
        let second = &dump.peers[1];
        assert_eq!(second.endpoint, None);
        assert_eq!(second.latest_handshake, None);
        assert_eq!(
            second.allowed_ips,
            vec!["10.210.0.7/32".to_string(), "10.210.0.9/32".to_string()]
        );
    }

    #[test]
    fn an_interface_without_a_key_parses_as_none() {
        let dump = parse_dump("(none)\t(none)\t0\toff\n").unwrap();
        assert_eq!(dump.private_key, None);
        assert_eq!(dump.listen_port, 0);
        assert!(dump.peers.is_empty());
    }

    #[test]
    fn malformed_dumps_are_refused_with_the_line() {
        let error = parse_dump("garbage").unwrap_err().to_string();
        assert!(error.contains("garbage"), "{error}");
        assert!(parse_dump("").is_err());
        assert!(parse_dump("k\tp\t51820\toff\nonly\ttwo\n").is_err());
    }

    #[test]
    fn peer_status_carries_handshakes_and_counters() {
        let status = parse_dump(DUMP).unwrap().peer_status();
        assert_eq!(status[0].rx_bytes, 1024);
        assert!(status[0].last_handshake.is_some());
        assert!(status[1].last_handshake.is_none());
    }

    #[test]
    fn reads_ipv4_addresses_and_link_state() {
        let addrs = "7: temps-wg0    inet 10.210.0.4/16 scope global temps-wg0\\       valid_lft forever preferred_lft forever\n";
        assert_eq!(parse_addresses(addrs), vec!["10.210.0.4/16".to_string()]);
        assert!(parse_addresses("").is_empty());
        assert!(parse_link_up(
            "7: temps-wg0: <POINTOPOINT,NOARP,UP,LOWER_UP> mtu 1420 qdisc noqueue"
        ));
        assert!(!parse_link_up(
            "7: temps-wg0: <POINTOPOINT,NOARP> mtu 1420 qdisc noop"
        ));
    }

    #[test]
    fn exec_args_target_the_sidecar_on_the_pinned_daemon() {
        assert_eq!(
            exec_args("unix:///tmp/d.sock", &["wg", "show"], true),
            vec![
                "--host",
                "unix:///tmp/d.sock",
                "exec",
                "-i",
                SIDECAR_CONTAINER,
                "wg",
                "show"
            ]
        );
        assert!(!exec_args("unix:///tmp/d.sock", &["true"], false).contains(&"-i".to_string()));
    }

    #[test]
    fn the_private_key_never_appears_in_the_configure_script() {
        let script = configure_script("10.210.0.4".parse().unwrap(), 16, 51820, 1420);
        assert!(script.contains("cat > \"$key\""));
        assert!(script.contains("ip addr add 10.210.0.4/16 dev temps-wg0"));
        assert!(script.contains("listen-port 51820"));
        assert!(script.contains("mtu 1420 up"));
        assert!(!script.contains("private.key"));
    }

    #[test]
    fn set_peer_args_include_the_endpoint_only_when_known() {
        let with = set_peer_args(
            "QUFBQQ==",
            Some("1.2.3.4:51820".parse().unwrap()),
            &["10.210.0.1/32".to_string()],
            25,
        );
        assert_eq!(
            with,
            vec![
                "wg",
                "set",
                "temps-wg0",
                "peer",
                "QUFBQQ==",
                "endpoint",
                "1.2.3.4:51820",
                "persistent-keepalive",
                "25",
                "allowed-ips",
                "10.210.0.1/32"
            ]
        );
        let without = set_peer_args("QUFBQQ==", None, &["10.210.0.1/32".to_string()], 25);
        assert!(!without.contains(&"endpoint".to_string()));
    }

    #[test]
    fn the_endpoint_is_pinned_once() {
        let endpoint = DockerEndpoint {
            cli: "/usr/local/bin/docker".into(),
            host: "unix:///tmp/one.sock".into(),
        };
        set_docker_endpoint(endpoint.clone()).unwrap();
        set_docker_endpoint(endpoint).unwrap();
        let other = DockerEndpoint {
            cli: "/usr/local/bin/docker".into(),
            host: "unix:///tmp/two.sock".into(),
        };
        assert!(set_docker_endpoint(other).is_err());
    }
}
