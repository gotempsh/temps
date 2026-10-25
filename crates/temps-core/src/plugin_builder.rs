// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Building an external plugin on another machine.
//!
//! Plugins are compiled in an isolated Docker container. A process that runs
//! no workloads of its own (a hosted control plane) has no Docker, so it asks a
//! worker node that does. The control plane never trusts the worker with more
//! than it trusts it for deployments: it sends source and receives a binary,
//! and hashes the binary itself before installing it.

use std::path::PathBuf;

use async_trait::async_trait;
use thiserror::Error;

/// One plugin build to run elsewhere.
#[derive(Debug, Clone)]
pub struct PluginBuildRequest {
    /// A tar archive of the plugin source tree (not extracted on the way).
    pub source_archive: PathBuf,
    /// The Bun compile target the binary must run on, e.g.
    /// `bun-linux-x64-musl`. It names the machine the plugin will run on, not
    /// the machine that builds it.
    pub bun_target: String,
    /// Where to write the compiled binary.
    pub output: PathBuf,
}

#[derive(Debug, Error)]
pub enum PluginBuildError {
    /// Nothing could take the build: no worker node is joined, or none of
    /// them can build. Nothing was compiled. The text is safe to show.
    #[error("{0}")]
    NoBuilder(String),
    /// A builder took the build and it failed. The text is the builder's
    /// reason.
    #[error("{0}")]
    Failed(String),
}

/// Runs plugin builds on a node that has Docker.
#[async_trait]
pub trait RemotePluginBuilder: Send + Sync {
    async fn build(&self, request: &PluginBuildRequest) -> Result<(), PluginBuildError>;
}
