//! `temps join` subcommand — joins a worker node to an existing cluster.
//!
//! Supports two modes:
//! - **Relay mode** (default): Uses `api.temps.sh` relay for WireGuard key exchange
//! - **Direct mode** (`--private-address`): Skips relay, uses user-managed networking
//!
//! After registration, saves the agent config to `~/.temps/agent.json` and exits.
//! Run `temps agent` separately to start the worker.

use clap::Args;

/// Join this machine to a Temps cluster as a worker node
#[derive(Args)]
pub struct JoinCommand {
    /// Cluster ID or control plane URL (e.g. "abc123" for relay mode,
    /// or "https://control-plane:3000" for direct mode)
    pub target: String,

    /// Join token provided by the cluster admin (prefer TEMPS_JOIN_TOKEN env var)
    #[arg(env = "TEMPS_JOIN_TOKEN")]
    pub token: String,

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

    /// Relay URL for WireGuard key exchange
    #[arg(long, default_value = "https://api.temps.sh", env = "TEMPS_RELAY_URL")]
    pub relay_url: String,

    /// Labels for node scheduling (key=value pairs)
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Expected SHA-256 fingerprint of the cluster CA, shown when the enrollment
    /// token was minted. When set, the join aborts if the CA returned by the
    /// control plane doesn't match — defeating a man-in-the-middle that swaps in
    /// its own CA (ADR-020 WS-2.2).
    #[arg(long)]
    pub ca_fingerprint: Option<String>,
}

/// Response body from the control plane registration endpoint.
#[derive(serde::Deserialize)]
struct RegisterResponse {
    id: i32,
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

/// Generate a per-node keypair + CSR. The private key never leaves this host.
/// `ip` is the address the control plane will connect to (the node's
/// private/WG IP) and MUST be a SAN, or the CP's server-cert hostname check
/// fails (ADR-020 WS-2.1).
fn generate_node_tls_material(node_name: &str, ip: &str) -> Option<NodeTlsMaterial> {
    let sans = vec![ip.to_string(), node_name.to_string()];
    match temps_core::node_pki::generate_node_keypair_csr(node_name, &sans) {
        Ok(csr) => Some(NodeTlsMaterial {
            key_pem: csr.key_pem,
            csr_pem: csr.csr_pem,
        }),
        Err(e) => {
            eprintln!("Warning: could not generate node TLS material ({e}); joining without mTLS.");
            None
        }
    }
}

/// Write the node key + leaf cert + cluster CA to the agent data dir (key 0600)
/// and return their paths for the agent config. Best-effort: on any IO error we
/// warn and return None so the node still joins (over plaintext HTTP).
fn write_node_certs(
    key_pem: &str,
    cert_pem: &str,
    ca_cert_pem: &str,
) -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    let dir = crate::commands::agent::agent_data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("Warning: could not create agent data dir for certs: {e}");
        return None;
    }
    let key_path = dir.join("node.key.pem");
    let cert_path = dir.join("node.cert.pem");
    let ca_path = dir.join("cluster-ca.pem");

    if let Err(e) = std::fs::write(&key_path, key_pem) {
        eprintln!("Warning: could not write node key: {e}");
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::write(&cert_path, cert_pem) {
        eprintln!("Warning: could not write node cert: {e}");
        return None;
    }
    if let Err(e) = std::fs::write(&ca_path, ca_cert_pem) {
        eprintln!("Warning: could not write cluster CA: {e}");
        return None;
    }
    Some((cert_path, key_path, ca_path))
}

