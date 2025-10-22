use crate::geoip_service::GeoIpService;
use anyhow::Context;

use chrono::Utc;
use sea_orm::{prelude::*, QueryFilter, QueryOrder, QuerySelect, Set};
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::ip_geolocations;
use tracing::{error, info};

#[derive(Debug)]
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
}

impl IpAddressService {
    pub fn new(db: Arc<DatabaseConnection>, geoip_service: Arc<GeoIpService>) -> Self {
        Self { db, geoip_service }
    }

    pub async fn get_or_create_ip(&self, ip_address_str: &str) -> anyhow::Result<IpAddressInfo> {
        let now = Utc::now();

        if let Some(existing_ip) = ip_geolocations::Entity::find()
            .filter(ip_geolocations::Column::IpAddress.eq(ip_address_str))
            .one(self.db.as_ref())
            .await?
        {
            return Ok(existing_ip.into());
        }

        let geo_data = match self
            .geoip_service
            .geolocate(
                ip_address_str
                    .parse::<std::net::IpAddr>()
                    .context("Invalid IP address")?,
            )
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
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        let result = new_ip
            .insert(self.db.as_ref())
            .await
            .context("Failed to create IP address record")?;

        info!("Created new IP address record for {}", ip_address_str);
        Ok(result.into())
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
                    .context("Invalid IP address in database")?,
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
        active_model.updated_at = Set(Utc::now());

        let updated = active_model
            .update(self.db.as_ref())
            .await
            .context("Failed to update IP address record")?;

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
            .context("Failed to load IP addresses")?;

        Ok(results.into_iter().map(|ip| ip.into()).collect())
    }
}
