// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps doctor mesh` (ADR 048 D9): this host's end of the WireGuard mesh,
//! each failing check with the action that fixes it. A node (it has
//! `agent.json`) is checked against its last network snapshot; the control
//! plane against its database.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clap::Args;
use colored::Colorize;
use temps_network::mesh_doctor::{self, CheckStatus, MeshCheck, NodeApiProbe};

use super::{CheckResult, DiagnosticReport};

/// Check this host's end of the WireGuard mesh
#[derive(Args, Debug, Clone)]
pub struct MeshDoctorArgs {
    /// Print the checks as JSON
    #[arg(long)]
    pub json: bool,
}

/// Which host was checked, and what was found.
pub(super) struct MeshDiagnosis {
    pub host: String,
    pub checks: Vec<MeshCheck>,
}

pub(super) async fn run(
    args: &MeshDoctorArgs,
    database_url: Option<&str>,
    data_dir: &Path,
) -> anyhow::Result<()> {
    let diagnosis = diagnose_this_host(database_url, data_dir).await?;
    let failed = diagnosis
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .count();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "host": diagnosis.host,
                "checks": diagnosis.checks,
            }))?
        );
    } else {
        println!();
        println!(
            "{}",
            format!("  Mesh doctor: {}", diagnosis.host)
                .bright_white()
                .bold()
        );
        println!();
        let mut report = DiagnosticReport::new();
        add_to_report(&diagnosis.checks, &mut report);
        report.print();
        report.print_summary();
        println!();
    }
    if failed > 0 {
        anyhow::bail!("{failed} mesh check(s) failed");
    }
    Ok(())
}

pub(super) fn add_to_report(checks: &[MeshCheck], report: &mut DiagnosticReport) {
    for check in checks {
        let message = match &check.fix {
            Some(fix) => format!("{}\n         fix: {fix}", check.detail),
            None => check.detail.clone(),
        };
        report.add(
            check.label.clone(),
            match check.status {
                CheckStatus::Pass => CheckResult::Pass(message),
                CheckStatus::Warn => CheckResult::Warn(message),
                CheckStatus::Fail => CheckResult::Fail(message),
                CheckStatus::Info => CheckResult::Info(message),
            },
        );
    }
}

async fn diagnose_this_host(
    database_url: Option<&str>,
    data_dir: &Path,
) -> anyhow::Result<MeshDiagnosis> {
    let agent_config = crate::commands::agent::agent_data_dir().join("agent.json");
    if agent_config.is_file() {
        return diagnose_node(&agent_config).await;
    }
    let Some(database_url) = database_url else {
        anyhow::bail!(
            "This host is neither a node ({} does not exist) nor a control plane \
             (TEMPS_DATABASE_URL is not set). On a node, run this as the user that ran \
             `temps join`; on the control plane, with the same environment as `temps serve`.",
            agent_config.display()
        );
    };
    let db = sea_orm::Database::connect(database_url)
        .await
        .map_err(|error| anyhow::anyhow!("could not connect to the database: {error}"))?;
    Ok(MeshDiagnosis {
        host: "control plane".to_string(),
        checks: control_plane_checks(&db, data_dir).await,
    })
}

async fn diagnose_node(agent_config: &PathBuf) -> anyhow::Result<MeshDiagnosis> {
    let contents = std::fs::read_to_string(agent_config)?;
    let config: temps_agent::AgentConfig = serde_json::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("{} is unreadable: {error}", agent_config.display()))?;
    let host = format!("node {}", config.node_name);
    let checks = match temps_agent::network_sync::mesh_doctor_expectations(&config) {
        Ok(Some(expected)) => {
            let mut observed = mesh_doctor::observe(&expected, &config.mesh_key_dir).await;
            observed.node_api = Some(temps_agent::network_sync::probe_control_plane(&config).await);
            mesh_doctor::diagnose(&expected, &observed, SystemTime::now())
        }
        Ok(None) => vec![MeshCheck {
            label: "Mesh".into(),
            status: CheckStatus::Info,
            detail: "the cluster's WireGuard mesh is off: this node reaches the control plane \
                     and the other nodes on a private network"
                .into(),
            fix: None,
        }],
        Err(reason) => vec![MeshCheck {
            label: "Mesh settings".into(),
            status: CheckStatus::Fail,
            detail: reason,
            fix: Some(
                "Start (or restart) `temps agent`: it fetches the mesh settings from the \
                 control plane and brings this node's end up. If it cannot reach the control \
                 plane, its log says why."
                    .into(),
            ),
        }],
    };
    Ok(MeshDiagnosis { host, checks })
}

/// The control plane's end, from its database; also run by `temps doctor`.
pub(super) async fn control_plane_checks(
    db: &sea_orm::DatabaseConnection,
    data_dir: &Path,
) -> Vec<MeshCheck> {
    let settings = match temps_network::mesh::load_settings(db).await {
        Ok(settings) => settings,
        Err(error) => {
            return vec![MeshCheck {
                label: "Mesh settings".into(),
                status: CheckStatus::Fail,
                detail: format!("could not read them: {error}"),
                fix: Some("Run `temps migrate`, then try again.".into()),
            }]
        }
    };
    let Some(settings) = settings else {
        return vec![MeshCheck {
            label: "Mesh".into(),
            status: CheckStatus::Info,
            detail: "off: nodes must reach this server and each other on a private network".into(),
            fix: Some(
                "To add nodes over the internet, turn it on from Worker Nodes, or run \
                 `sudo temps network setup-multi-node --wireguard`."
                    .into(),
            ),
        }];
    };
    let expected = match temps_network::control_plane::mesh_doctor_expectations(db).await {
        Ok(Some(expected)) => expected,
        Ok(None) => {
            return vec![MeshCheck {
                label: "Mesh".into(),
                status: CheckStatus::Fail,
                detail: "on, but this server has not brought its end up".into(),
                fix: Some(
                    "Restart `temps serve` and check its logs for \"WireGuard mesh\": it needs \
                     Linux kernel WireGuard and root or CAP_NET_ADMIN."
                        .into(),
                ),
            }]
        }
        Err(error) => {
            return vec![MeshCheck {
                label: "Mesh settings".into(),
                status: CheckStatus::Fail,
                detail: format!("could not read them: {error}"),
                fix: None,
            }]
        }
    };
    let mut observed =
        mesh_doctor::observe(&expected, &temps_network::mesh::key_dir(data_dir)).await;
    let node_api = SocketAddr::new(expected.address.into(), settings.node_api_port);
    observed.node_api = Some(NodeApiProbe {
        target: format!("node API on {node_api}"),
        result: match tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect(node_api),
        )
        .await
        {
            Ok(Ok(_)) => Ok("listening".to_string()),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("timed out".to_string()),
        },
    });
    mesh_doctor::diagnose(&expected, &observed, SystemTime::now())
}
