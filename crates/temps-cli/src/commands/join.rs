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
use super::host_preflight::PlaneMatch;

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
    /// requires user-managed networking between nodes). Without it (and
    /// without --relay-url), the worker joins outbound only: it dials the
    /// control plane's WireGuard endpoint and needs no address or inbound
    /// port of its own (Linux only).
    #[arg(long)]
    pub private_address: Option<String>,

    /// Listen address for the agent API
    #[arg(long, default_value = "127.0.0.1:3100")]
    pub agent_address: String,

    /// Relay URL for WireGuard key exchange (relay mode). There is no public
    /// relay to default to.
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

    /// Whether this worker takes public traffic (ADR 058 D7): `off` (no
    /// public ports), `fixed` (listen where --listen says), `delegated`
    /// (the console chooses: a tunnel, or open ports on one of this
    /// worker's addresses) or `private` (only a tailnet or LAN address,
    /// `--listen tailscale` or a private IP, ADR 064 D5). Without it a
    /// rejoin keeps the saved setting, and a new worker starts `off`.
    /// Change it later with `temps agent ingress`.
    #[arg(long, value_enum)]
    pub ingress: Option<IngressFlag>,

    /// With `--ingress fixed`: local address to listen on, or `auto` for the
    /// interface holding the default route. With `--ingress private`:
    /// `tailscale`, or a private address of this machine.
    #[arg(long, default_value = "auto")]
    pub listen: String,

    /// With `--ingress private`: let the agent configure this machine's
    /// tailscaled for tailnet names (ADR 065 D2). May be given without a
    /// `--listen` address. The agent must run as root.
    #[arg(long)]
    pub tailscale_serve: bool,

    /// With `--ingress fixed`: local port for public HTTP.
    #[arg(long, default_value_t = 80)]
    pub ingress_http_port: u16,

    /// With `--ingress fixed`: local port for public HTTPS.
    #[arg(long, default_value_t = 443)]
    pub ingress_https_port: u16,

    /// Join even though this machine is already joined to another control
    /// plane, replacing its agent config.
    #[arg(long)]
    pub force: bool,

    /// The control-plane address given on the command line, kept for
    /// `agent.json` after a mesh join replaces `target` with the mesh address.
    #[arg(skip)]
    join_url: Option<String>,

    /// The overlay network name to ask for when this machine already has the
    /// default one (ADR 059); decided by the preflight.
    #[arg(skip)]
    overlay_network_name: Option<String>,

    /// Whether the control plane being joined is the one the saved agent
    /// config belongs to (ADR 060); decided by the preflight for an outbound
    /// join, and by the control plane recognising this node.
    #[arg(skip)]
    plane_match: super::host_preflight::PlaneMatch,

    /// An outbound join sends the saved agent token only to a control plane
    /// that proved it is this node's: one pinned to the cluster CA this
    /// machine trusts, or one that answered the rejoin proof. Set when the
    /// mesh enrollment showed neither (ADR 060).
    #[arg(skip)]
    withhold_prior_token: bool,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressFlag {
    Off,
    Fixed,
    Delegated,
    Private,
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

/// This machine's occupied IPv4 ranges (ADR 056), sent at registration so
/// the control plane hands out an overlay subnet it does not already use.
/// `None` (field omitted) when they cannot be listed: the agent's heartbeat
/// reports them, and the control plane moves an unapplied colliding subnet.
///
/// `overlay` is the overlay this node will run: a network of another name,
/// another cluster's `temps0` included, is reported as occupied.
async fn occupied_ranges_for_registration(
    overlay: Option<&str>,
) -> Option<Vec<temps_network::occupied::OccupiedRange>> {
    let names = overlay
        .and_then(|name| temps_network::OverlayNames::new(name).ok())
        .unwrap_or_default();
    let config = temps_network::NetworkConfig::default().with_overlay_names(&names);
    let docker = bollard::Docker::connect_with_local_defaults().ok();
    let owned_devices = [
        config.bridge_name.as_str(),
        config.vxlan_dev_name.as_str(),
        temps_network::mesh::MESH_INTERFACE,
    ];
    match temps_network::occupied::collect(
        docker.as_ref(),
        &owned_devices,
        &config.docker_network_name,
    )
    .await
    {
        Ok(ranges) => Some(ranges),
        Err(error) => {
            println!(
                "Note: could not list this machine's routes and Docker networks ({error}); the \
                 agent reports them with its first heartbeat instead."
            );
            None
        }
    }
}

/// A client for registering with the control plane, and the trust the agent
/// keeps for it afterwards (persisted in `agent.json`).
struct ControlPlaneClient {
    client: reqwest::Client,
    trust: temps_agent::ControlPlaneTrust,
}

/// The relay receives the join token, so it is only contacted over HTTPS.
/// No loopback exception: there is no local relay to develop against, and a
/// token sent in clear text to the wrong port is still leaked.
fn require_https_relay(relay_url: &str) -> anyhow::Result<()> {
    let relay = url::Url::parse(relay_url)
        .map_err(|error| anyhow::anyhow!("the relay URL '{relay_url}' is invalid: {error}"))?;
    if relay.scheme() != "https" || relay.host_str().is_none_or(str::is_empty) {
        anyhow::bail!(
            "the relay URL '{relay_url}' must be https://<host>: the join token is sent to the \
             relay and is not sent over plain HTTP. Pass --relay-url https://... (or set \
             TEMPS_RELAY_URL)."
        );
    }
    Ok(())
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
    cluster_ca_client(&ca)
}

/// The client for an outbound mesh enrollment (ADR 055). The answer is the
/// join's trust anchor (the CA fingerprint, the control plane's WireGuard key
/// and endpoint), so it must come over HTTPS from the URL the operator gave:
/// plain HTTP is refused except to a loopback address, and redirects are
/// never followed (a redirected POST would re-send the join token).
fn outbound_enroll_client(enroll_url: &str) -> anyhow::Result<reqwest::Client> {
    let url = url::Url::parse(enroll_url)
        .map_err(|error| anyhow::anyhow!("'{enroll_url}' is not a valid URL: {error}"))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        scheme => anyhow::bail!(
            "Joining without an address needs an https:// control plane URL (got {scheme}://): \
             the control plane's answer is what this machine trusts from then on. Use the \
             https:// URL shown in the console, or join with --private-address <ip> on a \
             private network."
        ),
    }
    Ok(reqwest::Client::builder()
        .https_only(!loopback)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(OUTBOUND_ENROLL_CONNECT_TIMEOUT)
        .timeout(OUTBOUND_ENROLL_TIMEOUT)
        .build()?)
}

