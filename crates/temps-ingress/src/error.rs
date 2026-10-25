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

impl From<IngressError> for Problem {
    fn from(error: IngressError) -> Self {
        let (status, title) = match &error {
            IngressError::NotFound(_) => (StatusCode::NOT_FOUND, "Ingress Connector Not Found"),
            IngressError::Validation(_) => (StatusCode::BAD_REQUEST, "Invalid Ingress Connector"),
            IngressError::NotAllowed(_) => {
                (StatusCode::CONFLICT, "Worker Does Not Allow This Change")
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
