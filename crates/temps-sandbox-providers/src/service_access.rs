// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Trusted host protocol, carried over the existing authenticated worker RPC.
//! No command is accepted from a guest or routed by a guest-provided identity.
use serde::{Deserialize, Serialize};
use temps_service_access_runtime::{SessionDelivery, WorkerIdentity};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceAccessTarget {
    pub incarnation: String,
    pub container_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceAccessCommand {
    Inspect,
    Apply {
        expected: ServiceAccessTarget,
        delivery: Box<SessionDelivery>,
    },
    Retire {
        expected: ServiceAccessTarget,
        #[schema(value_type = String, format = "uuid")]
        boot_id: Uuid,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceAccessReply {
    Inspect {
        target: ServiceAccessTarget,
        worker: WorkerIdentity,
        connected: bool,
        binding_version: Option<u64>,
    },
    Applied {
        target: ServiceAccessTarget,
        snapshot_version: u64,
        binding_version: u64,
    },
    Retired {
        target: ServiceAccessTarget,
    },
}
