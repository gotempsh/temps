use crate::geoip_service::GeoIpService;
use chrono::Utc;
use moka::future::Cache;
use sea_orm::{prelude::*, QueryFilter, QueryOrder, QuerySelect, Set};
use std::sync::Arc;
use std::time::Duration;
use temps_core::UtcDateTime;
use temps_entities::ip_geolocations;
use tracing::{error, info};

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
}

impl IpAddressService {
    pub fn new(db: Arc<DatabaseConnection>, geoip_service: Arc<GeoIpService>) -> Self {
        let cache = Cache::builder()
            .max_capacity(GEO_CACHE_MAX_ENTRIES)
            .time_to_live(GEO_CACHE_TTL)
            .build();
        Self {
            db,
            geoip_service,
            cache,
        }
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
            let info: IpAddressInfo = existing_ip.into();
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
        active_model.country = Set(geo_data.country.unwrap_or_default());
        active_model.country_code = Set(geo_data.country_code);
        active_model.region = Set(geo_data.region);
        active_model.city = Set(geo_data.city);
        active_model.latitude = Set(geo_data.latitude);
        active_model.longitude = Set(geo_data.longitude);
        active_model.timezone = Set(geo_data.timezone);
        active_model.is_eu = Set(geo_data.is_eu);
        active_model.asn_org = Set(geo_data.asn_org);
        active_model.is_hosting_provider = Set(geo_data.is_hosting_provider);
        active_model.updated_at = Set(Utc::now());

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
