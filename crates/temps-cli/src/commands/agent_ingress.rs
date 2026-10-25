// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps agent ingress`: whether this worker takes public traffic, decided
//! on the worker (ADR 058 D7). The console can route to a worker, but only the
//! worker can open its ports, so this writes `ingress` in `agent.json` and the
//! running agent applies it within a few seconds.
//!
//! - `off`: no public ports (the default).
//! - `fixed`: the worker picks its listen address and ports.
//! - `delegated`: Temps picks among the addresses the worker reports.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use temps_core::worker_ingress::{IngressMode, LocalIngress};

const UNIT_PATH: &str = "/etc/systemd/system/temps-agent.service";

#[derive(Subcommand, Debug, Clone)]
pub enum AgentIngressCommand {
    /// Show what this worker allows and where it listens
    Status(Target),
    /// Take no public traffic (the default)
    Off(Target),
    /// Listen for public traffic on an address and ports this worker chooses
    Fixed(FixedArgs),
    /// Let Temps choose the listen address and ports among this worker's own
    Delegated(Target),
}

#[derive(Args, Debug, Clone)]
pub struct Target {
    /// Directory holding agent.json (default: TEMPS_DATA_DIR, the service's
    /// data directory, or ~/.temps)
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct FixedArgs {
    /// Local address to listen on, or `auto` for the interface holding the
    /// default route. Behind NAT, this is the LAN address the router forwards to.
    #[arg(long, default_value = "auto")]
    pub listen: String,
    #[arg(long, default_value_t = 80)]
    pub http_port: u16,
    #[arg(long, default_value_t = 443)]
    pub https_port: u16,
    #[command(flatten)]
    pub target: Target,
}

impl AgentIngressCommand {
    pub fn execute(&self) -> anyhow::Result<()> {
        let (target, desired) = match self {
            Self::Status(target) => return status(&config_path(target)?),
            Self::Off(target) => (target, LocalIngress::default()),
            Self::Delegated(target) => (
                target,
                LocalIngress {
                    mode: IngressMode::Delegated,
                    ..LocalIngress::default()
                },
            ),
            Self::Fixed(args) => (&args.target, fixed(args)?),
        };
        let path = config_path(target)?;
        write_ingress(&path, &desired)?;
        println!("Public ingress: {}", describe(&desired));
        println!(
            "Saved to {}. A running agent applies it within about 5 seconds.",
            path.display()
        );
        if unit_pins_listen() {
            println!(
                "Warning: {UNIT_PATH} starts the agent with --public-ingress-address, which overrides this setting. \
                 Run `temps agent service install` to rewrite the unit without it."
            );
        }
        match desired.mode {
            IngressMode::Off => println!("Remove this worker's DNS records if they still point here."),
            IngressMode::Fixed => println!(
                "Open ports {} and {} to this machine (behind NAT, forward 80 and 443 from the router to them), \
                 then enable public ingress for this worker in the console and run its reachability check.",
                desired.http_port, desired.https_port
            ),
            IngressMode::Delegated => println!(
                "Choose the listen address and public address for this worker in the console."
            ),
        }
        Ok(())
    }
}

fn fixed(args: &FixedArgs) -> anyhow::Result<LocalIngress> {
    let listen = match args.listen.trim() {
        "" | "auto" => None,
        value => {
            let ip: IpAddr = value.parse().map_err(|_| {
                anyhow::anyhow!("--listen '{value}' is not an IP address. Use an address of this machine, or `auto`.")
            })?;
            if ip.is_unspecified() || ip.is_loopback() {
                anyhow::bail!(
                    "--listen {ip} would not take traffic from outside this machine (or would collide with the \
                     overlay listener). Use the address of a network interface, or `auto`."
                );
            }
            Some(ip)
        }
    };
    if args.http_port == 0 || args.https_port == 0 || args.http_port == args.https_port {
        anyhow::bail!("--http-port and --https-port must be two different, non-zero ports.");
    }
    Ok(LocalIngress {
        mode: IngressMode::Fixed,
        listen,
        http_port: args.http_port,
        https_port: args.https_port,
    })
}

fn describe(ingress: &LocalIngress) -> String {
    match ingress.mode {
        IngressMode::Off => "off (no public ports)".to_string(),
        IngressMode::Delegated => {
            "delegated (Temps chooses the listen address and ports)".to_string()
        }
        IngressMode::Fixed => format!(
            "fixed on {} ports {}/{}",
            ingress.listen.map_or_else(
                || "the default-route address".to_string(),
                |ip| ip.to_string()
            ),
            ingress.http_port,
            ingress.https_port
        ),
    }
}

/// `--data-dir`, else `TEMPS_DATA_DIR`/`~/.temps`, else the data directory
/// the installed service runs with (`sudo` changes `~`).
fn config_path(target: &Target) -> anyhow::Result<PathBuf> {
    if let Some(dir) = &target.data_dir {
        return Ok(dir.join("agent.json"));
    }
    let default = super::agent::agent_data_dir().join("agent.json");
    if default.exists() {
        return Ok(default);
    }
    if let Some(dir) = unit_data_dir() {
        let path = dir.join("agent.json");
        if path.exists() {
            return Ok(path);
        }
    }
    anyhow::bail!(
        "No agent.json at {}. Run `temps join` first, or pass --data-dir with the directory the agent runs from.",
        default.display()
    )
}

fn unit_text() -> Option<String> {
    std::fs::read_to_string(UNIT_PATH).ok()
}

fn unit_data_dir() -> Option<PathBuf> {
    unit_text()?.lines().find_map(|line| {
        let value = line.trim().strip_prefix("Environment=")?.trim_matches('"');
        value.strip_prefix("TEMPS_DATA_DIR=").map(PathBuf::from)
    })
}

fn unit_pins_listen() -> bool {
    unit_text().is_some_and(|text| text.contains("--public-ingress-address"))
}

fn read_config(path: &Path) -> anyhow::Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Could not read {}: {e}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", path.display()))?;
    if !value.is_object() {
        anyhow::bail!("{} is not an agent config object", path.display());
    }
    Ok(value)
}

fn status(path: &Path) -> anyhow::Result<()> {
    let value = read_config(path)?;
    let config: temps_agent::AgentConfig = serde_json::from_value(value)
        .map_err(|e| anyhow::anyhow!("{} is not a valid agent config: {e}", path.display()))?;
    let ingress = config.local_ingress();
    println!("Public ingress: {}", describe(&ingress));
    println!("Config: {}", path.display());
    if unit_pins_listen() {
        println!("Note: {UNIT_PATH} starts the agent with --public-ingress-address, which overrides agent.json.");
    }
    Ok(())
}

/// Replace `ingress` in agent.json, keeping every other field, and retire the
/// legacy listen fields so the two cannot disagree.
fn write_ingress(path: &Path, ingress: &LocalIngress) -> anyhow::Result<()> {
    let mut value = read_config(path)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} is not an agent config object", path.display()))?;
    object.insert("ingress".to_string(), serde_json::to_value(ingress)?);
    object.insert(
        "public_ingress_address".to_string(),
        serde_json::Value::Null,
    );
    let text = serde_json::to_string_pretty(&value)?;
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(|e| {
            anyhow::anyhow!(
                "Could not write {}: {e}. Run this as the user the agent runs as (often root).",
                tmp.display()
            )
        })?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| anyhow::anyhow!("Could not replace {}: {e}", path.display()))?;
    Ok(())
}
