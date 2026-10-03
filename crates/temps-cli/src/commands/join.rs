// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps join` subcommand — joins a worker node to an existing cluster.
//!
//! Supports three modes:
//! - **Direct mode** (`--private-address`): registers over user-managed networking
//! - **Pairing** (`--pair <code>`): for a control plane this machine cannot
//!   reach; the control plane dials this machine, and it registers over the
//!   WireGuard mesh (ADR 048 D2b)
//! - **Relay mode** (`--relay-url`): uses an operator-run relay for WireGuard key
//!   exchange. There is no public relay at the moment, so the URL must be given.
//!
//! After registration, saves the agent config to `~/.temps/agent.json` and exits.
//! Run `temps agent` separately to start the worker.

use clap::Args;

use super::api_url::management_api_url;

/// Join this machine to a Temps cluster as a worker node
#[derive(Args)]
pub struct JoinCommand {
    /// Cluster ID or control plane URL (e.g. "abc123" for relay mode,
    /// or "https://control-plane:3000" for direct mode)
    #[arg(required_unless_present = "pair")]
    pub target: Option<String>,

    /// Join token provided by the cluster admin (prefer TEMPS_JOIN_TOKEN env var)
    #[arg(env = "TEMPS_JOIN_TOKEN", required_unless_present = "pair")]
    pub token: Option<String>,

    /// Pairing code from the control plane's Worker Nodes page (Add node).
    /// The control plane dials this machine on the WireGuard port, and the
    /// node registers over the mesh: for a control plane this machine cannot
    /// reach (a laptop, a server behind NAT). `-` reads the code from stdin,
    /// which keeps it out of the process list and shell history.
    #[arg(long, conflicts_with_all = ["target", "private_address", "relay_url"])]
    pub pair: Option<String>,

    /// Node name (defaults to hostname)
    #[arg(long)]
    pub name: Option<String>,

    /// Private IP address to use instead of WireGuard (skips relay,
    /// requires user-managed networking between nodes)
    #[arg(long)]
    pub private_address: Option<String>,

    /// Listen address for the agent API
    #[arg(long, default_value = "127.0.0.1:3100")]
    pub agent_address: String,

    /// Relay URL for WireGuard key exchange (relay mode). Required when
    /// --private-address is not given; there is no public relay to default to.
    #[arg(long, env = "TEMPS_RELAY_URL")]
    pub relay_url: Option<String>,

    /// Labels for node scheduling (key=value pairs)
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Expected SHA-256 fingerprint of the cluster CA, shown when the enrollment
    /// token was minted. When set, the join aborts if the CA returned by the
    /// control plane doesn't match — defeating a man-in-the-middle that swaps in
    /// its own CA (ADR-020 WS-2.2).
    #[arg(long)]
    pub ca_fingerprint: Option<String>,

    /// Network device the VXLAN overlay should bind to as its underlay
    /// parent (e.g. "enp6s0"). Defaults to auto-detecting the device
    /// carrying this host's IPv4 default route — set this only when the
    /// default route doesn't point at the interface that should carry
    /// overlay traffic (e.g. a private network on a VLAN sub-interface).
    #[arg(long)]
    pub underlay_dev: Option<String>,

    /// Optional MTU ceiling for the selected underlay. Normally the agent
    /// detects this from the interface. Set it only when the real path MTU is
    /// lower than the interface reports.
    #[arg(long)]
    pub underlay_mtu: Option<u32>,
}

/// Response body from the control plane registration endpoint.
#[derive(serde::Deserialize)]
struct RegisterResponse {
    id: i32,
    /// Whether the control plane requires this node to serve mTLS.
    #[serde(default)]
    mtls_required: bool,
    /// Signed per-node leaf cert (PEM) for mTLS — present when we sent a CSR.
    #[serde(default)]
    cert_pem: Option<String>,
    /// Cluster CA cert (PEM) the node pins as its trust root.
    #[serde(default)]
    ca_cert_pem: Option<String>,
}

/// Generated mTLS material to send + save during join (ADR-020 WS-2.1).
struct NodeTlsMaterial {
    key_pem: String,
    csr_pem: String,
}

/// The cluster CA the control plane's node API presents on the mesh, if its
/// SHA-256 matches `fingerprint` (from the pairing code). Retries for a
/// minute while the WireGuard handshake completes. The handshake here only
/// reads the presented chain: nothing is sent over it, and everything after
/// is verified against the CA this returns.
async fn pinned_cluster_ca(
    node_api: std::net::SocketAddr,
    fingerprint: &str,
) -> anyhow::Result<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match presented_chain(node_api).await {
            Ok(chain) => {
                return find_pinned_ca(chain, fingerprint).ok_or_else(|| {
                    anyhow::anyhow!(
                        "The control plane at {node_api} did not present the cluster CA \
                         from the pairing code. Aborting (possible man-in-the-middle)."
                    )
                });
            }
            Err(error) if tokio::time::Instant::now() < deadline => {
                tracing::debug!(%error, "node API not reachable over the mesh yet");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(error) => anyhow::bail!(
                "The mesh is up but the control plane's node API at {node_api} did not answer: \
                 {error}. Check `temps doctor mesh` on this machine and the Worker Nodes page."
            ),
        }
    }
}

/// The certificate in `chain` (DER) whose SHA-256 is `fingerprint` (hex, any
/// case, surrounding whitespace ignored), to be trusted as the only root.
/// The fingerprint names the cluster CA, so a chain of only a leaf never
/// matches, and an empty fingerprint matches nothing.
fn find_pinned_ca(chain: Vec<Vec<u8>>, fingerprint: &str) -> Option<Vec<u8>> {
    use sha2::Digest;
    let fingerprint = fingerprint.trim();
    if fingerprint.is_empty() {
        return None;
    }
    chain
        .into_iter()
        .find(|der| hex::encode(sha2::Sha256::digest(der)).eq_ignore_ascii_case(fingerprint))
}

/// The pairing code from the first non-empty line of stdin (`--pair -`).
fn read_code_from_stdin() -> anyhow::Result<String> {
    read_code(std::io::stdin().lock())
}

/// The first non-empty line of `input`, trimmed.
fn read_code(input: impl std::io::BufRead) -> anyhow::Result<String> {
    for line in input.lines() {
        let line = line?;
        let line = line.trim();
        if !line.is_empty() {
            return Ok(line.to_string());
        }
    }
    anyhow::bail!("--pair - reads the pairing code from stdin, but stdin was empty")
}

/// How relay-mode registration may trust the control plane the relay named.
#[derive(Debug, PartialEq, Eq)]
enum RelayRegistrationTrust {
    /// The control plane is on the relay's host, the host the operator
    /// chose: public TLS roots verify it, as in direct mode.
    RelayHost,
    /// Anywhere else: the join token goes only to a server that presents
    /// the cluster CA with this fingerprint (`--ca-fingerprint`).
    PinnedCa(String),
}

impl RelayRegistrationTrust {
    /// The trust the agent keeps for the control plane after this join: a
    /// control plane verified against the pinned cluster CA stays verified
    /// against it alone.
    fn agent_trust(&self) -> temps_agent::ControlPlaneTrust {
        match self {
            Self::RelayHost => temps_agent::ControlPlaneTrust::PublicRoots,
            Self::PinnedCa(_) => temps_agent::ControlPlaneTrust::ClusterCa,
        }
    }
}

/// A client for registering with the control plane, and the trust the agent
/// keeps for it afterwards (persisted in `agent.json`).
struct ControlPlaneClient {
    client: reqwest::Client,
    trust: temps_agent::ControlPlaneTrust,
}

