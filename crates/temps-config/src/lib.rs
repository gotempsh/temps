pub mod disk_status;
mod handler;
pub mod plugin;
mod service;

pub use disk_status::{
    collect_disk_status, disk_for_path, get_disk_info, DiskInfo, DiskSpaceAlert,
    DiskSpaceCheckResult, DiskStatusError,
};
pub use handler::{configure_routes, SettingsApiDoc, SettingsState};
pub use plugin::ConfigPlugin;
pub use service::{ConfigService, ConfigServiceError, ServerConfig};
