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
//! - `private`: listen only on a tailnet or LAN address, for addresses whose
//!   audience is private (ADR 064 D5). With `--tailscale-serve` the worker
//!   owner also lets the agent configure tailscaled's Serve for tailnet names
//!   (ADR 065 D2), with or without that listener.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use temps_core::worker_ingress::{
    check_private_listen, IngressMode, LocalIngress, PRIVATE_HTTPS_PORT, PRIVATE_HTTP_PORT,
    TAILSCALE_INTERFACE,
};

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
    /// Listen only on a private address (a tailnet or LAN), port 443, for
    /// addresses only your private network may open
    Private(PrivateArgs),
}

#[derive(Args, Debug, Clone)]
pub struct PrivateArgs {
    /// `tailscale` for this machine's Tailscale address (`tailscale0`), or a
    /// private address of this machine: 100.64.0.0/10, 10.0.0.0/8,
    /// 172.16.0.0/12, 192.168.0.0/16 or fd00::/8. The port is always 443,
    /// and 80 only redirects to it. Optional with `--tailscale-serve`, which
    /// then serves only through Tailscale Serve.
    #[arg(long)]
    pub listen: Option<String>,
    /// Let the agent configure this machine's tailscaled (ADR 065 D2): Serve
    /// entries for tailnet names (`<machine>.<tailnet>.ts.net` or a Tailscale
    /// Service), only its own, never Funnel. The agent must run as root.
    #[arg(long)]
    pub tailscale_serve: bool,
    #[command(flatten)]
    pub target: Target,
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
            Self::Private(args) => (
                &args.target,
                private(args.listen.as_deref(), args.tailscale_serve)?,
            ),
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
            IngressMode::Private => println!(
                "Nothing listens publicly. In the console, choose \"Only my private network\" \
                 in Make it reachable for the addresses this worker should serve."
            ),
        }
        if desired.tailscale_serve && !running_as_root() {
            println!(
                "Warning: --tailscale-serve needs the agent to run as root, since tailscaled lets \
                 only root point Serve at a socket. Run the agent as root (`temps agent service \
                 install` does)."
            );
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
        interface: None,
        http_port: args.http_port,
        https_port: args.https_port,
        tailscale_serve: false,
    })
}

#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// No Unix root to be: tailscaled's Serve sockets need one, so
/// `--tailscale-serve` is warned about as for any other non-root agent.
#[cfg(not(unix))]
fn running_as_root() -> bool {
    false
}

/// `--listen` for private ingress: `tailscale`, or a private address, and
/// `--tailscale-serve`, which may stand alone (ADR 065 D2). Used by `temps
/// join --ingress private` too, so both refuse the same things.
pub(crate) fn private(listen: Option<&str>, tailscale_serve: bool) -> anyhow::Result<LocalIngress> {
    let mut ingress = LocalIngress {
        mode: IngressMode::Private,
        http_port: PRIVATE_HTTP_PORT,
        https_port: PRIVATE_HTTPS_PORT,
        tailscale_serve,
        ..LocalIngress::default()
    };
    match listen.unwrap_or_default().trim() {
        "tailscale" => ingress.interface = Some(TAILSCALE_INTERFACE.to_string()),
        "" | "auto" if tailscale_serve => {}
        "" | "auto" => anyhow::bail!(
            "Private ingress needs to know which private address to listen on: use `--listen \
             tailscale`, or a private address of this machine, or `--tailscale-serve` to serve \
             only through Tailscale Serve."
        ),
        value => {
            let ip: IpAddr = value.parse().map_err(|_| {
                anyhow::anyhow!(
                    "--listen '{value}' is not `tailscale` or an IP address. Use `--listen \
                     tailscale`, or a private address of this machine."
                )
            })?;
            check_private_listen(ip).map_err(|why| anyhow::anyhow!("--listen {why}"))?;
            ingress.listen = Some(ip);
        }
    }
    Ok(ingress)
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
        IngressMode::Private => {
            let listener = match (&ingress.interface, ingress.listen) {
                (Some(interface), _) if interface == TAILSCALE_INTERFACE => {
                    Some("this machine's Tailscale address".to_string())
                }
                (Some(interface), _) => Some(format!("{interface}'s address")),
                (None, Some(ip)) => Some(ip.to_string()),
                (None, None) => None,
            };
            match (listener, ingress.tailscale_serve) {
                (Some(listener), false) => format!(
                    "private on {listener} port 443 (80 redirects to it); serves only private \
                     addresses"
                ),
                (Some(listener), true) => format!(
                    "private on {listener} port 443 (80 redirects to it), and through Tailscale \
                     Serve; serves only private addresses"
                ),
                (None, true) => {
                    "private through Tailscale Serve only; serves only private addresses"
                        .to_string()
                }
                (None, false) => "private on no address".to_string(),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_takes_tailscale_or_a_private_address_only() {
        let private = |listen: &str| super::private(Some(listen), false);
        let tailscale = private("tailscale").unwrap();
        assert_eq!(tailscale.mode, IngressMode::Private);
        assert_eq!(tailscale.interface.as_deref(), Some("tailscale0"));
        assert_eq!((tailscale.http_port, tailscale.https_port), (80, 443));
        assert_eq!(
            private("192.168.1.8").unwrap().listen,
            Some("192.168.1.8".parse().unwrap())
        );
        for refused in ["0.0.0.0", "::", "203.0.113.7", "auto", "eth0"] {
            let error = private(refused).unwrap_err().to_string();
            assert!(error.contains("--listen tailscale"), "{refused}: {error}");
        }
        assert!(describe(&tailscale).contains("Tailscale"));
        assert!(!tailscale.tailscale_serve);
    }

    // ADR 065 D2: the consent flag is saved, alone or with a listener, and
    // never set without being asked for.
    #[test]
    fn tailscale_serve_is_consent_saved_in_agent_json() {
        let serve_only = private(None, true).unwrap();
        assert_eq!(serve_only.mode, IngressMode::Private);
        assert!(serve_only.tailscale_serve);
        assert_eq!(
            (serve_only.listen, serve_only.interface.as_deref()),
            (None, None)
        );
        assert!(describe(&serve_only).contains("Tailscale Serve only"));
        let both = private(Some("tailscale"), true).unwrap();
        assert!(both.tailscale_serve);
        assert_eq!(both.interface.as_deref(), Some("tailscale0"));
        assert!(private(None, false).is_err());

        let saved = serde_json::to_value(&both).unwrap();
        assert_eq!(saved["tailscale_serve"], serde_json::json!(true));
        // Without consent the field is not written at all, so agent.json is
        // unchanged for every install that never opts in (ADR 065 D11).
        let without = serde_json::to_value(private(Some("tailscale"), false).unwrap()).unwrap();
        assert!(without.get("tailscale_serve").is_none());
        let old: LocalIngress = serde_json::from_value(
            serde_json::json!({"mode": "private", "interface": "tailscale0"}),
        )
        .unwrap();
        assert!(!old.tailscale_serve);
    }
}