/// Decide, before anything is sent, whether the join token may go to the
/// control-plane URL the relay returned.
///
/// The relay chooses that URL, so on its own it proves nothing: a relay (or
/// anyone who can alter the relay's response) could name its own HTTPS host
/// and receive the token. It is trusted when it is on the relay's host, the
/// one the operator typed, or when the operator pinned the cluster CA.
fn relay_registration_trust(
    relay_url: &str,
    control_plane_url: &str,
    ca_fingerprint: Option<&str>,
    target: &str,
) -> anyhow::Result<RelayRegistrationTrust> {
    let relay = url::Url::parse(relay_url)
        .map_err(|error| anyhow::anyhow!("the relay URL '{relay_url}' is invalid: {error}"))?;
    let control_plane = url::Url::parse(control_plane_url).map_err(|error| {
        anyhow::anyhow!(
            "the relay returned an invalid control plane URL '{control_plane_url}': {error}"
        )
    })?;
    if control_plane.scheme() != "https" {
        anyhow::bail!(
            "The relay named {control_plane_url} as the control plane. It is not HTTPS, so the \
             join token is not sent to it. Fix the control plane URL the relay returns."
        );
    }
    let fingerprint = ca_fingerprint
        .map(str::trim)
        .filter(|fingerprint| !fingerprint.is_empty());
    let same_host = match (relay.host(), control_plane.host()) {
        (Some(relay), Some(control_plane)) => relay
            .to_string()
            .eq_ignore_ascii_case(&control_plane.to_string()),
        _ => false,
    };
    match (same_host, fingerprint) {
        (true, _) => Ok(RelayRegistrationTrust::RelayHost),
        (false, Some(fingerprint)) => Ok(RelayRegistrationTrust::PinnedCa(fingerprint.to_string())),
        (false, None) => anyhow::bail!(
            "The relay at {relay_host} named {control_plane_url} as the control plane, a \
             different host. The join token is only sent to another host when it proves it holds \
             the cluster CA. Copy the cluster CA fingerprint from the control plane's Worker \
             Nodes page (Cluster trust) and run:\n  temps join {target} --relay-url \
             {relay_url} --ca-fingerprint <fingerprint>",
            relay_host = relay.host_str().unwrap_or(relay_url),
        ),
    }
}

/// A client that trusts only the cluster CA with `fingerprint`, as presented
/// by the server at `url`. Fails when that server does not present it.
async fn pinned_client_for(url: &url::Url, fingerprint: &str) -> anyhow::Result<reqwest::Client> {
    use rustls::pki_types::ServerName;

    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("{url} has no port"))?;
    let (address, name) = match url.host() {
        Some(url::Host::Ipv4(ip)) => (
            std::net::SocketAddr::new(ip.into(), port),
            ServerName::IpAddress(std::net::IpAddr::V4(ip).into()),
        ),
        Some(url::Host::Ipv6(ip)) => (
            std::net::SocketAddr::new(ip.into(), port),
            ServerName::IpAddress(std::net::IpAddr::V6(ip).into()),
        ),
        Some(url::Host::Domain(domain)) => {
            let address = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|error| anyhow::anyhow!("could not resolve {domain}: {error}"))?
                .next()
                .ok_or_else(|| anyhow::anyhow!("{domain} resolves to no address"))?;
            let name = ServerName::try_from(domain.to_string())
                .map_err(|error| anyhow::anyhow!("{domain} is not a valid TLS name: {error}"))?;
            (address, name)
        }
        None => anyhow::bail!("{url} has no host"),
    };
    let chain = presented_chain_named(address, name)
        .await
        .map_err(|error| anyhow::anyhow!("could not reach the control plane at {url}: {error}"))?;
    let ca = find_pinned_ca(chain, fingerprint).ok_or_else(|| {
        anyhow::anyhow!(
            "The control plane at {url} did not present the cluster CA with fingerprint \
             {fingerprint}. Aborting join before sending the join token (possible \
             man-in-the-middle)."
        )
    })?;
    Ok(reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_der(&ca)?)
        .build()?)
}

/// The certificate chain a TLS server presents (DER), without trusting it.
///
/// Only [`pinned_cluster_ca`] uses this, to find the CA whose fingerprint the
/// pairing code carries before any trust decision: the connection accepts
/// any certificate, so nothing but the handshake goes over it, and every
/// request afterwards uses a client that trusts only the pinned CA.
async fn presented_chain(address: std::net::SocketAddr) -> anyhow::Result<Vec<Vec<u8>>> {
    let name = rustls::pki_types::ServerName::IpAddress(address.ip().into());
    presented_chain_named(address, name).await
}

/// [`presented_chain`], sending `name` as the TLS server name (SNI).
async fn presented_chain_named(
    address: std::net::SocketAddr,
    name: rustls::pki_types::ServerName<'static>,
) -> anyhow::Result<Vec<Vec<u8>>> {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct Capture {
        chain: Mutex<Vec<Vec<u8>>>,
        provider: Arc<rustls::crypto::CryptoProvider>,
    }
    impl ServerCertVerifier for Capture {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            if let Ok(mut chain) = self.chain.lock() {
                chain.push(end_entity.to_vec());
                chain.extend(intermediates.iter().map(|cert| cert.to_vec()));
            }
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.provider
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    let capture = Arc::new(Capture {
        chain: Mutex::new(Vec::new()),
        provider,
    });
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(capture.clone())
        .with_no_client_auth();
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(address),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out connecting"))??;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        connector.connect(name, stream),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out in the TLS handshake"))??;
    let chain = capture
        .chain
        .lock()
        .map_err(|_| anyhow::anyhow!("certificate capture poisoned"))?
        .clone();
    Ok(chain)
}

fn load_saved_agent_config() -> Option<temps_agent::AgentConfig> {
    let config_path = crate::commands::agent::agent_data_dir().join("agent.json");
    let data = std::fs::read_to_string(config_path).ok()?;
    serde_json::from_str(&data).ok()
}

fn saved_config_for_reenrollment<'a>(
    saved: Option<&'a temps_agent::AgentConfig>,
    node_name: &str,
    control_plane_url: &str,
) -> Option<&'a temps_agent::AgentConfig> {
    let saved = saved?;
    let same_control_plane =
        saved.control_plane_url.trim_end_matches('/') == control_plane_url.trim_end_matches('/');
    (saved.node_name == node_name && same_control_plane).then_some(saved)
}

fn prior_token_for_reenrollment(
    saved: Option<&temps_agent::AgentConfig>,
    node_name: &str,
    control_plane_url: &str,
) -> Option<String> {
    saved_config_for_reenrollment(saved, node_name, control_plane_url)
        .map(|saved| saved.token.clone())
}

fn public_ingress_listener_settings(
    matching_saved: Option<&temps_agent::AgentConfig>,
) -> (Option<std::net::IpAddr>, u16, u16) {
    matching_saved.map_or((None, 80, 443), |saved| {
        (
            saved.public_ingress_address,
            saved.public_ingress_http_port,
            saved.public_ingress_https_port,
        )
    })
}

fn apply_saved_public_ingress_settings(
    config: &mut temps_agent::AgentConfig,
    matching_saved: Option<&temps_agent::AgentConfig>,
) {
    let (address, http_port, https_port) = public_ingress_listener_settings(matching_saved);
    config.public_ingress_address = address;
    config.public_ingress_http_port = http_port;
    config.public_ingress_https_port = https_port;
}

