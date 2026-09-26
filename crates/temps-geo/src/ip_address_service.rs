// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::geoip_service::{GeoIpService, GeoLocation};
use crate::settings_service::GeoSettingsService;
use chrono::Utc;
use moka::future::Cache;
use sea_orm::{prelude::*, QueryFilter, QueryOrder, QuerySelect, Set};
use std::sync::Arc;
use std::time::Duration;
use temps_core::UtcDateTime;
use temps_entities::ip_geolocations;
use tracing::{debug, error, info};

/// Max number of distinct IPs held in the geolocation cache. Bounds memory while
/// covering the working set of a busy proxy (bots + real visitors).
const GEO_CACHE_MAX_ENTRIES: u64 = 100_000;
/// How long a cached IP -> geolocation mapping stays valid. Geolocation is stable,
/// so a long TTL collapses repeated lookups for the same IP into a single DB hit.
const GEO_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone)]
pub struct IpAddressInfo {
    pub id: i32,
    pub ip: String,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

impl From<ip_geolocations::Model> for IpAddressInfo {
    fn from(ip: ip_geolocations::Model) -> Self {
        Self {
            id: ip.id,
            ip: ip.ip_address,
            country: Some(ip.country),
            region: ip.region,
            city: ip.city,
            latitude: ip.latitude,
            longitude: ip.longitude,
            created_at: ip.created_at,
            updated_at: ip.updated_at,
        }
    }
}

pub struct IpAddressService {
    db: Arc<DatabaseConnection>,
    geoip_service: Arc<GeoIpService>,
    /// In-memory cache keyed by IP string. Lets the proxy hot path (batch log
    /// enrichment, visitor tracking) resolve repeat IPs without a Postgres query,
    /// which is the dominant per-request DB load at high request rates.
    cache: Cache<String, IpAddressInfo>,
    /// Supplies the admin-configured staleness threshold for stored rows.
    ///
    /// Optional because the threshold is a data-freshness policy, not a
    /// dependency the service needs to function: callers with no settings row
    /// to read (unit tests, and the one-shot telemetry backfill whose audit
    /// entries carry no client IP at all) construct the service through
    /// [`IpAddressService::new`] and get the built-in default window. Every
    /// server path goes through [`IpAddressService::with_settings`].
    settings: Option<Arc<GeoSettingsService>>,
}

impl IpAddressService {
    /// Construct without a settings source: stored rows are re-resolved on the
    /// built-in [`temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS`] window.
    pub fn new(db: Arc<DatabaseConnection>, geoip_service: Arc<GeoIpService>) -> Self {
        Self::build(db, geoip_service, None)
    }

    /// Construct with the settings service, so the staleness window follows
    /// what the admin configured, re-read on each lookup that needs it.
    pub fn with_settings(
        db: Arc<DatabaseConnection>,
        geoip_service: Arc<GeoIpService>,
        settings: Arc<GeoSettingsService>,
    ) -> Self {
        Self::build(db, geoip_service, Some(settings))
    }

    fn build(
        db: Arc<DatabaseConnection>,
        geoip_service: Arc<GeoIpService>,
        settings: Option<Arc<GeoSettingsService>>,
    ) -> Self {
        let cache = Cache::builder()
            .max_capacity(GEO_CACHE_MAX_ENTRIES)
            .time_to_live(GEO_CACHE_TTL)
            .build();
        Self {
            db,
            geoip_service,
            cache,
            settings,
        }
    }

    /// The staleness window to apply right now.
    ///
    /// Read through `ConfigService`'s 5-second settings snapshot, so a change
    /// in the UI applies within seconds without adding a database query to
    /// every lookup. A settings read that fails degrades to the default rather
    /// than refusing to serve a geolocation.
    async fn stale_lookup_days(&self) -> i64 {
        let configured = match self.settings.as_ref() {
            Some(settings) => match settings.load().await {
                Ok(geo) => geo.effective_stale_lookup_days(),
                Err(e) => {
                    debug!(
                        error = %e,
                        default_days = temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS,
                        "could not read the geolocation settings; using the default staleness \
                         window for this lookup"
                    );
                    temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS
                }
            },
            None => temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS,
        };
        i64::from(configured)
    }