/// Bounds on the outbound enrollment request: a control plane that accepts
/// the connection but never answers must not hold `temps join` forever.
const OUTBOUND_ENROLL_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const OUTBOUND_ENROLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A client for the control plane that trusts only the cluster CA in
/// `ca_der` and verifies the control plane by its reserved name
/// (`temps_core::node_pki::CONTROL_PLANE_SERVER_NAME`), the same way the agent
/// does after the join, rather than by the URL's host: the cluster CA also
/// signs every worker's leaf for its own addresses.
fn cluster_ca_client(ca_der: &[u8]) -> anyhow::Result<reqwest::Client> {
    let trust = temps_agent::ControlPlaneCa::from_der(ca_der)
        .map_err(|error| anyhow::anyhow!("the pinned cluster CA is unusable: {error}"))?;
    Ok(temps_agent::with_control_plane_trust(
        reqwest::Client::builder().https_only(true),
        Some(trust),
    )
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

/// The saved agent config when this join is the same node joining the same
/// control plane again: same name, and the control plane known to be the
/// same one (ADR 060), or, when that is unknown, at the saved address. A
/// control plane known to be another never gets this node's credential.
fn saved_config_for_reenrollment<'a>(
    saved: Option<&'a temps_agent::AgentConfig>,
    node_name: &str,
    control_plane_url: &str,
    plane: PlaneMatch,
) -> Option<&'a temps_agent::AgentConfig> {
    let saved = saved?;
    let same_control_plane = match plane {
        PlaneMatch::Same => true,
        PlaneMatch::Different => false,
        PlaneMatch::Unknown => {
            saved.control_plane_url.trim_end_matches('/') == control_plane_url.trim_end_matches('/')
        }
    };
    (saved.node_name == node_name && same_control_plane).then_some(saved)
}

fn prior_token_for_reenrollment(
    saved: Option<&temps_agent::AgentConfig>,
    node_name: &str,
    control_plane_url: &str,
    plane: PlaneMatch,
) -> Option<String> {
    saved_config_for_reenrollment(saved, node_name, control_plane_url, plane)
        .map(|saved| saved.token.clone())
}

/// Proof for an outbound join that this machine is the node of its saved
/// agent config (ADR 060): sent unless the control plane is known to be
/// another one. It is bound to this enrollment's `join_token`, so unlike the
/// agent token it is no use to a control plane it was sent to by mistake.
fn rejoin_proof_for(
    saved: Option<&temps_agent::AgentConfig>,
    node_name: &str,
    plane: PlaneMatch,
    wg_public_key: &str,
    join_token: &str,
) -> Option<String> {
    let saved = saved?;
    if plane == PlaneMatch::Different || saved.node_name != node_name || saved.token.is_empty() {
        return None;
    }
    temps_core::node_pki::rejoin_proof(
        &temps_core::node_pki::agent_token_hash(&saved.token),
        node_name,
        wg_public_key,
        join_token,
    )
}

/// Whether an outbound join may send the saved agent token: only to a
/// control plane pinned to a cluster CA this machine already trusts, or one
/// that answered its rejoin proof. Without either, the node re-registers
/// with the join token alone, which updates a node that is not live (or is
/// refused for a live one, and says so).
fn prior_token_may_follow(ca_pinned: bool, acknowledged: bool) -> bool {
    ca_pinned || acknowledged
}