/// How to start the worker once it has joined.
fn print_start_agent_hint() {
    println!();
    println!("Start the worker:");
    println!("  temps agent service install   as a systemd service, restarted on failure and at boot (root)");
    println!("  temps agent                   in the foreground, or under your own supervisor");
}

/// Extract the port `temps agent` will listen on from `--agent-address`.
/// Uses `SocketAddr::from_str` rather than a manual `.split(':').next_back()`
/// so a bracketed IPv6 address with no port (e.g. "[::1]") doesn't glue the
/// closing bracket onto the extracted "port" -- falls back to the default
/// agent port only when `agent_address` isn't a parsable socket address at
/// all.
fn agent_listen_port(agent_address: &str) -> u16 {
    agent_address
        .parse::<std::net::SocketAddr>()
        .map(|addr| addr.port())
        .unwrap_or(3100)
}

/// Build a "host:port" URL authority, bracketing IPv6 the way
/// `SocketAddr`'s `Display` does ("[fc00::1]:3100") -- a bare
/// "{ip}:{port}" is unparsable for IPv6 since nothing marks where the
/// address ends and the port begins. Falls back to the unbracketed form
/// only if `ip` isn't itself a parsable IP address (shouldn't happen for a
/// validated `private_address`, but this must never produce a *worse*
/// address than the naive concatenation it replaces).
fn socket_authority(ip: &str, port: u16) -> String {
    match ip.parse::<std::net::IpAddr>() {
        Ok(ip) => std::net::SocketAddr::new(ip, port).to_string(),
        Err(_) => format!("{ip}:{port}"),
    }
}

/// Generate a per-node keypair + CSR. The private key never leaves this host.
/// `ip` is the address the control plane will connect to (the node's
/// private/WG IP) and MUST be a SAN, or the CP's server-cert hostname check
/// fails (ADR-020 WS-2.1).
fn generate_node_tls_material(node_name: &str, ip: &str) -> anyhow::Result<NodeTlsMaterial> {
    let sans = vec![ip.to_string(), node_name.to_string()];
    temps_core::node_pki::generate_node_keypair_csr(node_name, &sans)
        .map(|csr| NodeTlsMaterial {
            key_pem: csr.key_pem,
            csr_pem: csr.csr_pem,
        })
        .map_err(|e| anyhow::anyhow!("could not generate the node mTLS key and CSR: {e}"))
}

/// Write the node key + leaf cert + cluster CA to the agent data dir (key 0600)
/// and return their paths for the agent config. Any failure is fatal: once the
/// control plane records an HTTPS agent address, silently serving HTTP would
/// leave a broken node and weaken the operator's intended transport policy.
fn write_node_certs(
    key_pem: &str,
    cert_pem: &str,
    ca_cert_pem: &str,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    let dir = crate::commands::agent::agent_data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        anyhow::anyhow!(
            "could not create agent certificate directory '{}': {e}",
            dir.display()
        )
    })?;
    let key_path = dir.join("node.key.pem");
    let cert_path = dir.join("node.cert.pem");
    let ca_path = dir.join("cluster-ca.pem");

    std::fs::write(&key_path, key_pem)
        .map_err(|e| anyhow::anyhow!("could not write node key '{}': {e}", key_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |e| anyhow::anyhow!("could not restrict node key '{}': {e}", key_path.display()),
        )?;
    }
    std::fs::write(&cert_path, cert_pem).map_err(|e| {
        anyhow::anyhow!(
            "could not write node certificate '{}': {e}",
            cert_path.display()
        )
    })?;
    std::fs::write(&ca_path, ca_cert_pem)
        .map_err(|e| anyhow::anyhow!("could not write cluster CA '{}': {e}", ca_path.display()))?;
    Ok((cert_path, key_path, ca_path))
}

/// Persist the signed leaf + cluster CA from the register response, returning
/// the `(cert, key, ca)` paths for the agent config. A control plane that says
/// mTLS is required must return both certificates; otherwise enrollment fails.
fn persist_tls(
    material: &NodeTlsMaterial,
    response: &RegisterResponse,
) -> anyhow::Result<Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)>> {
    if !response.mtls_required {
        return Ok(None);
    }
    let cert_pem = response.cert_pem.as_ref().ok_or_else(|| {
        anyhow::anyhow!("control plane requires mTLS but returned no signed node certificate")
    })?;
    let ca_cert_pem = response.ca_cert_pem.as_ref().ok_or_else(|| {
        anyhow::anyhow!("control plane requires mTLS but returned no cluster CA certificate")
    })?;
    let paths = write_node_certs(&material.key_pem, cert_pem, ca_cert_pem)?;
    println!("mTLS certificate provisioned — the agent will serve TLS.");
    Ok(Some(paths))
}

/// Detect the container platform this machine will run workloads on.
///
/// Reads it from the local Docker daemon: that is the architecture which
/// decides whether an image can run here, and it differs from this binary's
/// whenever `DOCKER_HOST` points at another machine or an emulated daemon.
///
/// Returns `None` when the daemon can't be reached. Reporting the CLI's own
/// architecture instead would register a *confidently wrong* platform, and the
/// control plane trusts what a node reports — it would schedule on that value
/// and transfer an image the node cannot execute. An absent architecture is
/// handled safely (the node is scheduled as unverified) and the agent fills it
/// in on its first successful heartbeat.
async fn detect_local_platform() -> Option<String> {
    let docker = match bollard::Docker::connect_with_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            eprintln!(
                "Warning: could not connect to Docker ({}). Registering without a container \
                 platform; the agent reports it once the daemon is reachable.",
                e
            );
            return None;
        }
    };

    match docker.info().await {
        Ok(info) => {
            let os = info.os_type.unwrap_or_else(|| "linux".to_string());
            match info.architecture {
                Some(arch) => Some(temps_deployer::platform::normalize_platform(&os, &arch)),
                None => {
                    eprintln!(
                        "Warning: the Docker daemon reported no architecture. Registering \
                         without a container platform; the agent reports it later."
                    );
                    None
                }
            }
        }
        Err(e) => {
            eprintln!(
                "Warning: could not read Docker info ({}). Registering without a container \
                 platform; the agent reports it once the daemon is reachable.",
                e
            );
            None
        }
    }
}