    pub async fn get_or_create_ip(&self, ip_address_str: &str) -> anyhow::Result<IpAddressInfo> {
        // Fast path: serve repeat IPs straight from memory, no DB connection used.
        if let Some(cached) = self.cache.get(ip_address_str).await {
            return Ok(cached);
        }

        let now = Utc::now();

        if let Some(existing_ip) = ip_geolocations::Entity::find()
            .filter(ip_geolocations::Column::IpAddress.eq(ip_address_str))
            .one(self.db.as_ref())
            .await?
        {
            let info = self.refresh_if_stale(existing_ip, now).await;
            self.cache
                .insert(ip_address_str.to_string(), info.clone())
                .await;
            return Ok(info);
        }

        let geo_data =
            match self
                .geoip_service
                .geolocate(ip_address_str.parse::<std::net::IpAddr>().map_err(|e| {
                    anyhow::anyhow!("Invalid IP address '{}': {}", ip_address_str, e)
                })?)
                .await
            {
                Ok(data) => Some(data),
                Err(e) => {
                    error!(
                        "Failed to get geolocation data for IP {}: {}",
                        ip_address_str, e
                    );
                    None
                }
            };

        let new_ip = ip_geolocations::ActiveModel {
            ip_address: Set(ip_address_str.to_string()),
            country: Set(geo_data
                .as_ref()
                .and_then(|d| d.country.as_deref())
                .unwrap_or("")
                .to_string()),
            country_code: Set(geo_data.as_ref().and_then(|d| d.country_code.clone())),
            region: Set(geo_data.as_ref().and_then(|d| d.region.clone())),
            city: Set(geo_data.as_ref().and_then(|d| d.city.clone())),
            latitude: Set(geo_data.as_ref().and_then(|d| d.latitude)),
            longitude: Set(geo_data.as_ref().and_then(|d| d.longitude)),
            timezone: Set(geo_data.as_ref().and_then(|d| d.timezone.clone())),
            is_eu: Set(geo_data.as_ref().map(|d| d.is_eu).unwrap_or(false)),
            asn_org: Set(geo_data.as_ref().and_then(|d| d.asn_org.clone())),
            is_hosting_provider: Set(geo_data.as_ref().and_then(|d| d.is_hosting_provider)),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        let result = new_ip.insert(self.db.as_ref()).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to create IP address record for '{}': {}",
                ip_address_str,
                e
            )
        })?;

        info!("Created new IP address record for {}", ip_address_str);
        let info: IpAddressInfo = result.into();
        self.cache
            .insert(ip_address_str.to_string(), info.clone())
            .await;
        Ok(info)
    }

    /// Re-resolve a stored row whose geolocation has outlived the configured
    /// `geo.stale_lookup_days` window, so an IP that has since been reassigned
    /// to another city stops being reported from the row written the first
    /// time it was ever seen.
    ///
    /// The re-resolution is a local mmdb lookup with no network or external
    /// call, so a burst of expired rows cannot stampede anything, and each row
    /// costs one write per staleness window -- the write happens even when the
    /// values are identical, because it is what records that the row was
    /// verified. Any failure keeps the stored row: degrading to slightly stale
    /// data beats losing it.
    async fn refresh_if_stale(
        &self,
        stored: ip_geolocations::Model,
        now: UtcDateTime,
    ) -> IpAddressInfo {
        let age_days = (now - stored.updated_at).num_days();
        if age_days < self.stale_lookup_days().await {
            return stored.into();
        }

        let ip = match stored.ip_address.parse::<std::net::IpAddr>() {
            Ok(ip) => ip,
            Err(e) => {
                debug!(
                    ip_address = stored.ip_address,
                    error = %e,
                    "stored IP geolocation row has an unparsable address; leaving it untouched"
                );
                return stored.into();
            }
        };

        let resolved = match self.geoip_service.geolocate(ip).await {
            Ok(resolved) => resolved,
            Err(e) => {
                debug!(
                    ip_address = stored.ip_address,
                    age_days,
                    error = %e,
                    "could not re-resolve a stale IP geolocation; keeping the stored row"
                );
                return stored.into();
            }
        };

        // Written back even when nothing changed, which is the common case: the
        // staleness test is purely `now - updated_at`, so returning early here
        // would leave the row eternally stale and re-resolve it again on the
        // first lookup after every cache expiry, forever, for any IP that is
        // actively used and simply never moves. The row content is identical;
        // what the write is for is advancing `updated_at` -- the record of when
        // this row was last *verified* against the current database.
        let unchanged = !geolocation_differs(&stored, &resolved);

        let ip_address = stored.ip_address.clone();
        let stored_info: IpAddressInfo = stored.clone().into();
        let mut active: ip_geolocations::ActiveModel = stored.into();
        apply_geolocation(&mut active, resolved, now);

        match active.update(self.db.as_ref()).await {
            Ok(updated) => {
                if unchanged {
                    debug!(
                        ip_address = ip_address,
                        age_days,
                        "stale IP geolocation re-resolved to the same values; refreshed only its \
                         verification time"
                    );
                } else {
                    info!(
                        ip_address = ip_address,
                        age_days, "re-resolved a stale IP geolocation against the current database"
                    );
                }
                updated.into()
            }
            Err(e) => {
                error!(
                    ip_address = ip_address,
                    error = %e,
                    "failed to persist a re-resolved IP geolocation; serving the stored row"
                );
                stored_info
            }
        }
    }

    pub async fn update_geolocation(&self, ip_id: i32) -> anyhow::Result<IpAddressInfo> {
        let ip_record = ip_geolocations::Entity::find_by_id(ip_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("IP address not found"))?;

        let geo_data = self
            .geoip_service
            .geolocate(
                ip_record
                    .ip_address
                    .parse::<std::net::IpAddr>()
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Invalid IP address in database for record {}: {}",
                            ip_id,
                            e
                        )
                    })?,
            )
            .await?;

        let mut active_model: ip_geolocations::ActiveModel = ip_record.into();
        apply_geolocation(&mut active_model, geo_data, Utc::now());

        let updated = active_model
            .update(self.db.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to update IP address record {}: {}", ip_id, e))?;

        Ok(updated.into())
    }

    pub async fn get_ip_info(&self, ip_id: i32) -> anyhow::Result<IpAddressInfo> {
        let result = ip_geolocations::Entity::find_by_id(ip_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("IP address not found"))?;

        Ok(result.into())
    }

    pub async fn list_recent_ips(&self, limit: u64) -> anyhow::Result<Vec<IpAddressInfo>> {
        let results = ip_geolocations::Entity::find()
            .order_by_desc(ip_geolocations::Column::CreatedAt)
            .limit(limit)
            .all(self.db.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load IP addresses: {}", e))?;

        Ok(results.into_iter().map(|ip| ip.into()).collect())
    }
}

