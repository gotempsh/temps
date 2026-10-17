// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CI runners use the same BuildSpec and ImageInfo as enrolled builder agents.
use crate::{build_protocol::BuildSpec, ImageInfo};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunnerClaim {
    pub identity_token: String,
}
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunnerWork {
    pub token: String,
    pub spec: BuildSpec,
    pub registry_tag: String,
    pub run_id: String,
    pub timeout_seconds: u32,
    pub registry_credentials: Option<crate::remote::RemotePullCredentials>,
}
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunnerResult {
    pub build_id: String,
    pub run_id: String,
    pub digest: String,
    pub platforms: Vec<String>,
    pub image: ImageInfo,
    pub duration_ms: u64,
}
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunnerFailure {
    pub message: String,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunnerLogs {
    pub lines: Vec<String>,
}
