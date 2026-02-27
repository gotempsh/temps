//! Plugin runtime context providing access to shared services.

use sea_orm::DatabaseConnection;
use std::sync::Arc;

/// Runtime context provided to external plugins.
///
/// This is the plugin's window into the Temps ecosystem.
/// It provides direct database access (trusted model) and
/// configuration needed to operate within Temps.
#[derive(Clone)]
pub struct PluginContext {
    /// Database connection pool (shared with Temps)
    db: Arc<DatabaseConnection>,
    /// The plugin's name (from manifest)
    plugin_name: String,
    /// Directory for plugin-specific data files
    data_dir: std::path::PathBuf,
    /// HMAC secret for validating requests from Temps
    auth_secret: String,
}

impl PluginContext {
    /// Create a new plugin context.
    pub fn new(
        db: Arc<DatabaseConnection>,
        plugin_name: String,
        data_dir: std::path::PathBuf,
        auth_secret: String,
    ) -> Self {
        Self {
            db,
            plugin_name,
            data_dir,
            auth_secret,
        }
    }

    /// Get a reference to the database connection.
    ///
    /// This is the same Postgres database that Temps uses.
    /// The plugin has full read/write access to all tables,
    /// including `temps-entities` models.
    ///
    /// # Example
    /// ```rust,no_run
    /// use temps_entities::projects;
    /// use sea_orm::EntityTrait;
    ///
    /// async fn list_projects(ctx: &PluginContext) {
    ///     let projects = projects::Entity::find()
    ///         .all(ctx.db())
    ///         .await
    ///         .unwrap();
    /// }
    /// ```
    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    /// Get a shared Arc to the database connection.
    pub fn db_arc(&self) -> Arc<DatabaseConnection> {
        self.db.clone()
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

    /// Get the HMAC auth secret for request validation.
    ///
    /// Temps signs proxied requests with this secret.
    /// Use this to verify that incoming requests actually come from Temps.
    pub fn auth_secret(&self) -> &str {
        &self.auth_secret
    }
}