impl JoinCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        rt.block_on(async move { self.run().await })
    }

    async fn run(mut self) -> anyhow::Result<()> {
        let labels = self.parse_labels();

        let node_name = self
            .name
            .take()
            .unwrap_or_else(|| gethostname().unwrap_or_else(|| "worker".to_string()));

        println!("Joining Temps cluster as '{}'...", node_name);

        // Report the container platform at join time so the control plane can
        // schedule correctly from the very first deploy, instead of waiting up
        // to 30s for the first heartbeat to reveal the architecture.
        let platform = detect_local_platform().await;
        match platform.as_deref() {
            Some(platform) => println!("Container platform: {}", platform),
            None => println!("Container platform: unknown (will be reported by the agent)"),
        }

        if let Some(code) = self.pair.clone() {
            let code = if code == "-" {
                read_code_from_stdin()?
            } else {
                code
            };
            self.join_paired(&code, &labels, platform.as_deref())
                .await?;
        } else if let Some(private_addr) = self.private_address.clone() {
            // A public control-plane URL: public roots only, now and for the
            // agent afterwards.
            let control_plane = ControlPlaneClient {
                client: reqwest::Client::builder().build()?,
                trust: temps_agent::ControlPlaneTrust::PublicRoots,
            };
            self.join_direct(
                &node_name,
                &private_addr,
                &labels,
                platform.as_deref(),
                control_plane,
                None,
            )
            .await?;
        } else if let Some(relay_url) = self.relay_url.clone() {
            self.join_via_relay(&relay_url, &node_name, &labels, platform.as_deref())
                .await?;
        } else {
            anyhow::bail!(
                "No join mode selected. Pass --private-address <ip> to register over \
                 your own network (direct mode), or --relay-url <url> (or TEMPS_RELAY_URL) \
                 pointing at a relay you run for WireGuard key exchange. \
                 There is no public relay at the moment."
            );
        }

        Ok(())
    }

    /// Save agent config to the agent data directory with restrictive permissions (0600).
    fn save_agent_config(&self, config: &temps_agent::AgentConfig) -> anyhow::Result<()> {
        let temps_dir = crate::commands::agent::agent_data_dir();
        std::fs::create_dir_all(&temps_dir)?;

        // Set directory permissions to 0700 (owner only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temps_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        let config_path = temps_dir.join("agent.json");
        let json = serde_json::to_string_pretty(config)?;
        std::fs::write(&config_path, &json)?;

        // Set file permissions to 0600 (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))?;
        }

        println!("Agent config saved to {}", config_path.display());
        Ok(())
    }

    fn target(&self) -> &str {
        self.target.as_deref().unwrap_or_default()
    }

    fn token(&self) -> &str {
        self.token.as_deref().unwrap_or_default()
    }

    /// Direct mode: register with control plane using provided private address.
    /// `control_plane` carries the TLS trust for the control plane, both for
    /// this registration and, persisted, for the agent afterwards;
    /// `wg_endpoint` is where other mesh members dial this node when it is
    /// not `private_address` on the mesh port.
    async fn join_direct(
        &self,
        node_name: &str,
        private_address: &str,
        labels: &serde_json::Value,
        platform: Option<&str>,
        control_plane: ControlPlaneClient,
        wg_endpoint: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        // Reject dangerous ranges up front, and normalize to a bare IP: the
        // "address" field built below appends its own port
        // (`https://{private_address}:{agent_port}`), and the control plane
        // does the same when constructing proxy backend addresses from the
        // stored `nodes.private_address` -- a port-suffixed value here would
        // corrupt both, independent of the server's own validation.
        let private_address = temps_deployments::handlers::nodes::validate_node_private_address(
            private_address.trim(),
        )
        .map(|ip| ip.to_string())
        .map_err(|error| {
            anyhow::anyhow!("--private-address '{private_address}' is invalid: {error}")
        })?;
        let private_address = private_address.as_str();

        println!(
            "Using direct mode with private address: {}",
            private_address
        );

        // Generate a node token for agent authentication
        let agent_token = generate_token();

        // Register with the control plane.
        //
        // Direct mode targets a user-supplied URL that may traverse the
        // public internet. We always require valid TLS here — a MitM on
        // this request would steal the join token and let the attacker
        // register a malicious worker. The server-side `insecure_tls`
        // opt-in does NOT apply to CLI binaries on purpose (the caller's
        // client verifies public roots, or the pinned cluster CA over the
        // mesh).

        let register_url = management_api_url(self.target(), "/internal/nodes/register");

        // Generate per-node mTLS material; send the CSR so the control plane
        // can sign a leaf for us (ADR-020 WS-2.1). The leaf must be valid for
        // the private address the CP connects to.
        let tls_material = generate_node_tls_material(node_name, private_address.trim())?;
        let (public_ingress_private_key, public_ingress_public_key) =
            generate_public_ingress_key()?;
        let saved_config = load_saved_agent_config();
        let matching_saved =
            saved_config_for_reenrollment(saved_config.as_ref(), node_name, self.target());
        let prior_token =
            prior_token_for_reenrollment(saved_config.as_ref(), node_name, self.target());

        let agent_port = agent_listen_port(&self.agent_address);
        let agent_url_host = socket_authority(private_address, agent_port);

        let register_body = serde_json::json!({
            "name": node_name,
            "token": agent_token,
            "join_token": self.token(),
            // Modern joins always carry a CSR and advertise the TLS endpoint.
            // The control plane may still accept an old CSR-less HTTP worker
            // during migration, but a newly enrolled worker must never be
            // persisted as plaintext.
            "address": format!("https://{}", agent_url_host),
            "private_address": private_address,
            "labels": labels,
            "architecture": platform,
            "csr_pem": tls_material.csr_pem.clone(),
            "prior_token": prior_token,
            "edge_public_key": public_ingress_public_key,
        });

        let response = control_plane
            .client
            .post(&register_url)
            .json(&register_body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Failed to register with control plane ({}): {}",
                status,
                body
            );
        }

        let register_response: RegisterResponse = response.json().await?;

        println!(
            "Registered with control plane successfully (node_id={}).",
            register_response.id
        );

        // Verify the cluster CA out of band before trusting it (ADR-020 WS-2.2).
        self.verify_ca_fingerprint(&register_response)?;

        // Persist the signed leaf + cluster CA so `temps agent` can serve mTLS.
        let tls_paths = persist_tls(&tls_material, &register_response)?;

        // Save config for `temps agent`
        let mut config = temps_agent::AgentConfig {
            listen_address: self.agent_address.clone(),
            token: agent_token,
            node_name: node_name.to_string(),
            control_plane_url: self.target().to_string(),
            node_id: register_response.id,
            labels: labels.clone(),
            dns_data_dir: crate::commands::agent::agent_data_dir().join("dns"),
            tls_cert_path: tls_paths.as_ref().map(|p| p.0.clone()),
            tls_key_path: tls_paths.as_ref().map(|p| p.1.clone()),
            cluster_ca_path: tls_paths.as_ref().map(|p| p.2.clone()),
            require_mtls: register_response.mtls_required,
            underlay_dev: self.underlay_dev.clone(),
            underlay_mtu: self.underlay_mtu,
            private_address: Some(private_address.trim().to_string()),
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: Some(public_ingress_private_key),
            mesh_key_dir: crate::commands::agent::agent_data_dir().join("wireguard"),
            wg_endpoint: wg_endpoint.map(|endpoint| endpoint.to_string()),
            control_plane_trust: Some(control_plane.trust),
        };
        apply_saved_public_ingress_settings(&mut config, matching_saved);
        self.save_agent_config(&config)?;

        print_start_agent_hint();

        Ok(())
    }

    /// Pairing (ADR 048 D2b): the control plane dials this machine on the
    /// mesh port and learns its WireGuard key; the node brings the mesh up with
    /// the control plane as its only peer and registers over it, verifying the
    /// control plane against the cluster CA pinned in the code.
    async fn join_paired(
        &mut self,
        code: &str,
        labels: &serde_json::Value,
        platform: Option<&str>,
    ) -> anyhow::Result<()> {
        use temps_wireguard::pairing::{
            self, PairingCode, PairingError, PairingSession, RejectReason,
        };

        let code = PairingCode::decode(code).map_err(|error| {
            anyhow::anyhow!(
                "{error}. Copy the whole command from the control plane (Worker Nodes → Add \
                 node) again."
            )
        })?;
        let now = chrono::Utc::now().timestamp();
        if code.is_expired(now) {
            anyhow::bail!(
                "This pairing code expired. Create a new pairing in the control plane \
                 (Worker Nodes → Add node)."
            );
        }
        if !cfg!(target_os = "linux") {
            anyhow::bail!("Pairing brings up kernel WireGuard, so the node must run Linux.");
        }
        if self.name.as_deref().is_some_and(|name| name != code.name) {
            println!(
                "Ignoring --name: this pairing enrolls the node as '{}'.",
                code.name
            );
        }

        let key_dir = crate::commands::agent::agent_data_dir().join("wireguard");
        let key = {
            let dir = key_dir.clone();
            tokio::task::spawn_blocking(move || {
                temps_wireguard::mesh::MeshKey::load_or_create(&dir)
            })
            .await??
        };

        // 1. Answer the control plane on the mesh port, before WireGuard
        //    takes it.
        let socket =
            tokio::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, code.listen_port))
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Could not listen on UDP {port}: {error}. If the temps-wg0 interface \
                         exists, this machine is already on a mesh; otherwise free the port and \
                         run this again.",
                        port = code.listen_port
                    )
                })?;
        println!(
            "Waiting for the control plane to reach this machine at {} (UDP {})...",
            code.node_endpoint, code.listen_port
        );
        println!(
            "It retries every few seconds until {}. If nothing happens, open UDP {} inbound.",
            chrono::DateTime::from_timestamp(code.expires_at, 0)
                .map(|at| at.format("%H:%M UTC").to_string())
                .unwrap_or_else(|| "the code expires".to_string()),
            code.listen_port
        );
        let remaining = u64::try_from(code.expires_at - now).unwrap_or(0);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(remaining);
        let session = PairingSession::new(code.pairing_id()?, &code.pairing_secret()?);
        let heard_from = match pairing::respond(
            &socket,
            &session,
            &code.control_plane_public_key,
            key.public_key(),
            deadline,
        )
        .await
        {
            Ok(from) => from,
            Err(PairingError::TimedOut) => anyhow::bail!(
                "The control plane never reached this machine before the pairing expired. Check \
                 that UDP {port} is open inbound and that {endpoint} is this machine's public \
                 address, then create a new pairing.",
                port = code.listen_port,
                endpoint = code.node_endpoint
            ),
            Err(PairingError::Rejected(RejectReason::KeyInUse)) => anyhow::bail!(
                "The control plane refused this machine's WireGuard key: another node already \
                 uses it, so {key} was copied from another machine (a cloned disk or a shared \
                 home directory). If this machine is not that node, delete {key} and {snapshot}, \
                 then run this command again: it generates a new key. The pairing stays open \
                 until it expires.",
                key = key_dir.join("private.key").display(),
                snapshot = key_dir.join("network-snapshot.json").display()
            ),
            Err(PairingError::Rejected(RejectReason::PairingClosed)) => anyhow::bail!(
                "This pairing was cancelled, expired or already used. Create a new one on the \
                 control plane's Worker Nodes page (or `bunx @temps-sdk/cli nodes pair create`)."
            ),
            Err(error) => return Err(error.into()),
        };
        drop(socket);
        println!(
            "The control plane reached this machine (from {}). Bringing up the WireGuard mesh...",
            heard_from.ip()
        );

        // 2. The mesh, with the control plane as the only peer.
        let node_ip = code.node_endpoint.ip().to_string();
        let mesh_config = temps_agent::AgentConfig {
            listen_address: self.agent_address.clone(),
            token: String::new(),
            node_name: code.name.clone(),
            control_plane_url: String::new(),
            node_id: 0,
            labels: labels.clone(),
            dns_data_dir: crate::commands::agent::agent_data_dir().join("dns"),
            tls_cert_path: None,
            tls_key_path: None,
            cluster_ca_path: None,
            require_mtls: false,
            underlay_dev: self.underlay_dev.clone(),
            underlay_mtu: self.underlay_mtu,
            private_address: Some(node_ip.clone()),
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: None,
            mesh_key_dir: key_dir,
            wg_endpoint: Some(code.node_endpoint.to_string()),
            // Only brings the mesh up; it never calls the control plane.
            control_plane_trust: Some(temps_agent::ControlPlaneTrust::PublicRoots),
        };
        let cidr = ipnet::Ipv4Net::new(code.node_address, code.prefix_len)?.trunc();
        temps_agent::network_sync::bootstrap_mesh(
            &mesh_config,
            &temps_agent::network_sync::MeshBootstrap {
                cidr,
                listen_port: code.listen_port,
                endpoint: code.node_endpoint,
                address: code.node_address,
                control_plane_public_key: code.control_plane_public_key.clone(),
                control_plane_endpoint: code.control_plane_endpoint.clone(),
                control_plane_address: code.control_plane_address,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("could not bring up the WireGuard mesh: {error}"))?;

        // 3. Register over the mesh against the pinned cluster CA.
        let node_api = std::net::SocketAddr::new(
            std::net::IpAddr::V4(code.control_plane_address),
            code.node_api_port,
        );
        println!("Mesh is up. Reaching the control plane at {node_api} over it...");
        let ca_der = pinned_cluster_ca(node_api, &code.ca_fingerprint).await?;
        let client = reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_der(&ca_der)?)
            .build()?;
        self.target = Some(format!("https://{node_api}"));
        self.token = Some(code.join_token.clone());
        self.ca_fingerprint = Some(code.ca_fingerprint.clone());
        // The agent keeps reaching the control plane at this mesh address,
        // verified against the same pinned cluster CA and nothing else.
        let control_plane = ControlPlaneClient {
            client,
            trust: temps_agent::ControlPlaneTrust::ClusterCa,
        };
        self.join_direct(
            &code.name,
            &node_ip,
            labels,
            platform,
            control_plane,
            Some(code.node_endpoint),
        )
        .await
    }

    /// Relay mode: use an operator-run relay for WireGuard key exchange.
    async fn join_via_relay(
        &self,
        relay_url: &str,
        node_name: &str,
        labels: &serde_json::Value,
        platform: Option<&str>,
    ) -> anyhow::Result<()> {
        let relay_url = relay_url.trim_end_matches('/');
        println!("Using relay mode via {}...", relay_url);

        // Step 1: Check if WireGuard is available
        let wg_manager = temps_wireguard::WireGuardManager::default_config()?;

        wg_manager.check_available().await.map_err(|e| {
            anyhow::anyhow!(
                "WireGuard not available: {}. \
                 Use --private-address for user-managed networking.",
                e
            )
        })?;

        // Step 2: Generate WireGuard keypair
        let keypair = wg_manager.generate_keypair().await?;
        println!("Generated WireGuard keypair.");

        // Step 3: Contact relay to join cluster
        let client = reqwest::Client::new();

        let join_url = format!("{}/api/relay/clusters/{}/join", relay_url, self.target());

        // Detect our public endpoint (for WireGuard)
        let public_endpoint = detect_public_endpoint(wg_manager.listen_port()).await;

        let join_body = serde_json::json!({
            "join_token": self.token(),
            "node_name": node_name,
            "wg_public_key": keypair.public_key,
            "public_endpoint": public_endpoint,
            "labels": labels,
        });

        let response = client.post(&join_url).json(&join_body).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Relay join failed ({}): {}", status, body);
        }

        #[derive(serde::Deserialize)]
        struct RelayJoinResponse {
            control_plane_wg_pubkey: String,
            control_plane_endpoint: String,
            assigned_ip: String,
            control_plane_ip: String,
            control_plane_url: String,
            agent_token: String,
        }

        let relay_response: RelayJoinResponse = response.json().await?;
        // Decide before setting anything up whether the join token may go to
        // the control plane the relay named.
        let trust = relay_registration_trust(
            relay_url,
            &relay_response.control_plane_url,
            self.ca_fingerprint.as_deref(),
            self.target(),
        )?;

        // Step 4: Configure WireGuard interface
        let our_ip: std::net::Ipv4Addr = relay_response.assigned_ip.parse()?;
        wg_manager
            .init_interface(our_ip, &keypair.private_key)
            .await?;

        // Step 5: Add control plane as WireGuard peer
        let peer = temps_wireguard::WireGuardPeer {
            public_key: relay_response.control_plane_wg_pubkey,
            endpoint: relay_response.control_plane_endpoint,
            allowed_ips: format!("{}/32", relay_response.control_plane_ip),
        };
        wg_manager.add_peer(&peer).await?;

        println!(
            "WireGuard tunnel established: {} -> {}",
            relay_response.assigned_ip, relay_response.control_plane_ip
        );

        // Step 6: Register with control plane over WireGuard tunnel.
        // Traffic is encrypted by WireGuard, but the relay chose both the
        // tunnel's peer and the URL, so neither proves who receives the join
        // token. Strict TLS is mandatory: public roots when the control plane
        // is on the relay's host (the one the operator typed), otherwise only
        // the cluster CA pinned with --ca-fingerprint.
        let register_client = match &trust {
            RelayRegistrationTrust::RelayHost => reqwest::Client::builder().build()?,
            RelayRegistrationTrust::PinnedCa(fingerprint) => {
                let url = url::Url::parse(&relay_response.control_plane_url)?;
                let client = pinned_client_for(&url, fingerprint).await?;
                println!("Control plane verified against the pinned cluster CA.");
                client
            }
        };

        let register_url = management_api_url(
            &relay_response.control_plane_url,
            "/internal/nodes/register",
        );

        let agent_port = self
            .agent_address
            .split(':')
            .next_back()
            .unwrap_or("3100")
            .trim();

        // Generate per-node mTLS material and send the CSR (ADR-020 WS-2.1).
        // The leaf must be valid for the WG IP the CP connects to.
        let tls_material = generate_node_tls_material(node_name, &relay_response.assigned_ip)?;
        let (public_ingress_private_key, public_ingress_public_key) =
            generate_public_ingress_key()?;
        let saved_config = load_saved_agent_config();
        let matching_saved = saved_config_for_reenrollment(
            saved_config.as_ref(),
            node_name,
            relay_response.control_plane_url.as_str(),
        );
        let prior_token = prior_token_for_reenrollment(
            saved_config.as_ref(),
            node_name,
            relay_response.control_plane_url.as_str(),
        );

        let register_body = serde_json::json!({
            "name": node_name,
            "token": relay_response.agent_token,
            "join_token": self.token(),
            "address": format!("https://{}:{}", relay_response.assigned_ip, agent_port),
            "private_address": relay_response.assigned_ip,
            "wg_public_key": keypair.public_key,
            "public_endpoint": public_endpoint,
            "labels": labels,
            "architecture": platform,
            "csr_pem": tls_material.csr_pem.clone(),
            "prior_token": prior_token,
            "edge_public_key": public_ingress_public_key,
        });

        let response = register_client
            .post(&register_url)
            .json(&register_body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Failed to register with control plane over WireGuard ({}): {}",
                status,
                body
            );
        }

        // The response carries the signed identity and trust root. Treat an
        // invalid response as a failed enrollment: falling back to the relay's
        // node ID would silently configure a plaintext agent.
        let register_response: RegisterResponse = response.json().await.map_err(|error| {
            anyhow::anyhow!("control plane returned an invalid mTLS enrollment response: {error}")
        })?;
        let node_id = register_response.id;

        // Pin the CA *before* persisting any of it: `persist_tls` writes the
        // returned CA to disk and `temps agent` then trusts it for mTLS.
        self.verify_ca_fingerprint(&register_response)?;

        println!(
            "Registered with control plane successfully (node_id={}).",
            node_id
        );

        let tls_paths = persist_tls(&tls_material, &register_response)?;

        // Save config for `temps agent`
        let mut config = temps_agent::AgentConfig {
            listen_address: self.agent_address.clone(),
            token: relay_response.agent_token,
            node_name: node_name.to_string(),
            control_plane_url: relay_response.control_plane_url,
            node_id,
            labels: labels.clone(),
            dns_data_dir: crate::commands::agent::agent_data_dir().join("dns"),
            tls_cert_path: tls_paths.as_ref().map(|p| p.0.clone()),
            tls_key_path: tls_paths.as_ref().map(|p| p.1.clone()),
            cluster_ca_path: tls_paths.as_ref().map(|p| p.2.clone()),
            require_mtls: register_response.mtls_required,
            underlay_dev: self.underlay_dev.clone(),
            underlay_mtu: self.underlay_mtu,
            private_address: Some(relay_response.assigned_ip.clone()),
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: Some(public_ingress_private_key),
            mesh_key_dir: crate::commands::agent::agent_data_dir().join("wireguard"),
            wg_endpoint: None,
            control_plane_trust: Some(trust.agent_trust()),
        };
        apply_saved_public_ingress_settings(&mut config, matching_saved);
        self.save_agent_config(&config)?;

        print_start_agent_hint();

        Ok(())
    }

    /// Check the cluster CA the control plane returned against the
    /// out-of-band fingerprint the operator passed with `--ca-fingerprint`.
    ///
    /// Called from **both** join paths. It used to live inline in
    /// `join_direct` only, so an operator following the documented enrollment
    /// flow could run `temps join --ca-fingerprint ...` in the default relay
    /// mode and still silently persist whatever CA a malicious relay or a
    /// MITM'd registration endpoint returned — exactly the pinning the flag
    /// exists to provide. A missing CA is a hard failure too: "no certificate
    /// returned" must not be quietly treated as "nothing to verify".
    fn verify_ca_fingerprint(&self, register_response: &RegisterResponse) -> anyhow::Result<()> {
        let Some(expected) = self.ca_fingerprint.as_deref() else {
            return Ok(());
        };

        match register_response.ca_cert_pem.as_deref() {
            Some(ca_pem) => {
                let actual = temps_core::node_pki::ca_fingerprint_sha256(ca_pem)
                    .map_err(|e| anyhow::anyhow!("could not fingerprint received CA: {e}"))?;
                if !actual.eq_ignore_ascii_case(expected.trim()) {
                    anyhow::bail!(
                        "Cluster CA fingerprint mismatch — expected {expected}, got {actual}. \
                         Aborting join (possible man-in-the-middle)."
                    );
                }
                println!("Cluster CA fingerprint verified.");
                Ok(())
            }
            None => anyhow::bail!(
                "--ca-fingerprint was provided but the control plane returned no CA certificate."
            ),
        }
    }

    fn parse_labels(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for label in &self.labels {
            if let Some((key, value)) = label.split_once('=') {
                map.insert(
                    key.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
        }
        serde_json::Value::Object(map)
    }
}

/// Get the hostname of this machine.
fn gethostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
}