/// Whether the control plane answered this join's rejoin proof with
/// [`temps_core::node_pki::rejoin_ack`] keyed by the saved agent token's
/// hash, which only the control plane this node belongs to holds.
fn rejoin_acknowledged(
    saved: Option<&temps_agent::AgentConfig>,
    proof: Option<&str>,
    ack: Option<&str>,
) -> bool {
    let (Some(saved), Some(proof), Some(ack)) = (saved, proof, ack) else {
        return false;
    };
    temps_core::node_pki::rejoin_ack_matches(
        &temps_core::node_pki::agent_token_hash(&saved.token),
        proof,
        ack,
    )
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

    write_private_key(&key_path, key_pem)?;
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

/// Write a private key readable by its owner only, from the moment the file
/// exists: a fresh file created 0600 (not chmodded after the write, which
/// leaves it readable under the process umask meanwhile) and renamed over
/// the old key, so no descriptor opened on an older, looser file ever sees
/// the new key.
fn write_private_key(path: &std::path::Path, key_pem: &str) -> anyhow::Result<()> {
    use std::io::Write;

    let failed =
        |e: std::io::Error| anyhow::anyhow!("could not write node key '{}': {e}", path.display());
    let temporary = path.with_extension("pem.tmp");
    // Left behind by an interrupted run.
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(failed(e)),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temporary).and_then(|mut file| {
        #[cfg(unix)]
        {
            // Exactly 0600 whatever the umask removed.
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(key_pem.as_bytes())?;
        file.sync_all()
    });
    if let Err(e) = written.and_then(|()| std::fs::rename(&temporary, path)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(failed(e));
    }
    Ok(())
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
        self.preflight().await?;

        // Report the container platform at join time so the control plane can
        // schedule correctly from the very first deploy, instead of waiting up
        // to 30s for the first heartbeat to reveal the architecture.
        let platform = detect_local_platform().await;
        match platform.as_deref() {
            Some(platform) => println!("Container platform: {}", platform),
            None => println!("Container platform: unknown (will be reported by the agent)"),
        }

        match self.mode() {
            JoinMode::Paired(code) => {
                let code = if code == "-" {
                    read_code_from_stdin()?
                } else {
                    code
                };
                self.join_paired(&code, &labels, platform.as_deref())
                    .await?;
            }
            JoinMode::Direct(private_addr) => {
                // A public control-plane URL: public roots only, now and for
                // the agent afterwards.
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
            }
            JoinMode::Relay(relay_url) => {
                self.join_via_relay(&relay_url, &node_name, &labels, platform.as_deref())
                    .await?;
            }
            JoinMode::Outbound => {
                self.join_outbound(&node_name, &labels, platform.as_deref())
                    .await?;
            }
        }

        Ok(())
    }

    /// Check the host before anything spends the join token (ADR 059): stop
    /// when the join cannot work or would take this machine from another
    /// control plane, pick a free overlay network name when the default one
    /// is taken, and warn about held ports.
    async fn preflight(&mut self) -> anyhow::Result<()> {
        use super::host_preflight::{assess, gather, Purpose};

        // A bad `--ingress`/`--listen` would otherwise only surface when the
        // agent config is saved, after the control plane accepted the
        // registration and the join token was spent.
        let requested_ingress = self.requested_ingress()?;
        self.join_url = self.target.clone();
        let saved = load_saved_agent_config();
        let mut facts = gather(saved.as_ref()).await;
        // Relay mode's target is a cluster id and a pairing code carries no
        // address: neither compares with the saved control plane.
        let target = match self.mode() {
            JoinMode::Direct(_) | JoinMode::Outbound => self.target.clone(),
            JoinMode::Paired(_) | JoinMode::Relay(_) => None,
        };
        // Who the target is decides whether this is the saved control plane
        // (ADR 060); asked before the join token goes anywhere.
        if let (Some(_), Some(target)) = (&facts.saved, target.as_deref()) {
            facts.target_identity = Some(super::host_preflight::fetch_plane_identity(target).await);
        }
        // Without `--ingress` a rejoin keeps the saved mode and a new worker
        // starts `off`.
        let ingress = requested_ingress
            .or_else(|| saved.as_ref().map(temps_agent::AgentConfig::local_ingress))
            .unwrap_or_default();
        let (ports, serves_public) = super::host_preflight::served_ports(&ingress);
        let assessment = assess(
            &facts,
            Purpose::Join {
                target: target.as_deref(),
            },
            &ports,
            serves_public,
        );
        for finding in &assessment.findings {
            if !finding.blocks_join(self.force) {
                eprintln!("Warning: {}", finding.message());
            }
        }
        if let Some(blocking) = assessment.blocking(self.force) {
            anyhow::bail!("{}", blocking.message());
        }
        self.overlay_network_name = assessment.overlay_network_name;
        // An outbound join registers over the mesh, pinned to the cluster CA:
        // when the control plane was recognised by the CA this machine
        // already pins, pin that one, so the agent credential is only ever
        // sent to it. A direct join sends it before anything is pinned, so
        // it keeps deciding by address.
        if matches!(self.mode(), JoinMode::Outbound) {
            self.plane_match = assessment.plane;
            if assessment.plane == PlaneMatch::Same && self.ca_fingerprint.is_none() {
                let saved_ca = facts
                    .saved
                    .as_ref()
                    .and_then(|saved| saved.ca_fingerprint.clone());
                let announced_ca = facts
                    .target_identity
                    .as_ref()
                    .and_then(|identity| identity.as_ref().ok())
                    .and_then(|identity| identity.ca_fingerprint.as_ref());
                if announced_ca.is_some() {
                    self.ca_fingerprint = saved_ca;
                }
            }
        } else if assessment.plane == PlaneMatch::Different {
            self.plane_match = PlaneMatch::Different;
        }
        Ok(())
    }

    /// How this machine joins, from the flags: `--pair`, then
    /// `--private-address`, then `--relay-url`; with none of them, outbound
    /// only (ADR 055).
    fn mode(&self) -> JoinMode {
        if let Some(code) = self.pair.clone() {
            JoinMode::Paired(code)
        } else if let Some(private_address) = self.private_address.clone() {
            JoinMode::Direct(private_address)
        } else if let Some(relay_url) = self.relay_url.clone() {
            JoinMode::Relay(relay_url)
        } else {
            JoinMode::Outbound
        }
    }

    /// Save agent config to the agent data directory with restrictive permissions (0600).
    fn save_agent_config(&self, config: &temps_agent::AgentConfig) -> anyhow::Result<()> {
        let mut config = config.clone();
        self.apply_ingress(&mut config)?;
        let config = &config;
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

    /// `--ingress`, else what this worker allowed before a rejoin, else off.
    fn apply_ingress(&self, config: &mut temps_agent::AgentConfig) -> anyhow::Result<()> {
        let Some(ingress) = self.requested_ingress()? else {
            if config.ingress.is_none() {
                let saved = load_saved_agent_config();
                if let Some(saved) = saved_config_for_reenrollment(
                    saved.as_ref(),
                    &config.node_name,
                    &config.control_plane_url,
                    self.plane_match,
                ) {
                    config.ingress = saved.ingress.clone();
                }
            }
            return Ok(());
        };
        config.ingress = Some(ingress);
        // The legacy listen fields would disagree with an explicit mode.
        config.public_ingress_address = None;
        Ok(())
    }

    /// The ingress `--ingress` asks for, validated with its `--listen` and
    /// port flags; `None` without `--ingress`. The preflight calls this so a
    /// bad flag stops the join before anything is registered.
    fn requested_ingress(
        &self,
    ) -> anyhow::Result<Option<temps_core::worker_ingress::LocalIngress>> {
        use temps_core::worker_ingress::{IngressMode, LocalIngress};
        let tailscale_serve_needs_private = || {
            anyhow::anyhow!(
                "--tailscale-serve goes with --ingress private: `temps join … --ingress private \
                 --listen tailscale --tailscale-serve`."
            )
        };
        let Some(flag) = self.ingress else {
            // Consent without the ingress it belongs to would be dropped.
            if self.tailscale_serve {
                return Err(tailscale_serve_needs_private());
            }
            return Ok(None);
        };
        let mode = match flag {
            IngressFlag::Off => IngressMode::Off,
            IngressFlag::Fixed => IngressMode::Fixed,
            IngressFlag::Delegated => IngressMode::Delegated,
            IngressFlag::Private => {
                return super::agent_ingress::private(Some(&self.listen), self.tailscale_serve)
                    .map(Some)
            }
        };
        if self.tailscale_serve {
            return Err(tailscale_serve_needs_private());
        }
        let mut ingress = LocalIngress {
            mode,
            ..LocalIngress::default()
        };
        if mode == IngressMode::Fixed {
            ingress.listen = match self.listen.trim() {
                "" | "auto" => None,
                value => {
                    let ip: std::net::IpAddr = value.parse().map_err(|_| {
                        anyhow::anyhow!("--listen '{value}' is not an IP address. Use an address of this machine, or `auto`.")
                    })?;
                    if ip.is_unspecified() || ip.is_loopback() {
                        anyhow::bail!("--listen {ip} would not take traffic from outside this machine. Use the address of a network interface, or `auto`.");
                    }
                    Some(ip)
                }
            };
            if self.ingress_http_port == 0
                || self.ingress_https_port == 0
                || self.ingress_http_port == self.ingress_https_port
            {
                anyhow::bail!("--ingress-http-port and --ingress-https-port must be two different, non-zero ports.");
            }
            ingress.http_port = self.ingress_http_port;
            ingress.https_port = self.ingress_https_port;
        }
        Ok(Some(ingress))
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
        let matching_saved = saved_config_for_reenrollment(
            saved_config.as_ref(),
            node_name,
            self.target(),
            self.plane_match,
        );
        let prior_token = prior_token_for_reenrollment(
            saved_config.as_ref(),
            node_name,
            self.target(),
            self.plane_match,
        )
        .filter(|_| !self.withhold_prior_token);

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
            // ADR 056: the control plane allocates this node's overlay
            // subnet around what it already uses.
            "occupied_ranges": occupied_ranges_for_registration(
                self.overlay_network_name.as_deref(),
            )
            .await,
            // ADR 059: absent unless this machine already has the default
            // overlay name for another network.
            "overlay_network_name": self.overlay_network_name,
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
            ingress: None,
            mesh_key_dir: crate::commands::agent::agent_data_dir().join("wireguard"),
            wg_endpoint: wg_endpoint.map(|endpoint| endpoint.to_string()),
            control_plane_trust: Some(control_plane.trust),
            control_plane_legacy_cert_path: None,
            join_url: self.join_url.clone(),
            overlay_network_name: self.overlay_network_name.clone(),
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
            ingress: None,
            mesh_key_dir: key_dir,
            wg_endpoint: Some(code.node_endpoint.to_string()),
            // Only brings the mesh up; it never calls the control plane.
            control_plane_trust: Some(temps_agent::ControlPlaneTrust::PublicRoots),
            control_plane_legacy_cert_path: None,
            join_url: None,
            overlay_network_name: None,
        };
        let cidr = ipnet::Ipv4Net::new(code.node_address, code.prefix_len)?.trunc();
        temps_agent::network_sync::bootstrap_mesh(
            &mesh_config,
            &temps_agent::network_sync::MeshBootstrap {
                cidr,
                listen_port: code.listen_port,
                endpoint: Some(code.node_endpoint),
                address: code.node_address,
                control_plane_public_key: code.control_plane_public_key.clone(),
                control_plane_endpoint: code.control_plane_endpoint.clone(),
                control_plane_address: code.control_plane_address,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("could not bring up the WireGuard mesh: {error}"))?;

        // 3. Register over the mesh against the pinned cluster CA.
        self.register_over_mesh(
            MeshRegistration {
                node_api: std::net::SocketAddr::new(
                    std::net::IpAddr::V4(code.control_plane_address),
                    code.node_api_port,
                ),
                ca_fingerprint: &code.ca_fingerprint,
                join_token: &code.join_token,
                node_name: &code.name,
                private_address: &node_ip,
                wg_endpoint: Some(code.node_endpoint),
            },
            labels,
            platform,
        )
        .await
    }

    /// The second half of a mesh join (`--pair`, and the outbound-only join of
    /// ADR 055): with this machine's end of the mesh up, register over the
    /// node API on the control plane's mesh address, verifying it against the
    /// cluster CA pinned by `ca_fingerprint`. The agent keeps reaching the
    /// control plane there, with the same trust.
    async fn register_over_mesh(
        &mut self,
        mesh: MeshRegistration<'_>,
        labels: &serde_json::Value,
        platform: Option<&str>,
    ) -> anyhow::Result<()> {
        let node_api = mesh.node_api;
        println!("Mesh is up. Reaching the control plane at {node_api} over it...");
        let ca_der = pinned_cluster_ca(node_api, mesh.ca_fingerprint).await?;
        // Verified by the reserved control-plane name, not the mesh address:
        // the cluster CA also signs workers' leaves for their addresses.
        let client = cluster_ca_client(&ca_der)?;
        self.target = Some(format!("https://{node_api}"));
        self.token = Some(mesh.join_token.to_string());
        self.ca_fingerprint = Some(mesh.ca_fingerprint.to_string());
        // The agent keeps reaching the control plane at this mesh address,
        // verified against the same pinned cluster CA and nothing else.
        let control_plane = ControlPlaneClient {
            client,
            trust: temps_agent::ControlPlaneTrust::ClusterCa,
        };
        self.join_direct(
            mesh.node_name,
            mesh.private_address,
            labels,
            platform,
            control_plane,
            mesh.wg_endpoint,
        )
        .await
    }

    /// Outbound only (ADR 055): no `--private-address`, `--relay-url` or
    /// `--pair`. This machine sends its WireGuard public key to the control
    /// plane over HTTPS with the join token, brings the mesh up with the
    /// control plane as its only peer, dials the control plane's endpoint
    /// (keepalives hold the NAT mapping open), and registers over the mesh
    /// under the mesh address the control plane reserved for it. It needs no
    /// address and no inbound port of its own.
    async fn join_outbound(
        &mut self,
        node_name: &str,
        labels: &serde_json::Value,
        platform: Option<&str>,
    ) -> anyhow::Result<()> {
        use temps_deployments::handlers::nodes::MeshEnrollResponse;

        if !cfg!(target_os = "linux") {
            anyhow::bail!(
                "With no --private-address, this machine joins over a WireGuard tunnel it \
                 dials itself, which needs kernel WireGuard: run `temps join` on Linux. On \
                 another OS, pass --private-address <ip> with an address the control plane can \
                 reach."
            );
        }
        println!(
            "Joining outbound only: this machine dials the control plane and needs no inbound \
             port. (Pass --private-address <ip> to register at an address instead.)"
        );

        let key_dir = crate::commands::agent::agent_data_dir().join("wireguard");
        let key = {
            let dir = key_dir.clone();
            tokio::task::spawn_blocking(move || {
                temps_wireguard::mesh::MeshKey::load_or_create(&dir)
            })
            .await??
        };

        // 1. Hand the control plane our key; it reserves a mesh address.
        // Public roots, HTTPS only and no redirects: this answer carries the
        // CA fingerprint the rest of the join pins.
        let enroll_url = management_api_url(self.target(), "/internal/nodes/mesh-enroll");
        // ADR 060: when this machine is already a node of this control plane,
        // it proves so and is updated in place instead of refused.
        let rejoin_proof = rejoin_proof_for(
            load_saved_agent_config().as_ref(),
            node_name,
            self.plane_match,
            key.public_key(),
            self.token(),
        );
        let response = outbound_enroll_client(&enroll_url)?
            .post(&enroll_url)
            .json(&serde_json::json!({
                "name": node_name,
                "join_token": self.token(),
                "wg_public_key": key.public_key(),
                "labels": labels,
                "rejoin_proof": rejoin_proof,
            }))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!(
                        "The control plane at {enroll_url} did not answer within {}s. No \
                         network was set up on this machine. Check that the control plane is \
                         up, then re-run `temps join`; if the token is refused as already used, \
                         create a new one in Worker Nodes → Add node. A pending enrollment \
                         from this machine is resumed, not duplicated.",
                        OUTBOUND_ENROLL_TIMEOUT.as_secs()
                    )
                } else {
                    anyhow::anyhow!(
                        "Could not reach the control plane at {enroll_url}: {error}. Check the \
                         URL and that this machine can reach it over HTTPS."
                    )
                }
            })?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!(
                "The control plane at {target} does not support joining without an address \
                 (it answered 404 for {enroll_url}). Upgrade it, or join with \
                 --private-address <ip>.",
                target = self.target()
            );
        }
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("another address");
            anyhow::bail!(
                "{enroll_url} redirects ({status}) to {location}; the join token is not sent \
                 on to a redirect. Run `temps join` with the control plane's final https:// \
                 URL."
            );
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "The control plane refused to enroll this machine ({status}): {}",
                problem_detail(&body)
            );
        }
        let answer: MeshEnrollResponse = response.json().await.map_err(|error| {
            anyhow::anyhow!(
                "The control plane's answer to {enroll_url} is not a mesh enrollment: {error}. \
                 Upgrade the Temps CLI on this machine to match the control plane."
            )
        })?;
        let parse_address = |what: &str, value: &str| {
            value.parse::<std::net::Ipv4Addr>().map_err(|error| {
                anyhow::anyhow!(
                    "The control plane announced an invalid {what} '{value}': {error}. Upgrade \
                     the Temps CLI on this machine to match the control plane."
                )
            })
        };
        let node_address = parse_address("mesh address for this machine", &answer.node_address)?;
        let control_plane_address =
            parse_address("control plane mesh address", &answer.control_plane_address)?;
        // Checked before anything is set up, like `verify_ca_fingerprint`
        // after a direct registration.
        if let Some(expected) = self.ca_fingerprint.as_deref() {
            if !answer.ca_fingerprint.eq_ignore_ascii_case(expected.trim()) {
                anyhow::bail!(
                    "Cluster CA fingerprint mismatch — expected {expected}, the control plane \
                     announced {actual}. Aborting join (possible man-in-the-middle); nothing \
                     was set up on this machine.",
                    actual = answer.ca_fingerprint
                );
            }
            println!("Cluster CA fingerprint verified.");
        }
        // Only a control plane holding this node's agent token hash can
        // answer the proof: then it is the one this node belongs to. A bare
        // `rejoined_node_id` proves nothing, so it changes nothing.
        let acknowledged = answer.rejoined_node_id.is_some()
            && rejoin_acknowledged(
                load_saved_agent_config().as_ref(),
                rejoin_proof.as_deref(),
                answer.rejoin_ack.as_deref(),
            );
        if acknowledged {
            self.plane_match = PlaneMatch::Same;
        }
        // Registration over the mesh is pinned to the CA this answer
        // announces, which any control plane can choose. The saved agent
        // token goes only where it is known to belong: a CA this machine
        // already pinned (checked above), or a proof answered.
        self.withhold_prior_token =
            !prior_token_may_follow(self.ca_fingerprint.is_some(), acknowledged);
        if let Some(node_id) = answer.rejoined_node_id {
            println!(
                "This machine is already node '{}' (id {node_id}) of this control plane: it is \
                 updated in place and keeps its mesh address {node_address}. Bringing up the \
                 WireGuard mesh (dialing the control plane at {})...",
                answer.name, answer.control_plane_endpoint
            );
        } else {
            println!(
                "Reserved mesh address {node_address}. Bringing up the WireGuard mesh (dialing \
                 the control plane at {})...",
                answer.control_plane_endpoint
            );
        }

        // 2. The mesh, with the control plane as the only peer. No endpoint
        // of our own: the control plane learns it from our handshakes.
        let private_address = node_address.to_string();
        let mesh_config = temps_agent::AgentConfig {
            listen_address: self.agent_address.clone(),
            token: String::new(),
            node_name: answer.name.clone(),
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
            private_address: Some(private_address.clone()),
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: None,
            ingress: None,
            mesh_key_dir: key_dir,
            wg_endpoint: None,
            // Only brings the mesh up; it never calls the control plane.
            control_plane_trust: Some(temps_agent::ControlPlaneTrust::PublicRoots),
            control_plane_legacy_cert_path: None,
            join_url: None,
            overlay_network_name: None,
        };
        let cidr = ipnet::Ipv4Net::new(node_address, answer.prefix_len)?.trunc();
        temps_agent::network_sync::bootstrap_mesh(
            &mesh_config,
            &temps_agent::network_sync::MeshBootstrap {
                cidr,
                listen_port: answer.listen_port,
                endpoint: None,
                address: node_address,
                control_plane_public_key: answer.control_plane_public_key.clone(),
                control_plane_endpoint: Some(answer.control_plane_endpoint.clone()),
                control_plane_address,
            },
        )
        .await
        .map_err(|error| match answer.rejoined_node_id {
            Some(node_id) => anyhow::anyhow!(
                "could not bring up the WireGuard mesh: {error}. Node '{name}' (id {node_id}) \
                 was not changed on the control plane: fix the problem, then run `temps join` \
                 again on this machine with the same name and a new token.",
                name = answer.name
            ),
            None => anyhow::anyhow!(
                "could not bring up the WireGuard mesh: {error}. The control plane holds this \
                 machine's enrollment as pending pairing '{name}' until {expires}: fix the \
                 problem, then run `temps join` again on this machine with the same name and a \
                 new token, which resumes it (or cancel it on the Worker Nodes page).",
                name = answer.name,
                expires = answer.expires_at
            ),
        })?;

        // 3. Register over the mesh, under our own mesh address.
        self.register_over_mesh(
            MeshRegistration {
                node_api: std::net::SocketAddr::new(
                    std::net::IpAddr::V4(control_plane_address),
                    answer.node_api_port,
                ),
                ca_fingerprint: &answer.ca_fingerprint,
                join_token: &answer.join_token,
                node_name: &answer.name,
                private_address: &private_address,
                wg_endpoint: None,
            },
            labels,
            platform,
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
        // The join token goes to the relay: refuse before anything is set up.
        require_https_relay(relay_url)?;
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

        // Step 3: Contact relay to join cluster. HTTPS only, redirects
        // included: the request carries the join token.
        let client = reqwest::Client::builder().https_only(true).build()?;

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
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "could not set up the WireGuard interface: {}",
                    temps_network::with_permission_hint(error.to_string())
                )
            })?;

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
            RelayRegistrationTrust::RelayHost => {
                reqwest::Client::builder().https_only(true).build()?
            }
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
            self.plane_match,
        );
        let prior_token = prior_token_for_reenrollment(
            saved_config.as_ref(),
            node_name,
            relay_response.control_plane_url.as_str(),
            self.plane_match,
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
            // ADR 056: the control plane allocates this node's overlay
            // subnet around what it already uses.
            "occupied_ranges": occupied_ranges_for_registration(
                self.overlay_network_name.as_deref(),
            )
            .await,
            // ADR 059: absent unless this machine already has the default
            // overlay name for another network.
            "overlay_network_name": self.overlay_network_name,
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
            ingress: None,
            mesh_key_dir: crate::commands::agent::agent_data_dir().join("wireguard"),
            wg_endpoint: None,
            control_plane_trust: Some(trust.agent_trust()),
            control_plane_legacy_cert_path: None,
            join_url: self.join_url.clone(),
            overlay_network_name: self.overlay_network_name.clone(),
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

/// How `temps join` enrolls this machine; see [`JoinCommand::mode`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum JoinMode {
    /// `--pair <code>`: the control plane dials this machine (ADR 048 D2b).
    Paired(String),
    /// `--private-address <ip>`: register at that address.
    Direct(String),
    /// `--relay-url <url>`: an operator-run relay exchanges WireGuard keys.
    Relay(String),
    /// No address at all: this machine dials the control plane (ADR 055).
    Outbound,
}