/// Persist the signed leaf + cluster CA from the register response, returning
/// the `(cert, key, ca)` paths for the agent config. Returns `None` (so the
/// node serves plaintext HTTP) when no CSR was sent or the CP did not sign one.
fn persist_tls(
    material: &Option<NodeTlsMaterial>,
    response: &RegisterResponse,
) -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    let material = material.as_ref()?;
    let cert_pem = response.cert_pem.as_ref()?;
    let ca_cert_pem = response.ca_cert_pem.as_ref()?;
    let paths = write_node_certs(&material.key_pem, cert_pem, ca_cert_pem)?;
    println!("mTLS certificate provisioned — the agent will serve TLS.");
    Some(paths)
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

        if let Some(private_addr) = self.private_address.clone() {
            self.join_direct(&node_name, &private_addr, &labels).await?;
        } else {
            self.join_via_relay(&node_name, &labels).await?;
        }

        Ok(())
    }

    /// Save agent config to `~/.temps/agent.json` with restrictive permissions (0600).
    fn save_agent_config(&self, config: &temps_agent::AgentConfig) -> anyhow::Result<()> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let temps_dir = home.join(".temps");
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

    /// Direct mode: register with control plane using provided private address.
    async fn join_direct(
        &self,
        node_name: &str,
        private_address: &str,
        labels: &serde_json::Value,
    ) -> anyhow::Result<()> {
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
        // opt-in does NOT apply to CLI binaries on purpose.
        let client = reqwest::Client::builder().build()?;

        let register_url = format!("{}/api/internal/nodes/register", self.target);

        // Generate per-node mTLS material; send the CSR so the control plane
        // can sign a leaf for us (ADR-020 WS-2.1). The leaf must be valid for
        // the private address the CP connects to.
        let tls_material = generate_node_tls_material(node_name, private_address.trim());

        let register_body = serde_json::json!({
            "name": node_name,
            "token": agent_token,
            "join_token": self.token,
            "address": format!("http://{}:{}", private_address.trim(), self.agent_address.split(':').next_back().unwrap_or("3100").trim()),
            "private_address": private_address,
            "labels": labels,
            "csr_pem": tls_material.as_ref().map(|m| m.csr_pem.clone()),
        });

        let response = client
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

        // Verify the cluster CA out of band before trusting it (ADR-020 WS-2.2):
        // if the operator passed the expected fingerprint, the CA the control
        // plane returned must match it, or a MITM could have swapped its own CA.
        if let Some(expected) = self.ca_fingerprint.as_deref() {
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
                }
                None => anyhow::bail!(
                    "--ca-fingerprint was provided but the control plane returned no CA certificate."
                ),
            }
        }

        // Persist the signed leaf + cluster CA so `temps agent` can serve mTLS.
        let tls_paths = persist_tls(&tls_material, &register_response);

        // Save config for `temps agent`
        let config = temps_agent::AgentConfig {
            listen_address: self.agent_address.clone(),
            token: agent_token,
            node_name: node_name.to_string(),
            control_plane_url: self.target.clone(),
            node_id: register_response.id,
            labels: labels.clone(),
            dns_data_dir: crate::commands::agent::agent_data_dir().join("dns"),
            tls_cert_path: tls_paths.as_ref().map(|p| p.0.clone()),
            tls_key_path: tls_paths.as_ref().map(|p| p.1.clone()),
            cluster_ca_path: tls_paths.as_ref().map(|p| p.2.clone()),
        };
        self.save_agent_config(&config)?;

        println!();
        println!("Run 'temps agent' to start the worker.");

        Ok(())
    }

    /// Relay mode: use Temps Cloud relay for WireGuard key exchange.
    async fn join_via_relay(
        &self,
        node_name: &str,
        labels: &serde_json::Value,
    ) -> anyhow::Result<()> {
        println!("Using relay mode via {}...", self.relay_url);

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

        let join_url = format!("{}/api/relay/clusters/{}/join", self.relay_url, self.target);

        // Detect our public endpoint (for WireGuard)
        let public_endpoint = detect_public_endpoint(wg_manager.listen_port()).await;

        let join_body = serde_json::json!({
            "join_token": self.token,
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
            #[serde(default)]
            node_id: i32,
        }

        let relay_response: RelayJoinResponse = response.json().await?;

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
        // Traffic is encrypted by WireGuard, but the inner HTTP request
        // still uses the operator's TLS cert. Strict verification is
        // mandatory: this exchange carries the join token, and a MitM
        // (even one fronting a self-signed cert behind the tunnel) could
        // hijack worker registration.
        let register_client = reqwest::Client::builder().build()?;

        let register_url = format!(
            "{}/api/internal/nodes/register",
            relay_response.control_plane_url
        );

        let agent_port = self
            .agent_address
            .split(':')
            .next_back()
            .unwrap_or("3100")
            .trim();

        // Generate per-node mTLS material and send the CSR (ADR-020 WS-2.1).
        // The leaf must be valid for the WG IP the CP connects to.
        let tls_material = generate_node_tls_material(node_name, &relay_response.assigned_ip);

        let register_body = serde_json::json!({
            "name": node_name,
            "token": relay_response.agent_token,
            "join_token": self.token,
            "address": format!("http://{}:{}", relay_response.assigned_ip, agent_port),
            "private_address": relay_response.assigned_ip,
            "wg_public_key": keypair.public_key,
            "public_endpoint": public_endpoint,
            "labels": labels,
            "csr_pem": tls_material.as_ref().map(|m| m.csr_pem.clone()),
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

        // Parse the register response (node_id + signed certs); fall back to the
        // relay-provided node_id if the body can't be parsed.
        let register_response: RegisterResponse = match response.json().await {
            Ok(r) => r,
            Err(_) => RegisterResponse {
                id: relay_response.node_id,
                cert_pem: None,
                ca_cert_pem: None,
            },
        };
        let node_id = register_response.id;

        println!(
            "Registered with control plane successfully (node_id={}).",
            node_id
        );

        let tls_paths = persist_tls(&tls_material, &register_response);

        // Save config for `temps agent`
        let config = temps_agent::AgentConfig {
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
        };
        self.save_agent_config(&config)?;

        println!();
        println!("Run 'temps agent' to start the worker.");

        Ok(())
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
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen()).collect();
    hex::encode(bytes)
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