/// Generate a random authentication token.
fn generate_token() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random()).collect();
    hex::encode(bytes)
}

fn generate_public_ingress_key() -> Result<(String, String), temps_core::ecies::EciesError> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let secret = temps_core::ecies::generate_x25519_static_secret()?;
    let public = x25519_dalek::PublicKey::from(&secret);
    Ok((
        STANDARD.encode(secret.as_bytes()),
        STANDARD.encode(public.as_bytes()),
    ))
}

/// Try to detect our public IP and WireGuard port for the endpoint.
async fn detect_public_endpoint(wg_port: u16) -> Option<String> {
    // Try to get public IP via a simple HTTP service
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let response = client.get("https://api.ipify.org").send().await.ok()?;

    let public_ip = response.text().await.ok()?;
    let public_ip = public_ip.trim();

    if public_ip.is_empty() {
        return None;
    }

    Some(format!("{}:{}", public_ip, wg_port))
}

#[cfg(test)]
mod tests {
    use super::{
        agent_listen_port, apply_saved_public_ingress_settings, find_pinned_ca,
        generate_public_ingress_key, pinned_client_for, pinned_cluster_ca,
        prior_token_for_reenrollment, public_ingress_listener_settings, read_code,
        relay_registration_trust, saved_config_for_reenrollment, socket_authority,
        RelayRegistrationTrust,
    };
    use std::sync::Arc;

