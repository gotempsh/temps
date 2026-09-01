// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin runtime context providing access to platform services.

use crate::client::TempsClient;

/// Runtime context provided to external plugins.
///
/// This is the plugin's window into the Temps ecosystem.
/// Platform data normally flows through the [`TempsClient`] returned by
/// [`temps()`](Self::temps). A trusted plugin that explicitly declares
/// `requires_db` can also receive the direct database URL.
#[derive(Clone)]
pub struct PluginContext {
    /// Typed client for querying the Temps platform
    temps_client: TempsClient,
    /// The plugin's name (from manifest)
    plugin_name: String,
    /// Directory for plugin-specific data files
    data_dir: std::path::PathBuf,
    /// Direct database URL disclosed only when declared in the manifest.
    database_url: Option<String>,
    /// The instance's own data root, when Temps passed one.
    host_data_dir: Option<std::path::PathBuf>,
    /// Base URL at which the instance's API answers, when Temps passed one.
    host_api_url: Option<String>,
    /// Per-process assertion secret for validating requests from Temps.
    /// This is protocol integrity for trusted installed code, not process
    /// isolation under the shared host user.
    auth_secret: String,
}

impl PluginContext {
    /// Create a new plugin context.
    pub fn new(
        temps_client: TempsClient,
        plugin_name: String,
        data_dir: std::path::PathBuf,
        database_url: Option<String>,
        host_data_dir: Option<std::path::PathBuf>,
        host_api_url: Option<String>,
        auth_secret: String,
    ) -> Self {
        Self {
            temps_client,
            plugin_name,
            data_dir,
            database_url,
            host_data_dir,
            host_api_url,
            auth_secret,
        }
    }

    /// Get a client for querying the Temps platform.
    ///
    /// The client provides typed, read-only access to projects,
    /// environments, deployments, and other platform data.
    ///
    /// # Example
    /// ```rust,no_run
    /// use temps_plugin_sdk::prelude::*;
    ///
    /// async fn list_projects(ctx: &PluginContext) {
    ///     let projects = ctx.temps().list_projects().await.unwrap();
    ///     for p in projects {
    ///         println!("{}: {}", p.id, p.name);
    ///     }
    /// }
    /// ```
    pub fn temps(&self) -> &TempsClient {
        &self.temps_client
    }

    /// Typed platform API, acting as the caller of the request being served.
    ///
    /// Takes the actor token from [`crate::protocol::TempsAuth`] rather than
    /// inventing one: a platform API call is always made *for somebody*, and
    /// binding it to the caller is what keeps `permission_guard!` meaningful
    /// on the other side. A plugin cannot widen its own access this way — the
    /// call runs as that user and no further.
    pub fn api_for(
        &self,
        auth: &crate::protocol::TempsUserContext,
    ) -> Result<crate::api::PlatformApi, crate::error::PluginSdkError> {
        let token = auth
            .actor_token
            .as_deref()
            .ok_or(crate::error::PluginSdkError::NoActor)?;
        Ok(crate::api::PlatformApi::new(
            self.temps_client.clone(),
            temps_core::external_plugin::channel::ActorToken::new(token),
        ))
    }

    /// Typed platform API acting as a caller verified by the SDK runtime.
    pub fn api_as_caller(
        &self,
        caller: &crate::auth::AuthenticatedCaller,
    ) -> Result<crate::api::PlatformApi, crate::error::PluginSdkError> {
        let token = caller
            .actor_token()
            .ok_or(crate::error::PluginSdkError::NoActor)?;
        Ok(crate::api::PlatformApi::new(
            self.temps_client.clone(),
            temps_core::external_plugin::channel::ActorToken::new(token),
        ))
    }

    /// Get the plugin's name.
    pub fn plugin_name(&self) -> &str {
        &self.plugin_name
    }

    /// Get the plugin's data directory.
    ///
    /// Use this for storing plugin-specific files (caches, state, etc.).
    /// The directory is guaranteed to exist when the plugin starts.
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// Direct platform database URL for a trusted plugin that declared
    /// `requires_db`; absent for ordinary plugins.
    pub fn database_url(&self) -> Option<&str> {
        self.database_url.as_deref()
    }

    /// The Temps instance's own data root, if it passed one.
    ///
    /// `None` when the manifest did not declare privileged host-data access,
    /// so treat it as a capability to check rather than assume — a plugin that needs it
    /// should say so rather than fall back to guessing a path relative to
    /// [`Self::data_dir`].
    ///
    /// This is the platform's directory, not the plugin's: it holds state
    /// that belongs to the instance (deployment data roots, encryption key, auth
    /// secret). Read what you need; write only under [`Self::data_dir`].
    pub fn host_data_dir(&self) -> Option<&std::path::Path> {
        self.host_data_dir.as_deref()
    }

    /// Base URL at which the instance's API answers, if it passed one.
    ///
    /// Combine with [`Self::plugin_name`] to build a URL for this plugin's
    /// own routes; [`Self::mount_url`] does that.
    pub fn host_api_url(&self) -> Option<&str> {
        self.host_api_url.as_deref()
    }

    /// This plugin's externally-reachable base URL, e.g.
    /// `http://127.0.0.1:8080/api/x/my-plugin`.
    ///
    /// Use for URLs handed to something that cannot reach the plugin's Unix
    /// socket — a sandboxed agent, an external webhook sender. `None` on a
    /// Temps that did not pass `--host-api-url`.
    pub fn mount_url(&self) -> Option<String> {
        self.host_api_url
            .as_deref()
            .map(|base| format!("{}/api/x/{}", base.trim_end_matches('/'), self.plugin_name))
    }

    /// Get the HMAC auth secret for request validation.
    ///
    /// Temps signs proxied requests with this secret.
    /// Use this to verify that incoming requests actually come from Temps.
    pub fn auth_secret(&self) -> &str {
        &self.auth_secret
    }
}
