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
}

/// Response body from the control plane registration endpoint.
#[derive(serde::Deserialize)]
struct RegisterResponse {
    id: i32,
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

        let register_body = serde_json::json!({
            "name": node_name,
            "token": agent_token,
            "join_token": self.token,
            "address": format!("http://{}:{}", private_address.trim(), self.agent_address.split(':').next_back().unwrap_or("3100").trim()),
            "private_address": private_address,
            "labels": labels,
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

        // Save config for `temps agent`
        let config = temps_agent::AgentConfig {
            listen_address: self.agent_address.clone(),
            token: agent_token,
            node_name: node_name.to_string(),
            control_plane_url: self.target.clone(),
            node_id: register_response.id,
            labels: labels.clone(),
            dns_data_dir: crate::commands::agent::agent_data_dir().join("dns"),
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

        let register_body = serde_json::json!({
            "name": node_name,
            "token": relay_response.agent_token,
            "join_token": self.token,
            "address": format!("http://{}:{}", relay_response.assigned_ip, agent_port),
            "private_address": relay_response.assigned_ip,
            "wg_public_key": keypair.public_key,
            "public_endpoint": public_endpoint,
            "labels": labels,
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

        // Try to get node_id from the register response; fall back to relay response
        let node_id = match response.json::<RegisterResponse>().await {
            Ok(r) => r.id,
            Err(_) => relay_response.node_id,
        };

        println!(
            "Registered with control plane successfully (node_id={}).",
            node_id
        );

        // Save config for `temps agent`
        let config = temps_agent::AgentConfig {
            listen_address: self.agent_address.clone(),
            token: relay_response.agent_token,
            node_name: node_name.to_string(),
            control_plane_url: relay_response.control_plane_url,
            node_id,
            labels: labels.clone(),
            dns_data_dir: crate::commands::agent::agent_data_dir().join("dns"),
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