    /// A cluster CA, a leaf it signed for 127.0.0.1, and their DER.
    struct TestPki {
        ca_der: Vec<u8>,
        ca_fingerprint: String,
        leaf_der: Vec<u8>,
        leaf_key_pem: String,
    }

    fn test_pki() -> TestPki {
        use temps_core::node_pki;
        let ca = node_pki::generate_cluster_ca().unwrap();
        let sans = vec!["127.0.0.1".to_string()];
        let csr = node_pki::generate_node_keypair_csr("temps-control-plane", &sans).unwrap();
        let leaf = node_pki::sign_node_csr(&ca.cert_pem, &ca.key_pem, &csr.csr_pem, &sans).unwrap();
        let der = |pem: &str| {
            rustls_pemfile::certs(&mut pem.as_bytes())
                .next()
                .unwrap()
                .unwrap()
                .to_vec()
        };
        TestPki {
            ca_der: der(&ca.cert_pem),
            ca_fingerprint: node_pki::ca_fingerprint_sha256(&ca.cert_pem).unwrap(),
            leaf_der: der(&leaf.cert_pem),
            leaf_key_pem: csr.key_pem,
        }
    }

    /// A TLS server on 127.0.0.1 presenting `chain`, for handshakes only.
    async fn tls_server(pki: &TestPki, chain: Vec<Vec<u8>>) -> std::net::SocketAddr {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let chain = chain.into_iter().map(CertificateDer::from).collect();
        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut pki.leaf_key_pem.as_bytes())
                .unwrap()
                .unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _ = acceptor.accept(stream).await;
                });
            }
        });
        address
    }

    #[tokio::test]
    async fn the_cluster_ca_with_the_pinned_fingerprint_is_accepted() {
        let pki = test_pki();
        let server = tls_server(&pki, vec![pki.leaf_der.clone(), pki.ca_der.clone()]).await;
        let pinned = pinned_cluster_ca(server, &pki.ca_fingerprint)
            .await
            .unwrap();
        assert_eq!(pinned, pki.ca_der);
        // Case and surrounding whitespace do not matter.
        let shouted = format!("  {}\n", pki.ca_fingerprint.to_uppercase());
        assert_eq!(
            pinned_cluster_ca(server, &shouted).await.unwrap(),
            pki.ca_der
        );
    }

    #[tokio::test]
    async fn a_chain_without_the_pinned_ca_is_refused_as_a_possible_mitm() {
        let pki = test_pki();
        let server = tls_server(&pki, vec![pki.leaf_der.clone(), pki.ca_der.clone()]).await;
        let wrong = "00".repeat(32);
        let error = pinned_cluster_ca(server, &wrong).await.unwrap_err();
        assert!(error.to_string().contains("man-in-the-middle"), "{error}");

        // Another cluster's CA, presented in full, is not this one.
        let other = test_pki();
        let error = pinned_cluster_ca(server, &other.ca_fingerprint)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("man-in-the-middle"), "{error}");
    }

    #[tokio::test]
    async fn a_leaf_only_chain_is_refused() {
        let pki = test_pki();
        let server = tls_server(&pki, vec![pki.leaf_der.clone()]).await;
        let error = pinned_cluster_ca(server, &pki.ca_fingerprint)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("man-in-the-middle"), "{error}");
    }

    #[test]
    fn an_empty_fingerprint_pins_nothing() {
        let pki = test_pki();
        assert_eq!(find_pinned_ca(vec![pki.ca_der.clone()], "  "), None);
        assert_eq!(
            find_pinned_ca(
                vec![pki.leaf_der.clone(), pki.ca_der.clone()],
                &pki.ca_fingerprint
            ),
            Some(pki.ca_der)
        );
    }

    #[tokio::test]
    async fn relay_registration_pins_the_cluster_ca_before_sending_the_token() {
        let pki = test_pki();
        let server = tls_server(&pki, vec![pki.leaf_der.clone(), pki.ca_der.clone()]).await;
        let url = url::Url::parse(&format!("https://{server}")).unwrap();
        assert!(pinned_client_for(&url, &pki.ca_fingerprint).await.is_ok());
        let error = pinned_client_for(&url, &"00".repeat(32)).await.unwrap_err();
        assert!(
            error.to_string().contains("before sending the join token"),
            "{error}"
        );
    }

    #[test]
    fn a_relay_may_name_a_control_plane_on_its_own_host() {
        for control_plane in [
            "https://relay.example.com",
            "https://RELAY.example.com:3000/",
            "https://relay.example.com:8443/api",
        ] {
            assert_eq!(
                relay_registration_trust(
                    "https://relay.example.com",
                    control_plane,
                    None,
                    "cluster-1"
                )
                .unwrap(),
                RelayRegistrationTrust::RelayHost,
                "{control_plane}"
            );
        }
    }

    #[test]
    fn a_relay_naming_another_host_needs_the_pinned_cluster_ca() {
        let error = relay_registration_trust(
            "https://relay.example.com",
            "https://attacker.example.net",
            None,
            "cluster-1",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("attacker.example.net"), "{error}");
        assert!(
            error.contains(
                "temps join cluster-1 --relay-url https://relay.example.com --ca-fingerprint \
                 <fingerprint>"
            ),
            "{error}"
        );
        // A blank fingerprint is no fingerprint.
        assert!(relay_registration_trust(
            "https://relay.example.com",
            "https://attacker.example.net",
            Some("  "),
            "cluster-1",
        )
        .is_err());
        assert_eq!(
            relay_registration_trust(
                "https://relay.example.com",
                "https://10.100.0.1:3000",
                Some(" abcd "),
                "cluster-1",
            )
            .unwrap(),
            RelayRegistrationTrust::PinnedCa("abcd".to_string())
        );
    }

    #[test]
    fn a_relay_cannot_send_the_token_over_plain_http_or_a_bad_url() {
        for control_plane in ["http://relay.example.com", "not a url"] {
            assert!(relay_registration_trust(
                "https://relay.example.com",
                control_plane,
                Some("abcd"),
                "cluster-1"
            )
            .is_err());
        }
    }

    #[test]
    fn the_pairing_code_on_stdin_is_its_first_non_empty_line_trimmed() {
        assert_eq!(
            read_code("\n   \n  tpair1.abc  \nsecond line\n".as_bytes()).unwrap(),
            "tpair1.abc"
        );
        assert_eq!(
            read_code("tpair1.abc\r\n".as_bytes()).unwrap(),
            "tpair1.abc"
        );
        assert_eq!(read_code("tpair1.abc".as_bytes()).unwrap(), "tpair1.abc");
    }

    #[test]
    fn empty_stdin_is_an_error_that_says_so() {
        for input in ["", "\n", "  \n\t\n"] {
            let error = read_code(input.as_bytes()).unwrap_err();
            assert!(error.to_string().contains("stdin was empty"), "{input:?}");
        }
    }

    #[test]
    fn agent_listen_port_reads_ipv4_socket_addr() {
        assert_eq!(agent_listen_port("127.0.0.1:3100"), 3100);
    }

    #[test]
    fn agent_listen_port_reads_bracketed_ipv6_socket_addr() {
        assert_eq!(agent_listen_port("[::1]:8080"), 8080);
    }

    #[test]
    fn agent_listen_port_falls_back_when_bracketed_ipv6_has_no_port() {
        // Regression guard: a naive `.split(':').next_back()` on "[::1]"
        // (no port) would glue the closing bracket onto the extracted
        // "port" instead of recognizing there isn't one.
        assert_eq!(agent_listen_port("[::1]"), 3100);
    }

    #[test]
    fn socket_authority_brackets_ipv6() {
        assert_eq!(socket_authority("fc00::1", 3100), "[fc00::1]:3100");
    }

    #[test]
    fn socket_authority_leaves_ipv4_unbracketed() {
        assert_eq!(socket_authority("10.0.5.20", 3100), "10.0.5.20:3100");
    }

    fn saved_config() -> temps_agent::AgentConfig {
        temps_agent::AgentConfig {
            listen_address: "0.0.0.0:3100".to_string(),
            token: "existing-agent-token".to_string(),
            node_name: "worker-1".to_string(),
            control_plane_url: "https://control.example.com/".to_string(),
            node_id: 7,
            labels: serde_json::json!({}),
            dns_data_dir: std::path::PathBuf::from("/tmp/temps-dns"),
            tls_cert_path: None,
            tls_key_path: None,
            cluster_ca_path: None,
            require_mtls: false,
            underlay_dev: None,
            underlay_mtu: None,
            private_address: Some("10.100.0.7".to_string()),
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: None,
            mesh_key_dir: std::path::PathBuf::from("/tmp/temps-wireguard"),
            wg_endpoint: None,
            control_plane_trust: Some(temps_agent::ControlPlaneTrust::PublicRoots),
        }
    }

    #[test]
    fn test_reenrollment_proves_existing_matching_node_identity() {
        let saved = saved_config();
        assert_eq!(
            prior_token_for_reenrollment(Some(&saved), "worker-1", "https://control.example.com")
                .as_deref(),
            Some("existing-agent-token")
        );
    }

    #[test]
    fn matching_reenrollment_preserves_public_ingress_listener_settings() {
        let mut saved = saved_config();
        saved.public_ingress_address = Some("203.0.113.44".parse().unwrap());
        saved.public_ingress_http_port = 8080;
        saved.public_ingress_https_port = 8443;

        let matched =
            saved_config_for_reenrollment(Some(&saved), "worker-1", "https://control.example.com")
                .expect("same node identity should match");
        assert_eq!(matched.public_ingress_address, saved.public_ingress_address);
        assert_eq!(matched.public_ingress_http_port, 8080);
        assert_eq!(matched.public_ingress_https_port, 8443);
        assert_eq!(
            public_ingress_listener_settings(Some(matched)),
            (saved.public_ingress_address, 8080, 8443)
        );
        let mut produced = saved_config();
        produced.public_ingress_address = None;
        produced.public_ingress_http_port = 80;
        produced.public_ingress_https_port = 443;
        apply_saved_public_ingress_settings(&mut produced, Some(matched));
        assert_eq!(
            produced.public_ingress_address,
            saved.public_ingress_address
        );
        assert_eq!(produced.public_ingress_http_port, 8080);
        assert_eq!(produced.public_ingress_https_port, 8443);
    }

    #[test]
    fn foreign_saved_identity_does_not_supply_public_ingress_settings() {
        let mut saved = saved_config();
        saved.public_ingress_address = Some("203.0.113.44".parse().unwrap());
        saved.public_ingress_http_port = 8080;
        saved.public_ingress_https_port = 8443;

        assert!(saved_config_for_reenrollment(
            Some(&saved),
            "different-worker",
            "https://control.example.com",
        )
        .is_none());
        assert_eq!(public_ingress_listener_settings(None), (None, 80, 443));
        assert!(saved_config_for_reenrollment(
            Some(&saved),
            "worker-1",
            "https://different-control.example.com",
        )
        .is_none());
    }

    #[test]
    fn reenrollment_rotates_public_ingress_encryption_identity() {
        let (old_private, _) = generate_public_ingress_key().unwrap();
        let (new_private, new_public) = generate_public_ingress_key().unwrap();
        let mut saved = saved_config();
        saved.public_ingress_address = Some("203.0.113.44".parse().unwrap());
        saved.public_ingress_http_port = 8080;
        saved.public_ingress_https_port = 8443;
        saved.public_ingress_private_key = Some(old_private.clone());
        let mut produced = saved.clone();
        produced.public_ingress_address = None;
        produced.public_ingress_http_port = 80;
        produced.public_ingress_https_port = 443;
        produced.public_ingress_private_key = Some(new_private.clone());
        apply_saved_public_ingress_settings(&mut produced, Some(&saved));
        let serialized = serde_json::to_vec(&produced).unwrap();
        let persisted: temps_agent::AgentConfig = serde_json::from_slice(&serialized).unwrap();
        let persisted_private = persisted.public_ingress_private_key.as_deref().unwrap();
        assert_eq!(
            persisted.public_ingress_address,
            saved.public_ingress_address
        );
        assert_eq!(persisted.public_ingress_http_port, 8080);
        assert_eq!(persisted.public_ingress_https_port, 8443);
        assert_ne!(persisted_private, old_private);
        let plaintext = b"new certificate bundle";
        let (bundle, ephemeral_public) =
            temps_core::ecies::encrypt_for_edge(&new_public, plaintext).unwrap();

        assert_eq!(
            temps_core::ecies::decrypt_bundle(persisted_private, &ephemeral_public, &bundle)
                .unwrap(),
            plaintext
        );
        assert!(
            temps_core::ecies::decrypt_bundle(&old_private, &ephemeral_public, &bundle).is_err(),
            "the prior enrollment key must not decrypt bundles for the rotated identity"
        );
    }

    #[test]
    fn test_reenrollment_never_leaks_token_to_another_identity_or_control_plane() {
        let saved = saved_config();
        assert!(prior_token_for_reenrollment(
            Some(&saved),
            "another-worker",
            "https://control.example.com"
        )
        .is_none());
        assert!(prior_token_for_reenrollment(
            Some(&saved),
            "worker-1",
            "https://attacker.example.com"
        )
        .is_none());
    }

    /// The registration body must omit the architecture rather than assert
    /// this binary's. The control plane trusts a reported platform: a wrong
    /// one is scheduled on and gets an incompatible image transferred, whereas
    /// an absent one is handled as unverified until the agent reports for real.
    #[test]
    fn test_registration_body_omits_an_unknown_platform() {
        let with_platform = serde_json::json!({
            "name": "worker-1",
            "architecture": Some("linux/arm64"),
        });
        assert_eq!(with_platform["architecture"], "linux/arm64");

        let unknown: Option<&str> = None;
        let without_platform = serde_json::json!({
            "name": "worker-1",
            "architecture": unknown,
        });
        // `null` is what the control plane's `Option<String>` reads as "not
        // reported", which leaves any stored value untouched.
        assert!(
            without_platform["architecture"].is_null(),
            "unknown platform must not be sent as a value: {without_platform}"
        );
        assert_ne!(
            without_platform["architecture"],
            serde_json::json!(temps_deployer::platform::native_platform()),
            "the CLI binary's architecture must never stand in for the daemon's"
        );
    }
}
