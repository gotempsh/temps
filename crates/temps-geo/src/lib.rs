// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod geoip_service;
pub mod handlers;
pub mod ip_address_service;
pub mod plugin;
pub mod refresh;
pub mod settings_service;

pub use geoip_service::{
    resolve_mmdb_path, GeoIpError, GeoIpService, GeoLocation, MaxMindGeoIpService, MmdbSource,
    MockGeoIpService,
};
pub use handlers::AppState;
pub use ip_address_service::{IpAddressInfo, IpAddressService};
pub use plugin::GeoPlugin;
pub use refresh::{
    city_db_path, ensure_mmdb_present, fetch_latest_mmdb_bytes, geo_db_source_url,
    redact_license_key, spawn_db_file_watcher, write_mmdb_atomically, DbSource, RefreshOutcome,
    CITY_DB_FILENAME, DB_FILE_WATCH_INTERVAL, REDACTED,
};
pub use settings_service::GeoSettingsService;
