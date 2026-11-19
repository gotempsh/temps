// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IngressError {
    #[error("Ingress connector {0} not found")]
    NotFound(i32),
    #[error("{0}")]
    Validation(String),
    /// A placement names a node id with no `nodes` row. `0` is the control
    /// plane, which has none (ADR-107 D1).
    #[error("Node {node_id} does not exist (use 0 for the control plane).")]
    UnknownNode { node_id: i32 },
    /// A placement names a node that is not a worker.
    #[error("Node '{name}' is a {role}, not a worker; only workers run connectors.")]
    NotAWorker {
        node_id: i32,
        name: String,
        role: String,
    },
    /// A placement names a worker that is not active.
    #[error("Worker '{name}' is {status}, so it cannot run a connector. Bring it back (its agent must be running and connected) or pick another worker.")]
    InactiveWorker {
        node_id: i32,
        name: String,
        status: String,
    },
    /// A placement names the control plane, which cannot run a connector in
    /// this process (ADR-107 D2); the source says why and what to do.
    #[error("The control plane cannot run a connector: {0}")]
    ControlPlaneUnavailable(#[from] ControlPlaneTunnelsUnavailable),
    /// The worker has not delegated public ingress (ADR 058 D7).
    #[error("{0}")]
    NotAllowed(String),
    /// The tunnel provider refused or failed; the message says what to do.
    #[error("{0}")]
    Provider(String),
    #[error(transparent)]
    Dns(#[from] temps_dns::DnsError),
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Could not encrypt or decrypt the connector secret: {0}")]
    Encryption(String),
}

/// Why the control plane cannot run a connector placed on node 0 (ADR-107
/// D2, D3). Each says what to do instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlPlaneTunnelsUnavailable {
    /// `temps serve --role console`: the proxy is another process, so no
    /// listener here would carry the tunnel's traffic.
    #[error(
        "this server does not run the proxy in the same process as the console \
         (`temps serve --role console`), so nothing here would carry the tunnel's \
         traffic. Run the tunnel on a worker."
    )]
    SeparateProxy,
    /// The proxy could not bind its tunnel origin at startup.
    #[error(
        "The control plane's proxy is not listening on {address}, so a tunnel here would have \
         nowhere to send visitors. Either this server runs its proxy as a separate process \
         (`temps serve --role console`), or another program held port {port} when Temps \
         started (`ss -ltnp 'sport = :{port}'` shows which): free it and restart Temps, or \
         run the tunnel on a worker.",
        port = address.port()
    )]
    OriginUnbound { address: std::net::SocketAddr },
}

impl From<IngressError> for Problem {
    fn from(error: IngressError) -> Self {
        let (status, title) = match &error {
            IngressError::NotFound(_) => (StatusCode::NOT_FOUND, "Ingress Connector Not Found"),
            IngressError::Validation(_)
            | IngressError::UnknownNode { .. }
            | IngressError::NotAWorker { .. }
            | IngressError::InactiveWorker { .. } => {
                (StatusCode::BAD_REQUEST, "Invalid Ingress Connector")
            }
            IngressError::NotAllowed(_) => {
                (StatusCode::CONFLICT, "Worker Does Not Allow This Change")
            }
            IngressError::ControlPlaneUnavailable(_) => {
                (StatusCode::CONFLICT, "Control Plane Cannot Run Connectors")
            }
            IngressError::Provider(_) => (StatusCode::BAD_GATEWAY, "Tunnel Provider Error"),
            IngressError::Dns(_) => (StatusCode::BAD_GATEWAY, "DNS Error"),
            IngressError::Database(_) | IngressError::Encryption(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Ingress Connector Error")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}