/// Write `resolved` onto `active`, stamping `now` as the verification time.
fn apply_geolocation(
    active: &mut ip_geolocations::ActiveModel,
    resolved: GeoLocation,
    now: UtcDateTime,
) {
    active.country = Set(resolved.country.unwrap_or_default());
    active.country_code = Set(resolved.country_code);
    active.region = Set(resolved.region);
    active.city = Set(resolved.city);
    active.latitude = Set(resolved.latitude);
    active.longitude = Set(resolved.longitude);
    active.timezone = Set(resolved.timezone);
    active.is_eu = Set(resolved.is_eu);
    active.asn_org = Set(resolved.asn_org);
    active.is_hosting_provider = Set(resolved.is_hosting_provider);
    active.updated_at = Set(now);
}

/// Does a fresh lookup disagree with what the row already holds?
///
/// `country` is stored NOT NULL with `""` for "unknown", so the comparison
/// normalizes the resolved side the same way [`apply_geolocation`] stores it --
/// otherwise every row with an unknown country would look changed forever.
fn geolocation_differs(stored: &ip_geolocations::Model, resolved: &GeoLocation) -> bool {
    stored.country != resolved.country.clone().unwrap_or_default()
        || stored.country_code != resolved.country_code
        || stored.region != resolved.region
        || stored.city != resolved.city
        || stored.latitude != resolved.latitude
        || stored.longitude != resolved.longitude
        || stored.timezone != resolved.timezone
        || stored.is_eu != resolved.is_eu
        || stored.asn_org != resolved.asn_org
        || stored.is_hosting_provider != resolved.is_hosting_provider
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_row(now: UtcDateTime) -> ip_geolocations::Model {
        ip_geolocations::Model {
            id: 7,
            ip_address: "203.0.113.10".to_string(),
            country: "France".to_string(),
            country_code: Some("FR".to_string()),
            region: Some("Île-de-France".to_string()),
            city: Some("Paris".to_string()),
            latitude: Some(48.8566),
            longitude: Some(2.3522),
            timezone: Some("Europe/Paris".to_string()),
            is_eu: true,
            asn_org: Some("Example ISP".to_string()),
            is_hosting_provider: Some(false),
            created_at: now,
            updated_at: now,
        }
    }

    fn resolved_for(row: &ip_geolocations::Model) -> GeoLocation {
        GeoLocation {
            country: Some(row.country.clone()),
            country_code: row.country_code.clone(),
            city: row.city.clone(),
            latitude: row.latitude,
            longitude: row.longitude,
            region: row.region.clone(),
            timezone: row.timezone.clone(),
            is_eu: row.is_eu,
            asn_org: row.asn_org.clone(),
            is_hosting_provider: row.is_hosting_provider,
        }
    }

    #[test]
    fn identical_lookup_is_not_treated_as_a_change() {
        let row = stored_row(Utc::now());
        assert!(!geolocation_differs(&row, &resolved_for(&row)));
    }

    #[test]
    fn a_moved_city_is_detected() {
        let row = stored_row(Utc::now());
        let mut resolved = resolved_for(&row);
        resolved.city = Some("Lyon".to_string());
        assert!(geolocation_differs(&row, &resolved));
    }

    #[test]
    fn each_field_is_compared() {
        let row = stored_row(Utc::now());

        let mut country = resolved_for(&row);
        country.country = Some("Germany".to_string());
        assert!(geolocation_differs(&row, &country));

        let mut coordinates = resolved_for(&row);
        coordinates.latitude = Some(45.7640);
        assert!(geolocation_differs(&row, &coordinates));

        let mut timezone = resolved_for(&row);
        timezone.timezone = Some("Europe/Berlin".to_string());
        assert!(geolocation_differs(&row, &timezone));

        let mut eu = resolved_for(&row);
        eu.is_eu = false;
        assert!(geolocation_differs(&row, &eu));

        let mut hosting = resolved_for(&row);
        hosting.is_hosting_provider = Some(true);
        assert!(geolocation_differs(&row, &hosting));
    }

    /// An unknown country is stored as `""`, so a resolved `None` must compare
    /// equal -- otherwise every such row would be rewritten on every check.
    #[test]
    fn an_unknown_country_stored_as_empty_matches_a_resolved_none() {
        let mut row = stored_row(Utc::now());
        row.country = String::new();
        let mut resolved = resolved_for(&row);
        resolved.country = None;

        assert!(!geolocation_differs(&row, &resolved));
    }

    #[test]
    fn applying_a_lookup_stamps_the_verification_time() {
        let now = Utc::now();
        let row = stored_row(now - chrono::Duration::days(60));
        let mut active: ip_geolocations::ActiveModel = row.into();

        let resolved = GeoLocation {
            country: None,
            country_code: None,
            city: Some("Lyon".to_string()),
            latitude: None,
            longitude: None,
            region: None,
            timezone: None,
            is_eu: false,
            asn_org: None,
            is_hosting_provider: None,
        };
        apply_geolocation(&mut active, resolved, now);

        assert_eq!(active.country, Set(String::new()));
        assert_eq!(active.city, Set(Some("Lyon".to_string())));
        assert_eq!(active.updated_at, Set(now));
    }

    #[test]
    fn staleness_is_measured_against_the_configured_window() {
        let window = i64::from(
            temps_core::GeoSettings {
                stale_lookup_days: Some(30),
                ..temps_core::GeoSettings::default()
            }
            .effective_stale_lookup_days(),
        );
        let now = Utc::now();

        let fresh = stored_row(now - chrono::Duration::days(29));
        assert!((now - fresh.updated_at).num_days() < window);

        let stale = stored_row(now - chrono::Duration::days(31));
        assert!((now - stale.updated_at).num_days() >= window);
    }

    /// A row whose re-resolution matches what is stored must still have its
    /// verification time advanced. Staleness is measured purely as
    /// `now - updated_at`, so leaving the timestamp alone makes the row stale
    /// again on the very next lookup after the 6h cache entry expires, and an
    /// actively used IP whose geolocation never changes -- the common case --
    /// is re-resolved forever.
    #[tokio::test]
    async fn an_unchanged_re_resolution_still_advances_the_staleness_clock() {
        let now = Utc::now();
        // Exactly what the mock service resolves a public IP to, so the
        // re-resolution compares equal.
        let stored = ip_geolocations::Model {
            id: 7,
            ip_address: "203.0.113.10".to_string(),
            country: "Unknown".to_string(),
            country_code: Some("XX".to_string()),
            region: Some("Unknown".to_string()),
            city: Some("Unknown".to_string()),
            latitude: Some(0.0),
            longitude: Some(0.0),
            timezone: Some("UTC".to_string()),
            is_eu: false,
            asn_org: None,
            is_hosting_provider: None,
            created_at: now - chrono::Duration::days(400),
            updated_at: now - chrono::Duration::days(400),
        };
        let touched = ip_geolocations::Model {
            updated_at: now,
            ..stored.clone()
        };

        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![touched]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 7,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = IpAddressService::new(
            db,
            Arc::new(GeoIpService::Mock(crate::MockGeoIpService::new())),
        );

        let info = service.refresh_if_stale(stored.clone(), now).await;

        assert!(
            info.updated_at > stored.updated_at,
            "an identical re-resolution must still reset the staleness clock"
        );
        assert_eq!(info.updated_at, now);
        assert_eq!(
            info.city, stored.city,
            "the values themselves must not move"
        );
    }

    /// Without a settings source the service must still apply a sane window
    /// rather than treating every stored row as fresh forever (or as stale on
    /// every lookup, which would re-resolve the whole table).
    #[tokio::test]
    async fn the_default_window_applies_without_a_settings_source() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let service = IpAddressService::new(
            db,
            Arc::new(GeoIpService::Mock(crate::MockGeoIpService::new())),
        );

        assert_eq!(
            service.stale_lookup_days().await,
            i64::from(temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS)
        );
    }
}