/// What [`JoinCommand::register_over_mesh`] registers with.
struct MeshRegistration<'a> {
    /// The node API on the control plane's mesh address.
    node_api: std::net::SocketAddr,
    /// SHA-256 fingerprint of the cluster CA the node API must present.
    ca_fingerprint: &'a str,
    /// The pairing's single-use join token.
    join_token: &'a str,
    node_name: &'a str,
    /// The address this node registers at: its public IP for `--pair`, its
    /// own mesh address for an outbound-only join.
    private_address: &'a str,
    /// Where other members dial this node, when anyone can.
    wg_endpoint: Option<std::net::SocketAddr>,
}

/// The `detail` of a problem+json body, else the body itself.
fn problem_detail(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            let detail = value.get("detail")?.as_str()?.to_string();
            Some(match value.get("title").and_then(|title| title.as_str()) {
                Some(title) => format!("{title}: {detail}"),
                None => detail,
            })
        })
        .unwrap_or_else(|| body.to_string())
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
    #[cfg(unix)]
    use super::write_private_key;
    use super::{outbound_enroll_client, problem_detail, JoinCommand, JoinMode};

    /// The outbound enrollment's answer is the join's trust anchor: plain
    /// HTTP is refused except to loopback, so no one on the path can read the
    /// token or hand this machine their own CA.
    #[test]
    fn an_outbound_enrollment_needs_https_except_on_loopback() {
        let enroll = "/api/internal/nodes/mesh-enroll";
        assert!(outbound_enroll_client(&format!("https://cp.example.com{enroll}")).is_ok());
        assert!(outbound_enroll_client(&format!("http://127.0.0.1:8080{enroll}")).is_ok());
        assert!(outbound_enroll_client(&format!("http://localhost{enroll}")).is_ok());
        assert!(outbound_enroll_client(&format!("http://[::1]{enroll}")).is_ok());
        for refused in [
            format!("http://cp.example.com{enroll}"),
            format!("http://10.0.0.1{enroll}"),
            format!("ftp://cp.example.com{enroll}"),
        ] {
            let error = outbound_enroll_client(&refused).unwrap_err().to_string();
            assert!(error.contains("https://"), "{refused}: {error}");
        }
    }

    #[derive(clap::Parser)]
    struct JoinCli {
        #[command(flatten)]
        join: JoinCommand,
    }

    fn parse_join(args: &[&str]) -> Result<JoinCommand, clap::Error> {
        use clap::Parser;
        let argv = std::iter::once("temps-join").chain(args.iter().copied());
        JoinCli::try_parse_from(argv).map(|cli| cli.join)
    }

    /// The preflight refuses a bad ingress flag before the join token is
    /// spent, for every mode, not only once the config is saved.
    #[test]
    fn ingress_flags_are_checked_before_registration() {
        let join = |args: &[&str]| {
            let mut argv = vec!["https://cp.example.com", "token"];
            argv.extend_from_slice(args);
            parse_join(&argv).unwrap()
        };
        assert!(join(&[]).requested_ingress().unwrap().is_none());
        for refused in [
            &["--ingress", "private"][..],
            &["--ingress", "private", "--listen", "203.0.113.7"],
            &["--ingress", "private", "--listen", "eth0"],
            &["--ingress", "fixed", "--listen", "not-an-ip"],
            &["--ingress", "fixed", "--listen", "127.0.0.1"],
            &["--ingress", "fixed", "--ingress-http-port", "443"],
            &["--tailscale-serve"],
            &["--ingress", "fixed", "--tailscale-serve"],
        ] {
            assert!(join(refused).requested_ingress().is_err(), "{refused:?}");
        }
        for accepted in [
            &["--ingress", "off"][..],
            &["--ingress", "delegated"],
            &["--ingress", "fixed"],
            &["--ingress", "private", "--listen", "tailscale"],
            &["--ingress", "private", "--listen", "192.168.1.8"],
        ] {
            assert!(
                join(accepted).requested_ingress().unwrap().is_some(),
                "{accepted:?}"
            );
        }
    }

    /// ADR 055: a URL and a token with no address is a join of its own,
    /// outbound only, no longer an error. The other modes are unchanged.
    #[test]
    fn a_join_with_no_address_is_outbound_only() {
        // Clap would fill --relay-url from the environment.
        if std::env::var_os("TEMPS_RELAY_URL").is_some() {
            return;
        }
        let join = parse_join(&["https://cp.example.com", "token"]).unwrap();
        assert_eq!(join.mode(), JoinMode::Outbound);
        assert_eq!(join.target.as_deref(), Some("https://cp.example.com"));

        let join = parse_join(&[
            "https://cp.example.com",
            "token",
            "--private-address",
            "10.0.0.5",
        ])
        .unwrap();
        assert_eq!(join.mode(), JoinMode::Direct("10.0.0.5".into()));
        let join = parse_join(&[
            "abc123",
            "token",
            "--relay-url",
            "https://relay.example.com",
        ])
        .unwrap();
        assert_eq!(
            join.mode(),
            JoinMode::Relay("https://relay.example.com".into())
        );
        let join = parse_join(&["--pair", "-"]).unwrap();
        assert_eq!(join.mode(), JoinMode::Paired("-".into()));
        // `--pair` still refuses a URL or an address next to it.
        assert!(parse_join(&["--pair", "-", "--private-address", "10.0.0.5"]).is_err());
    }

    #[test]
    fn a_refusal_shows_the_problems_title_and_detail() {
        assert_eq!(
            problem_detail(r#"{"title":"WireGuard Mesh Off","detail":"Turn it on."}"#),
            "WireGuard Mesh Off: Turn it on."
        );
        assert_eq!(problem_detail("bad gateway"), "bad gateway");
    }
    use super::{
        agent_listen_port, apply_saved_public_ingress_settings, find_pinned_ca,
        generate_public_ingress_key, pinned_client_for, pinned_cluster_ca,
        prior_token_for_reenrollment, prior_token_may_follow, public_ingress_listener_settings,
        read_code, rejoin_proof_for, relay_registration_trust, require_https_relay,
        saved_config_for_reenrollment, socket_authority, PlaneMatch, RelayRegistrationTrust,
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
    fn the_pinned_cluster_ca_becomes_the_only_trust_for_the_control_plane() {
        let pki = test_pki();
        assert!(super::cluster_ca_client(&pki.ca_der).is_ok());
        let error = super::cluster_ca_client(b"not a certificate").unwrap_err();
        assert!(error.to_string().contains("pinned cluster CA"), "{error}");
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

    #[cfg(unix)]
    #[test]
    fn a_node_key_is_owner_only_from_creation_and_on_rewrite() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.key.pem");
        write_private_key(&path, "KEY-1").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "KEY-1");

        // A key left world-readable by an older version is tightened, and
        // the shorter new key leaves nothing of the old one behind.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private_key(&path, "K2").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "K2");
    }

    #[test]
    fn the_join_token_only_goes_to_an_https_relay() {
        assert!(require_https_relay("https://relay.example.com").is_ok());
        assert!(require_https_relay("https://relay.example.com:8443/base").is_ok());
        for relay in [
            "http://relay.example.com",
            "http://127.0.0.1:8080",
            "HTTP://relay.example.com",
            "ws://relay.example.com",
            "relay.example.com",
            "not a url",
            "",
        ] {
            let error = require_https_relay(relay).unwrap_err().to_string();
            assert!(error.contains(&format!("'{relay}'")), "{relay}: {error}");
        }
        let error = require_https_relay("http://relay.example.com")
            .unwrap_err()
            .to_string();
        assert!(error.contains("--relay-url https://"), "{error}");
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
            ingress: None,
            mesh_key_dir: std::path::PathBuf::from("/tmp/temps-wireguard"),
            wg_endpoint: None,
            control_plane_trust: Some(temps_agent::ControlPlaneTrust::PublicRoots),
            control_plane_legacy_cert_path: None,
            join_url: None,
            overlay_network_name: None,
        }
    }

    #[test]
    fn test_reenrollment_proves_existing_matching_node_identity() {
        let saved = saved_config();
        assert_eq!(
            prior_token_for_reenrollment(
                Some(&saved),
                "worker-1",
                "https://control.example.com",
                PlaneMatch::Unknown,
            )
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

        let matched = saved_config_for_reenrollment(
            Some(&saved),
            "worker-1",
            "https://control.example.com",
            PlaneMatch::Unknown,
        )
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
            PlaneMatch::Unknown,
        )
        .is_none());
        assert_eq!(public_ingress_listener_settings(None), (None, 80, 443));
        assert!(saved_config_for_reenrollment(
            Some(&saved),
            "worker-1",
            "https://different-control.example.com",
            PlaneMatch::Unknown,
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
    fn an_outbound_join_sends_the_saved_token_only_to_a_proven_control_plane() {
        // A mesh-key match alone proves nothing: the answer's CA is the
        // control plane's own choice.
        assert!(!prior_token_may_follow(false, false));
        // A CA this machine already pinned, or an answered rejoin proof.
        assert!(prior_token_may_follow(true, false));
        assert!(prior_token_may_follow(false, true));
        assert!(prior_token_may_follow(true, true));
    }

    #[test]
    fn test_reenrollment_never_leaks_token_to_another_identity_or_control_plane() {
        let saved = saved_config();
        assert!(prior_token_for_reenrollment(
            Some(&saved),
            "another-worker",
            "https://control.example.com",
            PlaneMatch::Same,
        )
        .is_none());
        assert!(prior_token_for_reenrollment(
            Some(&saved),
            "worker-1",
            "https://attacker.example.com",
            PlaneMatch::Unknown,
        )
        .is_none());
        // Known to be another control plane: not even at the saved address.
        assert!(prior_token_for_reenrollment(
            Some(&saved),
            "worker-1",
            "https://control.example.com",
            PlaneMatch::Different,
        )
        .is_none());
    }

    /// ADR 060: the same control plane under another address (an agent
    /// that saved the shared mesh address) still gets the node's credential.
    #[test]
    fn the_same_control_plane_under_another_address_is_a_reenrollment() {
        let saved = saved_config();
        assert_eq!(
            prior_token_for_reenrollment(
                Some(&saved),
                "worker-1",
                "https://10.201.0.1:51820",
                PlaneMatch::Same,
            )
            .as_deref(),
            Some("existing-agent-token")
        );
    }

    /// The re-join proof is the node's token hash bound to this join token,
    /// never the token itself, and is not sent to a control plane known to
    /// be another or for another name.
    #[test]
    fn the_rejoin_proof_is_bound_and_never_the_token() {
        let saved = saved_config();
        let proof = rejoin_proof_for(
            Some(&saved),
            "worker-1",
            PlaneMatch::Unknown,
            "key",
            "join-1",
        )
        .expect("a proof for the same node");
        assert!(!proof.contains("existing-agent-token"));
        assert_eq!(
            Some(proof.clone()),
            temps_core::node_pki::rejoin_proof(
                &temps_core::node_pki::agent_token_hash("existing-agent-token"),
                "worker-1",
                "key",
                "join-1",
            )
        );
        assert_ne!(
            rejoin_proof_for(Some(&saved), "worker-1", PlaneMatch::Same, "key", "join-2"),
            Some(proof)
        );
        assert_eq!(
            rejoin_proof_for(
                Some(&saved),
                "worker-1",
                PlaneMatch::Different,
                "key",
                "join-1"
            ),
            None
        );
        assert_eq!(
            rejoin_proof_for(Some(&saved), "worker-2", PlaneMatch::Same, "key", "join-1"),
            None
        );
        assert_eq!(
            rejoin_proof_for(None, "worker-1", PlaneMatch::Same, "key", "join-1"),
            None
        );
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
